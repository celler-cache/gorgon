// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::{Context, Result};
use bytes::{Buf, BytesMut};
use clap::Parser;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, error, info, instrument, trace};

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

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

    let listener = UnixListener::bind(&args.sock).context("Couldn't open socket for listening")?;
    loop {
        match listener.accept().await {
            Ok((cstream, _)) => run_session(&args, cstream).await?,
            Err(err) => {
                error!(?err, "Client connection failed");
            }
        }
    }
}

#[instrument(skip_all)]
async fn run_session(args: &Args, mut cstream: UnixStream) -> Result<()> {
    info!("<- Client connected!");

    let mut dstream = UnixStream::connect(&args.nix_daemon_sock)
        .await
        .context("Couldn't connect to daemon")?;
    debug!("-> Connected to Daemon");

    let (mut cstream_r, mut cstream_w) = cstream.split();
    let (mut dstream_r, mut dstream_w) = dstream.split();

    tokio::select! {
        r = process(true, &mut cstream_r, &mut dstream_w) => r?,
        r = process(false, &mut dstream_r, &mut cstream_w) => r?,
    };
    let (cshut_res, dshut_res) = tokio::join!(cstream.shutdown(), dstream.shutdown());
    cshut_res?;
    dshut_res?;

    Ok(())
}

#[instrument(skip_all)]
async fn process<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    c2d: bool,
    r: &mut R,
    w: &mut W,
) -> Result<()> {
    let mut buf = BytesMut::with_capacity(2048);
    loop {
        if r.read_buf(&mut buf).await? == 0 {
            debug!("EOF");
            return Ok(());
        }
        trace!(
            "{} [{:4}] {:x?}",
            if c2d { "->" } else { "<-" },
            buf.len(),
            &buf[..]
        );
        while buf.has_remaining() {
            w.write_buf(&mut buf).await?;
        }
    }
}
