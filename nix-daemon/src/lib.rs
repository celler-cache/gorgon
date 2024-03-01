// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod wire;

use chrono::{DateTime, Utc};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use std::collections::HashMap;
use std::future::Future;
use tap::TapOptional;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

/// Minimum supported protocol version. Older versions will be rejected.
///
/// Protocol 1.35 was introduced in Nix 2.15:
/// https://github.com/NixOS/nix/blob/2.15.0/src/libstore/worker-protocol.hh#L13
///
/// TODO: Support Protocol 1.21, used by Nix 2.3.
const MIN_PROTO: Proto = Proto(1, 35); // Nix >= 2.15.x

/// Maxmimum supported protocol version. Newer daemons will run in compatibility mode.
///
/// Protocol 1.35 is current as of Nix 2.19:
/// https://github.com/NixOS/nix/blob/2.19.3/src/libstore/worker-protocol.hh#L12
const MAX_PROTO: Proto = Proto(1, 35); // Nix <= 2.19.x

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

impl From<TryFromPrimitiveError<wire::Op>> for Error {
    fn from(value: TryFromPrimitiveError<wire::Op>) -> Self {
        Self::Invalid(format!("Op({:x})", value.number))
    }
}
impl From<TryFromPrimitiveError<Verbosity>> for Error {
    fn from(value: TryFromPrimitiveError<Verbosity>) -> Self {
        Self::Invalid(format!("Verbosity({:x})", value.number))
    }
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

/// Protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Proto(u8, u8);

impl From<u64> for Proto {
    fn from(raw: u64) -> Self {
        Self(((raw & 0xFF00) >> 8) as u8, (raw & 0x00FF) as u8)
    }
}
impl From<Proto> for u64 {
    fn from(v: Proto) -> Self {
        ((v.0 as u64) << 8) | (v.1 as u64)
    }
}

impl std::fmt::Display for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.0, self.1)
    }
}

impl Proto {
    fn since(&self, v: u8) -> bool {
        self.1 >= v
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

/// Interface to a Store.
pub trait Store {
    type C: AsyncReadExt + Unpin;

    type SetOptionsResult: ProgressResult<T = ()>;
    fn set_options(
        &mut self,
        opts: ClientSettings,
    ) -> impl Future<Output = Result<Progress<Self::C, Self::SetOptionsResult>>> + Send;
}

pub trait ProgressResult {
    type T;
    fn result(self) -> impl Future<Output = Result<Self::T>> + Send;
}
impl ProgressResult for () {
    type T = ();
    async fn result(self) -> Result<Self::T> {
        Ok(())
    }
}

pub struct Progress<'c, C: AsyncReadExt + Unpin, R: ProgressResult> {
    conn: &'c mut C,
    fuse: bool,
    then: R,
}

impl<'c, C: AsyncReadExt + Unpin, R: ProgressResult> Progress<'c, C, R> {
    fn new(conn: &'c mut C, then: R) -> Self {
        Self {
            conn,
            fuse: false,
            then,
        }
    }

    /// Returns the next Stderr message, or None after all have been consumed.
    /// This behaves like a fused iterator, and keeps returning None after that.
    pub async fn next(&mut self) -> Result<Option<Stderr>> {
        if self.fuse {
            Ok(None)
        } else {
            wire::read_stderr(&mut self.conn)
                .await
                .map(|v| v.tap_none(|| self.fuse = true))
        }
    }

    /// Discards any remaining Stderr messages and proceeds.
    pub async fn result(mut self) -> Result<R::T> {
        while let Some(_) = self.next().await? {}
        self.then.result().await
    }
}

/// Store backed by nix-daemon.
/// TODO: Not sure about this naming. Ask some people?
pub struct DaemonStore<C: AsyncReadExt + AsyncWriteExt + Unpin> {
    conn: C,
    pub proto: Proto,
}

impl DaemonStore<UnixStream> {
    pub async fn connect_unix<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::init(UnixStream::connect(path).await?).await
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin> DaemonStore<C> {
    async fn init(mut conn: C) -> Result<Self> {
        // Exchange magic numbers.
        wire::write_u64(&mut conn, wire::WORKER_MAGIC_1)
            .await
            .with_field("magic1")?;
        wire::read_u64(&mut conn)
            .await
            .and_then(|magic2| match magic2 {
                wire::WORKER_MAGIC_2 => Ok(magic2),
                _ => Err(Error::Invalid(format!("{:#x}", magic2))),
            })
            .with_field("magic2")?;

        // Check that we're talking to a new enough daemon, tell them our version.
        let proto = wire::read_proto(&mut conn)
            .await
            .and_then(|proto| {
                if proto.0 != 1 || proto < MIN_PROTO {
                    return Err(Error::Invalid(format!("{}", proto)));
                }
                Ok(proto)
            })
            .with_field("daemon_proto")?;
        wire::write_proto(&mut conn, MAX_PROTO)
            .await
            .with_field("client_proto")?;

        // Write some obsolete fields.
        if proto >= Proto(1, 14) {
            wire::write_u64(&mut conn, 0)
                .await
                .with_field("__obsolete_cpu_affinity")?;
        }
        if proto >= Proto(1, 11) {
            wire::write_bool(&mut conn, false)
                .await
                .with_field("__obsolete_reserve_space")?;
        }

        // And we don't currently do anything with these.
        if proto >= Proto(1, 33) {
            wire::read_string(&mut conn)
                .await
                .with_field("nix_version")?;
        }
        if proto >= Proto(1, 35) {
            // Option<bool>: 0 = None, 1 = Some(true), 2 = Some(false)
            wire::read_u64(&mut conn).await.with_field("remote_trust")?;
        }

        // Discard Stderr. There shouldn't be anything here anyway.
        while let Some(_) = wire::read_stderr(&mut conn).await? {}

        Ok(Self { conn, proto })
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin + Send> Store for DaemonStore<C> {
    type C = C;

    type SetOptionsResult = ();
    async fn set_options(
        &mut self,
        opts: ClientSettings,
    ) -> Result<Progress<Self::C, Self::SetOptionsResult>> {
        wire::write_op(&mut self.conn, wire::Op::SetOptions)
            .await
            .with_field("SetOptions.<op>")?;
        wire::write_client_settings(&mut self.conn, self.proto, &opts)
            .await
            .with_field("SetOptions.clientSettings")?;
        Ok(Progress::new(&mut self.conn, ()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sanity check for version comparisons.
    #[test]
    fn test_version_ord() {
        assert!(Proto(0, 1) > Proto(0, 0));
        assert!(Proto(1, 0) > Proto(0, 0));
        assert!(Proto(1, 0) > Proto(0, 1));
        assert!(Proto(1, 1) > Proto(1, 0));
    }
}
