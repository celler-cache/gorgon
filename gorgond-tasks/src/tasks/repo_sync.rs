// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    io::Write,
    path::PathBuf,
    time::Instant,
};

use chrono::{TimeDelta, prelude::*};
use clap::Parser;
use derive_more::Display;
use diesel::{prelude::*, sql_query, sql_types, upsert::excluded};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use git2::{Direction, Remote, RemoteCallbacks, Repository, RepositoryInitOptions};
use gorgon_build_helper::helper::{Helper, HelperCaller, HelperError, HelperNoStdin};
use gorgon_util::source_url::{GitUrl, SourceUrl};
use gorgond_client::Uuid;
use gorgond_diesel::{TransactionError, models, schema::gorgond as schema};
use humantime::Duration;
use itertools::Itertools;
use miette::Context;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, error_span, info, info_span, instrument, trace, warn};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use url::{Position, Url};

use crate::tasks::{TaskConfig, TaskMode, TaskSource};

type Result<T, E = Error> = std::result::Result<T, E>;

const REMOTE_STORAGE: &str = "storage";

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Diesel(#[from] diesel::result::Error),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Remote(#[from] RemoteError),
    #[error("Rollup failed: {_0}")]
    #[diagnostic(forward(1))]
    Rollup(Uuid, #[source] RollupError),

    #[error("Remote '{REMOTE_STORAGE}' error")]
    #[diagnostic(
        help = "This should never happen, your mirror repository is probably broken. Try deleting it?"
    )]
    StorageRemote(#[source] GitError),

    #[error("Ref '{_0}' has multiple commit sources: {_1} & {_2}")]
    RefMultipleCommitSources(String, Uuid, Uuid),
    #[error("Ref '{_0}' has multiple sources:\n\t{_1}\n\t{_2}")]
    RefMultipleSources(String, String, String),

    #[error("Ref '{_0}' does not refer to an object?")]
    RefNoTarget(String),
}
impl From<Error> for TransactionError {
    fn from(err: Error) -> Self {
        match err {
            Error::Diesel(err) => err.into(),
            _ => TransactionError::new(err),
        }
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Warn {
    #[error("Skipping remote...")]
    #[diagnostic(severity = "warning")]
    SkippingRemote(#[source] RemoteError),
    #[error("Skipping unresolved ref: {_0}")]
    #[diagnostic(severity = "warning")]
    UnresolvedRef(String),

    #[error("Removing unresolved commit source from rollup {_0}: {_1}")]
    #[diagnostic(severity = "warning")]
    RemovingUnresolvedRollupInput(Uuid, Uuid),
    #[error("Skipping rollup with no (resolvable) inputs: {_0}")]
    #[diagnostic(severity = "warning")]
    RemovingEmptyRollup(Uuid),
}

fn log_err_ref(err: &miette::Report) {
    match err.severity().unwrap_or(miette::Severity::Error) {
        miette::Severity::Error => error!("\n{err:?}"),
        miette::Severity::Warning => info!("\n{err:?}"),
        miette::Severity::Advice => info!("\n{err:?}"),
    }
}
fn log_err(err: impl Into<miette::Report>) {
    log_err_ref(&err.into());
}

#[derive(Debug, Clone)]
pub struct RepoSyncContext {
    pub gorgon_rollup_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingRollup {
    pub rollup: models::Rollup,
    pub inputs: Vec<models::RollupInput>,
}
#[derive(Debug, Display, Serialize, Deserialize)]
#[display("{}", repo.id)]
pub struct RepoSyncTask {
    pub repo: models::Repository,
    pub sources: BTreeMap<Uuid, models::CommitSource>,
    pub rollups: BTreeMap<Uuid, PendingRollup>,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedRollup {
    pub rollup: models::Rollup,
    #[serde_as(as = "BTreeMap<DisplayFromStr, BTreeSet<DisplayFromStr>>")]
    pub resolved_batches: BTreeMap<git2::Oid, BTreeSet<git2::Oid>>,
    pub rejected_sources: BTreeSet<Uuid>,
}
#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedCommit {
    pub commit: models::Commit,
    pub sources: BTreeMap<Uuid, models::CommitSource>,
}
#[serde_as]
#[derive(Debug, Display, Serialize, Deserialize)]
#[display("{}", repo.id)]
pub struct RepoSyncOutput {
    pub repo: models::Repository,
    #[serde_as(as = "BTreeMap<DisplayFromStr, _>")]
    pub resolved_commits: BTreeMap<git2::Oid, ResolvedCommit>,
    pub resolved_rollups: BTreeMap<Uuid, ResolvedRollup>,
    pub rejected_rollups: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Task: Sync Repositories")]
pub struct RepoSyncTaskSource {
    #[arg(long, default_missing_value = "true", num_args=0..=1)]
    pub task_repo_sync: Option<bool>,
    #[arg(long, default_value = "60s")]
    pub task_repo_sync_interval: Duration,
    #[arg(long, default_value = "6h", value_parser = crate::parse_duration)]
    pub task_repo_sync_ref_stale_after: TimeDelta,
}
impl TaskSource for RepoSyncTaskSource {
    const ID: &str = "RepoSync";
    const FRIENDLY: &str = "Sync Repositories";

    type Error = Error;
    type Task = RepoSyncTask;
    type Output = RepoSyncOutput;

    fn config(&self) -> TaskConfig {
        TaskConfig {
            mode: TaskMode::BlockingHeavy,
            enabled: self.task_repo_sync,
            interval: self.task_repo_sync_interval,
        }
    }

    type Context = RepoSyncContext;
    fn init(&self, helpers: &impl HelperCaller) -> miette::Result<Self::Context> {
        Ok(RepoSyncContext {
            gorgon_rollup_path: helpers.path("gorgon-rollup")?,
        })
    }

    async fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> Result<impl IntoIterator<Item = Self::Task>, Self::Error> {
        #[derive(Debug, QueryableByName)]
        #[diesel(check_for_backend(diesel::pg::Pg))]
        struct Row {
            #[diesel(sql_type = sql_types::Uuid)]
            repo_id: Uuid,
            #[diesel(sql_type = sql_types::Nullable<sql_types::Uuid>)]
            source_id: Option<Uuid>,
            #[diesel(sql_type = sql_types::Nullable<sql_types::Uuid>)]
            rollup_id: Option<Uuid>,
        }
        // Find repositories...
        let rows = sql_query(concat!(
            "SELECT r.id AS repo_id, cs.id AS source_id, rl.id AS rollup_id",
            "  FROM gorgond.repositories AS r",
            // ...that have commit sources... (TODO: Select only stale or unresolved ones)
            "  LEFT JOIN gorgond.commit_sources AS cs ",
            "    ON (cs.repository_id = r.id)",
            // ...or unrolled rollups... (TODO: Remember to include sources)
            "  LEFT JOIN gorgond.rollups AS rl ",
            "    ON (rl.repository_id = r.id AND rl.rolled_at IS NULL)",
            // ...skip rows that were joined with nothing...
            "  WHERE r.storage <> ''",
            "    AND (cs.id IS NOT NULL OR rl.id IS NOT NULL)",
            // ...and lock the repository, skipping ones already locked.
            //
            // This is the part where we need to drop down to raw SQL - Diesel has no DSL
            // to add `OF <target>` to a lock, which is needed when doing outer joins.
            "  FOR NO KEY UPDATE OF r SKIP LOCKED",
        ))
        .get_results::<Row>(conn)
        .await?;

        let repos = schema::repositories::table
            .filter(schema::repositories::id.eq_any(rows.iter().map(|r| r.repo_id)))
            .load_stream::<models::Repository>(conn)
            .await?
            .try_fold(BTreeMap::new(), async |mut repos, repo| {
                repos.insert(
                    repo.id,
                    RepoSyncTask {
                        repo,
                        sources: Default::default(),
                        rollups: Default::default(),
                    },
                );
                Ok(repos)
            })
            .await?;
        let repos = schema::commit_sources::table
            .filter(schema::commit_sources::id.eq_any(rows.iter().filter_map(|r| r.source_id)))
            .load_stream::<models::CommitSource>(conn)
            .await?
            .try_fold(repos, async |mut repos, src| {
                let repo = repos.get_mut(&src.repository_id).unwrap();
                repo.sources.insert(src.id, src);
                Ok(repos)
            })
            .await?;
        let repos = schema::rollups::table
            .filter(schema::rollups::id.eq_any(rows.iter().filter_map(|r| r.rollup_id)))
            .inner_join(
                schema::rollup_inputs::table
                    .on(schema::rollup_inputs::rollup_id.eq(schema::rollups::id)),
            )
            .load_stream::<(models::Rollup, models::RollupInput)>(conn)
            .await?
            .try_fold(repos, async |mut repos, (rollup, input)| {
                let repo = repos.get_mut(&rollup.repository_id).unwrap();
                repo.rollups
                    .entry(rollup.id)
                    .or_insert_with(|| PendingRollup {
                        rollup,
                        inputs: Default::default(),
                    })
                    .inputs
                    .push(input);
                Ok(repos)
            })
            .await?;

        Ok(repos.into_values())
    }

    async fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> Result<(), Self::Error> {
        let now = Utc::now();
        let mut repos = (data.into_iter())
            .map(|d| (d.repo.id, d))
            .collect::<BTreeMap<Uuid, Self::Output>>();

        save_commits(conn, &mut repos, now).await?;
        save_rollups(conn, &mut repos, now).await?;

        Ok(())
    }

    fn work(
        &self,
        token: &CancellationToken,
        ctx: &RepoSyncContext,
        RepoSyncTask {
            repo,
            sources,
            rollups,
        }: Self::Task,
    ) -> Result<Self::Output, Self::Error> {
        let now = Utc::now();

        let mut remotes = BTreeMap::<Url, RemoteWork>::new();
        let mut ref_to_src = BTreeMap::<String, models::CommitSource>::new();
        for source in sources.into_values() {
            let _enter = info_span!("source", id = %source.id, url = source.source).entered();

            // Skip invalid sources.
            let Ok(src) = SourceUrl::parse(&source.source)
                .context("Invalid Source URL")
                .inspect_err(log_err_ref)
            else {
                continue;
            };
            // Skip fixed commit URLs that are already resolved.
            if source.commit_id.is_some() && src.is_fixed() {
                debug!("Skipping already resolved fixed URL...");
                continue;
            }
            // Skip floating commit URLs that were resolved recently.
            if (source.fetched_at).is_some_and(|t| (t - now) > self.task_repo_sync_ref_stale_after)
            {
                debug!("Skipping still-fresh dynamic URL...");
                continue;
            }

            // Add it to the work for the relevant remote.
            let ref_ = remotes
                .entry(RemoteWork::url(&src).clone()) // TODO: Pointless allocation.
                .or_default()
                .add_source(&src)?;
            if let Some(ref_) = ref_ {
                ref_to_src.insert(ref_, source);
            }
        }

        // Do the mucking about with the git repo.
        let r = RepoWrapper::init_for_repo(&repo)?;
        let mut storage = r.storage_remote()?;

        // First, try to fetch cached refs from storage. For floating refs, this lets us
        // load the previous commit it referred to, and thus avoid having to fetch the
        // entire history from the remote as well, which is quite slow with large repos.
        info!("Fetching cached refs from storage...");
        r.with_remote(token, &mut storage, Direction::Fetch, |storage| {
            r.fetch(token, storage, ref_to_src.keys().map(|r| (r, r)))
        })?;

        // Then fetch up-to-date refs from remotes.
        {
            let span = info_span!("remotes");
            let _enter = span.enter();
            span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR);
            span.pb_set_length(remotes.len() as _);
            info!("Fetching refs from remotes...");
            for (i, (url, task)) in remotes.iter().enumerate() {
                span.pb_set_position(i as _);
                match r.with_remote(
                    token,
                    &mut r.remote_anonymous(url.as_str())?,
                    Direction::Fetch,
                    |remote| r.fetch(token, remote, &task.refs_from_git),
                ) {
                    Ok(_) => {}
                    Err(err @ RemoteError(_, _, RemoteErrorKind::Connect(_))) => log_err(err),
                    Err(err) => return Err(err.into()),
                };
            }
        }

        // Resolve refs to commit OIDs.
        info!("Resolving refs...");
        let mut resolved_sources = BTreeMap::<Uuid, git2::Oid>::new();
        let mut resolved_commits = BTreeMap::<git2::Oid, ResolvedCommit>::new();
        let mut refs_to_push = BTreeSet::<String>::new();
        for (ref_, src) in ref_to_src {
            // Unresolved refs happen, just warn about it (and don't try to push them).
            let Some(oid) = r.resolve(&ref_)? else {
                log_err(Warn::UnresolvedRef(ref_.clone()));
                continue;
            };

            refs_to_push.insert(ref_);
            resolved_sources.insert(src.id, oid);
            (resolved_commits.entry(oid))
                .or_insert_with(|| ResolvedCommit {
                    commit: models::Commit {
                        id: Uuid::now_v7(),
                        repository_id: repo.id,
                        hash: oid.as_bytes().to_vec(),
                        created_at: now,
                    },
                    sources: BTreeMap::new(),
                })
                .sources
                .insert(src.id, src);
        }

        // Kick off rollups.
        let (mut resolved_rollups, mut rejected_rollups) = (BTreeMap::new(), BTreeSet::new());
        if let Some(num_rollups) = Some(rollups.len()).filter(|n| *n > 0) {
            let span = info_span!("rollups").entered();
            span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR_ETA);
            span.pb_set_length(num_rollups as u64);
            info!(num = num_rollups, "Creating rollups...");

            for (_, pr) in rollups {
                let id = pr.rollup.id;
                match resolve_rollup(ctx, &r, &resolved_sources, pr)
                    .with_context(|| format!("Couldn't resolve rollup: {id}"))
                {
                    Ok(Some(rr)) => {
                        resolved_rollups.insert(id, rr);
                    }
                    Ok(None) => {
                        rejected_rollups.insert(id);
                    }
                    Err(err) => log_err(err),
                }
                span.pb_inc(1);
            }
        }

        info!("Pushing local refs to storage...");
        r.with_remote(token, &mut storage, Direction::Push, |storage| {
            r.push(token, storage, refs_to_push)
        })?;

        Ok(RepoSyncOutput {
            repo,
            resolved_commits,
            resolved_rollups,
            rejected_rollups,
        })
    }
}

async fn save_commits(
    conn: &mut AsyncPgConnection,
    repos: &mut BTreeMap<Uuid, RepoSyncOutput>,
    _now: DateTime<Utc>,
) -> Result<(), Error> {
    // Insert commits, retrieving the actual IDs.
    diesel::insert_into(schema::commits::table)
        .values(
            (repos.values())
                .flat_map(|d| d.resolved_commits.values())
                .map(|rc| &rc.commit)
                .collect::<Vec<&models::Commit>>(),
        )
        .on_conflict((schema::commits::repository_id, schema::commits::hash))
        .do_update()
        .set(schema::commits::created_at.eq(schema::commits::created_at))
        .returning(models::Commit::as_returning())
        .load_stream(conn)
        .await?
        .map_err(Error::from)
        .try_fold(&mut *repos, async |repos, commit| -> Result<_, Error> {
            let oid = git2::Oid::from_bytes(&commit.hash).into_git_error("")?;
            if let Some(repo) = repos.get_mut(&commit.repository_id) {
                if let Some(rc) = repo.resolved_commits.get_mut(&oid) {
                    rc.commit = commit;
                    for src in rc.sources.values_mut() {
                        src.commit_id = Some(rc.commit.id);
                    }
                }
            }
            Ok(repos)
        })
        .await?;

    // Upsert commit sources.
    diesel::insert_into(schema::commit_sources::table)
        .values(
            (repos.values())
                .flat_map(|d| d.resolved_commits.values())
                .flat_map(|rc| rc.sources.values())
                .collect::<Vec<&models::CommitSource>>(),
        )
        .on_conflict((
            schema::commit_sources::repository_id,
            schema::commit_sources::source,
        ))
        .do_update()
        .set(schema::commit_sources::commit_id.eq(excluded(schema::commit_sources::commit_id)))
        .execute(conn)
        .await?;

    Ok(())
}

async fn save_rollups(
    conn: &mut AsyncPgConnection,
    repos: &mut BTreeMap<Uuid, RepoSyncOutput>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let (mut batches, mut batch_inputs) = (Vec::new(), Vec::new());
    for r in repos.values_mut() {
        for rr in r.resolved_rollups.values_mut() {
            rr.rollup.rolled_at = Some(now);
            for (rc, inputs) in (rr.resolved_batches.iter())
                .filter_map(|(oid, is)| r.resolved_commits.get(oid).map(|c| (c, is)))
            {
                let batch = models::RollupBatch {
                    id: Uuid::now_v7(),
                    rollup_id: rr.rollup.id,
                    commit_id: rc.commit.id,
                    created_at: now,
                };
                for rc in inputs.iter().filter_map(|oid| r.resolved_commits.get(oid)) {
                    batch_inputs.push(models::RollupBatchInput {
                        batch_id: batch.id,
                        commit_id: rc.commit.id,
                    });
                }
                batches.push(batch);
            }
        }
    }

    // Create rollup batches and inputs.
    diesel::insert_into(schema::rollup_batches::table)
        .values(batches)
        .execute(conn)
        .await?;
    diesel::insert_into(schema::rollup_batch_inputs::table)
        .values(batch_inputs)
        .execute(conn)
        .await?;

    // Delete rejected rollups and inputs.
    diesel::delete(schema::rollups::table)
        .filter(schema::rollups::id.eq_any(repos.values().flat_map(|r| &r.rejected_rollups)))
        .execute(conn)
        .await?;
    for rr in (repos.values())
        .flat_map(|r| r.resolved_rollups.values())
        .filter(|rr| !rr.rejected_sources.is_empty())
    {
        diesel::delete(schema::rollup_inputs::table)
            .filter(schema::rollup_inputs::rollup_id.eq(rr.rollup.id))
            .filter(schema::rollup_inputs::source_id.eq_any(&rr.rejected_sources))
            .execute(conn)
            .await?;
    }

    // Update rolled_at timestamps for rollups.
    diesel::update(schema::rollups::table)
        .filter(schema::rollups::id.eq_any(repos.values().flat_map(|r| r.resolved_rollups.keys())))
        .set(schema::rollups::rolled_at.eq(diesel::dsl::now))
        .execute(conn)
        .await?;

    Ok(())
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum RollupError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Helper(#[from] HelperError),
    #[error(transparent)]
    HelperNoStdin(#[from] HelperNoStdin),
}

#[instrument(name = "rollup", skip_all, fields(id = %rollup.id))]
fn resolve_rollup(
    ctx: &RepoSyncContext,
    r: &RepoWrapper,
    resolved_sources: &BTreeMap<Uuid, git2::Oid>,
    PendingRollup { rollup, inputs }: PendingRollup,
) -> Result<Option<ResolvedRollup>, RollupError> {
    info!("Resolving Rollup...");

    // Resolve sources to commits, discarding sources that couldn't be resolved.
    let (mut pending_commits, rejected_sources) = inputs
        .into_iter()
        .map(|input| {
            resolved_sources.get(&input.source_id).ok_or_else(|| {
                log_err(Warn::RemovingUnresolvedRollupInput(
                    rollup.id,
                    input.source_id,
                ));
                input.source_id
            })
        })
        .partition_result::<BTreeSet<git2::Oid>, BTreeSet<Uuid>, _, _>();
    let num_commits = pending_commits.len();

    // Progress bar.
    let span = Span::current();
    span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR);
    span.pb_set_length(num_commits as u64);

    // Resolve commits into batches.
    let mut resolved_batches = BTreeMap::new();
    while !pending_commits.is_empty() {
        let num = resolved_batches.len() + 1;
        let (commit, parents) = gorgon_rollup(num, ctx, r, &pending_commits)?;
        for parent in parents.iter() {
            pending_commits.remove(parent);
        }
        resolved_batches.insert(commit, parents);
        span.pb_set_position((num_commits - pending_commits.len()) as u64);
    }
    if resolved_batches.is_empty() {
        log_err(Warn::RemovingEmptyRollup(rollup.id));
        return Ok(None);
    }
    Ok(Some(ResolvedRollup {
        rollup,
        resolved_batches,
        rejected_sources,
    }))
}

#[instrument(name = "batch", skip_all, fields(num = _num))]
fn gorgon_rollup(
    _num: usize,
    ctx: &RepoSyncContext,
    r: &RepoWrapper,
    commits: &BTreeSet<git2::Oid>,
) -> Result<(git2::Oid, BTreeSet<git2::Oid>), RollupError> {
    // Spawn gorgon-rollup process.
    let mut helper = Helper::new(&ctx.gorgon_rollup_path)
        .with_stdin(std::process::Stdio::piped())
        .with_stdout(std::process::Stdio::piped());
    let mut child = helper.start()?;

    // Write commits to stdin, then drop stdin to close it and proceed.
    (child.take_stdin())
        .map_err(RollupError::from)
        .and_then(|mut stdin| {
            (commits.iter())
                .try_for_each(|oid| {
                    debug!(%oid, "-> Attempting to merge commit...");
                    writeln!(&mut stdin, "{oid}")
                })
                .map_err(RollupError::from)
        })?;

    // When gorgon-rollup completes successfully, it exits printing a commit hash.
    info!("Rolling...");
    let output = child.wait_checked()?;
    let commit = git2::Oid::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .and_then(|oid| r.repo.find_commit(oid))
        .into_git_error(&r.path)?;
    info!("Successfully created rollup commit!");

    Ok((commit.id(), commit.parent_ids().collect()))
}

#[derive(Default)]
struct RemoteWork {
    pub refs_from_git: BTreeMap<String, String>,
}
impl RemoteWork {
    pub fn url(src: &SourceUrl) -> &Url {
        match src {
            SourceUrl::Git(GitUrl { base_url, .. }) => base_url,
        }
    }
    pub fn add_source(&mut self, src: &SourceUrl) -> Result<Option<String>, Error> {
        let (from, to) = match src {
            SourceUrl::Git(GitUrl { rev: Some(rev), .. }) => {
                (rev.as_str(), format!("refs/gorgon/rev/{rev}"))
            }
            SourceUrl::Git(GitUrl {
                base_url,
                ref_: Some(ref_),
                ..
            }) => (
                ref_.as_str(),
                format!(
                    "refs/gorgon/remote/{}/{ref_}",
                    &base_url[Position::BeforeHost..]
                ),
            ),
            SourceUrl::Git(GitUrl {
                rev: None,
                ref_: None,
                ..
            }) => {
                warn!("Git URL with no rev or ref!?");
                return Ok(None);
            }
        };
        if let Some(old) = self.refs_from_git.insert(to.clone(), from.into()) {
            return Err(Error::RefMultipleSources(to, old, from.into()));
        };
        Ok(Some(to))
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Git Repository: {0}")]
pub struct GitError(PathBuf, #[source] git2::Error);

trait IntoGitError<T> {
    fn into_git_error(self, path: impl Into<PathBuf>) -> T;
}
impl IntoGitError<GitError> for git2::Error {
    fn into_git_error(self, path: impl Into<PathBuf>) -> GitError {
        GitError(path.into(), self)
    }
}
impl<T> IntoGitError<Result<T, GitError>> for Result<T, git2::Error> {
    fn into_git_error(self, path: impl Into<PathBuf>) -> Result<T, GitError> {
        self.map_err(|err| err.into_git_error(path))
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[diagnostic(forward(2))]
pub struct RemoteError(String, String, #[source] RemoteErrorKind);
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (name, url) = (&self.0, &self.1);
        if !name.is_empty() {
            write!(f, "Remote ({name}): {url}")
        } else {
            write!(f, "Remote: {url}")
        }
    }
}
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum RemoteErrorKind {
    #[error("Connection Failed")]
    Connect(#[source] GitError),
    #[error("Couldn't fetch refs: {_1:?}")]
    Fetch(#[source] GitError, Vec<String>),
    #[error("Couldn't push refs: {_1:?}")]
    Push(#[source] GitError, Vec<String>),
}
impl RemoteErrorKind {
    pub fn with_remote(self, remote: &git2::Remote) -> RemoteError {
        RemoteError(
            remote.name().unwrap_or_default().to_string(),
            remote.url().unwrap_or_default().to_string(),
            self,
        )
    }
}

pub struct RepoWrapper {
    pub path: PathBuf,
    pub repo: Repository,
}
impl RepoWrapper {
    pub fn init_for_repo(r: &models::Repository) -> Result<Self> {
        let path = crate::DIRS
            .cache_dir()
            .join(format!("mirrors/{}/{}.git", r.kind, r.ident));
        Self::init(path, &r.storage)
    }
    #[instrument(skip_all)]
    pub fn init(path: PathBuf, storage: &str) -> Result<Self> {
        debug!(path = %path.display(), "Initializing repository...");
        let repo = Repository::init_opts(&path, RepositoryInitOptions::new().bare(true))
            .into_git_error(&path)?;

        debug!("Adding {REMOTE_STORAGE} remote...");
        match repo.remote(REMOTE_STORAGE, storage) {
            Ok(_) => Ok(()),
            Err(err) if err.code() == git2::ErrorCode::Exists => Ok(()),
            Err(err) => Err(Error::StorageRemote(err.into_git_error(&path))),
        }?;

        Ok(Self { path, repo })
    }

    pub fn storage_remote(&self) -> Result<git2::Remote> {
        Ok((self.repo.find_remote(REMOTE_STORAGE))
            .into_git_error(&self.path)
            .map_err(Error::StorageRemote)?)
    }
    pub fn remote_anonymous(&self, url: &str) -> Result<git2::Remote> {
        Ok((self.repo.remote_anonymous(url)).into_git_error(&self.path)?)
    }

    pub fn resolve(&self, ref_: &str) -> Result<Option<git2::Oid>, GitError> {
        match (self.repo)
            .find_reference(ref_)
            .and_then(|r| r.resolve())
            .map(|r| r.target())
        {
            Ok(v) => Ok(v),
            Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(err) => Err(err.into_git_error(&self.path)),
        }
    }

    #[instrument(skip_all)]
    pub fn fetch(
        &self,
        token: &CancellationToken,
        remote: &mut Remote,
        specs: impl IntoIterator<Item = (impl Display, impl Display)>,
    ) -> Result<(), RemoteError> {
        let span = Span::current();
        let specs = (specs.into_iter())
            .map(|(to, from)| format!("{from}:{to}"))
            .collect::<Vec<_>>();
        span.pb_set_message(&format!("Fetching {} refs...", specs.len()));
        debug!(?specs, "Fetching...");
        self.with_progress(token, |rcbs| {
            let mut opts = git2::FetchOptions::new();
            remote
                .fetch(&specs, Some(opts.remote_callbacks(rcbs)), None)
                .into_git_error(&self.path)
                .map_err(|err| RemoteErrorKind::Fetch(err, specs).with_remote(remote))
        })
    }
    #[instrument(skip_all)]
    pub fn push(
        &self,
        token: &CancellationToken,
        remote: &mut Remote,
        specs: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), RemoteError> {
        let span = Span::current();
        let specs = (specs.into_iter()).map(|s| s.into()).collect::<Vec<_>>();
        span.pb_set_message(&format!("Pushing {} refs...", specs.len()));
        debug!(?specs, "Pushing...");
        self.with_progress(token, |rcbs| {
            let mut opts = git2::PushOptions::new();
            opts.packbuilder_parallelism(
                std::thread::available_parallelism()
                    .map(|num| num.get() as u32)
                    .unwrap_or(1),
            );
            remote
                .push(&specs, Some(opts.remote_callbacks(rcbs)))
                .into_git_error(&self.path)
                .map_err(|err| RemoteErrorKind::Push(err, specs).with_remote(remote))
        })
    }

    #[instrument(name = "remote", skip_all, fields(name = remote.name(), url = remote.url()))]
    pub fn with_remote<T, F: FnOnce(&mut Remote) -> Result<T, RemoteError>>(
        &self,
        token: &CancellationToken,
        remote: &mut Remote,
        dir: git2::Direction,
        f: F,
    ) -> Result<T, RemoteError> {
        // We need to do this double result dance, since the inner result has a mutable
        // borrow of the remote, and so we can't re-borrow it in a `map_err` type thing.
        self.with_progress(token, |rcbs| -> Result<Result<T, RemoteError>, GitError> {
            let mut conn = info_span!("connect")
                .in_scope(|| {
                    debug!("Connecting...");
                    remote.connect_auth(dir, Some(rcbs), None)
                })
                .into_git_error(&self.path)?;
            Ok(f(conn.remote()))
        })
        .map_err(|err| RemoteErrorKind::Connect(err).with_remote(remote))?
    }
    fn with_progress<T, F: FnOnce(RemoteCallbacks) -> T>(
        &self,
        token: &CancellationToken,
        f: F,
    ) -> T {
        #[derive(Default)]
        struct Spans {
            count: Option<tracing::span::Span>,
            recv: Option<tracing::span::Span>,
            deltas: Option<tracing::span::Span>,
            index: Option<tracing::span::Span>,
            push: Option<tracing::span::Span>,
            pack: Option<tracing::span::Span>,
            pack_add: Option<tracing::span::Span>,
            pack_delta: Option<tracing::span::Span>,
        }

        let span = Span::current();
        let mut spans = Spans::default();
        {
            let mut rcbs = RemoteCallbacks::new();
            rcbs.transfer_progress(|p| {
                let total = p.total_objects();
                lazy_progress(
                    &mut spans.recv,
                    p.received_objects(),
                    total,
                    || info_span!(parent: &span, "receive"),
                );
                lazy_progress(
                    &mut spans.index,
                    p.indexed_objects(),
                    total,
                    || info_span!(parent: &span, "index"),
                );
                lazy_progress(
                    &mut spans.deltas,
                    p.indexed_deltas(),
                    p.total_deltas(),
                    || info_span!(parent: &span, "deltas"),
                );
                !token.is_cancelled()
            });
            rcbs.pack_progress(|stage, cur, tot| {
                let span = (spans.pack).get_or_insert_with(|| info_span!(parent: &span, "pack"));
                match stage {
                    git2::PackBuilderStage::AddingObjects => {
                        lazy_progress(&mut spans.pack_add, cur, tot, || {
                            let span = info_span!(parent: &*span, "add");
                            span.pb_set_message("Adding objects...");
                            span
                        });
                    }
                    git2::PackBuilderStage::Deltafication => {
                        lazy_progress(&mut spans.pack_delta, cur, tot, || {
                            let span = info_span!(parent: &*span, "delta");
                            span.pb_set_message("Deltafication...");
                            span
                        });
                    }
                }
            });
            rcbs.push_transfer_progress(|cur, tot, _bytes| {
                lazy_progress(
                    &mut spans.push,
                    cur,
                    tot,
                    || info_span!(parent: &span, "push"),
                );
            });
            rcbs.push_negotiation(|updates| {
                trace!("RCB: Push Negotiation");
                for update in updates {
                    debug!(
                        src = update.src_refname().unwrap_or("???"),
                        dst = update.dst_refname().unwrap_or("???"),
                        "-> Pushing update..."
                    );
                }
                Ok(())
            });

            let mut count_last_update = None;
            let count_update_freq = std::time::Duration::from_secs(1);
            rcbs.sideband_progress(|data| {
                let s = String::from_utf8_lossy(data);
                if !s.is_empty() {
                    if s.starts_with("Counting objects ") {
                        let now = Instant::now();
                        if count_last_update
                            .is_none_or(|v: Instant| now.duration_since(v) > count_update_freq)
                        {
                            let span = (spans.count)
                                .get_or_insert_with(|| error_span!(parent: &span, "count"));
                            span.in_scope(|| span.pb_set_message(&s));
                            count_last_update = Some(now);
                        }
                    } else {
                        let _enter = error_span!(parent: &span, "sideband");
                        spans.count = None;
                        debug!("{s}");
                    }
                }
                !token.is_cancelled()
            });
            rcbs.update_tips(|tip, from, to| {
                let _enter = error_span!(parent: &span, "tip", tip).entered();
                info!(%from, %to, "-> Tip Updated");
                !token.is_cancelled()
            });
            rcbs.push_update_reference(|ref_, reason| {
                let _enter = error_span!(parent: &span, "ref", ref_).entered();
                match reason {
                    Some(reason) => warn!(reason, "Reference Update Rejected"),
                    None => info!("Reference Updated"),
                };
                Ok(())
            });

            let r = f(rcbs);
            drop(spans);
            r
        }
    }
}

fn lazy_progress(
    o: &mut Option<tracing::span::Span>,
    pos: usize,
    tot: usize,
    f: impl FnOnce() -> Span,
) -> Option<&mut tracing::span::Span> {
    if pos > 0 && (tot == 0 || pos < tot) {
        let span = o.get_or_insert_with(|| {
            let span = f();
            span.pb_set_style(&gorgon_util_cli::PB_STYLE_BAR_ETA);
            span
        });
        span.in_scope(|| {
            span.pb_set_length(std::cmp::max(tot, pos) as u64);
            span.pb_set_position(pos as u64);
        });
        Some(span)
    } else {
        o.take();
        None
    }
}
