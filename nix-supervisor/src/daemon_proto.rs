// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::{anyhow, bail, Context, Result};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use tap::TapFallible;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::unix::{ReadHalf, WriteHalf},
};
use tracing::{debug, instrument};

const WORKER_MAGIC_1: u64 = 0x6e697863;
const WORKER_MAGIC_2: u64 = 0x6478696f;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Direction {
    ClientToDaemon,
    DaemonToClient,
}

#[derive(Debug)]
pub struct Stream<'s> {
    pub dir: Direction,
    pub r: BufReader<ReadHalf<'s>>,
    pub w: BufWriter<WriteHalf<'s>>,
}

impl<'s> Stream<'s> {
    pub async fn flush(&mut self) -> Result<()> {
        Ok(self.w.flush().await?)
    }

    pub async fn take_u64_le(&mut self) -> Result<u64> {
        let v = self.r.read_u64_le().await?;
        self.w.write_u64_le(v).await?;
        Ok(v)
    }

    pub async fn take_string(&mut self) -> Result<String> {
        let len = self.take_u64_le().await.context("Couldn't read length")? as usize;
        let padded_len = len + if len % 8 > 0 { 8 - (len % 8) } else { 0 };
        let mut buf = [0u8; 1024];
        self.r.read_exact(&mut buf[..padded_len]).await?;
        self.w.write_all(&buf[..padded_len]).await?;
        Ok(String::from_utf8_lossy(&buf[..len]).to_string())
    }

    pub async fn take_op(&mut self) -> Result<Op> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_interjection(&mut self) -> Result<Interjection> {
        Ok(self.take_u64_le().await?.try_into()?)
    }
}

/// Operation codes sent by the client to the daemon.
#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum Op {
    IsValidPath = 1,
    HasSubstitutes = 3,
    ObsoleteQueryPathHash = 4,   // obsolete
    ObsoleteQueryReferences = 5, // obsolete
    QueryReferrers = 6,
    AddToStore = 7,
    ObsoleteAddTextToStore = 8, // obsolete since 1.25, Nix 3.0. Use WorkerProto::Op::AddToStore
    BuildPaths = 9,
    EnsurePath = 10,
    AddTempRoot = 11,
    AddIndirectRoot = 12,
    SyncWithGC = 13,
    FindRoots = 14,
    ObsoleteExportPath = 16,   // obsolete
    ObsoleteQueryDeriver = 18, // obsolete
    SetOptions = 19,
    CollectGarbage = 20,
    QuerySubstitutablePathInfo = 21,
    ObsoleteQueryDerivationOutputs = 22, // obsolete
    QueryAllValidPaths = 23,
    QueryFailedPaths = 24,
    ClearFailedPaths = 25,
    QueryPathInfo = 26,
    ObsoleteImportPaths = 27,                // obsolete
    ObsoleteQueryDerivationOutputNames = 28, // obsolete
    QueryPathFromHashPart = 29,
    QuerySubstitutablePathInfos = 30,
    QueryValidPaths = 31,
    QuerySubstitutablePaths = 32,
    QueryValidDerivers = 33,
    OptimiseStore = 34,
    VerifyStore = 35,
    BuildDerivation = 36,
    AddSignatures = 37,
    NarFromPath = 38,
    AddToStoreNar = 39,
    QueryMissing = 40,
    QueryDerivationOutputMap = 41,
    RegisterDrvOutput = 42,
    QueryRealisation = 43,
    AddMultipleToStore = 44,
    AddBuildLog = 45,
    BuildPathsWithResults = 46,
}

/// Commandment interjected by the daemon to the client.
#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum Interjection {
    StderrNext = 0x6f6c6d67,
    StderrRead = 0x64617461,
    StderrWrite = 0x64617416,
    /// End of stderr messages.
    StderrLast = 0x616c7473,
    StderrError = 0x63787470,
    StderrStartActivity = 0x53545254,
    StderrStopActivity = 0x53544f50,
    StderrResult = 0x52534c54,
}

#[derive(Debug)]
pub struct State {
    pub daemon_proto: (u32, u32),
    pub client_proto: (u32, u32),
}

#[instrument(skip_all)]
pub async fn handshake<'s>(c2d: &mut Stream<'s>, d2c: &mut Stream<'s>) -> Result<State> {
    debug!("Performing handshake...");

    // Client sends WORKER_MAGIC_1 to the Daemon.
    c2d.take_u64_le()
        .await
        .tap_ok(|magic1| debug!(magic1, "-> Client sent WORKER_MAGIC_1"))
        .and_then(|magic1| {
            if magic1 != WORKER_MAGIC_1 {
                bail!("WORKER_MAGIC_1: {:x} != {:x}", magic1, WORKER_MAGIC_1);
            }
            Ok(magic1)
        })
        .context("Couldn't read WORKER_MAGIC_1")?;
    c2d.flush().await?;

    // Daemon replies with WORKER_MAGIC_2...
    d2c.take_u64_le()
        .await
        .tap_ok(|magic2| debug!(magic2, "<- Daemon sent WORKER_MAGIC_2"))
        .and_then(|magic2| {
            if magic2 != WORKER_MAGIC_2 {
                bail!("WORKER_MAGIC_2: {:x} != {:x}", magic2, WORKER_MAGIC_2);
            }
            Ok(magic2)
        })
        .context("Couldn't read WORKER_MAGIC_2")?;
    // ...and the daemon's protocol version.
    let daemon_proto = d2c
        .take_u64_le()
        .await
        .and_then(|proto_raw| {
            let (proto_major, proto_minor) = (
                ((proto_raw & 0xFF00) >> 8) as u32,
                (proto_raw & 0x00FF) as u32,
            );
            debug!(
                hex = format!("{:04x}", proto_raw),
                proto_major,
                proto_minor,
                "<- Daemon sent protocol version: {}.{:02} ({:04x})",
                proto_major,
                proto_minor,
                proto_raw,
            );
            if proto_major != 1 || proto_minor != 35 {
                bail!(
                    "Unsupported protocol version: {}.{:02} ({:04x})",
                    proto_major,
                    proto_minor,
                    proto_raw
                );
            }
            Ok((proto_major, proto_minor))
        })
        .context("Couldn't read protocol version")?;
    d2c.flush().await?;

    // Client sends its version.
    let client_proto = c2d
        .take_u64_le()
        .await
        .and_then(|proto_raw| {
            let (proto_major, proto_minor) = (
                ((proto_raw & 0xFF00) >> 8) as u32,
                (proto_raw & 0x00FF) as u32,
            );
            debug!(
                hex = format!("{:04x}", proto_raw),
                proto_major,
                proto_minor,
                "-> Client sent its protocol version: {}.{:02} ({:04x})",
                proto_major,
                proto_minor,
                proto_raw,
            );
            if proto_major != 1 || proto_minor != 35 {
                bail!(
                    "Unsupported protocol version: {}.{:02} ({:04x})",
                    proto_major,
                    proto_minor,
                    proto_raw
                );
            }
            Ok((proto_major, proto_minor))
        })
        .context("Couldn't read client version")?;

    // And a bunch of obsolete fields.
    if client_proto.1 >= 14
        && c2d
            .take_u64_le()
            .await
            .map(|v| v > 0)
            .tap_ok(|has_affinity| {
                debug!(
                    has_affinity,
                    "-> Client sent obsolete CPU affinity presence"
                )
            })
            .context("Couldn't read obsolete CPU affinity presence")?
    {
        c2d.take_u64_le()
            .await
            .tap_ok(|affinity| debug!(affinity, "-> Client sent obsolete CPU affinity"))
            .context("Couldn't read obsolete CPU affinity")?;
    }
    if client_proto.1 >= 11 {
        c2d.take_u64_le()
            .await
            .tap_ok(|reserved| debug!(reserved, "-> Client sent obsolete reserved space"))
            .context("Couldn't read obsolete reserved space")?;
    }
    c2d.flush().await?;

    // The daemon sends the nix version... as a string?
    if client_proto.1 >= 33 {
        d2c.take_string()
            .await
            .tap_ok(|nix_ver| debug!(nix_ver, "<- Daemon sent nix version string"))
            .context("Couldn't read Nix daemon version")?;
    }
    // And an Option<bool> of whether or not the client is trusted.
    if client_proto.1 >= 35 {
        d2c.take_u64_le()
            .await
            .and_then(|raw| match raw {
                0 => Ok(None),
                1 => Ok(Some(true)),
                2 => Ok(Some(false)),
                v => Err(anyhow!("Invalid client trust flag value: {}", v)),
            })
            .tap_ok(|trusted| debug!(trusted, "<- Daemon sent client trust flag"))
            .context("Couldn't read client trust flag")?;
    }
    d2c.flush().await?;
    c2d.flush().await?;

    Ok(State {
        daemon_proto,
        client_proto,
    })
}
