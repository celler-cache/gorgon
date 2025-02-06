// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like gorgond_client::Error, and puts a warning on *everything*
// returning Error if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use derive_more::Unwrap;
use gorgond_client::Client;
use miette::{Diagnostic, NamedSource};
use std::{fmt::Debug, io::Write};
use tracing::instrument;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic, Unwrap)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error("Serializing output failed")]
    DumpJSON(
        #[source] serde_json::Error,
        #[source_code] NamedSource<String>,
    ),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Gorgond(#[from] gorgond_client::Error),
}

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Base URL for the Gorgon API to talk to.
    #[arg(
        long,
        short = 'B',
        env = "GORGON_BASE_URL",
        default_value = "http://localhost:9999/"
    )]
    base_url: url::Url,

    #[command(subcommand)]
    command: Command,
}

impl Args {
    fn new_client(&self) -> Result<Client> {
        Ok(Client::new(self.base_url.clone())?)
    }
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Test your connection to gorgond.
    Ping(Ping),
}
impl Command {
    pub async fn run(&self, args: &Args) -> Result<()> {
        match &args.command {
            Command::Ping(cmd) => self.dump(cmd.run(args).await?),
        }
    }

    fn dump<V: serde::Serialize + Debug>(&self, v: V) -> Result<()> {
        let mut stdout = std::io::stdout();
        colored_json::write_colored_json(&v, &mut stdout)
            .map_err(|err| Error::DumpJSON(err, NamedSource::new("", format!("{v:#?}"))))?;
        Ok(stdout.write_all(b"\n")?)
    }
}

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    #[instrument(name = "ping", skip_all)]
    pub async fn run(&self, args: &Args) -> Result<gorgond_client::RootResponse> {
        Ok(args.new_client()?.root().await?)
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

async fn run(args: Args) -> Result<()> {
    init_logging(&args);
    args.command.run(&args).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .expect("Couldn't install miette hook");

    if let Err(err) = run(Args::parse()).await.map_err(miette::Report::from) {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
