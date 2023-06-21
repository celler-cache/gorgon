// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::{bail, Context, Result};
use bytes::{Buf, BytesMut};
use clap::Parser;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, error, info, instrument, trace};

const WORKER_MAGIC_1: u64 = 0x6e697863;
const WORKER_MAGIC_2: u64 = 0x6478696f;

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

    let (mut c2d, mut d2c) = {
        let (cr, cw) = cstream.split();
        let (dr, dw) = dstream.split();
        let c2d = Conn {
            dir: Direction::ClientToDaemon,
            r: cr,
            w: dw,
        };
        let d2c = Conn {
            dir: Direction::DaemonToClient,
            r: dr,
            w: cw,
        };
        (c2d, d2c)
    };

    handshake(&mut c2d, &mut d2c)
        .await
        .context("Couldn't complete handshake")?;
    tokio::select! {
        r = process(&mut c2d) => r?,
        r = process(&mut d2c) => r?,
    };
    let (cshut_res, dshut_res) = tokio::join!(cstream.shutdown(), dstream.shutdown());
    cshut_res?;
    dshut_res?;

    Ok(())
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Direction {
    ClientToDaemon,
    DaemonToClient,
}

#[derive(Debug, Copy, Clone)]
struct Conn<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin> {
    dir: Direction,
    r: R,
    w: W,
}

// Based on src/libstore/daemon.cc in the nix tree.
#[instrument(skip_all)]
async fn handshake<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    mut c2d: &mut Conn<R, W>,
    mut d2c: &mut Conn<R, W>,
) -> Result<()> {
    let mut buf = BytesMut::with_capacity(256);

    // Step 1: Client sends WORKER_MAGIC_1 to the Daemon.
    tee_len(&mut buf, &mut c2d, 8, |mut b| {
        let magic1 = b.get_u64_le();
        debug!(magic1, "<- read magic1: {:x}", magic1);
        if magic1 != WORKER_MAGIC_1 {
            bail!("Incorrect WORKER_MAGIC_1: {} != {}", WORKER_MAGIC_1, magic1);
        }
        Ok(())
    })
    .await
    .context("Step 1 (WORKER_MAGIC_1) failed")?;

    // Step 2: Daemon replies with [ WORKER_MAGIC_2, protocol version ].
    tee_len(&mut buf, &mut d2c, 16, |mut b| {
        let magic2 = b.get_u64_le();
        debug!(magic2, "-> read magic2: {:x}", magic2);
        if magic2 != WORKER_MAGIC_2 {
            bail!(
                "Incorrect WORKER_MAGIC_2: {:x} != {:x}",
                WORKER_MAGIC_2,
                magic2
            );
        }

        let proto_ver = b.get_u64_le();
        let (proto_major, proto_minor) = ((proto_ver & 0xFF00) >> 8, proto_ver & 0x00FF);
        debug!(
            proto_ver,
            proto_major,
            proto_minor,
            "-> read protocol version: {:}.{:02}",
            proto_major,
            proto_minor
        );
        if proto_major != 1 || proto_minor != 34 {
            bail!(
                "Unsupported Protocol Version: {:}.{:02}",
                proto_major,
                proto_minor
            );
        }
        Ok((proto_major, proto_minor))
    })
    .await
    .context("Step 2 (WORKER_MAGIC_2, protocol version) failed")?;

    // Step 3: Client introduction.
    let client_ver = tee_len(&mut buf, &mut c2d, 8, |mut b| {
        let client_ver = b.get_u64_le();
        debug!(client_ver, "-> read client_ver: {:}", client_ver);
        Ok(client_ver)
    })
    .await
    .context("Step 3.0 (Client version) failed")?;

    // Step 3.2: Skip obsolete CPU affinity, if present.
    if client_ver >= 14
        && take_u64(&mut buf, &mut c2d)
            .await
            .context("Step 3.1.1 (Obsolete CPU Affinity marker) failed")?
            > 0
    {
        let cpu_affinity = take_u64(&mut buf, &mut c2d)
            .await
            .context("Step 3.1.2 (Obsolete CPU Affinity) failed")?;
        debug!(cpu_affinity, "-> [v14] read obsolete CPU affinity");
    } else {
        debug!("-> [v14] no obsolete CPU affinity to skip");
    }

    // Step 3.3: Skip obsolete reserved space.
    if client_ver >= 11 {
        let reserved = take_u64(&mut buf, &mut c2d)
            .await
            .context("Step 3.2 (Obsolete reserved space)")?;
        debug!(reserved, "-> [v11] read obsolete reserved space");
    }
    flush(&mut buf, &mut c2d).await?;

    Ok(())
}

#[instrument(skip_all)]
async fn process<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    conn: &mut Conn<R, W>,
) -> Result<()> {
    let mut buf = BytesMut::with_capacity(2048);
    loop {
        if conn.r.read_buf(&mut buf).await? == 0 {
            debug!("EOF");
            return Ok(());
        }
        trace!(
            "{} [{:4}] {:x?}",
            if conn.dir == Direction::ClientToDaemon {
                "->"
            } else {
                "<-"
            },
            buf.len(),
            &buf[..]
        );
        while buf.has_remaining() {
            conn.w.write_buf(&mut buf).await?;
        }
    }
}

async fn tee_len<
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
    F: FnMut(BytesMut) -> Result<FR>,
    FR,
>(
    buf: &mut BytesMut,
    conn: &mut Conn<R, W>,
    len: usize,
    mut f: F,
) -> Result<FR> {
    loop {
        trace!("have {} bytes of {}", buf.remaining(), len);
        if buf.remaining() < len {
            if conn.r.read_buf(buf).await? == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
        } else {
            break;
        }
    }
    trace!(
        "{} [{:4}] {:x?}",
        if conn.dir == Direction::ClientToDaemon {
            "->"
        } else {
            "<-"
        },
        buf.len(),
        &buf[..]
    );
    let ret = f(buf.clone())?;
    flush(&mut buf.take(len), conn).await?;
    Ok(ret)
}

async fn take_u64<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    buf: &mut BytesMut,
    conn: &mut Conn<R, W>,
) -> Result<u64> {
    tee_len(buf, conn, 8, |mut b| Ok(b.get_u64_le())).await
}

async fn flush<B: Buf, R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    buf: &mut B,
    conn: &mut Conn<R, W>,
) -> Result<()> {
    let total = buf.remaining();
    while buf.has_remaining() {
        trace!(total, remaining = buf.remaining(), "flushing buffer...");
        conn.w.write_buf(buf).await?;
    }
    Ok(())
}
