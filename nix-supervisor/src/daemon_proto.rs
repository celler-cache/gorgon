// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::{anyhow, bail, Context, Result};
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use tap::TapFallible;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
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
    /// Flushes the underlying writer.
    pub async fn flush(&mut self) -> Result<()> {
        Ok(self.w.flush().await?)
    }

    /// Takes a u64 from the stream, little endian.
    pub async fn take_u64_le(&mut self) -> Result<u64> {
        let v = self.r.read_u64_le().await?;
        self.w.write_u64_le(v).await?;
        Ok(v)
    }

    /// Takes a boolean from the stream, encoded as u64, >0 is true.
    pub async fn take_bool(&mut self) -> Result<bool> {
        Ok(self.take_u64_le().await? > 0)
    }

    /// Copies exactly `len` bytes from the reader to the writer, ignoring the contents.
    pub async fn copy(&mut self, len: usize) -> Result<usize> {
        let mut copied = 0;
        while copied < len {
            self.r.fill_buf().await?;
            let mut buf = self.r.buffer();
            if buf.len() > len - copied {
                buf = &buf[..len - copied];
            }
            self.w.write_all(buf).await?;
            copied += buf.len();
            self.r.consume(buf.len());
        }
        Ok(copied)
    }

    /// Copies a (length, data) chunk, as used by nix' FramedSource/FramedSink.
    pub async fn copy_frame(&mut self) -> Result<usize> {
        let len: usize = self
            .take_u64_le()
            .await
            .map(|v| v as usize)
            .context("Reading frame length")?;
        if len > 0 {
            self.copy(len)
                .await
                .with_context(|| format!("Copying frame ({} bytes)", len))?;
        }
        Ok(len)
    }

    /// Takes exactly `len` bytes from the stream, and stores them in `buf`.
    /// It's up to the caller to ensure that `buf` can hold at least `len`.
    pub async fn take_len_buf<'a>(&mut self, buf: &'a mut [u8], len: usize) -> Result<&'a [u8]> {
        self.r.read_exact(&mut buf[..len]).await?;
        self.w.write_all(&buf[..len]).await?;
        Ok(&buf[..len])
    }

    /// Takes a string from the stream. Strings are prefixed with a u64 length, but the
    /// data is padded to the next 8-byte boundary, eg. a 1-byte string becomes 16 bytes
    /// on the wire: 8 for the length, 1 for the data, then 7 bytes of discarded 0x00s.
    pub async fn take_string(&mut self) -> Result<String> {
        let len = self.take_u64_le().await.context("Couldn't read length")? as usize;
        let padded_len = len + if len % 8 > 0 { 8 - (len % 8) } else { 0 };
        if padded_len < 1024 {
            let mut buf = [0u8; 1024];
            self.take_len_buf(&mut buf, padded_len)
                .await
                .context("Couldn't read short string")?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            let mut buf = vec![0u8; padded_len];
            self.take_len_buf(&mut buf, padded_len)
                .await
                .with_context(|| {
                    format!(
                        "Couldn't read string (length {}, padded to {})",
                        len, padded_len
                    )
                })?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        }
    }

    /// Takes a list of strings from the stream - a u64 count followed by that many
    /// strings using the normal `take_string()` encoding.
    ///
    /// TODO: This should be a proper Stream; the same encoding is used for Sets.
    pub async fn take_strings(&mut self) -> Result<Vec<String>> {
        let count = self.take_u64_le().await.context("Couldn't read count")? as usize;
        let mut strings = Vec::with_capacity(count);
        for i in 0..count {
            strings.push(
                self.take_string()
                    .await
                    .with_context(|| format!("Couldn't read strings[{}]", i))?,
            );
        }
        Ok(strings)
    }

    pub async fn take_string_pairs(&mut self) -> Result<Vec<(String, String)>> {
        let count = self.take_u64_le().await.context("Couldn't read count")? as usize;
        let mut pairs = Vec::with_capacity(count);
        for i in 0..count {
            pairs.push((
                self.take_string()
                    .await
                    .with_context(|| format!("Couldn't read pairs[{}].0", i))?,
                self.take_string()
                    .await
                    .with_context(|| format!("Couldn't read pairs[{}].0", i))?,
            ));
        }
        Ok(pairs)
    }

    pub async fn take_log_fields(&mut self) -> Result<Vec<LogField>> {
        let count = self.take_u64_le().await.context("Couldn't read count")? as usize;
        let mut fields = Vec::with_capacity(count);
        for i in 0..count {
            let kind: LogFieldType = self
                .take_u64_le()
                .await
                .with_context(|| format!("Couldn't read type for fields[{}]", i))?
                .try_into()
                .with_context(|| format!("Invalid type for fields[{}]", i))?;
            fields.push(match kind {
                LogFieldType::Int => LogField::Int(self.take_u64_le().await?),
                LogFieldType::String => LogField::String(self.take_string().await?),
            });
        }
        Ok(fields)
    }

    pub async fn take_op(&mut self) -> Result<Op> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_interjection(&mut self) -> Result<Interjection> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_verbosity(&mut self) -> Result<Verbosity> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_activity_type(&mut self) -> Result<ActivityType> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_result_type(&mut self) -> Result<ResultType> {
        Ok(self.take_u64_le().await?.try_into()?)
    }

    pub async fn take_pathinfo(&mut self, state: &State) -> Result<PathInfo> {
        let deriver = self.take_string().await.context("deriver")?;
        let nar_hash = self.take_string().await.context("narHash")?;
        let refs = self.take_strings().await.context("references")?;
        let reg_time = self.take_u64_le().await.context("registrationTime")?;
        let nar_size = self.take_u64_le().await.context("narSize")?;

        let ultimate = if state.client_proto.1 >= 16 {
            self.take_bool().await.context("ultimate [v16]")?
        } else {
            false
        };
        let sigs = if state.client_proto.1 >= 16 {
            self.take_strings().await.context("sigs [v16]")?
        } else {
            vec![]
        };
        let ca = if state.client_proto.1 >= 16 {
            self.take_string().await.context("ca [v16]")?
        } else {
            "".into()
        };

        Ok(PathInfo {
            deriver,
            nar_hash,
            refs,
            reg_time,
            nar_size,
            ultimate,
            sigs,
            ca,
        })
    }

    pub async fn take_keyed_build_results(&mut self) -> Result<Vec<(String, BuildResult)>> {
        let count = self.take_u64_le().await.context("Reading count")? as usize;
        let mut results = Vec::with_capacity(count);
        for i in 0..count {
            results.push((
                self.take_string()
                    .await
                    .with_context(|| format!("Reading key for results[{}]", i))?,
                self.take_build_result()
                    .await
                    .with_context(|| format!("Reading results[{}]", i))?,
            ));
        }
        Ok(results)
    }

    pub async fn take_build_result(&mut self) -> Result<BuildResult> {
        let status = self
            .take_u64_le()
            .await
            .map(|v| v.into())
            .context("status")?;
        let error_msg = self.take_string().await.context("error_msg")?;
        let times_built = self.take_u64_le().await.context("times_built")?;
        let is_non_deterministic = self.take_bool().await.context("is_non_deterministic")?;
        let start_time = self.take_u64_le().await.context("start_time")?;
        let stop_time = self.take_u64_le().await.context("stop_time")?;
        let built_outputs = self.take_string_pairs().await.context("built_outputs")?;

        Ok(BuildResult {
            status,
            error_msg,
            times_built,
            is_non_deterministic,
            start_time,
            stop_time,
            built_outputs,
        })
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
    /// Log a message to stderr.
    StderrNext = 0x6f6c6d67,
    StderrRead = 0x64617461,
    StderrWrite = 0x64617416,
    /// End of interjections, stop processing.
    StderrLast = 0x616c7473,
    StderrError = 0x63787470,
    /// Activity started.
    StderrStartActivity = 0x53545254,
    /// Activity ended.
    StderrStopActivity = 0x53544f50,
    /// Result yielded.
    StderrResult = 0x52534c54,
}

#[derive(Debug, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum Verbosity {
    Error = 0,
    Warn = 1,
    Notice = 2,
    Info = 3,
    Talkative = 4,
    Chatty = 5,
    Debug = 6,
    Vomit = 7,
    #[num_enum(catch_all)]
    Invalid(u64) = u64::MAX,
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum ActivityType {
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

#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum ResultType {
    FileLinked = 100,
    BuildLogLine = 101,
    UntrustedPath = 102,
    CorruptedPath = 103,
    SetPhase = 104,
    Progress = 105,
    SetExpected = 106,
    PostBuildLogLine = 107,
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum LogFieldType {
    Int = 0,
    String = 1,
}

#[derive(Debug)]
pub enum LogField {
    Int(u64),
    String(String),
}

#[derive(Debug)]
pub struct PathInfo {
    pub deriver: String,
    pub nar_hash: String,
    pub refs: Vec<String>,
    pub reg_time: u64,
    pub nar_size: u64,
    pub ultimate: bool,
    pub sigs: Vec<String>,
    pub ca: String,
}

#[derive(Debug, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum BuildResultStatus {
    Built = 0,
    Substituted,
    AlreadyValid,
    PermanentFailure,
    InputRejected,
    OutputRejected,
    TransientFailure,
    ObsoleteCachedFailure,
    TimedOut,
    MiscFailure,
    DependencyFailed,
    LogLimitExceeded,
    NotDeterministic,
    ResolvesToAlreadyValid,
    NoSubstituters,

    #[num_enum(catch_all)]
    Invalid(u64) = u64::MAX,
}

#[derive(Debug)]
pub struct BuildResult {
    pub status: BuildResultStatus,
    pub error_msg: String,
    pub times_built: u64,
    pub is_non_deterministic: bool,
    /// time_t
    pub start_time: u64,
    /// time_t
    pub stop_time: u64,
    pub built_outputs: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct State {
    pub nix_version: String,
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
            let (proto_major, proto_minor) = check_version(proto_raw)?;
            debug!(
                hex = format!("{:04x}", proto_raw),
                proto_major,
                proto_minor,
                "<- Daemon sent protocol version: {}.{:02} (0x{:04X})",
                proto_major,
                proto_minor,
                proto_raw,
            );
            Ok((proto_major, proto_minor))
        })
        .context("Couldn't read protocol version")?;
    d2c.flush().await?;

    // Client sends its version.
    let client_proto = c2d
        .take_u64_le()
        .await
        .and_then(|proto_raw| {
            let (proto_major, proto_minor) = check_version(proto_raw)?;
            debug!(
                hex = format!("{:04x}", proto_raw),
                proto_major,
                proto_minor,
                "-> Client sent its protocol version: {}.{:02} (0x{:04X})",
                proto_major,
                proto_minor,
                proto_raw,
            );
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
    let nix_version = if client_proto.1 >= 33 {
        d2c.take_string()
            .await
            .tap_ok(|nix_ver| debug!(nix_ver, "<- Daemon sent nix version string"))
            .context("Couldn't read Nix daemon version")?
    } else {
        "".into()
    };
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
        nix_version,
        daemon_proto,
        client_proto,
    })
}

fn parse_version(proto_raw: u64) -> (u32, u32) {
    (
        ((proto_raw & 0xFF00) >> 8) as u32,
        (proto_raw & 0x00FF) as u32,
    )
}

fn check_version(proto_raw: u64) -> Result<(u32, u32)> {
    let (proto_major, proto_minor) = parse_version(proto_raw);
    if proto_major != 1 || (proto_minor < 34 || proto_minor > 35) {
        bail!(
            "Unsupported protocol version: {}.{:02} (0x{:04X})",
            proto_major,
            proto_minor,
            proto_raw
        );
    }
    Ok((proto_major, proto_minor))
}
