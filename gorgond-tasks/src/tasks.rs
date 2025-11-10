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
pub mod repo_sync;

pub enum TaskMode {
    NonBlocking,
    BlockingHeavy,
}

pub struct TaskConfig {
    pub mode: TaskMode,
    pub enabled: Option<bool>,
    pub interval: Duration,
}

pub trait TaskSource: clap::Parser + Debug + Clone + Send + Sync {
    const ID: &str;
    const FRIENDLY: &str;

    type Error: StdError + Diagnostic + Into<TransactionError> + Send + Sync + 'static;
    type Task: Display + Serialize + DeserializeOwned + Send + Sync;
    type Output: Display + Serialize + DeserializeOwned + Send + Sync;

    /// Returns the task's config.
    fn config(&self) -> TaskConfig;

    /// Finds a set of tasks for this worker to perform, locking them if applicable.
    fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> impl Future<Output = Result<impl IntoIterator<Item = Self::Task>, Self::Error>> + Send;

    /// Perform heavier work on a worker thread.
    fn work(
        &self,
        token: &CancellationToken,
        task: Self::Task,
    ) -> Result<Self::Output, Self::Error>;

    /// Finish executing the task.
    fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum TaskError<S: TaskSource + Debug> {
    #[error("Task Panicked: {}", S::FRIENDLY)]
    Join(#[from] tokio::task::JoinError),
    #[error("Task Failed: {}", S::FRIENDLY)]
    #[diagnostic(transparent)]
    Task(#[source] S::Error),
}

fn pb_work<S: TaskSource>(
    span: &Span,
    token: &CancellationToken,
    src: &S,
    task: S::Task,
) -> Result<S::Output, TaskError<S>> {
    trace!("Working...");
    src.work(token, task)
        .map_err(TaskError::Task)
        .inspect(|_| span.pb_inc(1))
}

#[instrument(skip_all)]
async fn run<S: TaskSource + 'static>(
    src: &S,
    token: &CancellationToken,
    conn: &mut AsyncPgConnection,
) -> Result<(), TaskError<S>> {
    let span = Span::current();
    span.pb_set_style(&gorgon_util_cli::PB_STYLE_SPIN);

    let cfg = src.config();

    info!("Running...");

    // Find work to do.
    let tasks = async {
        debug!("Finding tasks...");
        (src.find(conn).await)
            .map(|it| it.into_iter().collect::<Vec<S::Task>>())
            .map_err(TaskError::Task)
    }
    .instrument(info_span!(parent: &span, "find"))
    .await?;

    let num_tasks = tasks.len();
    span.pb_set_length(num_tasks as _);
    span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR);

    // Do it!
    let data: Vec<S::Output> = async {
        debug!(num_tasks, "Working...");
        match cfg.mode {
            TaskMode::NonBlocking => (tasks.into_iter())
                .map(|t| pb_work(&span, token, src, t))
                .collect::<Result<Vec<_>, TaskError<S>>>(),
            TaskMode::BlockingHeavy => tokio::task::spawn_blocking({
                let (span, src, token) = (span.clone(), src.clone(), token.clone());
                move || {
                    (tasks.into_par_iter())
                        .map(|t| pb_work(&span, &token, &src, t))
                        .collect::<Result<Vec<S::Output>, TaskError<S>>>()
                }
            })
            .await
            .map_err(TaskError::Join)?,
        }
    }
    .instrument(info_span!(parent: &span, "work"))
    .await?;

    // Finalize it.
    async {
        debug!("Finishing...");
        src.finish(conn, data).await.map_err(TaskError::Task)
    }
    .await?;

    Ok(())
}

async fn run_loop<S: TaskSource + 'static>(
    span: Span,
    token: CancellationToken,
    pool: AsyncPgPool,
    src: S,
) {
    let cfg = src.config();

    // arrested for rust crimes
    let mut ticker = tokio::time::interval(cfg.interval.into());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while async {
        debug!("Sleeping...");
        (ticker.tick())
            .with_cancellation_token(&token)
            .await
            .is_some()
    }
    .instrument(error_span!(parent: &span, "waiting", t = %cfg.interval))
    .await
    {
        let name = S::FRIENDLY;
        if let Err(err) = (async || -> miette::Result<()> {
            let mut conn = async {
                debug!("Getting database connection...");
                pool.get().await
            }
            .instrument(debug_span!("wait_for_connection"))
            .await
            .map_err(TransactionError::from)?;

            conn.transaction(|conn| {
                async { run(&src, &token, conn).await.map_err(TransactionError::new) }.scope_boxed()
            })
            .await?;

            Ok(())
        })
        .retry(ExponentialBuilder::default())
        .sleep(tokio::time::sleep)
        .when(|_| !token.is_cancelled())
        .notify(|err, d| {
            let d = Duration::from(d);
            warn!("Task Failed ({name}) -- retrying in {d}...\n\n{err:?}");
        })
        .await
        {
            if !token.is_cancelled() {
                warn!("Task Failed ({name}) -- going back to sleep...\n\n{err:?}");
            }
        }
    }
}

#[derive(Debug, Clone, clap::Parser)]
pub struct AllTaskArgs {
    #[command(flatten)]
    pub bisect_templates: bisect_templates::BisectTemplateSource,
    #[command(flatten)]
    pub repo_sync: repo_sync::RepoSyncTaskSource,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "Tasks")]
pub struct TaskRunnerArgs {
    /// Run all tasks, except those individiually disabled.
    #[arg(long, short)]
    pub all_tasks: bool,
}
impl TaskRunnerArgs {
    #[instrument(name = "tasks", skip_all)]
    pub async fn run_all(self, token: &CancellationToken, pool: &AsyncPgPool, tasks: AllTaskArgs) {
        Span::current().pb_set_style(&gorgon_util_cli::PB_STYLE_BLANK);

        let mut set = JoinSet::new();
        self.spawn(&mut set, token, pool, tasks.bisect_templates);
        self.spawn(&mut set, token, pool, tasks.repo_sync);
        while set.join_next().await.is_some() {}
    }

    fn spawn<S: TaskSource + 'static>(
        &self,
        set: &mut JoinSet<()>,
        token: &CancellationToken,
        pool: &AsyncPgPool,
        src: S,
    ) {
        let parent = Span::current();
        let span = info_span!(parent: parent, "task", kind = S::ID);
        span.pb_set_style(&gorgon_util_cli::PB_STYLE_BLANK);

        if src.config().enabled.unwrap_or(self.all_tasks) {
            set.spawn(run_loop(span.clone(), token.clone(), pool.clone(), src).instrument(span));
        } else {
            debug!("Skipping disabled task...");
        }
    }
}
