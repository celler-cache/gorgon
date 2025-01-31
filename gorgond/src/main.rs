// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::Result;
use clap::Parser;
use diesel::pg::Pg;
use diesel_async::{
    AsyncConnection,
    pg::AsyncPgConnection,
    pooled_connection::{AsyncDieselConnectionManager, deadpool::Pool},
};
use std::{cmp::min, error::Error as StdError};
use tracing::{debug, debug_span, info, instrument};

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Database URL.
    #[arg(long, short, env = "GORGOND_DATABASE_URL")]
    database_url: String,
}

#[instrument(skip_all)]
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
                .with_default(level),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

#[instrument(skip_all)]
async fn run_migrations<C: AsyncConnection<Backend = Pg> + 'static>(conn: C) -> Result<()> {
    use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
    use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

    // Migrations are embedded into the binary.
    const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

    // anyhow::Error can't deal with a Box containing an un-Sized error.
    #[derive(Debug, thiserror::Error)]
    #[error(transparent)]
    struct MigrationError(#[from] Box<dyn StdError + Send + Sync>);

    // Bridge the async connection back into a sync one for diesel-migrations.
    tokio::task::block_in_place(move || -> Result<(), MigrationError> {
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
    })?;

    Ok(())
}

#[instrument(skip_all, fields(args.database_url))]
async fn init_db(args: &Args) -> Result<Pool<AsyncPgConnection>> {
    let cfg = AsyncDieselConnectionManager::<AsyncPgConnection>::new(&args.database_url);
    let pool = Pool::builder(cfg).build()?;
    run_migrations(pool.get().await?).await?;
    Ok(pool)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args);
    init_db(&args).await?;

    Ok(())
}
