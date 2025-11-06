// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    error::Error as StdError,
    fmt::{Debug, Display},
};

use backon::{ExponentialBuilder, Retryable};
use diesel_async::{AsyncConnection, AsyncPgConnection};
use gorgond_diesel::{AsyncPgPool, TransactionError};
use humantime::Duration;
use miette::{Diagnostic, Result};
use rayon::prelude::*;
use scoped_futures::ScopedFutureExt;
use serde::{Serialize, de::DeserializeOwned};
use tokio::task::JoinSet;
use tokio_util::{future::FutureExt, sync::CancellationToken};
use tracing::{
    Instrument, Span, debug, debug_span, error_span, info, info_span, instrument, trace, warn,
};
use tracing_indicatif::span_ext::IndicatifSpanExt;

pub mod bisect_templates;

pub enum TaskMode {
    NonBlocking,
    BlockingHeavy,
}

pub struct TaskConfig {
    pub mode: TaskMode,
    pub interval: Duration,
}

pub trait TaskSource: clap::Parser + Debug + Clone + Send + Sync {
    const ID: &str;
    const FRIENDLY: &str;

    type Error: StdError + Diagnostic + Into<TransactionError> + Send + Sync + 'static;
    type Task: Display + Serialize + DeserializeOwned + Send + Sync;
    type Output: Display + Serialize + DeserializeOwned + Send + Sync;

    /// Returns the task's config.
    fn config(&self) -> Option<TaskConfig>;

    /// Finds a set of tasks for this worker to perform, locking them if applicable.
    fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> impl Future<Output = Result<impl IntoIterator<Item = Self::Task>, Self::Error>> + Send;

    /// Perform heavier work on a worker thread.
    fn work(&self, task: Self::Task) -> Result<Self::Output, Self::Error>;

    /// Finish executing the task.
    fn finish<D: IntoIterator<Item = Self::Output> + Send>(
        &self,
        conn: &mut AsyncPgConnection,
        data: D,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        D::IntoIter: Send;
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum TaskError<S: TaskSource + Debug> {
    #[error("Task Panicked: {}", S::FRIENDLY)]
    Join(#[from] tokio::task::JoinError),
    #[error("Task Failed: {}", S::FRIENDLY)]
    #[diagnostic(transparent)]
    Task(#[source] S::Error),
}

#[instrument(name = "work", skip_all, fields(task = %task))]
fn pb_work<S: TaskSource>(span: &Span, src: &S, task: S::Task) -> Result<S::Output, TaskError<S>> {
    trace!("Working...");
    src.work(task)
        .map_err(TaskError::Task)
        .inspect(|_| span.pb_inc(1))
}

#[instrument(skip_all)]
async fn run<S: TaskSource + 'static>(
    src: &S,
    conn: &mut AsyncPgConnection,
) -> Result<(), TaskError<S>> {
    let span = Span::current();
    span.pb_set_style(&gorgon_util_cli::PB_STYLE_SPIN);

    // Skip disabled tasks...
    let Some(cfg) = src.config() else {
        debug!(task = S::ID, "Task is disabled, skipping...");
        return Ok(());
    };
    info!("Running...");

    // Find work to do.
    let tasks = async {
        debug!("Finding tasks...");
        (src.find(conn).await)
            .map(|it| it.into_iter().collect::<Vec<S::Task>>())
            .map_err(TaskError::Task)
    }
    .instrument(info_span!("find"))
    .await?;

    let num_tasks = tasks.len();
    span.pb_set_length(num_tasks as _);
    span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR);

    // Do it!
    let data: Vec<S::Output> = async {
        debug!(num_tasks, "Working...");
        match cfg.mode {
            TaskMode::NonBlocking => (tasks.into_iter())
                .map(|t| pb_work(&span, src, t))
                .collect::<Result<Vec<_>, TaskError<S>>>(),
            TaskMode::BlockingHeavy => tokio::task::spawn_blocking({
                let src = src.clone();
                move || {
                    (tasks.into_par_iter())
                        .map(|t| pb_work(&span, &src, t))
                        .collect::<Result<Vec<S::Output>, TaskError<S>>>()
                }
            })
            .await
            .map_err(TaskError::Join)?,
        }
    }
    .instrument(info_span!("work"))
    .await?;

    // Finalize it.
    async {
        debug!("Finishing...");
        src.finish(conn, data).await.map_err(TaskError::Task)
    }
    .await?;

    Ok(())
}

#[instrument(name = "task", skip_all, fields(kind = S::ID))]
pub async fn run_loop<S: TaskSource + 'static>(
    token: CancellationToken,
    pool: AsyncPgPool,
    src: S,
) {
    Span::current().pb_set_style(&gorgon_util_cli::PB_STYLE_BLANK);

    let Some(cfg) = src.config() else {
        debug!("Task disabled; skipping...");
        return;
    };

    let mut ticker = tokio::time::interval(cfg.interval.into());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while async {
        debug!("Sleeping...");
        (ticker.tick())
            .with_cancellation_token(&token)
            .await
            .is_some()
    }
    .instrument(error_span!("waiting", t = %cfg.interval))
    .await
    {
        (async || -> Result<(), TransactionError> {
            let mut conn = async {
                debug!("Getting database connection...");
                pool.get().await
            }
            .instrument(debug_span!("wait_for_connection"))
            .await?;

            conn.transaction(|conn| {
                async { run(&src, conn).await.map_err(TransactionError::new) }.scope_boxed()
            })
            .await
        })
        .retry(ExponentialBuilder::default())
        .sleep(tokio::time::sleep)
        .notify(|err, d| {
            let (t, d) = (S::FRIENDLY, Duration::from(d));
            warn!("Task Failed ({t}) -- retrying in {d}...\n\n{err:?}");
        })
        .await
        .expect("Failed task not retried!?");
    }
}

#[derive(Debug, Clone, clap::Parser)]
pub struct AllTasks {
    #[command(flatten)]
    pub bisect_templates: bisect_templates::BisectTemplateSource,
}
impl AllTasks {
    #[instrument(name = "tasks", skip_all)]
    pub async fn run_all(self, token: &CancellationToken, pool: &AsyncPgPool) {
        Span::current().pb_set_style(&gorgon_util_cli::PB_STYLE_BLANK);

        let mut tasks = JoinSet::new();
        tasks.spawn(run_loop(token.clone(), pool.clone(), self.bisect_templates));
        while tasks.join_next().await.is_some() {}
    }
}
