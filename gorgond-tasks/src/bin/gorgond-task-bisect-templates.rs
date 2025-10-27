// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::collections::BTreeMap;

use chrono::prelude::*;
use clap::Parser;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use gorgond_diesel::{TransactionError, models, schema::gorgond as schema};
use miette::{IntoDiagnostic, Result};
use scoped_futures::ScopedFutureExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, Parser)]
pub struct Args {
    #[command(flatten)]
    db: gorgond_diesel::Args,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

#[derive(Debug, Serialize, Queryable, Selectable)]
struct MergeData {
    #[diesel(embed)]
    pub merge: models::RepositoryMergeRequest,
    #[diesel(embed)]
    pub version: models::RepositoryMergeRequestVersion,
    #[diesel(embed)]
    pub source: models::CommitSource,
    #[diesel(embed)]
    pub commit: models::Commit,
}
#[derive(Debug, Serialize)]
struct InputData {
    pub input: models::BuildTemplateInput,
    pub merges: BTreeMap<Uuid, MergeData>,
}
#[derive(Debug, Serialize)]
struct TemplateData {
    pub template: models::BuildTemplate,
    pub inputs: BTreeMap<Uuid, InputData>,
}

async fn find(
    conn: &mut AsyncPgConnection,
) -> Result<BTreeMap<Uuid, TemplateData>, TransactionError> {
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
                    .eq(schema::build_template_inputs::repository_id))
                .inner_join(
                    schema::repository_merge_request_versions::table
                        .on(schema::repository_merge_request_versions::merge_request_id
                            .eq(schema::repository_merge_requests::id)),
                )
                .inner_join(
                    schema::commit_sources::table.on(
                        schema::commit_sources::id
                            .eq(schema::repository_merge_request_versions::source_id
                                .assume_not_null()),
                    ),
                )
                .inner_join(schema::commits::table.on(
                    schema::commits::id.eq(schema::commit_sources::commit_id.assume_not_null()),
                )),
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
                    .or_insert_with(|| TemplateData {
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
        .map_err(|err| err.into())
}

async fn poll(_args: &Args, conn: &mut AsyncPgConnection) -> Result<(), TransactionError> {
    let now = Utc::now();
    let lonely_versions = find(conn).await?;
    colored_json::write_colored_json(&lonely_versions, &mut std::io::stdout()).into_diagnostic()?;

    for (_, TemplateData { template, inputs }) in lonely_versions {
        info!(%template.id, template.slug, "-> Template...");
        for (_, InputData { input, merges }) in inputs {
            info!(%input.id, input.key, merges = merges.len(), "--> Input...");

            let mut commit_ids = merges.values().map(|m| m.commit.id).collect::<Vec<_>>();
            commit_ids.sort();
            commit_ids.dedup();
            let idempotency_hash = (commit_ids.iter())
                .fold(Sha256::new(), |hash, id| hash.chain_update(id))
                .finalize()
                .to_vec();

            let bisection_id = Uuid::now_v7();
            let bisection_id = diesel::insert_into(schema::bisections::table)
                .values(models::Bisection {
                    id: bisection_id,
                    idempotency_hash,
                    repository_id: Some(input.repository_id),
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
                .on_conflict(schema::bisections::idempotency_hash)
                .do_nothing()
                .returning(schema::bisections::id)
                .get_result::<Uuid>(conn)
                .await
                .optional()?
                .unwrap_or(bisection_id);
            diesel::insert_into(schema::bisection_commits::table)
                .values(
                    (commit_ids.into_iter())
                        .map(|commit_id| models::BisectionCommit {
                            bisection_id,
                            commit_id,
                        })
                        .collect::<Vec<_>>(),
                )
                .execute(conn)
                .await?;
        }
    }

    Ok(())
}

async fn run(args: Args) -> Result<()> {
    let pool = args.db.connect().await?;
    let mut conn = pool.get().await.into_diagnostic()?;
    conn.transaction(|conn| async { poll(&args, conn).await }.scope_boxed())
        .await?;

    Ok(())
}

fn main() {
    let args = Args::parse();

    gorgon_util_cli::logger(args.common).init();
    gorgon_util_cli::main_async(args.common, run(args));
}
