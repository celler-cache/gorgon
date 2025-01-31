// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::Result;
use clap::{Parser, Subcommand};
use gorgond::Client;
use std::io::Write;
use tracing::instrument;

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Base URL for the Gorgon API to talk to.
    #[arg(long, short = 'B', default_value = "http://localhost:9999/")]
    base_url: url::Url,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    Ping(Ping),
}
impl Command {
    pub async fn run(&self, args: &Args) -> Result<()> {
        match &args.command {
            Command::Ping(cmd) => self.dump(cmd.run(args).await?),
        }
    }

    fn dump<V: serde::Serialize>(&self, v: V) -> Result<()> {
        let mut stdout = std::io::stdout();
        colored_json::write_colored_json(&v, &mut stdout)?;
        Ok(stdout.write_all(b"\n")?)
    }
}

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    #[instrument(name = "ping", skip_all)]
    pub async fn run(&self, args: &Args) -> Result<gorgond::RootResponse> {
        Ok(init_client(args)?.root().await?)
    }
}

fn init_logging(args: &Args) {
    use tracing::Level;
    use tracing_subscriber::prelude::*;

    let level = match args.verbose as i8 - args.quiet as i8 {
        ..=-2 => Level::ERROR,
        -1 => Level::WARN,
        0 => Level::INFO,
        1 => Level::DEBUG,
        2.. => Level::TRACE,
    };
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::Targets::new().with_default(level))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

fn init_client(args: &Args) -> Result<Client> {
    Ok(Client::new(reqwest::Client::new(), args.base_url.clone())?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args);
    args.command.run(&args).await
}
