// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::collections::{BTreeMap, BTreeSet};

use chrono::prelude::*;
use clap::Parser;
use derive_more::Display;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use gorgon_build_helper::helper::HelperCaller;
use gorgond_diesel::{TransactionError, models, schema::gorgond as schema};
use humantime::Duration;
use miette::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::tasks::{TaskMode, TaskSource};

use super::TaskConfig;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum BisectTemplateError {
    #[error(transparent)]
    Diesel(#[from] diesel::result::Error),
}
impl From<BisectTemplateError> for TransactionError {
    fn from(err: BisectTemplateError) -> Self {
        match err {
            BisectTemplateError::Diesel(err) => TransactionError::from(err),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Queryable, Selectable)]
pub struct MergeData {
    #[diesel(embed)]
    pub merge: models::RepositoryMergeRequest,
    #[diesel(embed)]
    pub version: models::RepositoryMergeRequestVersion,
    #[diesel(embed)]
    pub source: models::CommitSource,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct InputData {
    pub input: models::BuildTemplateInput,
    pub merges: BTreeMap<Uuid, MergeData>,
}
#[derive(Debug, Display, Serialize, Deserialize)]
#[display("{}", template.id)]
pub struct BisectTemplateTask {
    pub template: models::BuildTemplate,
    pub inputs: BTreeMap<Uuid, InputData>,
}

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Task: Bisect Templates")]
pub struct BisectTemplateSource {
    #[arg(long, default_missing_value = "true", num_args=0..=1)]
    pub task_bisect_templates: Option<bool>,
    #[arg(long, default_value = "10s")]
    pub task_bisect_templates_interval: Duration,
}
impl TaskSource for BisectTemplateSource {
    const ID: &str = "BisectTemplates";
    const FRIENDLY: &str = "Bisect Templates";

    type Error = BisectTemplateError;
    type Task = BisectTemplateTask;
    type Output = BisectTemplateTask;

    fn config(&self) -> TaskConfig {
        TaskConfig {
            mode: TaskMode::NonBlocking,
            enabled: self.task_bisect_templates,
            interval: self.task_bisect_templates_interval,
        }
    }

    type Context = ();
    fn init(&self, _helpers: &impl HelperCaller) -> miette::Result<Self::Context> {
        Ok(())
    }

    async fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> Result<impl IntoIterator<Item = Self::Task>, Self::Error> {
        schema::build_template_inputs::table
            .inner_join(schema::build_templates::table)
            // Exclude inputs that already have active bisections.
            .filter(
                schema::build_template_inputs::id.ne_all(
                    schema::bisections::table
                        .filter(schema::bisections::ended_at.is_null())
                        .filter(schema::bisections::input_id.is_not_null())
                        .select(schema::bisections::input_id.assume_not_null()),
                ),
            )
            // Find associated MRs and latest version.
            .inner_join(
                schema::repository_merge_requests::table
                    .on(schema::repository_merge_requests::repository_id
                        .eq(schema::build_template_inputs::repository_id)
                        // Only open MRs.
                        .and(schema::repository_merge_requests::closed_at.is_null()))
                    .inner_join(
                        schema::repository_merge_request_versions::table
                            .on(schema::repository_merge_request_versions::merge_request_id
                                .eq(schema::repository_merge_requests::id)),
                    )
                    .inner_join(
                        schema::commit_sources::table.on(schema::commit_sources::id
                            .eq(schema::repository_merge_request_versions::source_id
                                .assume_not_null())),
                    ),
            )
            .distinct_on(schema::repository_merge_request_versions::merge_request_id)
            // --
            .select((
                models::BuildTemplate::as_select(),
                models::BuildTemplateInput::as_select(),
                MergeData::as_select(),
            ))
            .load_stream::<(models::BuildTemplate, models::BuildTemplateInput, MergeData)>(conn)
            .await?
            .try_fold(
                BTreeMap::new(),
                async |mut templates, (template, input, merge)| {
                    templates
                        .entry(template.id)
                        .or_insert_with(|| BisectTemplateTask {
                            template,
                            inputs: BTreeMap::new(),
                        })
                        .inputs
                        .entry(input.id)
                        .or_insert_with(|| InputData {
                            input,
                            merges: BTreeMap::new(),
                        })
                        .merges
                        .entry(merge.merge.id)
                        .or_insert(merge);
                    Ok(templates)
                },
            )
            .await
            .map(|v| v.into_values())
            .map_err(|err| err.into())
    }

    fn work(
        &self,
        _token: &CancellationToken,
        _ctx: &Self::Context,
        task: Self::Task,
    ) -> Result<Self::Output, Self::Error> {
        Ok(task)
    }

    async fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> Result<(), Self::Error> {
        let now = Utc::now();

        for BisectTemplateTask { template, inputs } in data {
            info!(%template.id, template.slug, "-> Template...");
            for (_, InputData { input, merges }) in inputs {
                info!(%input.id, input.key, merges = merges.len(), "--> Input...");

                // The idempotency hash is a SHA256 of the UUIDs (not hashes) of the commits,
                // in binary form, sorted and deduplicated.
                let source_ids: BTreeSet<Uuid> = merges.values().map(|m| m.source.id).collect();
                let source_hash = (source_ids.iter())
                    .fold(Sha256::new(), Sha256::chain_update)
                    .finalize()
                    .to_vec();

                // Create or reuse a rollup.
                let rollup_id = Uuid::now_v7();
                let rollup_id = diesel::insert_into(schema::rollups::table)
                    .values(models::Rollup {
                        id: rollup_id,
                        repository_id: input.repository_id,
                        source_hash,
                        rolled_at: None,
                        updated_at: now,
                        created_at: now,
                    })
                    .on_conflict((schema::rollups::repository_id, schema::rollups::source_hash))
                    .do_nothing()
                    .returning(schema::rollups::id)
                    .get_result::<Uuid>(conn)
                    .await
                    .optional()?
                    .unwrap_or(rollup_id);
                diesel::insert_into(schema::rollup_inputs::table)
                    .values(
                        (source_ids.into_iter())
                            .map(|source_id| models::RollupInput {
                                rollup_id,
                                source_id,
                            })
                            .collect::<Vec<_>>(),
                    )
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?;

                // Then create a bisection job for the rollup.
                diesel::insert_into(schema::bisections::table)
                    .values(models::Bisection {
                        id: Uuid::now_v7(),
                        rollup_id,
                        template_id: Some(input.template_id),
                        template_kind: template.kind.clone(),
                        template_expr: template.expr.clone(),
                        input_id: Some(input.id),
                        input_key: input.key,
                        started_at: None,
                        ended_at: None,
                        updated_at: now,
                        created_at: now,
                    })
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?;
            }
        }

        Ok(())
    }
}
