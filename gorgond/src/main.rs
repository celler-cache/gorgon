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
use miette::Diagnostic;
use std::{cmp::min, error::Error as StdError};
use tracing::{debug, debug_span, info, instrument};

pub mod http;
pub mod models;
pub mod schema;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error("Database connection pool error")]
    Deadpool(#[from] diesel_async::pooled_connection::deadpool::PoolError),
    #[error("Failed to build database connection pool")]
    DeadpoolBuild(#[from] diesel_async::pooled_connection::deadpool::BuildError),
    #[error("Migrations failed")]
    MigrationError(#[source] Box<dyn StdError + Send + Sync>),
}

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Database URL.
    #[arg(long, short = 'D', env = "GORGOND_DATABASE_URL")]
    db_url: String,

    #[arg(long, short = 'H', default_value = "localhost:9999")]
    http_addr: String,
}

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
    .map_err(Error::MigrationError)
}

#[instrument(name = "db", level = "DEBUG", skip_all)]
async fn init_db(args: &Args) -> Result<Pool<AsyncPgConnection>> {
    debug!(db_url = args.db_url, "Connecting to database...");
    let cfg = AsyncDieselConnectionManager::<AsyncPgConnection>::new(&args.db_url);
    let pool = Pool::builder(cfg).build()?;
    db_migrate(pool.get().await?).await?;
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
                // tokio_postgres TRACEs every connection poll event, that's just tmi.
                .with_target("tokio_postgres::connection", min(level, Level::DEBUG))
                .with_target("axum::rejections", Level::TRACE)
                .with_default(level),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

async fn run(args: Args) -> Result<()> {
    init_logging(&args);
    http::run(&args, init_db(&args).await?).await
}

#[tokio::main]
async fn main() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .expect("Couldn't install miette hook");

    if let Err(err) = run(Args::parse()).await.map_err(miette::Report::from) {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
