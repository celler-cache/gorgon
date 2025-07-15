// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{io::Write, path::Path, sync::Arc};

use chrono::prelude::*;
use clap::Parser;
use diesel::prelude::*;
use diesel_async::{
    AsyncPgConnection, RunQueryDsl, TransactionManager, pooled_connection::PoolTransactionManager,
};
use gorgon_build_helper::helper::{Helper, HelperCaller};
use gorgond_client::{Url, api, models::Repository};
use gorgond_diesel::AsyncPgPool;
use miette::{Context, IntoDiagnostic, Result, miette};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use tokio::runtime::Runtime;
use tracing::{info, instrument};

use crate::schema::gorgond_github as schema;

#[derive(Debug, Clone, Parser)]
#[group(id = "sync")]
pub struct Args {
    /// Base URL for the Gorgon API to talk to.
    #[arg(
        long,
        short = 'B',
        env = "GORGON_BASE_URL",
        default_value = "http://localhost:9999/"
    )]
    base_url: Url,

    /// Number of threads to use.
    #[arg(long, short, default_value_t = 2)]
    threads: usize,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
}
impl Args {
    #[instrument(name = "sync", skip_all)]
    pub fn run(self, pool: AsyncPgPool, rt: Arc<Runtime>) -> Result<()> {
        let client = gorgond_client::Client::new(&self.base_url)?;
        let caller = self.helpers.caller()?;
        let exe = std::env::current_exe()
            .into_diagnostic()
            .context("Couldn't get current executable path")?;
        let args = ["ingest"];

        // Set up the Rayon thread pool.
        rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .thread_name(|idx| format!("rayon-{idx:02}"))
            .build_global()
            .into_diagnostic()
            .context("Couldn't create thread pool")?;

        // Ask gorgond for Github repositories, iterating over them in parallel.
        // TODO: Prioritise these - remember Github's rate limits.
        rt.block_on(
            client
                .repositories()
                .kind("github")
                .list(api::RepositoriesListRequest {}),
        )?
        .get()
        .repositories
        .par_iter()
        .try_for_each_with((rt, pool, caller), |(rt, pool, caller), repo| {
            // TODO: Make this clonable.
            let helper = caller.helper("gorgon-github-events")?;

            let mut conn = rt
                .block_on(pool.get())
                .into_diagnostic()
                .context("Couldn't get database connection")?;

            // Replicates the behaviour of `AsyncConnection::transaction()`, but without
            // blocking the entire thread on an async closure.
            rt.block_on(PoolTransactionManager::begin_transaction(&mut conn))
                .into_diagnostic()
                .context("Couldn't begin transaction")?;
            match self.run_repo(rt, &mut conn, repo, helper, &exe, &args) {
                Ok(()) => rt
                    .block_on(PoolTransactionManager::commit_transaction(&mut conn))
                    .into_diagnostic()
                    .with_context(|| repo.spec.ident.to_string()),
                Err(err) => {
                    match rt.block_on(PoolTransactionManager::rollback_transaction(&mut conn)) {
                        Ok(()) | Err(diesel::result::Error::BrokenTransactionManager) => Err(err),
                        Err(err) => Err(err).into_diagnostic(),
                    }
                }
            }
        })
    }

    #[instrument(name = "repo", skip_all, fields(ident = &*repo.spec.ident))]
    pub fn run_repo(
        &self,
        rt: &mut Arc<Runtime>,
        conn: &mut AsyncPgConnection,
        repo: &Repository,
        helper: Helper,
        exe: &Path,
        args: &[&str],
    ) -> Result<()> {
        let now = Utc::now();

        // Fetch previously seen events, as inputs to `gorgon-github-events`.
        let last_event_ids: Vec<i64> = rt
            .block_on(
                schema::events::table
                    .select(schema::events::id)
                    .filter(
                        schema::events::repo_id.eq_any(
                            schema::repos::table
                                .select(schema::repos::id)
                                .filter(schema::repos::name.eq(&*repo.spec.ident)),
                        ),
                    )
                    .limit(100)
                    .load(conn),
            )
            .into_diagnostic()
            .context("Couldn't fetch previous events")?;

        // Call `gorgon-github-events`...
        info!("-> Polling...");
        self.call_gorgon_github_events(helper, &repo.spec.ident, exe, args, last_event_ids)
            .context("gorgon-github-events")?;

        // ...then update the `polled_at` timestamp.
        info!("  -> Updating polled_at...");
        rt.block_on(
            diesel::update(schema::repos::table)
                .filter(schema::repos::name.eq(&*repo.spec.ident))
                .set((
                    schema::repos::repository_id.eq(repo.id),
                    schema::repos::polled_at.eq(now),
                ))
                .execute(conn),
        )
        .into_diagnostic()
        .context("Couldn't update polled_at")?;

        Ok(())
    }

    #[instrument(name = "gorgon-github-events", skip_all)]
    pub fn call_gorgon_github_events(
        &self,
        helper: Helper,
        ident: &str,
        exe: &Path,
        args: &[&str],
        last_event_ids: impl IntoIterator<Item = i64>,
    ) -> Result<()> {
        let mut helper = helper
            .with_arg(ident)
            .with_arg(exe)
            .with_args(args)
            .with_stdin(std::process::Stdio::piped());
        let mut child = helper.start()?;
        {
            let mut stdin = child.child.stdin.take().ok_or(miette!("No stdin!?"))?;
            for id in last_event_ids {
                writeln!(&mut stdin, "{id}")
                    .into_diagnostic()
                    .context("Couldn't write Event ID")?;
            }
        }
        child.wait_checked()?;
        Ok(())
    }
}
