// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use derive_more::Display;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use gorgond_diesel::{TransactionError, models, schema::gorgond as schema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BisectJanitorReason {
    NoRollups,
}

#[derive(Debug, Display, Clone, Serialize, Deserialize)]
#[display("{}", bisection.id)]
pub struct BisectJanitorTask {
    pub reason: BisectJanitorReason,
    pub bisection: models::Bisection,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "Task: Bisect")]
pub struct BisectJanitor {
    #[arg(long, default_missing_value = "true", num_args=0..=1)]
    pub task_bisect_janitor: Option<bool>,
    #[arg(long, default_value = "10s")]
    pub task_bisect_janitor_interval: humantime::Duration,
}
impl crate::tasks::TaskSource for BisectJanitor {
    const ID: &str = "Bisect Janitor";
    const FRIENDLY: &str = "Bisect Janitor";

    type Error = gorgond_diesel::TransactionError;
    type Task = BisectJanitorTask;
    type Output = BisectJanitorTask;

    fn config(&self) -> super::TaskConfig {
        super::TaskConfig {
            mode: super::TaskMode::NonBlocking,
            enabled: self.task_bisect_janitor,
            interval: self.task_bisect_janitor_interval,
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
        conn: &mut diesel_async::AsyncPgConnection,
    ) -> miette::Result<impl IntoIterator<Item = Self::Task>, Self::Error> {
        let no_rollups = schema::bisections::table
            .filter(
                schema::bisections::id.ne_all(
                    schema::bisection_rollups::table
                        .select(schema::bisection_rollups::bisection_id)
                        .distinct(),
                ),
            )
            .distinct()
            .load_stream::<models::Bisection>(conn)
            .await?
            .map_ok(|bisection| BisectJanitorTask {
                bisection,
                reason: BisectJanitorReason::NoRollups,
            })
            .try_collect::<Vec<BisectJanitorTask>>()
            .await?;

        Ok(no_rollups.into_iter())
    }

    fn work(
        &self,
        _: &tokio_util::sync::CancellationToken,
        _: &Self::Context,
        task: BisectJanitorTask,
    ) -> miette::Result<BisectJanitorTask, Self::Error> {
        Ok(task)
    }

    async fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> miette::Result<(), Self::Error> {
        diesel::delete(
            schema::bisections::table
                .filter(schema::bisections::id.eq_any(data.into_iter().map(|t| t.bisection.id))),
        )
        .execute(conn)
        .await?;
        Ok(())
    }
}
