// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::collections::{BTreeMap, BTreeSet};

use chrono::prelude::*;
use clap::Parser;
use conflate::Merge;
use derive_more::Display;
use diesel::{prelude::*, sql_query, sql_types};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use gorgond_diesel::{TransactionError, models, schema::gorgond as schema};
use humantime::Duration;
use itertools::Itertools;
use miette::Result;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, info_span, instrument};
use uuid::Uuid;

use crate::tasks::TaskSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectTaskStep {
    pub step: models::BisectionStep,
    pub build: models::Build,
    pub has_forks: bool,
}
impl BisectTaskStep {
    fn new(step: models::BisectionStep, build: models::Build, has_forks: bool) -> Self {
        Self {
            step,
            build,
            has_forks,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectTaskRollupBatch {
    pub batch: models::RollupBatch,
    pub commits: BTreeSet<Uuid>,
    pub step: Option<BisectTaskStep>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectTaskRollup {
    pub rollup: models::Rollup,
    pub batches: BTreeMap<Uuid, BisectTaskRollupBatch>,
}
impl BisectTaskRollup {
    fn new(rollup: models::Rollup) -> Self {
        Self {
            rollup,
            batches: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Display, Serialize, Deserialize)]
#[display("{}", bisection.id)]
pub struct BisectTask {
    pub bisection: models::Bisection,
    pub rollups: BTreeMap<Uuid, BisectTaskRollup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedStep {
    pub step: models::BisectionStep,
    pub build: models::Build,
    pub build_input: models::BuildInput,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedBatch {
    pub batch: models::RollupBatch,
    pub resolved_step: Option<ResolvedStep>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRollup {
    pub rollup: models::Rollup,
    pub resolved_batches: BTreeMap<Uuid, ResolvedBatch>,
}
#[derive(Debug, Clone, Display, Serialize, Deserialize)]
#[display("{}", bisection.id)]
pub struct BisectOutput {
    pub bisection: models::Bisection,
    pub resolved_rollups: BTreeMap<Uuid, ResolvedRollup>,
}

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Task: Bisect")]
pub struct Bisect {
    #[arg(long, default_missing_value = "true", num_args=0..=1)]
    pub task_bisect: Option<bool>,
    #[arg(long, default_value = "10s")]
    pub task_bisect_interval: Duration,
}
impl TaskSource for Bisect {
    const ID: &str = "Bisect";
    const FRIENDLY: &str = "Bisect ";

    type Error = TransactionError;
    type Task = BisectTask;
    type Output = BisectOutput;

    fn config(&self) -> super::TaskConfig {
        super::TaskConfig {
            mode: super::TaskMode::NonBlocking,
            enabled: self.task_bisect,
            interval: self.task_bisect_interval,
        }
    }

    type Context = ();
    fn init(
        &self,
        _helpers: &impl gorgon_build_helper::helper::HelperCaller,
    ) -> miette::Result<Self::Context> {
        Ok(())
    }

    async fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> miette::Result<impl IntoIterator<Item = Self::Task>, Self::Error> {
        // When doing a FOR [NO KEY] UPDATE with LEFT JOINs, we need to specify of what.
        // Diesel doesn't currently have a DSL to do this, so drop down to raw SQL.
        //
        // FOOTGUN WARNING: Doing raw SQL queries uses QueryableByName, not Queryable.
        // This means that fields are unmarshalled by column name, which causes silently
        // wrong results if eg. you SELECT * FROM two different tables with "id" columns.
        #[derive(Debug, QueryableByName)]
        #[diesel(check_for_backend(diesel::pg::Pg))]
        struct Row {
            #[diesel(sql_type = sql_types::Uuid)]
            bisection_id: Uuid,
            #[diesel(sql_type = sql_types::Uuid)]
            rollup_id: Uuid,
            #[diesel(sql_type = sql_types::Uuid)]
            batch_id: Uuid,
            #[diesel(sql_type = sql_types::Nullable<sql_types::Uuid>)]
            step_id: Option<Uuid>,
            #[diesel(sql_type = sql_types::Bool)]
            has_forks: bool,
        }
        let rows = sql_query(
            r#"
SELECT br.bisection_id, batch.rollup_id, batch.id AS batch_id,
       step.id AS step_id, build.id AS build_id, build.result AS build_result,
       forks.num IS NOT NULL AS has_forks
  FROM gorgond.bisection_rollups AS br
  INNER JOIN gorgond.rollup_batches AS batch ON (br.rollup_id = batch.rollup_id)
  LEFT JOIN gorgond.bisection_steps AS step ON (step.batch_id = batch.id)
  LEFT JOIN gorgond.builds AS build ON (build.id = step.build_id)

  -- Count number of "forks" originating from this step.
  LEFT JOIN (
    SELECT COUNT(br.rollup_id) AS num, br.fork_step_id AS step_id
    FROM gorgond.bisection_rollups AS br
    GROUP BY br.fork_step_id) AS forks ON (forks.step_id = step.id)

  -- 1. Does this batch lack a step or build?
  WHERE (step.id IS NULL OR build.id IS NULL)
  -- 2. Did this step fail, but no forks have been spawned?
    OR (build.result = 'failure' AND forks.num IS NULL)

  ORDER BY rollup_id, batch_id
  FOR NO KEY UPDATE OF batch SKIP LOCKED
"#,
        )
        .get_results::<Row>(conn)
        .await?;

        // Find rollups.
        let rollups = schema::rollups::table
            .filter(schema::rollups::id.eq_any(rows.iter().map(|r| r.rollup_id).dedup()))
            .load_stream::<models::Rollup>(conn)
            .await?
            .map_ok(|r| (r.id, BisectTaskRollup::new(r)))
            .try_collect::<BTreeMap<Uuid, BisectTaskRollup>>()
            .await?;
        // Fill in batches and commits.
        let rollups = schema::rollup_batches::table
            .inner_join(
                schema::rollup_batch_commits::table
                    .on(schema::rollup_batch_commits::batch_id.eq(schema::rollup_batches::id)),
            )
            .filter(schema::rollup_batches::id.eq_any(rows.iter().map(|r| r.batch_id).dedup()))
            .load_stream::<(models::RollupBatch, models::RollupBatchCommit)>(conn)
            .await?
            .try_fold(rollups, async |mut rollups, (batch, bc)| {
                rollups
                    .get_mut(&batch.rollup_id)
                    .expect("batch for unknown rollup!?")
                    .batches
                    .entry(batch.id)
                    .or_insert(BisectTaskRollupBatch {
                        batch,
                        commits: BTreeSet::new(),
                        step: None,
                    })
                    .commits
                    .insert(bc.commit_id);
                Ok(rollups)
            })
            .await?;

        // Find bisections, and split the rollups up between them.
        let bisects = schema::bisections::table
            .filter(schema::bisections::id.eq_any(rows.iter().map(|r| r.bisection_id).dedup()))
            .load_stream::<models::Bisection>(conn)
            .await?
            .try_fold(BTreeMap::new(), async |mut bisects, bisection| {
                bisects.insert(
                    bisection.id,
                    BisectTask {
                        rollups: (rows.iter())
                            .filter(|r| r.bisection_id == bisection.id)
                            .filter_map(|r| rollups.get(&r.rollup_id).cloned())
                            .map(|r| (r.rollup.id, r))
                            .collect(),
                        bisection,
                    },
                );
                Ok(bisects)
            })
            .await?;
        // Fill in bisection steps. Remember that the same rollup can appear in multiple,
        // distinct bisections, and so we can't do this on the rollups stage.
        let bisects = schema::bisection_steps::table
            .inner_join(
                schema::builds::table.on(schema::builds::id.eq(schema::bisection_steps::build_id)),
            )
            .filter(
                schema::bisection_steps::id.eq_any(rows.iter().filter_map(|r| r.step_id).dedup()),
            )
            .load_stream::<(models::BisectionStep, models::Build)>(conn)
            .await?
            .try_fold(bisects, async |mut bisects, (step, build)| {
                let row = (rows.iter())
                    .find(|r| r.step_id == Some(step.id))
                    .expect("unknown step!?");
                bisects
                    .get_mut(&row.bisection_id)
                    .expect("step for unknown bisection!?")
                    .rollups
                    .get_mut(&row.rollup_id)
                    .expect("step for unknown rollup!?")
                    .batches
                    .get_mut(&row.batch_id)
                    .expect("step for unknown batch!?")
                    .step = Some(BisectTaskStep::new(step, build, row.has_forks));
                Ok(bisects)
            })
            .await?;

        Ok(bisects.into_values())
    }

    fn work(
        &self,
        _: &CancellationToken,
        _: &Self::Context,
        task: BisectTask,
    ) -> miette::Result<BisectOutput, Self::Error> {
        Ok(self.resolve_task(Utc::now(), task))
    }

    async fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> miette::Result<(), Self::Error> {
        // This is silly, but basically exists for debuggability.
        #[derive(Default)]
        struct MergedData {
            pub builds: Vec<models::Build>,
            pub build_inputs: Vec<models::BuildInput>,
            pub steps: Vec<models::BisectionStep>,
        }
        let MergedData {
            builds,
            build_inputs,
            steps,
        } = data
            .into_iter()
            .flat_map(|d| d.resolved_rollups.into_values())
            .flat_map(|d| d.resolved_batches.into_values())
            .flat_map(|d| d.resolved_step)
            .fold(
                MergedData::default(),
                |mut data,
                 ResolvedStep {
                     step,
                     build,
                     build_input,
                 }| {
                    data.steps.push(step);
                    data.builds.push(build);
                    data.build_inputs.push(build_input);
                    data
                },
            );

        diesel::insert_into(schema::builds::table)
            .values(builds)
            .execute(conn)
            .await?;
        diesel::insert_into(schema::build_inputs::table)
            .values(build_inputs)
            .execute(conn)
            .await?;
        diesel::insert_into(schema::bisection_steps::table)
            .values(steps)
            .execute(conn)
            .await?;

        Ok(())
    }
}

impl Bisect {
    fn resolve_task(
        &self,
        now: DateTime<Utc>,
        BisectTask { bisection, rollups }: BisectTask,
    ) -> BisectOutput {
        BisectOutput {
            resolved_rollups: (rollups.into_values())
                .filter_map(|r| self.resolve_rollup(now, &bisection, r))
                .map(|r| (r.rollup.id, r))
                .collect(),
            bisection,
        }
    }
    #[instrument(name = "rollup", skip_all, fields(id = %rollup.id))]
    fn resolve_rollup(
        &self,
        now: DateTime<Utc>,
        b: &models::Bisection,
        BisectTaskRollup { rollup, batches }: BisectTaskRollup,
    ) -> Option<ResolvedRollup> {
        Some(ResolvedRollup {
            resolved_batches: (batches.into_values())
                .filter_map(|batch| self.resolve_batch(now, b, batch))
                .map(|b| (b.batch.id, b))
                .collect(),
            rollup,
        })
        .filter(|r| !r.resolved_batches.is_empty())
    }
    #[instrument(name = "batch", skip_all, fields(id = %batch.id))]
    fn resolve_batch(
        &self,
        now: DateTime<Utc>,
        b: &models::Bisection,
        BisectTaskRollupBatch { batch, step, .. }: BisectTaskRollupBatch,
    ) -> Option<ResolvedBatch> {
        Some(ResolvedBatch {
            resolved_step: (step.is_none()).then(|| self.resolve_step(now, b, &batch)),
            batch,
        })
        .filter(|rb| rb.resolved_step.is_some())
    }
    #[instrument(name = "step")]
    fn resolve_step(
        &self,
        now: DateTime<Utc>,
        b: &models::Bisection,
        batch: &models::RollupBatch,
    ) -> ResolvedStep {
        let build_id = Uuid::now_v7();
        ResolvedStep {
            step: models::BisectionStep {
                id: Uuid::now_v7(),
                bisection_id: b.id,
                batch_id: batch.id,
                build_id,
                updated_at: now,
                created_at: now,
            },
            build: models::Build {
                id: build_id,
                project_id: b.project_id,
                kind: b.template_kind.clone(),
                expr: b.template_expr.clone(),
                worker_id: None,
                result: None,
                allocated_at: None,
                heartbeat_at: None,
                started_at: None,
                ended_at: None,
                updated_at: now,
                created_at: now,
            },
            build_input: models::BuildInput {
                id: Uuid::now_v7(),
                build_id,
                key: b.input_key.clone(),
                source: String::new(),
                commit_id: Some(batch.commit_id),
            },
        }
    }
}
