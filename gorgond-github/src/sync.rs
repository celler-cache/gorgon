// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{io::Write, path::PathBuf, sync::Arc};

use chrono::prelude::*;
use clap::Parser;
use diesel::{pg::Pg, prelude::*};
use diesel_async::{
    AsyncPgConnection, RunQueryDsl, TransactionManager,
    pooled_connection::{PoolTransactionManager, deadpool::Object},
};
use futures::FutureExt;
use gorgon_build_helper::helper::{Helper, HelperCaller};
use gorgonctl::output;
use gorgond_client::{Client, Url, Uuid, api, models as client};
use gorgond_diesel::AsyncPgPool;
use miette::{Context, IntoDiagnostic, Result, miette};
use rayon::prelude::*;
use tokio::runtime::Runtime;
use tracing::{Span, debug, info, info_span, instrument};
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
                let _enter = info_span!(parent: &span, "repo", ident = &*repo.spec.ident).entered();
                let _drop = with_drop((), |_| span.pb_inc(1));

                let mut conn = rt
                    .block_on(pool.get())
                    .into_diagnostic()
                    .context("Couldn't get database connection")?;

                transaction(rt, &mut conn, |conn| {
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
                    helper
                        .call(&repo.spec.ident, last_event_ids)
                        .context("gorgon-github-events")?;

                    // ...then update the `polled_at` timestamp.
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
                })
            })
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

        info!("Finding refs for unprocessed Events...");
        let event_refs = rt
            .block_on({
                let q = schema::events::table
                    .filter(schema::events::synced_at.is_null())
                    .select((schema::events::pull_request_id, schema::events::ref_id))
                    .distinct()
                    .order_by((schema::events::pull_request_id, schema::events::ref_id));
                debug!("{}", diesel::debug_query::<Pg, _>(&q));
                q.load::<(Option<i64>, Option<Uuid>)>(&mut conn)
            })
            .into_diagnostic()
            .context("Couldn't load unprocessed Events")?;

        info!("Finding stale Pull Requests...");
        let prs = rt
            .block_on({
                let q = schema::pull_requests::table
                    .left_join(schema::repos::table)
                    // We only care about PRs to repos that gorgond wants to track.
                    .filter(schema::repos::repository_id.is_not_null())
                    .filter(
                        // PRs that haven't yet been registered with gorgond.
                        (schema::pull_requests::merge_request_id.is_null())
                            // PRs with unprocessed events.
                            .or(schema::pull_requests::id
                                .eq_any(event_refs.iter().filter_map(|(pid, _)| *pid)))
                            .or(schema::pull_requests::ref_id
                                .eq_any(event_refs.iter().filter_map(|(_, rid)| *rid)))
                            // PRs that have been modified since last sync.
                            // This is a catch-all for when we've processed some events,
                            // but couldn't inform gorgond of the changes yet.
                            .or((schema::pull_requests::synced_at.is_null())
                                .or(schema::pull_requests::synced_at
                                    .lt(schema::pull_requests::updated_at.nullable()))),
                    )
                    .select((
                        models::PullRequest::as_select(),
                        schema::repos::repository_id.assume_not_null(),
                    ))
                    .order_by((
                        schema::pull_requests::repo_id,
                        schema::pull_requests::number,
                    ));
                debug!("{}", diesel::debug_query::<Pg, _>(&q));
                q.load::<(models::PullRequest, Uuid)>(&mut conn)
            })
            .into_diagnostic()
            .context("Couldn't load stale Pull Requests")?;
        if prs.is_empty() {
            info!("-> Nothing to do!");
            return Ok(());
        }
        let pr_it = prs.iter().map(|(pr, _)| pr);

        info!("Finding associated Events...");
        let events = rt
            .block_on({
                let q = schema::events::table
                    .left_join(schema::refs::table)
                    .left_join(schema::repos::table.on(schema::refs::repo_id.eq(schema::repos::id)))
                    .filter(
                        (schema::events::pull_request_id.eq_any(pr_it.clone().map(|pr| pr.id)))
                            .or(schema::events::ref_id.eq_any(pr_it.clone().map(|pr| pr.ref_id))),
                    )
                    .select((
                        models::Event::as_select(),
                        (schema::repos::name, schema::refs::ref_name).nullable(),
                    ))
                    .order_by((
                        schema::events::pull_request_id,
                        schema::events::ref_id,
                        schema::events::emitted_at,
                    ));
                debug!("{}", diesel::debug_query::<Pg, _>(&q));
                q.load::<(models::Event, Option<(String, String)>)>(&mut conn)
            })
            .into_diagnostic()
            .context("Couldn't load Events for stale Pull Requests")?;

        debug!(
            event_refs = event_refs.len(),
            prs = prs.len(),
            events = events.len(),
            "-> Loaded"
        );

        let repo_rsp = rt.block_on(
            client.batch().repositories().merges().import(
                api::BatchRepositoryMergeRequestsImportRequest {
                    merge_requests: (prs.iter())
                        .map(|(pr, repo_id)| {
                            (*repo_id, {
                                (events.iter())
                                .filter(|(e, _)|  e.pull_request_id == Some(pr.id))
                                .fold(
                                    client::RepositoryMergeRequestSpec {
                                        ident: pr.number.to_string().into(),
                                        title: pr.title.as_str().into(),
                                        description: pr.body.as_str().into(),
                                        closed_at: None,
                                    },
                                    |mut mr, (event, _)| {
                                        match event.pull_request_action {
                                            Some(models::EventPullRequestAction::Opened)
                                            | Some(models::EventPullRequestAction::Reopened) => {
                                                mr.closed_at = None
                                            }
                                            Some(models::EventPullRequestAction::Closed) => {
                                                mr.closed_at = Some(event.emitted_at)
                                            }
                                            _ => {}
                                        }
                                        mr
                                    },
                                )
                            })
                        })
                        .collect(),
                },
            ),
        )?;
        let _version_rsp = rt.block_on(
            client.batch().repositories().merges().versions().import(
                api::BatchRepositoryMergeRequestVersionsImportRequest {
                    merge_request_versions: (pr_it.clone())
                        .zip(repo_rsp.get().merge_requests.iter())
                        .flat_map(|(pr, mr)| {
                            (events.iter())
                                .filter(|(e, _)| e.ref_id == Some(pr.ref_id) && !e.head.is_empty())
                                .filter_map(|(e, r)| r.as_ref().map(|r| (e, r)))
                                .map(|(e, (fork_name, _))| {
                                    let head = hex::encode(&e.head);
                                    (
                                        mr.id,
                                        client::RepositoryMergeRequestVersionSpec {
                                            source: format!(
                                                "git+https://github.com/{fork_name}?rev={head}"
                                            )
                                            .into(),
                                            ident: head.into(),
                                            current_at: e.emitted_at,
                                        },
                                    )
                                })
                        })
                        .collect(),
                },
            ),
        )?;

        transaction(&rt, &mut conn, |conn| {
            rt.block_on(futures::future::try_join3(
                // Set `merge_request_id` where it's not already in the database.
                futures::future::try_join_all(
                    ((pr_it.clone())
                        .zip(repo_rsp.get().merge_requests.iter())
                        .filter(|(pr, _)| pr.merge_request_id.is_none()))
                    .map(|(pr, mr)| {
                        diesel::update(schema::pull_requests::table)
                            .set(schema::pull_requests::merge_request_id.eq(mr.id))
                            .filter(schema::pull_requests::id.eq(pr.id))
                            .execute(conn)
                            .map(|r| {
                                (r.map(|_| ()).into_diagnostic())
                                    .context("Couldn't set merge_request_id")
                            })
                    }),
                ),
                // Update `synced_at`.
                {
                    let q = diesel::update(schema::pull_requests::table)
                        .filter(schema::pull_requests::id.eq_any(pr_it.clone().map(|pr| pr.id)))
                        .set(schema::pull_requests::synced_at.eq(now));
                    debug!("{}", diesel::debug_query::<Pg, _>(&q));
                    q.execute(conn).map(|r| {
                        (r.map(|_| ()).into_diagnostic())
                            .context("Couldn't set Pull Requests' synced_at")
                    })
                },
                {
                    let q = diesel::update(schema::events::table)
                        .filter(
                            schema::events::id.eq_any(
                                (events.iter())
                                    .filter(|(e, _)| e.synced_at.is_none())
                                    .map(|(e, _)| e.id),
                            ),
                        )
                        .set(schema::events::synced_at.eq(now));
                    debug!("{}", diesel::debug_query::<Pg, _>(&q));
                    q.execute(conn).map(|r| {
                        (r.map(|_| ()).into_diagnostic())
                            .context("Couldn't set Pull Requests' synced_at")
                    })
                },
            ))
        })?;

        suspend_tracing_indicatif(|| {
            self.output.with(
                |repo_rsp| output::merge_table(repo_rsp.table_iter()),
                repo_rsp.get(),
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
        (child.child.stdin.take().ok_or(miette!("No stdin!?"))).and_then(|mut stdin| {
            last_event_ids.into_iter().try_for_each(|id| {
                writeln!(&mut stdin, "{id}")
                    .into_diagnostic()
                    .context("Couldn't write Event ID")
            })
        })?;
        child.wait_checked()?;
        Ok(())
    }
}
