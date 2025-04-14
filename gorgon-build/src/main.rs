// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::Parser;
use gorgon_build_helper::helper::HelperCaller;
use gorgond_client::{models::Build, Client, Yoke};
use log::info;
use miette::{miette, Context, IntoDiagnostic, Result};
use std::{path::PathBuf, process::Stdio};

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// Base URL for the Gorgon API to talk to.
    #[arg(
        long,
        short = 'B',
        env = "GORGON_BASE_URL",
        default_value = "http://localhost:9999/"
    )]
    base_url: reqwest::Url,

    /// Arbitrary ID of this builder.
    #[arg(short, long, env = "GORGON_BUILD_ID")]
    id: String,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

fn init_client(args: &Args) -> Result<Client> {
    Ok(Client::new(args.base_url.clone())?)
}

#[derive(Debug, PartialEq, Eq)]
pub struct FetcherConfig {
    pub command: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuilderConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub current_dir: Option<PathBuf>,
}

async fn request_build(args: &Args, client: &Client) -> Result<Yoke<Build<'static>>> {
    info!("Requesting something to build...");
    Ok(client
        .builds()
        .allocate(&args.id)
        .await?
        .map_project(|rsp, _| rsp.build))
}

pub fn run(args: Args) -> miette::Result<()> {
    // We need to make some async calls to the gorgond API, but most of what this program
    // does is very sync-shaped. So instead of this being an async program that spends
    // most of its runtime doing `block_in_place` or `spawn_blocking`, just make a runtime
    // on the current thread and `block_on` the occasional async block.
    let main_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()
        .context("Couldn't create async runtime")?;

    // Ask gorgond for something to do.
    let client = init_client(&args).context("Couldn't build gorgond client")?;
    let build_ = main_rt.block_on(async {
        request_build(&args, &client)
            .await
            .context("Couldn't get something to build; lost and without purpose")
    })?;
    let build = build_.get();
    info!(id:% = build.id; " -> Got a task!");

    // Find builder.
    let caller = args.helpers.caller()?;
    let builder = caller.helper(format!("gorgon-build-{}", build.kind))?;

    // Run the builder, send it the build spec over stdin.
    info!("Starting build...");
    let mut build_task = builder
        .with(&args.helpers)
        .with_arg("-")
        .with_stdin(Stdio::piped())
        .into_task(None);
    let mut build_child = build_task.start().context("Couldn't start build")?;
    if let Some(mut stdin) = build_child.child.stdin.as_mut() {
        serde_json::to_writer_pretty(&mut stdin, &build)
            .into_diagnostic()
            .context("Couldn't write Build to builder's stdin")?;
    } else {
        return Err(miette!("Builder process has no stdin?"));
    };
    build_child.wait_checked()?;

    info!("Done!");
    Ok(())
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common.log_format, args.common.level()).init();
    gorgon_util_cli::main(args.common.output_format, || run(args));
}
