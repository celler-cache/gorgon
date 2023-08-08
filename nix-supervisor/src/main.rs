// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

mod daemon_proto;

use anyhow::{Context, Result};
use clap::Parser;
use daemon_proto::{Direction, Stream};
use std::path::PathBuf;
use tap::TapFallible;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::{UnixListener, UnixStream},
    select,
    time::{timeout, Duration},
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
    let _state = daemon_proto::handshake(&mut c2d, &mut d2c)
        .await
        .tap_err(|err| error!(?err, "Handshake failed"))
        .context("Handshake failed")?;

    // Main loop!
    loop {
        trace!("[ main loop ]");

        // Wait for one of the streams to become readable.
        let dir = if c2d.r.buffer().len() > 0 {
            Direction::ClientToDaemon
        } else if d2c.r.buffer().len() > 0 {
            Direction::DaemonToClient
        } else {
            select! {
                biased;
                _ = d2c.r.get_ref().readable() => Direction::DaemonToClient,
                _ = c2d.r.get_ref().readable() => Direction::ClientToDaemon,
            }
        };
        trace!(?dir, "Stream became readable");
        match match dir {
            Direction::DaemonToClient => forward(&mut d2c).await,
            Direction::ClientToDaemon => forward(&mut c2d).await,
        } {
            Ok(_) => (),
            Err(err) => {
                if let Some(err) = err.downcast_ref::<std::io::Error>() {
                    if err.kind() == std::io::ErrorKind::UnexpectedEof {
                        info!("Disconnected");
                        break Ok(());
                    }
                }
                break Err(err);
            }
        }
    }
}

#[instrument(skip_all)]
async fn forward<'s>(conn: &mut Stream<'s>) -> Result<()> {
    // Even when the reader is allegedly readable, the first read() call sometimes hangs.
    // Hack around this for now by just setting a timeout and trying again if it happens.
    let buf = if let Ok(res) = timeout(Duration::from_millis(10), conn.r.fill_buf()).await {
        res.tap_err(|err| error!(?err, ?conn.dir, "Couldn't read"))
            .with_context(|| format!("Couldn't read from {}", conn.dir.reader()))?
    } else {
        warn!(?conn.dir, "Read timed out");
        return Ok(());
    };

    // An empty read means the connection was closed.
    if buf.len() == 0 {
        // TODO: This is a quite expected EOF, we should have our own error enum.
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }

    trace!(
        len = buf.len(),
        ?buf,
        "{} Read from {}",
        conn.dir.log_arrow(),
        conn.dir.reader()
    );

    conn.w
        .write_all(buf)
        .await
        .tap_err(|err| error!(?err, ?conn.dir, "Couldn't write"))
        .with_context(|| format!("Couldn't write to {}", conn.dir.writer()))?;
    conn.w
        .flush()
        .await
        .tap_err(|err| error!(?err, ?conn.dir, "Couldn't flush writer"))
        .with_context(|| format!("Couldn't flush writer to {}", conn.dir.writer()))?;

    // Tell the reader we consumed all the data it gave us.
    {
        let buf_len = buf.len(); // lifetime hack.
        conn.r.consume(buf_len);
    }
    Ok(())
}
