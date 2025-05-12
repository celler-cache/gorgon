// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy doesn't like gorgond_client::Error.
#![allow(clippy::result_large_err)]

use std::sync::atomic::{AtomicUsize, Ordering};

use clap::{Parser, Subcommand};
use miette::Result;

pub mod models;
pub mod run;
pub mod schema;

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    pub verbose: u8,
    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    pub quiet: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Run the Gorgon daemon.
    Run(run::Args),
}
impl Command {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Run(args) => run::run(args).await,
        }
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
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target("axum::rejections", Level::TRACE)
                // These log way too much internal information. Comment out if debugging them.
                .with_target("tokio_postgres", Level::INFO)
                .with_default(level),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

fn main() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .expect("Couldn't install miette hook");

    let args = Args::parse();
    init_logging(&args);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name_fn(|| {
            static THREAD_ID: AtomicUsize = AtomicUsize::new(0);
            let id = THREAD_ID.fetch_add(1, Ordering::SeqCst);
            format!("gorgond-tokio-{id}")
        })
        .build()
        .expect("Couldn't build tokio runtime");

    if let Err(err) = rt.block_on(args.command.run()) {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
