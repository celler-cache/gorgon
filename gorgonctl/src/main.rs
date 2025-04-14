// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like gorgond_client::Error, and puts a warning on *everything*
// returning Error if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use gorgond_client::{api, Client, Yoke};
use miette::{IntoDiagnostic, NamedSource, Result};
use std::{fmt::Debug, io::Write};
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
    fn level(&self) -> tracing::Level {
        match self.verbose as i8 - self.quiet as i8 {
            ..=-2 => tracing::Level::ERROR,
            -1 => tracing::Level::WARN,
            0 => tracing::Level::INFO,
            1 => tracing::Level::DEBUG,
            2.. => tracing::Level::TRACE,
        }
    }

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
            Command::Ping(cmd) => self.dump(cmd.run(args).await?.get()),
        }
    }

    fn dump<V: serde::Serialize + Debug>(&self, v: V) -> Result<()> {
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("Serializing output failed")]
        struct DumpJsonError(
            #[source] serde_json::Error,
            #[source_code] NamedSource<String>,
        );
        let mut stdout = std::io::stdout();
        colored_json::write_colored_json(&v, &mut stdout)
            .map_err(|err| DumpJsonError(err, NamedSource::new("", format!("{v:#?}"))))?;
        stdout.write_all(b"\n").into_diagnostic()
    }
}

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    #[instrument(name = "ping", skip_all)]
    pub async fn run(&self, args: &Args) -> Result<Yoke<api::RootResponse>> {
        Ok(args.new_client()?.root().await?)
    }
}

async fn run(args: Args) -> Result<()> {
    args.command.run(&args).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    use tracing_subscriber::prelude::*;
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::Targets::new().with_default(args.level()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .unwrap();

    if let Err(err) = run(args).await {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
