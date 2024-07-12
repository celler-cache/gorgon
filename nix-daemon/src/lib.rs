// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod nix;

use chrono::{DateTime, Utc};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use std::fmt::Debug;
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stderr {
    /// A plain line of stderr output.
    Next(String),
    /// An error propagated from Nix.
    Error(NixError),
    /// An activity (such as a build) was started.
    StartActivity(StderrStartActivity),
    /// An activity (such as a build) finished.
    StopActivity { act_id: u64 },
    /// A progress update from an activity.
    Result(StderrResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum StderrActivityType {
    Unknown = 0,
    CopyPath = 100,
    FileTransfer = 101,
    Realise = 102,
    CopyPaths = 103,
    Builds = 104,
    Build = 105,
    OptimiseStore = 106,
    VerifyPaths = 107,
    Substitute = 108,
    QueryPathInfo = 109,
    PostBuildHook = 110,
    BuildWaiting = 111,
}
impl From<TryFromPrimitiveError<StderrActivityType>> for Error {
    fn from(value: TryFromPrimitiveError<StderrActivityType>) -> Self {
        Self::Invalid(format!("StderrActivityType({:x})", value.number))
    }
}

// TODO: Decode fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrStartActivity {
    pub act_id: u64,
    pub level: Verbosity,
    pub kind: StderrActivityType,
    pub s: String,
    pub fields: Vec<StderrField>,
    pub parent_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum StderrResultType {
    FileLinked = 100,
    BuildLogLine = 101,
    UntrustedPath = 102,
    CorruptedPath = 103,
    SetPhase = 104,
    Progress = 105,
    SetExpected = 106,
    PostBuildLogLine = 107,
}
impl From<TryFromPrimitiveError<StderrResultType>> for Error {
    fn from(value: TryFromPrimitiveError<StderrResultType>) -> Self {
        Self::Invalid(format!("StderrResultType({:x})", value.number))
    }
}

// TODO: Decode fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrResult {
    pub act_id: u64,
    pub kind: StderrResultType,
    pub fields: Vec<StderrField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StderrField {
    Int(u64),
    String(String),
}

impl StderrField {
    pub fn as_int(&self) -> Option<&u64> {
        if let Self::Int(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_string(&self) -> Option<&String> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum BuildMode {
    Normal,
    Repair,
    Check,
}
impl From<TryFromPrimitiveError<BuildMode>> for Error {
    fn from(value: TryFromPrimitiveError<BuildMode>) -> Self {
        Self::Invalid(format!("BuildMode({:x})", value.number))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum BuildResultStatus {
    Built = 0,
    Substituted = 1,
    AlreadyValid = 2,
    PermanentFailure = 3,
    InputRejected = 4,
    OutputRejected = 5,
    /// "possibly transient", the CppNix source helpfully points out.
    TransientFailure = 6,
    /// No longer used, apparently - TODO: Figure out since when.
    CachedFailure = 7,
    TimedOut = 8,
    MiscFailure = 9,
    DependencyFailed = 10,
    LogLimitExceeded = 11,
    NotDeterministic = 12,
    ResolvesToAlreadyValid = 13,
    NoSubstituters = 14,
}
impl From<TryFromPrimitiveError<BuildResultStatus>> for Error {
    fn from(value: TryFromPrimitiveError<BuildResultStatus>) -> Self {
        Self::Invalid(format!("BuildResultStatus({:x})", value.number))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildResult {
    pub status: BuildResultStatus,
    pub error_msg: String,
    pub times_built: u64,
    pub is_non_deterministic: bool,
    pub start_time: DateTime<Utc>,
    pub stop_time: DateTime<Utc>,
    /// Map of (name, path).
    pub built_outputs: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    type Error: From<Error> + Send + Sync;

    /// Returns the next Stderr message, or None after all have been consumed.
    /// This must behave like a fused iterator - once None is returned, all further calls
    /// must immediately return None, without corrupting the underlying datastream, etc.
    fn next(&mut self) -> impl Future<Output = Result<Option<Stderr>, Self::Error>> + Send;

    /// Discards any further messages from `next()` and proceeds.
    fn result(self) -> impl Future<Output = Result<Self::T, Self::Error>> + Send;
}

/// Helper methods for Progress.
pub trait ProgressExt: Progress {
    /// Calls `f()` for each message returned from `self.next()`, then `self.result()`.
    fn tap<F: Fn(Stderr) + Send>(
        self,
        f: F,
    ) -> impl Future<Output = Result<Self::T, Self::Error>> + Send;

    /// Returns all messages from `self.next()` and `self.result()`.
    fn split(self) -> impl Future<Output = (Vec<Stderr>, Result<Self::T, Self::Error>)> + Send;
}
impl<P: Progress> ProgressExt for P {
    // TODO: This name is bad.
    async fn tap<F: Fn(Stderr)>(mut self, f: F) -> Result<Self::T, Self::Error> {
        while let Some(stderr) = self.next().await? {
            f(stderr)
        }
        self.result().await
    }

    async fn split(mut self) -> (Vec<Stderr>, Result<Self::T, Self::Error>) {
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
    type Error: From<Error> + Send + Sync;

    /// Returns whether a store path is valid.
    fn is_valid_path<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Future<Output = Result<impl Progress<T = bool, Error = Self::Error>, Self::Error>> + Send;

    /// Returns whether QuerySubstitutablePathInfos would return anything.
    fn has_substitutes<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Future<Output = Result<impl Progress<T = bool, Error = Self::Error>, Self::Error>> + Send;

    /// Adds a file to the store.
    fn add_to_store<
        SN: AsRef<str> + Send + Sync + Debug,
        SC: AsRef<str> + Send + Sync + Debug,
        Refs,
        R,
    >(
        &mut self,
        name: SN,
        cam_str: SC,
        refs: Refs,
        repair: bool,
        source: R,
    ) -> impl Future<
        Output = Result<impl Progress<T = (String, PathInfo), Error = Self::Error>, Self::Error>,
    > + Send
    where
        Refs: IntoIterator + Send + Debug,
        Refs::IntoIter: ExactSizeIterator + Send,
        Refs::Item: AsRef<str> + Send + Sync,
        R: AsyncReadExt + Unpin + Send + Debug;

    /// Builds the specified paths.
    fn build_paths<Paths>(
        &mut self,
        paths: Paths,
        mode: BuildMode,
    ) -> impl Future<Output = Result<impl Progress<T = (), Error = Self::Error>, Self::Error>> + Send
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Ensure the specified store path exists.
    fn ensure_path<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Future<Output = Result<impl Progress<T = (), Error = Self::Error>, Self::Error>> + Send;

    /// Creates a temporary GC root, which persists until the daemon restarts.
    fn add_temp_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Future<Output = Result<impl Progress<T = (), Error = Self::Error>, Self::Error>> + Send;

    /// Creates a persistent GC root. This is what's normally meant by a GC root.
    fn add_indirect_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> impl Future<Output = Result<impl Progress<T = (), Error = Self::Error>, Self::Error>> + Send;

    /// Returns the (link, target) of all known GC roots.
    fn find_roots(
        &mut self,
    ) -> impl Future<
        Output = Result<
            impl Progress<T = HashMap<String, String>, Error = Self::Error>,
            Self::Error,
        >,
    > + Send;

    /// Applies client options. This changes the behaviour of future commands.
    fn set_options(
        &mut self,
        opts: ClientSettings,
    ) -> impl Future<Output = Result<impl Progress<T = (), Error = Self::Error>, Self::Error>> + Send;

    /// Returns a PathInfo struct for the given path.
    fn query_pathinfo<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Future<
        Output = Result<impl Progress<T = Option<PathInfo>, Error = Self::Error>, Self::Error>,
    > + Send;

    /// Returns which of the passed paths are valid.
    fn query_valid_paths<Paths>(
        &mut self,
        paths: Paths,
        use_substituters: bool,
    ) -> impl Future<Output = Result<impl Progress<T = Vec<String>, Error = Self::Error>, Self::Error>>
           + Send
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Returns paths which can be substituted.
    fn query_substitutable_paths<Paths>(
        &mut self,
        paths: Paths,
    ) -> impl Future<Output = Result<impl Progress<T = Vec<String>, Error = Self::Error>, Self::Error>>
           + Send
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync;

    /// Returns a list of valid derivers for a path.
    /// This is sort of like PathInfo.deriver, but it doesn't lie to you.
    fn query_valid_derivers<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> impl Future<Output = Result<impl Progress<T = Vec<String>, Error = Self::Error>, Self::Error>>
           + Send;

    /// Takes a list of paths and queries which would be built, substituted or unknown,
    /// along with an estimate of the cumulative download and NAR sizes.
    fn query_missing<Ps>(
        &mut self,
        paths: Ps,
    ) -> impl Future<
        Output = Result<impl Progress<T = QueryMissing, Error = Self::Error>, Self::Error>,
    > + Send
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync;

    /// Returns a map of (output, store path) for the given derivation.
    fn query_derivation_output_map<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> impl Future<
        Output = Result<
            impl Progress<T = HashMap<String, String>, Error = Self::Error>,
            Self::Error,
        >,
    > + Send;

    fn build_paths_with_results<Ps>(
        &mut self,
        paths: Ps,
        mode: BuildMode,
    ) -> impl Future<
        Output = Result<
            impl Progress<T = HashMap<String, BuildResult>, Error = Self::Error>,
            Self::Error,
        >,
    > + Send
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync;
}

#[derive(Debug, PartialEq, Eq)]
pub struct QueryMissing {
    pub will_build: Vec<String>,
    pub will_substitute: Vec<String>,
    pub unknown: Vec<String>,
    pub download_size: u64,
    pub nar_size: u64,
}
