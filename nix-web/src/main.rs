// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use clap::Parser;
use tracing::info;

#[derive(Debug, Parser)]
struct Args {
    /// Path to the nix database.
    #[arg(long, short, default_value = "/nix/var/nix/db/db.sqlite")]
    db_path: std::path::PathBuf,

    /// Increase log level.
    #[arg(short, long, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(short, long, action=clap::ArgAction::Count)]
    quiet: u8,
}

#[async_std::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_logging(&args);

    info!("Hello, world!");
    Ok(())
}

fn init_logging(args: &Args) {
    tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_max_level(match 2 + args.verbose - args.quiet {
            0 => tracing::Level::ERROR,
            1 => tracing::Level::WARN,
            2 => tracing::Level::INFO,
            3 => tracing::Level::DEBUG,
            4.. => tracing::Level::TRACE,
        })
        .init();
}
