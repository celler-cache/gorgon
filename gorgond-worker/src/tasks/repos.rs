// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{collections::BTreeMap, fmt::Display, path::PathBuf};

use chrono::prelude::*;
use crossbeam::channel::Sender;
use diesel::{dsl, prelude::*};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use git2::{Direction, Remote, RemoteCallbacks, Repository, RepositoryInitOptions};
use gorgon_util::source_url::{GitUrl, SourceUrl};
use gorgond_client::Uuid;
use gorgond_diesel::{models, schema};
use miette::{Context, IntoDiagnostic, Result, bail};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error_span, info, info_span, instrument, trace, warn};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use url::{Position, Url};

use crate::tasks::Task;

const REMOTE_STORAGE: &str = "storage";

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Git(#[from] GitError),

    #[error("Remote '{REMOTE_STORAGE}' error")]
    #[diagnostic(
        help = "This should never happen, your mirror repository is probably broken. Try deleting it?"
    )]
    StorageRemote(#[source] GitError),
    #[error("Remote Error: {_0}")]
    Remote(String, #[source] GitError),

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

pub struct RepoTask {
    pub repo: models::Repository,
}
impl RepoTask {
    #[instrument(name = "repo", skip_all)]
    pub async fn poll(send: &Sender<Task>, conn: &mut AsyncPgConnection) -> miette::Result<()> {
        schema::gorgond::repositories::table
            .left_join(schema::gorgond::commit_sources::table)
            .group_by(schema::gorgond::repositories::id)
            .select(models::Repository::as_select())
            .having(dsl::count(schema::gorgond::commit_sources::id).gt(0))
            .load_stream::<models::Repository>(conn)
            .await
            .into_diagnostic()
            .context("Couldn't fetch repositories")?
            .map_err(miette::Report::from_err)
            .try_for_each(async |repo| {
                send.send(Task::Repo(RepoTask { repo }))
                    .into_diagnostic()
                    .context("Couldn't send RepoTask")
            })
            .await
    }

    #[instrument(name = "repo", skip_all, fields(
        id = %self.repo.id,
        kind = self.repo.kind,
        name = self.repo.ident,
    ))]
    pub fn run(
        &self,
        args: &crate::Args,
        rt: &Runtime,
        token: &CancellationToken,
        conn: &mut AsyncPgConnection,
    ) -> miette::Result<()> {
        let now = Utc::now();

        // Re-select and lock the repository. Skip if someone else has already locked it.
        let Some(repo) = rt
            .block_on(
                models::Repository::all()
                    .find(self.repo.id)
                    .for_no_key_update()
                    .skip_locked()
                    .get_result(conn),
            )
            .optional()
            .into_diagnostic()
            .context("Couldn't update/lock repository")?
        else {
            debug!("Skipping missing or locked repository...");
            return Ok(());
        };

        // Find sources.
        let sources = rt
            .block_on(
                models::CommitSource::belonging_to(&repo)
                    .for_no_key_update()
                    .load::<models::CommitSource>(conn),
            )
            .into_diagnostic()
            .context("Couldn't load CommitSources")?;

        // Parse source URLs.
        let mut remotes = BTreeMap::<Url, RemoteTask>::new();
        let mut ref_source_id = BTreeMap::<String, Uuid>::new();
        for source in sources.iter() {
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
            if (source.fetched_at).is_some_and(|t| (t - now) > args.ref_stale) {
                debug!("Skipping fresh dynamic URL...");
                continue;
            }

            // Add it to the work for the relevant remote.
            let ref_ = remotes
                .entry(RemoteTask::url(&src).clone()) // TODO: Pointless allocation.
                .or_default()
                .add_source(&src)?;
            if let Some(ref_) = ref_ {
                if let Some(old_id) = ref_source_id.insert(ref_.clone(), source.id) {
                    bail!(Error::RefMultipleCommitSources(ref_, old_id, source.id));
                }
            }
        }

        // Do the mucking about with the git repo.
        let r = RepoWrapper::init_for_repo(&repo)?;
        let mut storage = r.storage_remote()?;

        info!("Fetching cached refs from storage...");
        r.with_remote(token, &mut storage, Direction::Fetch, |storage| {
            r.fetch(
                token,
                storage,
                remotes
                    .values()
                    .flat_map(|t| t.refs_from_git.keys().map(|k| (k, k))),
            )
        })
        .context("Couldn't fetch from storage")?;

        {
            let span = info_span!("remotes");
            let _enter = span.enter();
            span.pb_set_style(&crate::PB_STYLE_BAR);
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
                    Err(err) => bail!(err),
                };
            }
        }

        info!("Pushing local refs to storage...");
        let repo_refs = (r.repo)
            .references_glob("refs/gorgon/*")
            .and_then(|refs| {
                refs.filter_map(|r| r.map(|r| r.name().map(String::from)).transpose())
                    .collect::<Result<Vec<_>, git2::Error>>()
            })
            .into_git_error(&r.path)?;
        r.with_remote(token, &mut storage, Direction::Push, |storage| {
            r.push(token, storage, repo_refs)
        })
        .context("Couldn't push to storage")?;

        // Resolve refs to commit OIDs.
        info!("Resolving refs...");
        let ref_oids = (ref_source_id.keys())
            .filter_map(|ref_| match r.resolve(ref_) {
                Ok(Some(oid)) => Some(Ok((ref_.clone(), oid))),
                Ok(None) => {
                    let err = miette::Report::new(Error::WarnUnresolvedRef(ref_.clone()));
                    warn!("\n{err}");
                    None
                }
                Err(err) => Some(Err(err)),
            })
            .collect::<Result<BTreeMap<String, git2::Oid>, _>>()
            .context("Couldn't resolve refs")?;

        // Create Commits.
        info!("Creating Commits...");
        let now = Utc::now();
        rt.block_on(
            diesel::insert_into(schema::gorgond::commits::table)
                .values(
                    (ref_oids.values())
                        .map(|oid| -> Result<_> {
                            Ok(models::Commit {
                                id: Uuid::now_v7(),
                                repository_id: repo.id,
                                hash: oid.as_bytes().into(),
                                created_at: now,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .on_conflict((
                    schema::gorgond::commits::repository_id,
                    schema::gorgond::commits::hash,
                ))
                .do_nothing()
                .execute(conn),
        )
        .into_diagnostic()
        .context("Couldn't create commits")?;

        // Update CommitSources.
        for (ref_, id) in ref_source_id {
            let Some(oid) = ref_oids.get(&ref_) else {
                let err = miette::Report::new(Error::WarnUnresolvedRef(ref_.clone()));
                warn!("\n{err}");
                continue;
            };
            info!(%id, %oid, ref_, "Updating CommitSource...");
            rt.block_on(
                diesel::update(schema::gorgond::commit_sources::table)
                    .filter(schema::gorgond::commit_sources::id.eq(id))
                    .set(
                        schema::gorgond::commit_sources::commit_id.eq(
                            schema::gorgond::commits::table
                                .filter(schema::gorgond::commits::repository_id.eq(repo.id))
                                .filter(schema::gorgond::commits::hash.eq(oid.as_bytes().to_vec()))
                                .select(schema::gorgond::commits::id)
                                .single_value(),
                        ),
                    )
                    .execute(conn),
            )
            .into_diagnostic()
            .context("Couldn't update CommitSources")?;
        }

        Ok(())
    }

    pub fn context(&self) -> String {
        format!(
            "RepoTask({}, {}:{})",
            self.repo.id, self.repo.kind, self.repo.ident
        )
    }
}

#[derive(Default)]
pub struct RemoteTask {
    pub refs_from_git: BTreeMap<String, String>,
}
impl RemoteTask {
    pub fn url(src: &SourceUrl) -> &Url {
        match src {
            SourceUrl::Git(GitUrl { base_url, .. }) => base_url,
        }
    }
    pub fn add_source(&mut self, src: &SourceUrl) -> Result<Option<String>> {
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
            bail!(Error::RefMultipleSources(to, old, from.into()));
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
        Ok((self.repo.remote_anonymous(url))
            .into_git_error(&self.path)
            .map_err(|err| Error::Remote(url.to_string(), err))?)
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

            rcbs.sideband_progress(|data| {
                let s = String::from_utf8_lossy(data);
                if !s.is_empty() {
                    if s.starts_with("Counting objects ") {
                        let span = (spans.count)
                            .get_or_insert_with(|| error_span!(parent: &span, "count"));
                        span.in_scope(|| span.pb_set_message(&s));
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
            span.pb_set_style(&crate::PB_STYLE_BAR_ETA);
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
