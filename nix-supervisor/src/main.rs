// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

mod daemon_proto;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use daemon_proto::{Direction, Interjection, Op, State, Stream};
use std::path::PathBuf;
use tap::TapFallible;
use tokio::{
    io::{BufReader, BufWriter},
    net::{UnixListener, UnixStream},
    select,
};
use tracing::{debug, error, info, instrument, trace, warn};

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Path to the real nix daemon socket.
    #[arg(long, short = 'S', default_value = "/nix/var/nix/daemon-socket/socket")]
    nix_daemon_sock: PathBuf,

    /// Path to our listening socket.
    #[arg(long, short = 's', default_value = "nix-supervisor.sock")]
    sock: PathBuf,
}

fn init_logging(args: &Args) {
    tracing_subscriber::fmt()
        .with_max_level(match 2 + args.verbose - args.quiet {
            0 => tracing::Level::ERROR,
            1 => tracing::Level::WARN,
            2 => tracing::Level::INFO,
            3 => tracing::Level::DEBUG,
            4.. => tracing::Level::TRACE,
        })
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args);

    let listener = UnixListener::bind(&args.sock)
        .tap_err(|err| error!(?err, ?args.sock, "Couldn't bind to socket"))
        .with_context(|| format!("Couldn't bind to socket: {}", args.sock.to_string_lossy()))?;
    info!(
        path = ?listener
            .local_addr()?
            .as_pathname()
            .ok_or_else(|| anyhow!("socket is unnamed; this should be impossible"))?
            .canonicalize()?
            .to_string_lossy(),
        "Running!"
    );

    loop {
        debug!("Waiting for a connection...");
        match listener.accept().await {
            Ok((cstream, addr)) => {
                info!(?addr, "Client connected");
                run_session(&args, cstream).await?;
            }
            Err(err) => error!(?err, "Couldn't accept connection"),
        }
    }
}

#[instrument(skip_all)]
async fn run_session(args: &Args, mut cstream: UnixStream) -> Result<()> {
    // We've got a client connection already; connect to the daemon on their behalf.
    debug!(?args.nix_daemon_sock, "Connecting to nix daemon on client's behalf...");
    let mut dstream = UnixStream::connect(&args.nix_daemon_sock)
        .await
        .tap_err(|err| error!(?err, ?args.nix_daemon_sock, "Couldn't connect to nix daemon socket"))
        .with_context(|| {
            format!(
                "Couldn't connect to nix daemon socket: {}",
                args.nix_daemon_sock.to_string_lossy()
            )
        })?;

    // Split the connections up into a client-to-daemon and a daemon-to-client stream.
    let (mut c2d, mut d2c) = {
        let (cr, cw) = cstream.split();
        let (dr, dw) = dstream.split();
        let c2d = daemon_proto::Stream {
            dir: daemon_proto::Direction::ClientToDaemon,
            r: BufReader::new(cr),
            w: BufWriter::new(dw),
        };
        let d2c = daemon_proto::Stream {
            dir: daemon_proto::Direction::DaemonToClient,
            r: BufReader::new(dr),
            w: BufWriter::new(cw),
        };
        (c2d, d2c)
    };

    // Perform the new connection handshake.
    let state = daemon_proto::handshake(&mut c2d, &mut d2c)
        .await
        .tap_err(|err| error!(?err, "Handshake failed"))
        .context("Handshake failed")?;

    // Main loop!
    loop {
        trace!("[ main loop ]");

        let dir = if c2d.r.buffer().len() > 0 {
            trace!(
                len = c2d.r.buffer().len(),
                "Client-to-Daemon reader has unprocessed data"
            );
            Direction::ClientToDaemon
        } else if d2c.r.buffer().len() > 0 {
            trace!(
                len = d2c.r.buffer().len(),
                "Daemon-to-Client reader has unprocessed data"
            );
            Direction::DaemonToClient
        } else {
            trace!("No leftover buffers, waiting for sockets...");
            select! {
                biased;
                _ = d2c.r.get_ref().readable() => Direction::DaemonToClient,
                _ = c2d.r.get_ref().readable() => Direction::ClientToDaemon,
            }
        };

        if dir == Direction::DaemonToClient {
            trace!("Daemon ready; reading interjection...");
            interjections(&mut d2c)
                .await
                .context("Couldn't process interjections")?;
        } else {
            trace!("Client ready; reading Op...");
            let op = c2d.take_op().await.context("Couldn't read Op")?;
            match op {
                Op::IsValidPath => do_is_valid_path(&state, &mut c2d, &mut d2c).await,
                Op::AddToStore => do_add_to_store(&state, &mut c2d, &mut d2c).await,
                Op::BuildPaths => do_build_paths(&state, &mut c2d, &mut d2c).await,
                Op::SetOptions => do_set_options(&state, &mut c2d, &mut d2c).await,
                Op::QueryDerivationOutputMap => {
                    do_query_derivation_output_map(&state, &mut c2d, &mut d2c).await
                }

                // We don't want to implement obsolete Ops.
                Op::ObsoleteQueryPathHash
                | Op::ObsoleteQueryReferences
                | Op::ObsoleteAddTextToStore
                | Op::ObsoleteExportPath
                | Op::ObsoleteQueryDeriver
                | Op::ObsoleteQueryDerivationOutputs
                | Op::ObsoleteImportPaths
                | Op::ObsoleteQueryDerivationOutputNames => {
                    Err(anyhow!("Obsolete Op received: {:?}", op))
                }

                // Oops, we haven't implemented this yet.
                _ => Err(anyhow!("Unimplemented Op received: {:?}", op)),
            }
            .tap_err(|err| error!(?err, ?op, "Couldn't process Op"))
            .with_context(|| format!("Couldn't process Op: {:?}", op))?;
            c2d.flush()
                .await
                .context("Couldn't flush writer after op")?;
        }
    }
}

/// Process a set of interjections (eg. log lines) sent from the daemon.
#[instrument(skip_all)]
async fn interjections<'s>(d2c: &mut Stream<'s>) -> Result<()> {
    loop {
        let inter = d2c
            .take_interjection()
            .await
            .context("Couldn't read interjection")?;

        match inter {
            Interjection::StderrLast => {
                trace!("End of queued stderr");
                break;
            }

            Interjection::StderrStartActivity => {
                let act_id = d2c.take_u64_le().await.context("Reading Activity ID")?;
                let level = d2c.take_verbosity().await.context("Reading Verbosity")?;
                let kind = d2c
                    .take_activity_type()
                    .await
                    .context("Reading ActivityType")?;
                let s = d2c.take_string().await.context("Reading string")?;
                let fields = d2c.take_log_fields().await.context("Reading fields")?;
                let parent_id = d2c.take_u64_le().await.context("Reading Parent ID")?;
                trace!(
                    act_id,
                    ?level,
                    ?kind,
                    s,
                    ?fields,
                    parent_id,
                    "Start Activity"
                );
            }

            Interjection::StderrStopActivity => {
                let act_id = d2c.take_u64_le().await.context("Reading Activity ID")?;
                trace!(?act_id, "Stop Activity");
            }

            Interjection::StderrResult => {
                let act_id = d2c.take_u64_le().await.context("Reading Activity ID")?;
                let kind = d2c.take_result_type().await.context("Reading ResultType")?;
                let fields = d2c.take_log_fields().await.context("Reading fields")?;
                trace!(act_id, ?kind, ?fields, "Result");
            }
            _ => bail!("Daemon sent unimplemented interjection: {:?}", inter),
        }
    }
    d2c.flush()
        .await
        .context("Couldn't flush writer after interjections")?;
    Ok(())
}

#[instrument(skip_all)]
async fn do_is_valid_path<'s>(
    _state: &State,
    c2d: &mut Stream<'s>,
    d2c: &mut Stream<'s>,
) -> Result<()> {
    debug!("Processing IsValidPath");

    let path = c2d.take_string().await.context("Reading 'path'")?;
    debug!(path, "-> Client sent query");
    c2d.flush().await.context("Flushing Client-to-Daemon")?;

    interjections(d2c).await.context("Running Interjections")?;

    let is_valid = d2c.take_bool().await.context("Reading 'is_valid'")?;
    debug!(is_valid, "<- Daemon replied");
    d2c.flush().await.context("Flushing Daemon-to-Client")?;

    Ok(())
}

#[instrument(skip_all)]
async fn do_add_to_store<'s>(
    state: &State,
    c2d: &mut Stream<'s>,
    d2c: &mut Stream<'s>,
) -> Result<()> {
    if state.client_proto.1 >= 25 {
        debug!("Processing AddToStore (v25)");
        let name = c2d.take_string().await.context("Reading 'name'")?;
        let cam_str = c2d.take_string().await.context("Reading 'camStr'")?;
        let refs = c2d.take_strings().await.context("Reading 'refs'")?;
        let repair = c2d.take_bool().await.context("Reading 'repair'")?;
        debug!(name, cam_str, ?refs, repair, "-> Client sent AddToStore");

        // Chunking used by a FramedSource/FramedSink on the nix side.
        let (mut total_num, mut total_len) = (0, 0);
        for num in 0.. {
            let len: usize = c2d
                .take_u64_le()
                .await
                .map(|v| v as usize)
                .with_context(|| format!("Reading chunks[{}] length", num))?;
            if len == 0 {
                trace!(num, "-> End of chunked transfer");
                break;
            }
            total_len += len;
            total_num += 1;

            trace!(num, len, "  -> Copying chunk...");
            c2d.copy(len)
                .await
                .with_context(|| format!("Copying chunks[{}]", num))?;
        }
        debug!(
            len = total_len,
            chunks = total_num,
            "-> Client sent file contents"
        );
        c2d.flush()
            .await
            .context("Flushing Client-to-Daemon Writer")?;

        // Process logs from the daemon.
        interjections(d2c)
            .await
            .context("Couldn't process interjections")?;

        // PathInfo
        let path = d2c.take_string().await.context("Reading PathInfo->path")?;
        let deriver = d2c
            .take_string()
            .await
            .context("Reading PathInfo->deriver")?;
        let nar_hash = d2c
            .take_string()
            .await
            .context("Reading PathInfo->narHash")?;
        let refs = d2c
            .take_strings()
            .await
            .context("Reading PathInfo->references")?;
        let reg_time = d2c
            .take_u64_le()
            .await
            .context("Reading PathInfo->registrationTime")?;
        let nar_size = d2c
            .take_u64_le()
            .await
            .context("Reading PathInfo->narSize")?;

        let ultimate = if state.client_proto.1 >= 16 {
            d2c.take_bool()
                .await
                .context("Reading PathInfo->ultimate [v16]")?
        } else {
            false
        };
        let sigs = if state.client_proto.1 >= 16 {
            d2c.take_strings()
                .await
                .context("Reading PathInfo->sigs [v16]")?
        } else {
            vec![]
        };
        let ca = if state.client_proto.1 >= 16 {
            d2c.take_string()
                .await
                .context("Reading PathInfo->ca [v16]")?
        } else {
            "".into()
        };
        debug!(
            path,
            deriver,
            nar_hash,
            ?refs,
            reg_time,
            nar_size,
            ultimate,
            ?sigs,
            ?ca,
            "<- Daemon sent PathInfo"
        );
        d2c.flush()
            .await
            .context("Flushing Daemon-to-Client Writer")?;

        Ok(())
    } else {
        bail!("AddToStore is only implemented for protocol version >= 25");
    }
}

#[instrument(skip_all)]
async fn do_build_paths<'s>(
    state: &State,
    c2d: &mut Stream<'s>,
    d2c: &mut Stream<'s>,
) -> Result<()> {
    debug!("Processing BuildPaths");

    let paths = c2d.take_strings().await.context("Reading 'paths'")?;
    let mode = if state.client_proto.1 >= 15 {
        Some(c2d.take_u64_le().await.context("Reading 'mode'")?)
    } else {
        None
    };
    debug!(?paths, ?mode, "-> Client sent BuildPaths");
    c2d.flush().await.context("Flushing Client-to-Daemon")?;

    interjections(d2c)
        .await
        .context("Monitoring build (interjections)")?;

    let is_valid = d2c.take_bool().await.context("Reading 'is_valid'")?;
    debug!(is_valid, "<- Daemon replied");
    d2c.flush().await.context("Flushing Daemon-to-Client")?;

    Ok(())
}

#[instrument(skip_all)]
async fn do_set_options<'s>(
    _state: &State,
    c2d: &mut Stream<'s>,
    _d2c: &mut Stream<'s>,
) -> Result<()> {
    debug!("Processing SetOptions");

    let keep_failed = c2d.take_u64_le().await.context("keepFailed")?;
    let keep_going = c2d.take_u64_le().await.context("keepGoing")?;
    let try_fallback = c2d.take_u64_le().await.context("tryFallback")?;
    let verbosity = c2d.take_verbosity().await.context("verbosity")?;
    let max_build_jobs = c2d.take_u64_le().await.context("maxBuildJobs")?;
    let max_silent_time = c2d.take_u64_le().await.context("maxSilentTime")?;
    c2d.take_u64_le()
        .await
        .tap_ok(|v| {
            if *v != 1 {
                warn!(v, "Client sent obsolete option: useBuildHook")
            }
        })
        .context("(obsolete) useBuildHook")?;
    let verbose_build = c2d.take_verbosity().await.context("verboseBuild")?;
    c2d.take_u64_le()
        .await
        .tap_ok(|v| {
            if *v != 0 {
                warn!(v, "Client sent obsolete option: logType")
            }
        })
        .context("(obsolete) logType")?;
    c2d.take_u64_le()
        .await
        .tap_ok(|v| {
            if *v != 0 {
                warn!(v, "Client sent obsolete option: printBuildTrace")
            }
        })
        .context("(obsolete) printBuildTrace")?;
    let build_cores = c2d.take_u64_le().await.context("buildCores")?;
    let use_substitutes = c2d.take_u64_le().await.context("useSubstitutes")?;
    let overrides = c2d.take_string_pairs().await.context("overrides")?;

    debug!(
        keepFailed = keep_failed,
        keepGoing = keep_going,
        tryFallback = try_fallback,
        ?verbosity,
        maxBuildJobs = max_build_jobs,
        maxSilentTime = max_silent_time,
        verboseBuild = ?verbose_build,
        buildCores = build_cores,
        useSubstitutes = use_substitutes,
        ?overrides,
        "SetOptions"
    );

    Ok(())
}

#[instrument(skip_all)]
async fn do_query_derivation_output_map<'s>(
    _state: &State,
    c2d: &mut Stream<'s>,
    d2c: &mut Stream<'s>,
) -> Result<()> {
    debug!("Processing QueryDerivationOutputMap");

    c2d.take_string()
        .await
        .tap_ok(|path| debug!(path, "-> Client sent query"))
        .context("Reading 'path'")?;
    c2d.flush().await.context("Flushing Client-to-Daemon")?;

    interjections(d2c).await.context("Running Interjections")?;

    d2c.take_string_pairs()
        .await
        .tap_ok(|pis| {
            for (out, path) in pis.iter() {
                debug!(out, path, "<- Daemon sent PathInfo");
            }
        })
        .context("Couldn't read StorePaths")?;
    d2c.flush().await.context("Flushing Daemon-to-Client")?;

    Ok(())
}
