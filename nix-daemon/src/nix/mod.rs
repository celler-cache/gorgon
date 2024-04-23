// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod wire;

use crate::{ClientSettings, Error, PathInfo, Progress, Result, ResultExt, Stderr, Store};
use std::fmt::Debug;
use std::future::Future;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use tracing::instrument;

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

    /// Returns the next Stderr message, or None after all have been consumed.
    /// This behaves like a fused iterator, and keeps returning None after that.
    async fn next(&mut self) -> Result<Option<Stderr>> {
        if self.fuse {
            Ok(None)
        } else {
            match wire::read_stderr(&mut self.store.conn).await? {
                Some(Stderr::Error(err)) => Err(Error::NixError(err)),
                // Some(stderr) => Ok(Some(stderr)),
                None => {
                    self.fuse = true;
                    Ok(None)
                }
            }
        }
    }

    /// Discards any remaining Stderr messages and proceeds.
    async fn result(mut self) -> Result<Self::T> {
        while let Some(_) = self.next().await? {}
        (self.then)(self.store).await
    }
}

/// Builder for a DaemonStore.
#[derive(Debug, Default)]
pub struct DaemonStoreBuilder {
    // This will do things in the future.
}

impl DaemonStoreBuilder {
    /// Initializes a DaemonStore by adopting a connection.
    ///
    /// It's up to the caller that the connection is in a state to begin a nix handshake, eg.
    /// it behaves like a fresh connection to the daemon socket - if this is a connection through
    /// a proxy, any proxy handshakes should already have taken place, etc.
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

    /// Connects to a Nix daemon via a unix socket. The path is usually `/nix/var/nix/daemon-socket/socket`.
    pub async fn connect_unix<P: AsRef<std::path::Path>>(
        self,
        path: P,
    ) -> Result<DaemonStore<UnixStream>> {
        self.init(UnixStream::connect(path).await?).await
    }
}

/// Store backed by a nix-daemon.
#[derive(Debug)]
pub struct DaemonStore<C: AsyncReadExt + AsyncWriteExt + Unpin> {
    conn: C,
    buffer: [u8; 1024],
    pub proto: Proto,
}

impl DaemonStore<UnixStream> {
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
        wire::read_u64(&mut self.conn)
            .await
            .and_then(|magic2| match magic2 {
                wire::WORKER_MAGIC_2 => Ok(magic2),
                _ => Err(Error::Invalid(format!("{:#x}", magic2))),
            })
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
    // FIXME: The daemon expects /nix/store/foo, not /nix/store/foo/bin/bar.
    // In the nix codebase, libstore chops the latter into the former before making the
    // call, but I'm unsure of how to do it here.
    #[instrument(skip(self))]
    async fn is_valid_path<S: AsRef<str> + Send + Sync + Debug>(
        &mut self,
        path: S,
    ) -> Result<impl Progress<T = bool>> {
        wire::write_op(&mut self.conn, wire::Op::IsValidPath)
            .await
            .with_field("IsValidPath.<op>")?;
        wire::write_string(&mut self.conn, &path)
            .await
            .with_field("IsValidPath.path")?;
        Ok(DaemonProgress::new(self, |s| async move {
            wire::read_bool(&mut s.conn).await
        }))
    }

    /// Adds a file to the store.
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
    ) -> Result<impl Progress<T = (String, PathInfo)>>
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
    async fn set_options(&mut self, opts: ClientSettings) -> Result<impl Progress<T = ()>> {
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
    ) -> Result<impl Progress<T = Option<PathInfo>>> {
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
