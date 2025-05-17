// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::error::Error as StdError;

use diesel::{migration::MigrationSource, pg::Pg};
use diesel_async::{
    AsyncConnection,
    pg::AsyncPgConnection,
    pooled_connection::{AsyncDieselConnectionManager, deadpool::Pool},
};
use diesel_migrations::MigrationHarness;
use miette::{Context, IntoDiagnostic, Result};
use tracing::{debug, debug_span, info, instrument};

#[derive(Debug, Clone, clap::Parser)]
#[group(id = "db")]
#[command(next_help_heading = "Database Connection")]
pub struct Args {
    /// Database URL.
    #[arg(long, short = 'D', env = "GORGOND_DATABASE_URL")]
    db_url: String,
}
impl Args {
    #[instrument(name = "db", level = "DEBUG", skip_all)]
    pub async fn init(
        &self,
        migrations: impl MigrationSource<Pg> + Send + 'static,
    ) -> Result<Pool<AsyncPgConnection>> {
        debug!(db_url = self.db_url, "Connecting to database...");
        let cfg = AsyncDieselConnectionManager::<AsyncPgConnection>::new(&self.db_url);
        let pool = Pool::builder(cfg)
            .build()
            .into_diagnostic()
            .context("Couldn't build database connection pool")?;
        async {
            let conn = pool
                .get()
                .await
                .into_diagnostic()
                .context("Couldn't get connection for migrations")?;
            migrate(conn, migrations).await
        }
        .await
        .context("Couldn't run migrations")?;
        Ok(pool)
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(transparent)]
pub struct MigrationError(#[from] Box<dyn StdError + Send + Sync>);

#[instrument(name = "migrate", level = "DEBUG", skip_all)]
pub async fn migrate<
    C: AsyncConnection<Backend = Pg> + 'static,
    M: MigrationSource<Pg> + Send + 'static,
>(
    conn: C,
    migrations: M,
) -> Result<()> {
    use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;

    // Bridge the async connection back into a sync one for diesel-migrations.
    tokio::task::spawn_blocking(move || -> Result<()> {
        debug!("Running migrations...");
        let mut conn = AsyncConnectionWrapper::<C>::from(conn);
        match conn
            .pending_migrations(migrations)
            .map_err(MigrationError)
            .context("Couldn't get pending migrations")?
        {
            pending if pending.is_empty() => debug!("▶️ Migrations already up to date"),
            pending => {
                info!(num = pending.len(), "⏸️ Database has pending migrations");
                for migration in pending {
                    let name = migration.name();
                    let _enter = debug_span!("migration", %name).entered();
                    info!(%name, "⏩ Applying migration...");

                    conn.run_migration(&migration)
                        .map_err(MigrationError)
                        .with_context(|| format!("{}", migration.name()))?;
                }
                info!("▶️ Database successfully migrated!");
            }
        }
        Ok(())
    })
    .await
    .expect("gorgond-diesel::migrate panicked!")?;

    Ok(())
}
