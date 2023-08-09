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
    loop {
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
            let inter = d2c
                .take_interjection()
                .await
                .context("Couldn't read interjection")?;
            do_interjection(&mut d2c, inter)
                .await
                .context("Couldn't process interjection")?;
            d2c.flush()
                .await
                .context("Couldn't flush writer after interjection")?;
        } else {
            trace!("Client ready; reading Op...");
            let op = c2d.take_op().await.context("Couldn't read Op")?;
            match op {
                Op::SetOptions => do_set_options(&state, &mut c2d, &mut d2c).await,
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

#[instrument(skip_all)]
async fn do_interjection<'s>(_d2c: &mut Stream<'s>, inter: Interjection) -> Result<()> {
    match inter {
        Interjection::StderrLast => debug!("End of queued stderr"),
        _ => bail!("Daemon sent unimplemented interjection: {:?}", inter),
    };
    Ok(())
}

#[instrument(skip_all)]
async fn do_set_options<'s>(
    state: &State,
    c2d: &mut Stream<'s>,
    _d2c: &mut Stream<'s>,
) -> Result<()> {
    debug!("Processing SetOptions");

    #[instrument]
    fn format_verbosity(v: u64) -> &'static str {
        match v {
            0 => "Error",
            1 => "Warn",
            2 => "Notice",
            3 => "Info",
            4 => "Talkative",
            5 => "Chatty",
            6 => "Debug",
            7 => "Vomit",
            _ => {
                warn!(v, "Invalid verbosity received");
                "???"
            }
        }
    }

    let keep_failed = c2d.take_u64_le().await.context("keepFailed")?;
    let keep_going = c2d.take_u64_le().await.context("keepGoing")?;
    let try_fallback = c2d.take_u64_le().await.context("tryFallback")?;
    let verbosity = c2d
        .take_u64_le()
        .await
        .map(format_verbosity)
        .context("verbosity")?;
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
    let verbose_build = c2d
        .take_u64_le()
        .await
        .map(format_verbosity)
        .context("verboseBuild")?;
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

    let num_overrides = if state.client_proto.1 > 12 {
        c2d.take_u64_le().await.context("override count")?
    } else {
        0
    };
    info!(
        keepFailed = keep_failed,
        keepGoing = keep_going,
        tryFallback = try_fallback,
        verbosity,
        maxBuildJobs = max_build_jobs,
        maxSilentTime = max_silent_time,
        verboseBuild = verbose_build,
        buildCores = build_cores,
        useSubstitutes = use_substitutes,
        "SetOptions"
    );

    // Note: This part is untested.
    for num in 0..num_overrides {
        let name = c2d
            .take_string()
            .await
            .with_context(|| format!("SetOptions->overrides[{}].name", num))?;
        let value = c2d
            .take_string()
            .await
            .with_context(|| format!("SetOptions->overrides[{}].value", num))?;
        info!(num, name, value, "SetOptions override");
    }

    Ok(())
}
