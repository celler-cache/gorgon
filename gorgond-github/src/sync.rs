// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{io::Write, path::PathBuf, sync::Arc};

use chrono::prelude::*;
use clap::Parser;
use diesel::prelude::*;
use diesel_async::{
    AsyncPgConnection, RunQueryDsl, TransactionManager,
    pooled_connection::{PoolTransactionManager, deadpool::Object},
};
use gorgon_build_helper::helper::{Helper, HelperCaller};
use gorgonctl::output;
use gorgond_client::{Client, Url, Uuid, api, models as client};
use gorgond_diesel::AsyncPgPool;
use miette::{Context, IntoDiagnostic, Result, miette};
use rayon::prelude::*;
use tokio::runtime::Runtime;
use tracing::{Span, info, info_span, instrument};
use tracing_indicatif::{span_ext::IndicatifSpanExt, suspend_tracing_indicatif};
use with_drop::with_drop;

use crate::{models, schema::gorgond_github as schema};

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

    /// Don't fetch new events from Github.
    #[arg(long, short = 'G')]
    no_github: bool,

    /// Don't sync pull requests to gorgond.
    #[arg(long, short = 'P')]
    no_pull_requests: bool,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    output: gorgonctl::output::OutputArgs,
}
impl Args {
    #[instrument(name = "sync", skip_all)]
    pub fn run(self, pool: AsyncPgPool, rt: Arc<Runtime>) -> Result<()> {
        let now = Utc::now();
        let client = Client::new(&self.base_url)?;

        // Set up the Rayon thread pool.
        rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .thread_name(|idx| format!("rayon-{idx:02}"))
            .build_global()
            .into_diagnostic()
            .context("Couldn't create thread pool")?;

        if !self.no_github {
            self.sync_github(&pool, rt.clone(), &client, now)?;
        }
        if !self.no_pull_requests {
            self.sync_pull_requests(&pool, rt.clone(), &client, now)?;
        }

        Ok(())
    }

    #[instrument(name = "github", skip_all)]
    fn sync_github(
        &self,
        pool: &AsyncPgPool,
        rt: Arc<Runtime>,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let helper = GorgonGithubEvents {
            path: self.helpers.caller()?.path("gorgon-github-events")?,
            exe: std::env::current_exe()
                .into_diagnostic()
                .context("Couldn't get current executable path")?,
        };

        // Ask gorgond for Github repositories, iterating over them in parallel.
        // TODO: Prioritise these - remember Github's rate limits.
        let rsp = rt.block_on(
            client
                .repositories()
                .kind("github")
                .list(api::RepositoriesListRequest {}),
        )?;
        let repos = &rsp.get().repositories;

        // Top-level progress bar.
        let span = Span::current();
        span.pb_set_style(&crate::STYLE_BAR);
        span.pb_set_length(repos.len() as u64);

        repos
            .par_iter()
            .try_for_each_with((rt, pool), |(rt, pool), repo| {
                let _enter = span.enter();
                let _drop = with_drop((), |_| span.pb_inc(1));

                let mut conn = rt
                    .block_on(pool.get())
                    .into_diagnostic()
                    .context("Couldn't get database connection")?;

                transaction(rt, &mut conn, |conn| {
                    self.sync_github_repo(rt, conn, repo, &helper, now)
                })
            })
    }

    #[instrument(name = "repo", skip_all, fields(ident = &*repo.spec.ident))]
    pub fn sync_github_repo(
        &self,
        rt: &Arc<Runtime>,
        conn: &mut AsyncPgConnection,
        repo: &client::Repository,
        helper: &GorgonGithubEvents,
        now: DateTime<Utc>,
    ) -> Result<()> {
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
        helper
            .call(&repo.spec.ident, last_event_ids)
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

    #[instrument(name = "pull_requests", skip_all)]
    fn sync_pull_requests(
        &self,
        pool: &AsyncPgPool,
        rt: Arc<Runtime>,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut conn = get_conn(&rt, pool)?;

        info!("Finding stale Pull Requests...");
        let prs = rt
            .block_on(
                schema::pull_requests::table
                    .inner_join(schema::repos::table)
                    .filter(schema::repos::repository_id.is_not_null())
                    .filter(schema::pull_requests::merge_request_id.is_null())
                    .filter(
                        (schema::pull_requests::synced_at.is_null())
                            .or(schema::pull_requests::synced_at
                                .lt(schema::pull_requests::updated_at.nullable())),
                    )
                    .select((
                        models::PullRequest::as_select(),
                        schema::repos::repository_id.assume_not_null(),
                    ))
                    .order_by((schema::repos::repository_id, schema::pull_requests::number))
                    .load::<(models::PullRequest, Uuid)>(&mut conn),
            )
            .into_diagnostic()
            .context("Couldn't load stale Pull Requests")?;
        if prs.is_empty() {
            info!("-> None!");
            return Ok(());
        }

        let span = Span::current();
        span.pb_set_style(&crate::STYLE_BAR);
        span.pb_set_length(prs.len() as u64);

        let vs = prs
            .par_iter()
            .map(|(pr, repo_id)| {
                let _drop = with_drop((), |_| span.pb_inc(1));
                let _enter = info_span!(parent: &span, "pr", pr.id, pr.title);
                info!("Syncing PR...");

                let ident = pr.number.to_string();
                let repo_client = client.repositories().id(*repo_id);
                let merge_client = repo_client.merges().ident(&ident);
                let rsp = rt.block_on(merge_client.put(api::RepositoryMergeRequestPutRequest {
                    merge_request: client::PartialRepositoryMergeRequestSpec {
                        ident: Some(ident.as_str().into()),
                        title: Some(pr.title.as_str().into()),
                        description: Some(pr.body.as_str().into()),
                        closed_at: None,
                    },
                }))?;
                Ok((pr, rsp.get().to_owned()))
            })
            .collect::<Result<Vec<_>>>()?;

        transaction(&rt, &mut conn, |conn| {
            for (pr, rsp) in vs.iter() {
                if pr.merge_request_id.is_none() {
                    rt.block_on(
                        diesel::update(schema::pull_requests::table)
                            .set(schema::pull_requests::merge_request_id.eq(rsp.merge_request.id))
                            .execute(conn),
                    )
                    .into_diagnostic()
                    .context("Couldn't update merge_request_id")?;
                }
            }
            rt.block_on(
                diesel::update(schema::pull_requests::table)
                    .filter(schema::pull_requests::id.eq_any(vs.iter().map(|(pr, _)| pr.id)))
                    .set(schema::pull_requests::synced_at.eq(now))
                    .execute(conn),
            )
            .into_diagnostic()
            .context("Couldn't update synced_at")?;
            Ok(())
        })?;

        // Everything about this is an awful bodge, and this interface needs to be fixed.
        suspend_tracing_indicatif(|| {
            self.output.with(
                |vs| {
                    output::merge_table(
                        vs.iter()
                            .map(|rsp| (&rsp.merge_request, Some(&rsp.repository))),
                    )
                },
                &vs.into_iter().map(|(_, rsp)| rsp).collect::<Vec<_>>(),
            )
        })
    }
}

pub fn get_conn(rt: &Arc<Runtime>, pool: &AsyncPgPool) -> Result<Object<AsyncPgConnection>> {
    rt.block_on(pool.get())
        .into_diagnostic()
        .context("Couldn't get database connection")
}

/// Replicates the behaviour of `AsyncConnection::transaction()`, but without
/// blocking the entire thread on an async closure.
pub fn transaction<T, F: FnOnce(&mut Object<AsyncPgConnection>) -> Result<T>>(
    rt: &Arc<Runtime>,
    conn: &mut Object<AsyncPgConnection>,
    f: F,
) -> Result<T> {
    rt.block_on(PoolTransactionManager::begin_transaction(conn))
        .into_diagnostic()
        .context("Couldn't begin transaction")?;
    match f(conn) {
        Ok(v) => rt
            .block_on(PoolTransactionManager::commit_transaction(conn))
            .into_diagnostic()
            .map(|_| v),
        Err(err) => match rt.block_on(PoolTransactionManager::rollback_transaction(conn)) {
            Ok(()) | Err(diesel::result::Error::BrokenTransactionManager) => Err(err),
            Err(err) => Err(err).into_diagnostic(),
        },
    }
}

#[derive(Debug, Clone)]
pub struct GorgonGithubEvents {
    pub path: PathBuf,
    pub exe: PathBuf,
}
impl GorgonGithubEvents {
    #[instrument(name = "gorgon-github-events", skip_all)]
    pub fn call(
        &self,
        ident: impl AsRef<str>,
        last_event_ids: impl IntoIterator<Item = i64>,
    ) -> Result<()> {
        let mut helper = Helper::new(&self.path)
            .with_arg(ident.as_ref())
            .with_arg(&self.exe)
            .with_args(["ingest"])
            .with_stdin(std::process::Stdio::piped())
            .with_stdout(std::process::Stdio::null())
            .with_stderr(std::process::Stdio::piped());
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
