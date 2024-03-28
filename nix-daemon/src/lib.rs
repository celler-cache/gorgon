// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod nix;

use chrono::{DateTime, Utc};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use std::{collections::HashMap, future::Future};
use thiserror::Error;
use tokio::io::AsyncReadExt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

trait ResultExt<T, E> {
    fn with_field(self, f: &'static str) -> Result<T>;
}

impl<T, E: Into<Error>> ResultExt<T, E> for Result<T, E> {
    fn with_field(self, f: &'static str) -> Result<T> {
        self.map_err(|err| Error::Field(f, Box::new(err.into())))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    /// This error was encountered while reading/writing a specific field.
    #[error("`{0}`: {1}")]
    Field(&'static str, #[source] Box<Error>),
    #[error("invalid value: {0}")]
    Invalid(String),

    /// Nix protocol error.
    #[error("{0}")]
    NixError(NixError),

    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NixError {
    pub level: Verbosity,
    pub msg: String,
    pub traces: Vec<String>,
}

impl std::fmt::Display for NixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.level, self.msg)?;
        for trace in self.traces.iter() {
            write!(f, "\n\t{}", trace)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stderr {
    Error(NixError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum Verbosity {
    Error = 0,
    Warn,
    Notice,
    Info,
    Talkative,
    Chatty,
    Debug,
    Vomit,
}
impl From<TryFromPrimitiveError<Verbosity>> for Error {
    fn from(value: TryFromPrimitiveError<Verbosity>) -> Self {
        Self::Invalid(format!("Verbosity({:x})", value.number))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ClientSettings {
    pub keep_failed: bool,
    pub keep_going: bool,
    pub try_fallback: bool,
    pub verbosity: Verbosity,
    pub max_build_jobs: u64,
    pub max_silent_time: u64,
    pub verbose_build: bool,
    pub build_cores: u64,
    pub use_substitutes: bool,
    pub overrides: HashMap<String, String>,
}

/// Data about a Nix store path.
#[derive(Debug, PartialEq, Eq)]
pub struct PathInfo {
    /// Derivation that produced this path.
    pub deriver: Option<String>,
    /// Paths referenced by this path.
    pub references: Vec<String>,

    /// NAR hash (in the form: [algo]-[hash]).
    pub nar_hash: String,
    /// NAR size.
    pub nar_size: u64,

    /// Is this path "ultimately trusted", eg. built locally?
    pub ultimate: bool,
    /// Optional signatures, eg. from a binary cache.
    pub signatures: Vec<String>,
    /// An assertion that this path is content-addressed, eg. for fixed-output derivations.
    pub ca: Option<String>,

    /// When the path was registered, eg. placed into the local store.
    pub registration_time: DateTime<Utc>,
}

/// An in-progress operation, which produces a series of status updates before continuing.
pub trait Progress: Send {
    type T: Send;

    /// Returns the next Stderr message, or None after all have been consumed.
    /// This must behave like a fused iterator - once None is returned, all further calls
    /// must immediately return None, without corrupting the underlying datastream, etc.
    fn next(&mut self) -> impl Future<Output = Result<Option<Stderr>>> + Send;

    /// Discards any further messages from `next()` and proceeds.
    fn result(self) -> impl Future<Output = Result<Self::T>> + Send;
}

/// Helper methods for Progress.
pub trait ProgressExt: Progress {
    /// Calls `f()` for each message returned from `self.next()`, then `self.result()`.
    fn tap<F: Fn(Stderr) + Send>(self, f: F) -> impl Future<Output = Result<Self::T>> + Send;

    /// Returns all messages from `self.next()` and `self.result()`.
    fn split(self) -> impl Future<Output = (Vec<Stderr>, Result<Self::T>)> + Send;
}
impl<P: Progress> ProgressExt for P {
    async fn tap<F: Fn(Stderr)>(mut self, f: F) -> Result<Self::T> {
        while let Some(stderr) = self.next().await? {
            f(stderr)
        }
        self.result().await
    }

    async fn split(mut self) -> (Vec<Stderr>, Result<Self::T>) {
        let mut stderrs = Vec::new();
        loop {
            match self.next().await {
                Ok(Some(stderr)) => stderrs.push(stderr),
                Err(err) => break (stderrs, Err(err)),
                Ok(None) => break (stderrs, self.result().await),
            }
        }
    }
}

/// Interface to a Nix store.
pub trait Store {
    /// Returns whether a store path is valid.
    fn is_valid_path<P: AsRef<str> + Send + Sync>(
        &mut self,
        path: P,
    ) -> impl Future<Output = Result<impl Progress<T = bool>>> + Send;

    /// Adds a file to the store.
    fn add_to_store<SN: AsRef<str> + Send + Sync, SC: AsRef<str> + Send + Sync, Refs, R>(
        &mut self,
        name: SN,
        cam_str: SC,
        refs: Refs,
        repair: bool,
        source: R,
    ) -> impl Future<Output = Result<impl Progress<T = (String, PathInfo)>>> + Send
    where
        Refs: IntoIterator + Send,
        Refs::IntoIter: ExactSizeIterator + Send,
        Refs::Item: AsRef<str> + Send + Sync,
        R: AsyncReadExt + Unpin + Send;

    /// Applies client options. This changes the behaviour of future commands.
    fn set_options(
        &mut self,
        opts: ClientSettings,
    ) -> impl Future<Output = Result<impl Progress<T = ()>>> + Send;

    /// Returns a PathInfo struct for the given path.
    fn query_pathinfo<S: AsRef<str> + Send + Sync>(
        &mut self,
        path: S,
    ) -> impl Future<Output = Result<impl Progress<T = Option<PathInfo>>>> + Send;
}
