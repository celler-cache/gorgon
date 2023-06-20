// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, debug_span, error, info, trace};

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
            Ok((mut cstream, _)) => {
                let span = debug_span!("session");
                let _enter = span.enter();
                info!("<- Client connected!");

                let mut dstream = UnixStream::connect(&args.nix_daemon_sock)
                    .await
                    .context("Couldn't connect to daemon")?;
                debug!("-> Connected to Daemon");

                let (mut cstream_r, mut cstream_w) = cstream.split();
                let (mut dstream_r, mut dstream_w) = dstream.split();
                let c2d_res = process(true, &mut cstream_r, &mut dstream_w);
                let d2c_res = process(false, &mut dstream_r, &mut cstream_w);
                tokio::try_join!(c2d_res, d2c_res)?;
            }
            Err(err) => {
                error!(?err, "Client connection failed");
            }
        }
    }
}

async fn process<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    c2d: bool,
    r: &mut R,
    w: &mut W,
) -> Result<()> {
    let span = if c2d {
        debug_span!("process_c2d")
    } else {
        debug_span!("process_d2c")
    };
    let _enter = span.enter();

    tokio::io::copy(r, w).await?;
    Ok(())
}
