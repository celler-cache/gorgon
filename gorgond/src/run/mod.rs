// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{collections::HashMap, future::Future, num::NonZero};

use clap::Parser;
use diesel_migrations::{embed_migrations, EmbeddedMigrations};
use gorgond_client::Url;
use miette::{Context, IntoDiagnostic, Result};
use tokio::task::{AbortHandle, Id, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, enabled, field::Empty, info, instrument, Level};

pub mod http;

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Events Store")]
pub struct EventsArgs {
    /// Base URL for public event log URLs.
    #[arg(long, short = 'e', env = "GORGON_EVENT_BASE_URL")]
    event_base_url: Url,
}

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// Number of worker threads to use.
    #[arg(
        long,
        short,
        default_value_t = std::thread::available_parallelism().unwrap(),
    )]
    worker_threads: NonZero<usize>,

    #[command(flatten)]
    db: gorgond_diesel::Args,
    #[command(flatten)]
    events: EventsArgs,
    #[command(flatten)]
    http: http::HttpArgs,
}

#[instrument(name = "ctrl_c", skip_all, fields(indicatif.pb_hide = Empty))]
async fn wait_for_ctrlc(token: CancellationToken) -> Result<()> {
    debug!("Waiting for Ctrl+C...");
    token
        .run_until_cancelled(tokio::signal::ctrl_c())
        .await
        .transpose()
        .into_diagnostic()
        .context("Couldn't wait for Ctrl+C")?;
    info!("Ctrl+C pressed; shutting down...");
    token.cancel();
    Ok(())
}

#[derive(Default)]
pub(crate) struct Tasks {
    pub set: JoinSet<Result<()>>,
    pub names: HashMap<Id, &'static str>,
}
impl Tasks {
    pub(crate) fn spawn_named(
        &mut self,
        name: &'static str,
        f: impl Future<Output = Result<()>> + Send + 'static,
    ) -> AbortHandle {
        let handle = self.set.spawn(f);
        self.names.insert(handle.id(), name);
        handle
    }
    fn name(&self, id: Id) -> &'static str {
        self.names.get(&id).copied().unwrap_or("[???]")
    }

    async fn join_next_with_name(&mut self) -> Option<(&'static str, Result<()>)> {
        self.set
            .join_next_with_id()
            .await
            .map(|r| match r {
                Ok((id, r)) => (id, r),
                Err(err) => (err.id(), Err(err).into_diagnostic()),
            })
            .map(|(id, r)| (self.name(id), r))
            .map(|(name, r)| (name, r.context(name)))
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Tasks Failed")]
struct TaskErrors(#[related] Vec<miette::Report>);

pub async fn run(args: Args) -> Result<()> {
    // Connect to database, run migrations.
    const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
    let pool = args.db.init(MIGRATIONS, "gorgond").await?;

    // Set up the Rayon thread pool.
    rayon::ThreadPoolBuilder::new()
        .num_threads(args.worker_threads.into())
        .thread_name(|idx| format!("gorgond-rayon{}", idx + 1))
        .build_global()
        .into_diagnostic()
        .context("Couldn't create thread pool")?;

    // Spawn tasks.
    let token = CancellationToken::new();
    let mut tasks = Tasks::default();
    tasks.spawn_named("Ctrl+C", wait_for_ctrlc(token.clone()));
    tasks.spawn_named(
        "HTTP Server",
        http::run(
            args.http,
            http::S {
                token: token.child_token(),
                pool: pool.clone(),
                events_base: args.events.event_base_url,
            },
        ),
    );

    let mut errs = None::<Vec<miette::Report>>;
    while let Some((name, r)) = tasks.join_next_with_name().await {
        match r {
            Ok(()) => debug!(name, "Task exited"),
            Err(err) => {
                if enabled!(Level::DEBUG) {
                    debug!(name, err = format!("\n{err:?}"), "Task failed");
                }
                token.cancel();
                errs.get_or_insert_with(|| Vec::with_capacity(tasks.set.len()))
                    .push(err);
            }
        }
    }
    errs.map(|errs| Err(TaskErrors(errs).into()))
        .unwrap_or(Ok(()))
}
