// SPDX-FileCopyrightText: 2026 embr <hi@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use chrono::Duration;
use git2::{
    Direction, FetchOptions, PushOptions, Remote, RemoteCallbacks, Repository,
    RepositoryInitOptions,
};
use gorgon_util::source_url::{GitUrl, SourceUrl};
use gorgond_diesel::models;
use miette::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error_span, info, info_span, instrument, trace, warn, Span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

const REMOTE_STORAGE: &str = "storage";

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum MirrorError {
    #[error("Remote '{REMOTE_STORAGE}' error")]
    #[diagnostic(
        help = "This should never happen, your mirror repository is probably broken. Try deleting it?"
    )]
    StorageRemote(#[source] GitError),

    #[error("Source URL has neither rev= nor ref=, what are we fetching?")]
    NoRevOrRef,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{0}")]
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

pub struct Mirror {
    pub path: PathBuf,
    pub repo: Repository,
}
impl Mirror {
    pub fn init_repo(r: &models::Repository) -> Result<Self> {
        let path = crate::DIRS
            .cache_dir()
            .join(format!("mirrors/{}/{}.git", r.kind, r.ident));
        Self::init(path, &r.storage)
    }
    #[instrument(name = "mirror", skip_all)]
    pub fn init(path: PathBuf, storage: &str) -> Result<Self> {
        debug!(path = %path.display(), "Initializing mirror...");
        let repo = Repository::init_opts(&path, RepositoryInitOptions::new().bare(true))
            .into_git_error(&path)
            .context("Couldn't init git repository")?;

        debug!("Adding {REMOTE_STORAGE} remote...");
        match repo.remote(REMOTE_STORAGE, storage) {
            Ok(_) => Ok(()),
            Err(err) if err.code() == git2::ErrorCode::Exists => Ok(()),
            Err(err) => Err(MirrorError::StorageRemote(err.into_git_error(&path))),
        }?;

        Ok(Self { path, repo })
    }

    pub fn storage_remote(&self) -> Result<Remote, MirrorError> {
        (self.repo.find_remote(REMOTE_STORAGE))
            .into_git_error(&self.path)
            .map_err(MirrorError::StorageRemote)
    }

    pub fn fetch_sources(
        &self,
        token: &CancellationToken,
        ref_srcs: impl IntoIterator<Item = (impl AsRef<str>, SourceUrl)>,
    ) -> Result<()> {
        let span = Span::current();
        let mut storage = self.storage_remote()?;

        let mut all_refs = Vec::<String>::new();
        let mut remote_specs = BTreeMap::<String, Vec<String>>::new();
        for (dst_ref, src) in ref_srcs {
            let dst_ref = dst_ref.as_ref();
            all_refs.push(dst_ref.to_string());

            match src {
                SourceUrl::Git(GitUrl {
                    base_url,
                    rev: src_rev,
                    ref_: src_ref,
                }) => {
                    let from = (src_rev.or(src_ref)).ok_or(MirrorError::NoRevOrRef)?;
                    (remote_specs.entry(base_url.as_str().into()).or_default())
                        .push(format!("{from}:{dst_ref}"));
                }
            }
        }

        span.pb_set_style(&crate::PB_STYLE_BAR);
        span.pb_set_length(remote_specs.len() as u64 + 2);

        // Try fetching refs from storage first.
        {
            let _enter = info_span!("storage_fetch").entered();
            span.pb_set_message("Fetching from storage...");
            info!(?all_refs, "Fetching from storage...");
            self.fetch_specs(token, &mut storage, &all_refs)
        }
        .context("Couldn't fetch from storage")?;
        span.pb_inc(1);

        // Then try fetching specs from remotes.
        span.pb_set_message("Fetching from remotes...");
        for (url, specs) in remote_specs.into_iter() {
            let _enter = info_span!("remote", url).entered();
            let mut remote = (self.repo.remote_anonymous(&url))
                .into_git_error(&self.path)
                .with_context(|| url.clone())
                .context("Couldn't create anonymous remote")?;
            self.with_remote(&span, token, &mut remote, Direction::Fetch, |remote| {
                info!(?specs, "Fetching...");
                self.fetch_specs(token, remote, &specs)
                    .with_context(|| url)
                    .context("Couldn't fetch remote targets")
            })?;
            span.pb_inc(1);
        }

        // And push back to storage.
        {
            let _enter = info_span!(
                "storage_push",
                name = storage.name(),
                url = storage.url(),
                pushurl = storage.pushurl()
            )
            .entered();
            span.pb_set_message("Pushing to storage...");
            let repo_refs = self
                .repo
                .references_glob("refs/gorgon/*")
                .and_then(|refs| {
                    refs.filter_map(|r| r.map(|r| r.name().map(String::from)).transpose())
                        .collect::<Result<Vec<_>, git2::Error>>()
                })
                .into_git_error(&self.path)?;
            info!(?repo_refs, "Pushing to storage...");
            self.push_specs(token, &mut storage, &repo_refs)
                .context("Couldn't push to storage")
        }?;
        span.pb_inc(1);

        Ok(())
    }
    fn fetch_specs(
        &self,
        token: &CancellationToken,
        remote: &mut Remote,
        specs: &[String],
    ) -> Result<(), GitError> {
        self.with_progress(&Span::current(), token, |rcbs| {
            let mut opts = FetchOptions::new();
            remote
                .fetch(specs, Some(opts.remote_callbacks(rcbs)), None)
                .into_git_error(&self.path)
        })
    }
    fn push_specs(
        &self,
        token: &CancellationToken,
        remote: &mut Remote,
        specs: &[String],
    ) -> Result<(), GitError> {
        self.with_progress(&Span::current(), token, |rcbs| {
            let mut opts = PushOptions::new();
            opts.packbuilder_parallelism(
                std::thread::available_parallelism()
                    .map(|num| num.get() as u32)
                    .unwrap_or(1),
            );
            remote
                .push(specs, Some(opts.remote_callbacks(rcbs)))
                .into_git_error(&self.path)
        })
    }

    fn with_remote<T, F: FnOnce(&mut Remote) -> Result<T>>(
        &self,
        span: &Span,
        token: &CancellationToken,
        remote: &mut Remote,
        dir: git2::Direction,
        f: F,
    ) -> Result<T> {
        self.with_progress(span, token, |rcbs| {
            // Manually connect. This step is sometimes *very slow* on this connection,
            // for some reason, so this just makes it visible.
            let mut conn = info_span!("connect").in_scope(|| {
                debug!("Connecting...");
                let start = Instant::now();
                remote
                    .connect_auth(dir, Some(rcbs), None)
                    .inspect(move |_| {
                        let t = Duration::from_std(Instant::now().duration_since(start))
                            .unwrap_or_default();
                        debug!(%t, "Connected!")
                    })
                    .into_git_error(&self.path)
                    .context("Couldn't connect")
            })?;
            f(conn.remote())
        })
    }

    fn with_progress<T, F: FnOnce(RemoteCallbacks) -> T>(
        &self,
        span: &Span,
        token: &CancellationToken,
        f: F,
    ) -> T {
        let (mut span_recv, mut span_deltas, mut span_index, mut span_pack, mut span_push) =
            (None, None, None, None, None);
        {
            let mut rcbs = RemoteCallbacks::new();
            rcbs.transfer_progress(|p| {
                let total = p.total_objects();
                lazy_progress(
                    &mut span_recv,
                    p.received_objects(),
                    total,
                    || info_span!(parent: span, "receive"),
                );
                lazy_progress(
                    &mut span_index,
                    p.indexed_objects(),
                    total,
                    || info_span!(parent: span, "index"),
                );
                lazy_progress(
                    &mut span_deltas,
                    p.indexed_deltas(),
                    p.total_deltas(),
                    || info_span!(parent: span, "deltas"),
                );
                !token.is_cancelled()
            });
            rcbs.pack_progress(|stage, cur, tot| {
                lazy_progress(
                    &mut span_pack,
                    cur,
                    tot,
                    || info_span!(parent: span, "pack"),
                )
                .inspect(|span| {
                    span.pb_set_message(match stage {
                        git2::PackBuilderStage::AddingObjects => "Adding objects...",
                        git2::PackBuilderStage::Deltafication => "Deltafication...",
                    });
                });
            });
            rcbs.push_transfer_progress(|cur, tot, _bytes| {
                lazy_progress(
                    &mut span_push,
                    cur,
                    tot,
                    || info_span!(parent: span, "push"),
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
                debug!("{}", String::from_utf8_lossy(data));
                !token.is_cancelled()
            });
            rcbs.update_tips(|tip, from, to| {
                let _enter = error_span!(parent: span, "tip", tip).entered();
                info!(%from, %to, "-> Tip Updated");
                !token.is_cancelled()
            });
            rcbs.push_update_reference(|ref_, reason| {
                let _enter = error_span!(parent: span, "ref", ref_).entered();
                match reason {
                    Some(reason) => warn!(reason, "Reference Update Rejected"),
                    None => info!("Reference Updated"),
                };
                Ok(())
            });

            f(rcbs)
        }
    }
}

fn lazy_progress(
    o: &mut Option<tracing::span::EnteredSpan>,
    pos: usize,
    tot: usize,
    f: impl FnOnce() -> Span,
) -> Option<&mut tracing::span::EnteredSpan> {
    if pos > 0 && (tot == 0 || pos < tot) {
        let span = o.get_or_insert_with(|| {
            let span = f();
            span.pb_set_style(&crate::PB_STYLE_BAR_ETA);
            span.entered()
        });
        span.pb_set_length(std::cmp::max(tot, pos) as u64);
        span.pb_set_position(pos as u64);
        Some(span)
    } else {
        o.take();
        None
    }
}
