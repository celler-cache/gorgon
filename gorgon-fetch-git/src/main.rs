// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use clap::Parser;
use gorgon_build_helper::helper::HelperCaller;
use miette::{Context, Result};
use url::Url;

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// URL to fetch.
    url: Url,

    /// Revision to check out.
    #[arg(long)]
    rev: String,

    #[command(flatten)]
    fetcher: gorgon_build_helper::fetcher::FetcherArgs,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

fn run(args: Args) -> Result<()> {
    let git_caller = args.helpers.caller()?;
    git_caller
        .helper("git")?
        .with_arg("clone")
        .with_arg(args.url.as_str())
        .with_arg(&args.fetcher.out)
        .run()
        .with_context(|| format!("Couldn't clone repository: {}", &args.url))?;
    git_caller
        .helper("git")?
        .with_arg("checkout")
        .with_arg(&args.rev)
        .with_cwd(&args.fetcher.out)
        .run()
        .with_context(|| format!("Couldn't check out revision: {}", &args.rev))?;

    Ok(())
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common.log_format, args.common.level()).init();
    gorgon_util_cli::main(args.common.output_format, || run(args));
}
