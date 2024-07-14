// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

//! Interfaces to nix-daemon (or compatible) Stores.
//! ------------------------------------------------
//!
//! This module currently implements support for Protocol 1.35, and Nix 2.15+.
//!
//! Support for older versions will be added in the future - in particular, Protocol 1.21
//! used by Nix 2.3.

pub mod wire;

use crate::{
    BuildMode, ClientSettings, Error, PathInfo, Progress, Result, ResultExt, Stderr, Store,
};
use std::future::Future;
use std::{collections::HashMap, fmt::Debug};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use tokio_stream::StreamExt;
use tracing::{info, instrument};

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

/// Internal [`crate::Progress`] implementation used by [`DaemonStore`].
pub struct DaemonProgress<'s, C, T: Send, F, FF>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    F: FnOnce(&'s mut DaemonStore<C>) -> FF + Send,
    FF: Future<Output = Result<T>> + Send,
{
    store: &'s mut DaemonStore<C>,
    fuse: bool,
    then: F,
}
impl<'s, C, T: Send, F, FF> DaemonProgress<'s, C, T, F, FF>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    F: FnOnce(&'s mut DaemonStore<C>) -> FF + Send,
    FF: Future<Output = Result<T>> + Send,
{
    fn new(store: &'s mut DaemonStore<C>, then: F) -> Self {
        Self {
            store,
            fuse: false,
            then,
        }
    }
}
impl<'s, C, T: Send, F, FF> Progress for DaemonProgress<'s, C, T, F, FF>
where
    C: AsyncReadExt + AsyncWriteExt + Unpin + Send,
    F: FnOnce(&'s mut DaemonStore<C>) -> FF + Send,
    FF: Future<Output = Result<T>> + Send,
{
    type T = T;
    type Error = Error;

    async fn next(&mut self) -> Result<Option<Stderr>> {
        if self.fuse {
            Ok(None)
        } else {
            match wire::read_stderr(&mut self.store.conn).await? {
                Some(Stderr::Error(err)) => Err(Error::NixError(err)),
                Some(stderr) => Ok(Some(stderr)),
                None => {
                    self.fuse = true;
                    Ok(None)
                }
            }
        }
    }

    async fn result(mut self) -> Result<Self::T> {
        while let Some(_) = self.next().await? {}
        (self.then)(self.store).await
    }
}

#[derive(Debug, Default)]
pub struct DaemonStoreBuilder {
    // This will do things in the future.
}

impl DaemonStoreBuilder {
    /// Initializes a [`DaemonStore`] by adopting a connection.
    ///
    /// It's up to the caller that the connection is in a state to begin a nix handshake, eg.
    /// it behaves like a fresh connection to the daemon socket - if this is a connection through
    /// a proxy, any proxy handshakes should already have taken place, etc.
    ///
    /// ```no_run
    /// use tokio::net::UnixStream;
    /// use nix_daemon::nix::DaemonStore;
    ///
    /// # async {
    /// let conn = UnixStream::connect("/nix/var/nix/daemon-socket/socket").await?;
    /// let store = DaemonStore::builder().init(conn).await?;
    /// # Ok::<_, nix_daemon::Error>(())
    /// # };
    /// ```
    pub async fn init<C: AsyncReadExt + AsyncWriteExt + Unpin>(
        self,
        conn: C,
    ) -> Result<DaemonStore<C>> {
        let mut store = DaemonStore {
            conn,
            buffer: [0u8; 1024],
            proto: Proto(0, 0),
        };
        store.handshake().await?;
        Ok(store)
    }

    /// Connects to a Nix daemon via a unix socket.
    /// The path is usually `/nix/var/nix/daemon-socket/socket`.
    ///
    /// ```no_run
    /// use nix_daemon::{Store, Progress, nix::DaemonStore};
    ///
    /// # async {
    /// let store = DaemonStore::builder()
    ///     .connect_unix("/nix/var/nix/daemon-socket/socket")
    ///     .await?;
    /// # Ok::<_, nix_daemon::Error>(())
    /// # };
    /// ```
    pub async fn connect_unix<P: AsRef<std::path::Path>>(
        self,
        path: P,
    ) -> Result<DaemonStore<UnixStream>> {
        self.init(UnixStream::connect(path).await?).await
    }
}

/// Store backed by a `nix-daemon` (or compatible store). Implements [`crate::Store`].
///
/// ```no_run
/// use nix_daemon::{Store, Progress, nix::DaemonStore};
///
/// # async {
/// let mut store = DaemonStore::builder()
///     .connect_unix("/nix/var/nix/daemon-socket/socket")
///     .await?;
///
/// let is_valid_path = store.is_valid_path("/nix/store/...").await?.result().await?;
/// # Ok::<_, nix_daemon::Error>(())
/// # };
/// ```
#[derive(Debug)]
pub struct DaemonStore<C: AsyncReadExt + AsyncWriteExt + Unpin> {
    conn: C,
    buffer: [u8; 1024],
    /// Negotiated protocol version.
    pub proto: Proto,
}

impl DaemonStore<UnixStream> {
    /// Returns a Builder.
    pub fn builder() -> DaemonStoreBuilder {
        DaemonStoreBuilder::default()
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin> DaemonStore<C> {
    #[instrument(skip(self))]
    async fn handshake(&mut self) -> Result<()> {
        // Exchange magic numbers.
        wire::write_u64(&mut self.conn, wire::WORKER_MAGIC_1)
            .await
            .with_field("magic1")?;
        match wire::read_u64(&mut self.conn).await {
            Ok(magic2 @ wire::WORKER_MAGIC_2) => Ok(magic2),
            Ok(v) => Err(Error::Invalid(format!("{:#x}", v))),
            Err(err) => Err(err.into()),
        }
        .with_field("magic2")?;

        // Check that we're talking to a new enough daemon, tell them our version.
        self.proto = wire::read_proto(&mut self.conn)
            .await
            .and_then(|proto| {
                if proto.0 != 1 || proto < MIN_PROTO {
                    return Err(Error::Invalid(format!("{}", proto)));
                }
                Ok(proto)
            })
            .with_field("daemon_proto")?;
        wire::write_proto(&mut self.conn, MAX_PROTO)
            .await
            .with_field("client_proto")?;

        // Write some obsolete fields.
        if self.proto >= Proto(1, 14) {
            wire::write_u64(&mut self.conn, 0)
                .await
                .with_field("__obsolete_cpu_affinity")?;
        }
        if self.proto >= Proto(1, 11) {
            wire::write_bool(&mut self.conn, false)
                .await
                .with_field("__obsolete_reserve_space")?;
        }

        // And we don't currently do anything with these.
        if self.proto >= Proto(1, 33) {
            wire::read_string(&mut self.conn)
                .await
                .with_field("nix_version")?;
        }
        if self.proto >= Proto(1, 35) {
            // Option<bool>: 0 = None, 1 = Some(true), 2 = Some(false)
            wire::read_u64(&mut self.conn)
                .await
                .with_field("remote_trust")?;
        }

        // Discard Stderr. There shouldn't be anything here anyway.
        while let Some(_) = wire::read_stderr(&mut self.conn).await? {}
        Ok(())
    }
}

impl<C: AsyncReadExt + AsyncWriteExt + Unpin + Send> Store for DaemonStore<C> {
    type Error = Error;

    #[instrument(skip(self))]
    async fn is_valid_path<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> Result<impl Progress<T = bool, Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::IsValidPath)
            .await
            .with_field("IsValidPath.<op>")?;
        wire::write_string(&mut self.conn, &path)
            .await
            .with_field("IsValidPath.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            Ok(wire::read_bool(&mut s.conn).await?)
        }))
    }

    #[instrument(skip(self))]
    async fn has_substitutes<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> Result<impl Progress<T = bool, Error = Self::Error>, Self::Error> {
        wire::write_op(&mut self.conn, wire::Op::HasSubstitutes)
            .await
            .with_field("HasSubstitutes.<op>")?;
        wire::write_string(&mut self.conn, &path)
            .await
            .with_field("HasSubstitutes.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            Ok(wire::read_bool(&mut s.conn).await?)
        }))
    }

    #[instrument(skip(self, source))]
    async fn add_to_store<
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
        mut source: R,
    ) -> Result<impl Progress<T = (String, PathInfo), Error = Self::Error>>
    where
        Refs: IntoIterator + Send + Debug,
        Refs::IntoIter: ExactSizeIterator + Send,
        Refs::Item: AsRef<str> + Send + Sync,
        R: AsyncReadExt + Unpin + Send + Debug,
    {
        match self.proto {
            Proto(1, 25..) => {
                wire::write_op(&mut self.conn, wire::Op::AddToStore)
                    .await
                    .with_field("AddToStore.<op>")?;
                wire::write_string(&mut self.conn, name)
                    .await
                    .with_field("AddToStore.name")?;
                wire::write_string(&mut self.conn, cam_str)
                    .await
                    .with_field("AddToStore.camStr")?;
                wire::write_strings(&mut self.conn, refs)
                    .await
                    .with_field("AddToStore.refs")?;
                wire::write_bool(&mut self.conn, repair)
                    .await
                    .with_field("AddToStore.repair")?;
                wire::copy_to_framed(&mut source, &mut self.conn, &mut self.buffer)
                    .await
                    .with_field("AddToStore.<source>")?;
                Ok(DaemonProgress::new(self, |slf| async move {
                    Ok((
                        wire::read_string(&mut slf.conn).await.with_field("name")?,
                        wire::read_pathinfo(&mut slf.conn, slf.proto)
                            .await
                            .with_field("PathInfo")?,
                    ))
                }))
            }
            _ => Err(Error::Invalid(format!(
                "AddToStore is not implemented for Protocol {}",
                self.proto
            ))),
        }
    }

    #[instrument(skip(self))]
    async fn build_paths<Paths>(
        &mut self,
        paths: Paths,
        mode: BuildMode,
    ) -> Result<impl Progress<T = (), Error = Self::Error>>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        wire::write_op(&mut self.conn, wire::Op::BuildPaths)
            .await
            .with_field("BuildPaths.<op>")?;
        wire::write_strings(&mut self.conn, paths)
            .await
            .with_field("BuildPaths.paths")?;
        if self.proto >= Proto(1, 15) {
            wire::write_build_mode(&mut self.conn, mode)
                .await
                .with_field("BuildPaths.build_mode")?;
        }
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_u64(&mut s.conn).await.with_field("__unused__")?;
            Ok(())
        }))
    }

    #[instrument(skip(self))]
    async fn ensure_path<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> Result<impl Progress<T = (), Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::EnsurePath)
            .await
            .with_field("EnsurePath.<op>")?;
        wire::write_string(&mut self.conn, path)
            .await
            .with_field("EnsurePath.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_u64(&mut s.conn).await.with_field("__unused__")?;
            Ok(())
        }))
    }

    #[instrument(skip(self))]
    async fn add_temp_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> Result<impl Progress<T = (), Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::AddTempRoot)
            .await
            .with_field("AddTempRoot.<op>")?;
        wire::write_string(&mut self.conn, path)
            .await
            .with_field("AddTempRoot.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_u64(&mut s.conn).await.with_field("__unused__")?;
            Ok(())
        }))
    }

    #[instrument(skip(self))]
    async fn add_indirect_root<Path: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: Path,
    ) -> Result<impl Progress<T = (), Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::AddIndirectRoot)
            .await
            .with_field("AddIndirectRoot.<op>")?;
        wire::write_string(&mut self.conn, path)
            .await
            .with_field("AddIndirectRoot.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_u64(&mut s.conn).await.with_field("__unused__")?;
            Ok(())
        }))
    }

    #[instrument(skip(self))]
    async fn find_roots(
        &mut self,
    ) -> Result<impl Progress<T = HashMap<String, String>, Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::FindRoots)
            .await
            .with_field("FindRoots.<op>")?;
        Ok(DaemonProgress::new(self, |s| async move {
            let count = wire::read_u64(&mut s.conn)
                .await
                .with_field("FindRoots.roots[].<count>")?;
            let mut roots = HashMap::with_capacity(count as usize);
            for _ in 0..count {
                let link = wire::read_string(&mut s.conn)
                    .await
                    .with_field("FindRoots.roots[].link")?;
                let target = wire::read_string(&mut s.conn)
                    .await
                    .with_field("FindRoots.roots[].target")?;
                roots.insert(link, target);
            }
            Ok(roots)
        }))
    }

    #[instrument(skip(self))]
    async fn set_options(
        &mut self,
        opts: ClientSettings,
    ) -> Result<impl Progress<T = (), Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::SetOptions)
            .await
            .with_field("SetOptions.<op>")?;
        wire::write_client_settings(&mut self.conn, self.proto, &opts)
            .await
            .with_field("SetOptions.clientSettings")?;
        Ok(DaemonProgress::new(self, |_| async move { Ok(()) }))
    }

    #[instrument(skip(self))]
    async fn query_pathinfo<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> Result<impl Progress<T = Option<PathInfo>, Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::QueryPathInfo)
            .await
            .with_field("QueryPathInfo.<op>")?;
        wire::write_string(&mut self.conn, &path)
            .await
            .with_field("QueryPathInfo.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            if wire::read_bool(&mut s.conn).await? {
                Ok(Some(wire::read_pathinfo(&mut s.conn, s.proto).await?))
            } else {
                Ok(None)
            }
        }))
    }

    async fn query_valid_paths<Paths>(
        &mut self,
        paths: Paths,
        use_substituters: bool,
    ) -> Result<impl Progress<T = Vec<String>, Error = Self::Error>, Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        match self.proto {
            Proto(1, 12..) => {
                wire::write_op(&mut self.conn, wire::Op::QueryValidPaths)
                    .await
                    .with_field("QueryValidPaths.<op>")?;
                wire::write_strings(&mut self.conn, paths)
                    .await
                    .with_field("QueryValidPaths.path")?;
                if self.proto >= Proto(1, 27) {
                    wire::write_bool(&mut self.conn, use_substituters)
                        .await
                        .with_field("QueryValidPaths.use_substituters")?;
                }
                Ok(DaemonProgress::new(self, |s| async move {
                    wire::read_strings(&mut s.conn)
                        .collect::<Result<Vec<_>>>()
                        .await
                }))
            }
            _ => Err(Error::Invalid(format!(
                "QueryValidPaths is not implemented for Protocol {}",
                self.proto
            ))),
        }
    }

    #[instrument(skip(self))]
    async fn query_substitutable_paths<Paths>(
        &mut self,
        paths: Paths,
    ) -> Result<impl Progress<T = Vec<String>, Error = Self::Error>, Self::Error>
    where
        Paths: IntoIterator + Send + Debug,
        Paths::IntoIter: ExactSizeIterator + Send,
        Paths::Item: AsRef<str> + Send + Sync,
    {
        wire::write_op(&mut self.conn, wire::Op::QuerySubstitutablePaths)
            .await
            .with_field("QuerySubstitutablePaths.<op>")?;
        wire::write_strings(&mut self.conn, paths)
            .await
            .with_field("QuerySubstitutablePaths.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_strings(&mut s.conn)
                .collect::<Result<Vec<_>>>()
                .await
        }))
    }

    #[instrument(skip(self))]
    async fn query_valid_derivers<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> Result<impl Progress<T = Vec<String>, Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::QueryValidDerivers)
            .await
            .with_field("QueryValidDerivers.<op>")?;
        wire::write_string(&mut self.conn, &path)
            .await
            .with_field("QueryValidDerivers.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            Ok(wire::read_strings(&mut s.conn)
                .collect::<Result<Vec<String>>>()
                .await
                .with_field("QueryValidDerivers.paths")?)
        }))
    }

    #[instrument(skip(self))]
    async fn query_missing<Ps>(
        &mut self,
        paths: Ps,
    ) -> Result<impl Progress<T = crate::Missing, Error = Self::Error>>
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync,
    {
        wire::write_op(&mut self.conn, wire::Op::QueryMissing)
            .await
            .with_field("QueryMissing.<op>")?;
        wire::write_strings(&mut self.conn, paths)
            .await
            .with_field("QueryMissing.paths")?;
        Ok(DaemonProgress::new(self, |s| async move {
            let will_build = wire::read_strings(&mut s.conn)
                .collect::<Result<Vec<String>>>()
                .await
                .with_field("QueryMissing.will_build")?;
            let will_substitute = wire::read_strings(&mut s.conn)
                .collect::<Result<Vec<String>>>()
                .await
                .with_field("QueryMissing.will_substitute")?;
            let unknown = wire::read_strings(&mut s.conn)
                .collect::<Result<Vec<String>>>()
                .await
                .with_field("QueryMissing.unknown")?;
            let download_size = wire::read_u64(&mut s.conn)
                .await
                .with_field("QueryMissing.download_size")?;
            let nar_size = wire::read_u64(&mut s.conn)
                .await
                .with_field("QueryMissing.nar_size")?;
            Ok(crate::Missing {
                will_build,
                will_substitute,
                unknown,
                download_size,
                nar_size,
            })
        }))
    }

    #[instrument(skip(self))]
    async fn query_derivation_output_map<P: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: P,
    ) -> Result<impl Progress<T = HashMap<String, String>, Error = Self::Error>> {
        wire::write_op(&mut self.conn, wire::Op::QueryDerivationOutputMap)
            .await
            .with_field("QueryDerivationOutputMap.<op>")?;
        wire::write_string(&mut self.conn, path)
            .await
            .with_field("QueryDerivationOutputMap.paths")?;
        Ok(DaemonProgress::new(self, |s| async move {
            let mut outputs = HashMap::new();
            let count = wire::read_u64(&mut s.conn)
                .await
                .with_field("QueryDerivationOutputMap.outputs[].<count>")?;
            for _ in 0..count {
                let name = wire::read_string(&mut s.conn)
                    .await
                    .with_field("QueryDerivationOutputMap.outputs[].name")?;
                let path = wire::read_string(&mut s.conn)
                    .await
                    .with_field("QueryDerivationOutputMap.outputs[].path")?;
                outputs.insert(name, path);
            }
            Ok(outputs)
        }))
    }

    #[instrument(skip(self))]
    async fn build_paths_with_results<Ps>(
        &mut self,
        paths: Ps,
        mode: BuildMode,
    ) -> Result<
        impl Progress<T = HashMap<String, crate::BuildResult>, Error = Self::Error>,
        Self::Error,
    >
    where
        Ps: IntoIterator + Send + Debug,
        Ps::IntoIter: ExactSizeIterator + Send,
        Ps::Item: AsRef<str> + Send + Sync,
    {
        wire::write_op(&mut self.conn, wire::Op::BuildPathsWithResults)
            .await
            .with_field("BuildPathsWithResults.<op>")?;
        wire::write_strings(&mut self.conn, paths)
            .await
            .with_field("BuildPathsWithResults.paths")?;
        wire::write_build_mode(&mut self.conn, mode)
            .await
            .with_field("BuildPathsWithResults.build_mode")?;
        Ok(DaemonProgress::new(self, |s| async move {
            let count = wire::read_u64(&mut s.conn)
                .await
                .with_field("BuildPathsWithResults.results.<count>")?;
            let mut results = HashMap::with_capacity(count as usize);
            for _ in 0..count {
                let path = wire::read_string(&mut s.conn)
                    .await
                    .with_field("BuildPathsWithResults.results[].path")?;
                let result = wire::read_build_result(&mut s.conn, s.proto)
                    .await
                    .with_field("BuildPathsWithResults.results[].result")?;
                results.insert(path, result);
            }
            Ok(results)
        }))
    }
}

#[derive(Debug)]
pub struct DaemonProtocolAdapterBuilder<'s, S: Store> {
    pub store: &'s mut S,
    pub nix_version: String,
    pub remote_trust: Option<bool>,
}

impl<'s, S: Store> DaemonProtocolAdapterBuilder<'s, S> {
    fn new(store: &'s mut S) -> Self {
        Self {
            store,
            nix_version: concat!("gorgon/nix-daemon ", env!("CARGO_PKG_VERSION")).to_string(),
            remote_trust: None,
        }
    }

    /// Initializes a [`DaemonProtocolAdapter`] by adopting a connection.
    ///
    /// It's up to the caller that the connection is in a state to begin a nix handshake, eg.
    /// it behaves like a fresh connection to the daemon socket - if this is a connection through
    /// a proxy, any proxy handshakes should already have taken place, etc.
    pub async fn adopt<
        R: AsyncReadExt + Unpin + Send + Debug,
        W: AsyncWriteExt + Unpin + Send + Debug,
    >(
        self,
        r: R,
        w: W,
    ) -> Result<DaemonProtocolAdapter<'s, S, R, W>> {
        DaemonProtocolAdapter::handshake(r, w, self.store, self.nix_version, self.remote_trust)
            .await
    }
}

/// Handles an incoming `nix-daemon` protocol connection, and forwards calls to a
/// [`crate::Store`].
///
/// ```no_run
/// use tokio::net::UnixListener;
/// use nix_daemon::nix::{DaemonStore, DaemonProtocolAdapter};
///
/// #[tokio::main]
/// async fn main() -> Result<(), nix_daemon::Error> {
///     // Accept a connection.
///     let listener = UnixListener::bind("/tmp/nix-proxy.sock")?;
///     let (conn, _addr) = listener.accept().await?;
///
///     // Connect to the real store.
///     let mut store = DaemonStore::builder()
///         .connect_unix("/nix/var/nix/daemon-socket/socket")
///         .await?;
///
///     // Proxy the connection to it.
///     let (cr, cw) = conn.into_split();
///     let mut adapter = DaemonProtocolAdapter::builder(&mut store)
///         .adopt(cr, cw)
///         .await?;
///     Ok(adapter.run().await?)
/// }
/// ```
///
/// See [nix-supervisor](https://codeberg.org/gorgon/gorgon/src/branch/main/nix-supervisor) for
/// a more advanced example of how to use this (with a custom Store implementation).
pub struct DaemonProtocolAdapter<'s, S: Store, R, W>
where
    R: AsyncReadExt + Unpin + Send + Debug,
    W: AsyncWriteExt + Unpin + Send + Debug,
{
    r: R,
    w: W,
    store: &'s mut S,
    /// Negotiated protocol version.
    pub proto: Proto,
}

impl<'s, S: Store>
    DaemonProtocolAdapter<'s, S, tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>
{
    pub fn builder(store: &'s mut S) -> DaemonProtocolAdapterBuilder<'s, S> {
        DaemonProtocolAdapterBuilder::new(store)
    }
}

impl<'s, S: Store, R, W> DaemonProtocolAdapter<'s, S, R, W>
where
    R: AsyncReadExt + Unpin + Send + Debug,
    W: AsyncWriteExt + Unpin + Send + Debug,
{
    #[instrument(skip(r, w, store))]
    async fn handshake(
        mut r: R,
        mut w: W,
        store: &'s mut S,
        nix_version: String,
        remote_trust: Option<bool>,
    ) -> Result<Self> {
        // Exchange magic numbers.
        match wire::read_u64(&mut r).await {
            Ok(magic1 @ wire::WORKER_MAGIC_1) => Ok(magic1),
            Ok(v) => Err(Error::Invalid(format!("{:#x}", v))),
            Err(err) => Err(err.into()),
        }
        .with_field("magic1")?;
        wire::write_u64(&mut w, wire::WORKER_MAGIC_2)
            .await
            .with_field("magic2")?;

        // Tell the client our latest supported protocol version, then they pick that or lower.
        wire::write_proto(&mut w, MAX_PROTO)
            .await
            .with_field("daemon_proto")?;
        let proto = wire::read_proto(&mut r)
            .await
            .and_then(|proto| {
                if proto.0 != 1 || proto < MIN_PROTO {
                    return Err(Error::Invalid(format!("{}", proto)));
                }
                Ok(proto)
            })
            .with_field("client_proto")?;

        // Discard some obsolete fields.
        if proto >= Proto(1, 14) {
            wire::read_u64(&mut r)
                .await
                .with_field("__obsolete_cpu_affinity")?;
        }
        if proto >= Proto(1, 11) {
            wire::read_bool(&mut r)
                .await
                .with_field("__obsolete_reserve_space")?;
        }

        // And use values from the builder for these.
        if proto >= Proto(1, 33) {
            wire::write_string(&mut w, &nix_version)
                .await
                .with_field("nix_version")?;
        }
        if proto >= Proto(1, 35) {
            wire::write_u64(
                &mut w,
                match remote_trust {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                },
            )
            .await
            .with_field("remote_trust")?;
        }

        // Stderr is always empty.
        wire::write_stderr(&mut w, None)
            .await
            .with_field("stderr")?;
        Ok(Self { r, w, store, proto })
    }

    /// Runs the connection until the client disconnects. TODO: Cancellation.
    pub async fn run(&mut self) -> Result<(), S::Error> {
        loop {
            match wire::read_op(&mut self.r).await {
                Ok(wire::Op::IsValidPath) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("IsValidPath.path")?;

                    let is_valid =
                        forward_stderr(&mut self.w, self.store.is_valid_path(path).await?).await?;
                    wire::write_bool(&mut self.w, is_valid)
                        .await
                        .with_field("IsValidPath.is_valid")?;
                }
                Ok(wire::Op::HasSubstitutes) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("HasSubstitutes.path")?;
                    let has_substitutes =
                        forward_stderr(&mut self.w, self.store.has_substitutes(path).await?)
                            .await?;
                    wire::write_bool(&mut self.w, has_substitutes)
                        .await
                        .with_field("HasSubstitutes.has_substitutes")?;
                }
                Ok(wire::Op::AddToStore) => match self.proto {
                    Proto(1, 25..) => {
                        let name = wire::read_string(&mut self.r)
                            .await
                            .with_field("AddToStore.name")?;
                        let cam_str = wire::read_string(&mut self.r)
                            .await
                            .with_field("AddToStore.camStr")?;
                        let refs = wire::read_strings(&mut self.r)
                            .collect::<Result<Vec<_>>>()
                            .await
                            .with_field("AddToStore.refs")?;
                        let repair = wire::read_bool(&mut self.r)
                            .await
                            .with_field("AddToStore.repair")?;
                        let source = wire::FramedReader::new(&mut self.r);

                        let (name, pi) = forward_stderr(
                            &mut self.w,
                            self.store
                                .add_to_store(name, cam_str, refs, repair, source)
                                .await?,
                        )
                        .await?;
                        wire::write_string(&mut self.w, name)
                            .await
                            .with_field("AddToStore.name")?;
                        wire::write_pathinfo(&mut self.w, self.proto, &pi)
                            .await
                            .with_field("AddToStore.pi")?;
                    }
                    _ => {
                        return Err(Error::Invalid(format!(
                            "AddToStore is not implemented for Protocol {}",
                            self.proto
                        ))
                        .into())
                    }
                },
                Ok(wire::Op::BuildPaths) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("BuildPaths.paths")?;
                    let mode = if self.proto >= Proto(1, 15) {
                        wire::read_build_mode(&mut self.r)
                            .await
                            .with_field("BuildPaths.build_mode")?
                    } else {
                        BuildMode::Normal
                    };

                    forward_stderr(&mut self.w, self.store.build_paths(paths, mode).await?).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("BuildPaths.__unused__")?;
                }
                Ok(wire::Op::EnsurePath) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("EnsurePath.path")?;
                    forward_stderr(&mut self.w, self.store.ensure_path(path).await?).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("EnsurePath.__unused__")?;
                }
                Ok(wire::Op::AddTempRoot) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("AddTempRoot.path")?;

                    forward_stderr(&mut self.w, self.store.add_temp_root(path).await?).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("AddTempRoot.__unused__")?;
                }
                Ok(wire::Op::AddIndirectRoot) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("AddIndirectRoot.path")?;

                    forward_stderr(&mut self.w, self.store.add_indirect_root(path).await?).await?;
                    wire::write_u64(&mut self.w, 1)
                        .await
                        .with_field("AddIndirectRoot.__unused__")?;
                }
                Ok(wire::Op::FindRoots) => {
                    let roots = forward_stderr(&mut self.w, self.store.find_roots().await?).await?;

                    wire::write_u64(&mut self.w, roots.len() as u64)
                        .await
                        .with_field("FindRoots.roots[].<count>")?;
                    for (link, target) in roots {
                        wire::write_string(&mut self.w, link)
                            .await
                            .with_field("FindRoots.roots[].link")?;
                        wire::write_string(&mut self.w, target)
                            .await
                            .with_field("FindRoots.roots[].target")?;
                    }
                }
                Ok(wire::Op::SetOptions) => {
                    let ops = wire::read_client_settings(&mut self.r, self.proto)
                        .await
                        .with_field("SetOptions.clientSettings")?;
                    forward_stderr(&mut self.w, self.store.set_options(ops).await?).await?;
                }
                Ok(wire::Op::QueryPathInfo) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryPathInfo.path")?;

                    let pi =
                        forward_stderr(&mut self.w, self.store.query_pathinfo(path).await?).await?;

                    wire::write_bool(&mut self.w, pi.is_some())
                        .await
                        .with_field("QueryPathInfo.is_valid")?;
                    if let Some(pi) = pi {
                        wire::write_pathinfo(&mut self.w, self.proto, &pi)
                            .await
                            .with_field("QueryPathInfo.path_info")?;
                    }
                }
                Ok(wire::Op::QueryValidPaths) => match self.proto {
                    Proto(1, 12..) => {
                        let paths = wire::read_strings(&mut self.r)
                            .collect::<Result<Vec<_>>>()
                            .await
                            .with_field("QueryValidPaths.path")?;
                        let use_substituters = if self.proto >= Proto(1, 27) {
                            wire::read_bool(&mut self.r)
                                .await
                                .with_field("QueryValidPaths.use_substituters")?
                        } else {
                            true
                        };

                        let valid_paths = forward_stderr(
                            &mut self.w,
                            self.store
                                .query_valid_paths(paths, use_substituters)
                                .await?,
                        )
                        .await?;

                        wire::write_strings(&mut self.w, valid_paths)
                            .await
                            .with_field("QueryValidPaths.valid_path")?;
                    }
                    _ => {
                        return Err(Error::Invalid(format!(
                            "QueryValidPaths is not implemented for Protocol {}",
                            self.proto
                        ))
                        .into())
                    }
                },
                Ok(wire::Op::QuerySubstitutablePaths) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("QuerySubstitutablePaths.paths")?;
                    let sub_paths = forward_stderr(
                        &mut self.w,
                        self.store.query_substitutable_paths(paths).await?,
                    )
                    .await?;
                    wire::write_strings(&mut self.w, sub_paths)
                        .await
                        .with_field("QuerySubstitutablePaths.sub_paths")?;
                }
                Ok(wire::Op::QueryMissing) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("QueryMissing.paths")?;

                    let crate::Missing {
                        will_build,
                        will_substitute,
                        unknown,
                        download_size,
                        nar_size,
                    } = forward_stderr(&mut self.w, self.store.query_missing(paths).await?).await?;

                    wire::write_strings(&mut self.w, will_build)
                        .await
                        .with_field("QueryMissing.will_build")?;
                    wire::write_strings(&mut self.w, will_substitute)
                        .await
                        .with_field("QueryMissing.will_substitute")?;
                    wire::write_strings(&mut self.w, unknown)
                        .await
                        .with_field("QueryMissing.unknown")?;
                    wire::write_u64(&mut self.w, download_size)
                        .await
                        .with_field("QueryMissing.download_size")?;
                    wire::write_u64(&mut self.w, nar_size)
                        .await
                        .with_field("QueryMissing.nar_size")?;
                }
                Ok(wire::Op::QueryValidDerivers) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryValidDerivers.path")?;

                    let derivers =
                        forward_stderr(&mut self.w, self.store.query_valid_derivers(path).await?)
                            .await?;
                    wire::write_strings(&mut self.w, derivers)
                        .await
                        .with_field("QueryValidDerivers.paths")?
                }
                Ok(wire::Op::QueryDerivationOutputMap) => {
                    let path = wire::read_string(&mut self.r)
                        .await
                        .with_field("QueryDerivationOutputMap.paths")?;

                    let outputs = forward_stderr(
                        &mut self.w,
                        self.store.query_derivation_output_map(path).await?,
                    )
                    .await?;
                    wire::write_u64(&mut self.w, outputs.len() as u64)
                        .await
                        .with_field("QueryDerivationOutputMap.outputs[].<count>")?;
                    for (name, path) in outputs {
                        wire::write_string(&mut self.w, name)
                            .await
                            .with_field("QueryDerivationOutputMap.outputs[].name")?;
                        wire::write_string(&mut self.w, path)
                            .await
                            .with_field("QueryDerivationOutputMap.outputs[].path")?;
                    }
                }
                Ok(wire::Op::BuildPathsWithResults) => {
                    let paths = wire::read_strings(&mut self.r)
                        .collect::<Result<Vec<_>>>()
                        .await
                        .with_field("BuildPathsWithResults.paths")?;
                    let mode = wire::read_build_mode(&mut self.r)
                        .await
                        .with_field("BuildPathsWithResults.build_mode")?;

                    let results = forward_stderr(
                        &mut self.w,
                        self.store.build_paths_with_results(paths, mode).await?,
                    )
                    .await?;

                    wire::write_u64(&mut self.w, results.len() as u64)
                        .await
                        .with_field("BuildPathsWithResults.results.<count>")?;
                    for (path, result) in results {
                        wire::write_string(&mut self.w, path)
                            .await
                            .with_field("BuildPathsWithResults.results[].path")?;
                        wire::write_build_result(&mut self.w, &result, self.proto)
                            .await
                            .with_field("BuildPathsWithResults.results[].result")?;
                    }
                }
                Ok(v) => todo!("{:#?}", v),

                Err(Error::IO(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    info!("Client disconnected");
                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

async fn forward_stderr<W: AsyncWriteExt + Unpin, P: Progress>(
    w: &mut W,
    mut prog: P,
) -> Result<P::T, P::Error> {
    while let Some(stderr) = prog.next().await? {
        wire::write_stderr(w, Some(stderr)).await?;
    }
    wire::write_stderr(w, None).await?;
    prog.result().await
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
