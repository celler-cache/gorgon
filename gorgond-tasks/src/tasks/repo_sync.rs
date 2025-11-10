// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    path::PathBuf,
    time::Instant,
};

use chrono::{TimeDelta, prelude::*};
use clap::Parser;
use derive_more::Display;
use diesel::{prelude::*, sql_query, upsert::excluded};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use git2::{Direction, Remote, RemoteCallbacks, Repository, RepositoryInitOptions};
use gorgon_util::source_url::{GitUrl, SourceUrl};
use gorgond_client::Uuid;
use gorgond_diesel::{TransactionError, models, schema};
use humantime::Duration;
use miette::Context;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error_span, info, info_span, instrument, trace, warn};
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
    Git(#[from] GitError),
    #[error(transparent)]
    Remote(#[from] RemoteError),

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

    #[error("Skipping remote...")]
    #[diagnostic(severity = "warning")]
    WarnSkippingRemote(#[source] RemoteError),
    #[error("Skipping unresolved ref: {_0}")]
    #[diagnostic(severity = "warning")]
    WarnUnresolvedRef(String),
}
impl From<Error> for TransactionError {
    fn from(err: Error) -> Self {
        match err {
            Error::Diesel(err) => err.into(),
            _ => TransactionError::new(err),
        }
    }
}

#[derive(Debug, Display, Serialize, Deserialize)]
#[display("{}", repo.id)]
pub struct RepoSyncTask {
    pub repo: models::Repository,
    pub sources: BTreeMap<Uuid, models::CommitSource>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedCommit {
    pub commit: models::Commit,
    pub sources: Vec<models::CommitSource>,
}
#[serde_as]
#[derive(Debug, Display, Serialize, Deserialize)]
#[display("{}", repo.id)]
pub struct RepoSyncOutput {
    pub repo: models::Repository,
    #[serde_as(as = "BTreeMap<DisplayFromStr, _>")]
    pub resolved_commits: BTreeMap<git2::Oid, ResolvedCommit>,
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

    async fn find(
        &self,
        conn: &mut AsyncPgConnection,
    ) -> Result<impl IntoIterator<Item = Self::Task>, Self::Error> {
        sql_query(concat!(
            "SELECT * FROM gorgond.repositories AS r",
            "  LEFT JOIN gorgond.commit_sources AS cs ON (cs.repository_id = r.id)",
            // "LEFT JOIN gorgond.rollups AS rl ON (rl.repository_id = r.id AND rl.rolled_at IS NULL)",
            "  WHERE cs.id IS NOT NULL", // OR rl.id IS NOT NULL
            "  FOR NO KEY UPDATE OF r SKIP LOCKED",
        ))
        .load_stream::<(models::Repository, Option<models::CommitSource>)>(conn)
        .await?
        .try_fold(BTreeMap::new(), async |mut tasks, (repo, src)| {
            let task = (tasks.entry(repo.id)).or_insert_with(|| RepoSyncTask {
                repo,
                sources: BTreeMap::new(),
            });
            if let Some(src) = src {
                task.sources.entry(src.id).or_insert(src);
            }
            Ok(tasks)
        })
        .await
        .map(|v| v.into_values())
        .map_err(|err| err.into())
    }

    async fn finish(
        &self,
        conn: &mut AsyncPgConnection,
        data: Vec<Self::Output>,
    ) -> Result<(), Self::Error> {
        let mut commits = BTreeMap::new();
        for mut out in data {
            commits.append(&mut out.resolved_commits);
        }

        // Insert commits, update source references.
        let commits = diesel::insert_into(schema::gorgond::commits::table)
            .values(
                (commits.values())
                    .by_ref()
                    .map(|v| &v.commit)
                    .collect::<Vec<&models::Commit>>(),
            )
            .on_conflict((
                schema::gorgond::commits::repository_id,
                schema::gorgond::commits::hash,
            ))
            .do_nothing()
            .returning(models::Commit::as_returning())
            .load_stream(conn)
            .await?
            .map_err(Error::from)
            .try_fold(commits, async |mut commits, commit| -> Result<_, Error> {
                let oid = git2::Oid::from_bytes(&commit.hash).into_git_error("")?;
                let entry = commits.get_mut(&oid).unwrap();
                entry.commit.id = commit.id;
                for src in entry.sources.iter_mut() {
                    src.commit_id = Some(commit.id);
                }
                Ok(commits)
            })
            .await?;

        // Update sources.
        diesel::insert_into(schema::gorgond::commit_sources::table)
            .values(
                commits
                    .into_values()
                    .flat_map(|v| v.sources)
                    .collect::<Vec<models::CommitSource>>(),
            )
            .on_conflict((
                schema::gorgond::commit_sources::repository_id,
                schema::gorgond::commit_sources::source,
            ))
            .do_update()
            .set(
                schema::gorgond::commit_sources::commit_id
                    .eq(excluded(schema::gorgond::commit_sources::commit_id)),
            )
            .execute(conn)
            .await?;

        Ok(())
    }

    fn work(
        &self,
        token: &CancellationToken,
        task: Self::Task,
    ) -> Result<Self::Output, Self::Error> {
        let now = Utc::now();

        let mut remotes = BTreeMap::<Url, RemoteWork>::new();
        let mut refs = BTreeMap::<String, models::CommitSource>::new();
        for source in task.sources.into_values() {
            let _enter = info_span!("source", id = %source.id, url = source.source).entered();

            // Skip invalid sources.
            let Ok(src) = SourceUrl::parse(&source.source)
                .context("Invalid Source URL")
                .inspect_err(|err| warn!("\n{err:?}"))
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
                refs.insert(ref_, source);
            }
        }

        // Do the mucking about with the git repo.
        let r = RepoWrapper::init_for_repo(&task.repo)?;
        let mut storage = r.storage_remote()?;

        // First, try to fetch cached refs from storage. For floating refs, this lets us
        // load the previous commit it referred to, and thus avoid having to fetch the
        // entire history from the remote as well, which is quite slow with large repos.
        info!("Fetching cached refs from storage...");
        r.with_remote(token, &mut storage, Direction::Fetch, |storage| {
            r.fetch(token, storage, refs.keys().map(|r| (r, r)))
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
                    Err(err @ RemoteError(_, _, RemoteErrorKind::Connect(_))) => {
                        let err = miette::Report::new(Error::WarnSkippingRemote(err));
                        warn!("\n{err:?}");
                    }
                    Err(err) => return Err(err.into()),
                };
            }
        }

        // Resolve refs to commit OIDs.
        info!("Resolving refs...");
        let mut output = RepoSyncOutput {
            repo: task.repo,
            resolved_commits: BTreeMap::new(),
        };
        let mut local_refs = BTreeSet::<String>::new();
        for (ref_, src) in refs {
            // Unresolved refs happen, just warn about it (and don't try to push them).
            let Some(oid) = r.resolve(&ref_)? else {
                let err = miette::Report::new(Error::WarnUnresolvedRef(ref_.clone()));
                warn!("\n{err}");
                continue;
            };
            local_refs.insert(ref_);

            // Map it to the output.
            (output.resolved_commits)
                .entry(oid)
                .or_insert_with(|| ResolvedCommit {
                    commit: models::Commit {
                        id: Uuid::now_v7(),
                        repository_id: output.repo.id,
                        hash: oid.as_bytes().to_vec(),
                        created_at: now,
                    },
                    sources: Vec::new(),
                })
                .sources
                .push(src);
        }

        info!("Pushing local refs to storage...");
        r.with_remote(token, &mut storage, Direction::Push, |storage| {
            r.push(token, storage, local_refs)
        })?;

        Ok(output)
    }
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
