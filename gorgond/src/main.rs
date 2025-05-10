// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy doesn't like gorgond_client::Error.
#![allow(clippy::result_large_err)]

use clap::Parser;
use diesel::pg::Pg;
use diesel_async::{
    pg::AsyncPgConnection,
    pooled_connection::{deadpool::Pool, AsyncDieselConnectionManager},
    AsyncConnection,
};
use gorgond_client::Url;
use miette::{Context, IntoDiagnostic, Result};
use std::{
    collections::HashMap,
    error::Error as StdError,
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::task::{AbortHandle, Id, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, enabled, info, instrument, Level};

pub mod http;
pub mod models;
pub mod schema;

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Database Connection")]
struct DatabaseArgs {
    /// Database URL.
    #[arg(long, short = 'D', env = "GORGOND_DATABASE_URL")]
    db_url: String,
}

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Events Store")]
struct EventsArgs {
    /// Base URL for public event log URLs.
    #[arg(long, short = 'e', env = "GORGON_EVENT_BASE_URL")]
    event_base_url: Url,
}

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,
    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    #[command(flatten)]
    db: DatabaseArgs,
    #[command(flatten)]
    events: EventsArgs,
    #[command(flatten)]
    http: http::HttpArgs,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(transparent)]
struct MigrationError(#[from] Box<dyn StdError + Send + Sync>);

#[instrument(name = "migrate", level = "DEBUG", skip_all)]
async fn db_migrate<C: AsyncConnection<Backend = Pg> + 'static>(conn: C) -> Result<()> {
    use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
    use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

    // Migrations are embedded into the binary.
    const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

    // Bridge the async connection back into a sync one for diesel-migrations.
    tokio::task::block_in_place(move || {
        debug!("Running migrations...");
        let mut conn = AsyncConnectionWrapper::<C>::from(conn);
        match conn.pending_migrations(MIGRATIONS)? {
            pending if pending.is_empty() => debug!("▶️ Migrations already up to date"),
            pending => {
                info!(num = pending.len(), "⏸️ Database has pending migrations");
                for migration in pending {
                    let name = migration.name();
                    let _enter = debug_span!("migration", %name).entered();
                    info!(%name, "⏩ Applying migration...");

                    conn.run_migration(&migration)?;
                }
                info!("▶️ Database successfully migrated!");
            }
        }
        Ok(())
    })
    .map_err(MigrationError)
    .context("Migrations Failed")
}

#[instrument(name = "db", level = "DEBUG", skip_all)]
async fn init_db(args: &Args) -> Result<Pool<AsyncPgConnection>> {
    debug!(db_url = args.db.db_url, "Connecting to database...");
    let cfg = AsyncDieselConnectionManager::<AsyncPgConnection>::new(&args.db.db_url);
    let pool = Pool::builder(cfg)
        .build()
        .into_diagnostic()
        .context("Couldn't build database connection pool")?;
    let conn = pool
        .get()
        .await
        .into_diagnostic()
        .context("Couldn't get connection from pool")?;
    db_migrate(conn).await?;
    Ok(pool)
}

fn init_logging(args: &Args) {
    use tracing::Level;
    use tracing_subscriber::prelude::*;

    let level = match args.verbose as i8 - args.quiet as i8 {
        ..=-2 => Level::ERROR,
        -1 => Level::WARN,
        0 => Level::INFO,
        1 => Level::DEBUG,
        2.. => Level::TRACE,
    };
    tracing_subscriber::Registry::default()
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target("axum::rejections", Level::TRACE)
                // These log way too much internal information. Comment out if debugging them.
                .with_target("tokio_postgres", Level::INFO)
                .with_default(level),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

#[instrument(name = "ctrl_c", skip_all)]
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

async fn run(args: Args) -> miette::Result<()> {
    init_logging(&args);

    let pool = init_db(&args).await?;
    let token = CancellationToken::new();
    let mut tasks = Tasks::default();
    tasks.spawn_named("Ctrl+C", wait_for_ctrlc(token.clone()));
    tasks.spawn_named(
        "HTTP Server",
        http::run(
            token.child_token(),
            args.http,
            http::S {
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

fn main() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .expect("Couldn't install miette hook");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name_fn(|| {
            static THREAD_ID: AtomicUsize = AtomicUsize::new(0);
            let id = THREAD_ID.fetch_add(1, Ordering::SeqCst);
            format!("gorgond-tokio-{id}")
        })
        .build()
        .expect("Couldn't build tokio runtime");

    if let Err(err) = rt.block_on(run(Args::parse())) {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
