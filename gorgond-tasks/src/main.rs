// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, field::Empty, info, instrument};
use tracing_indicatif::style::ProgressStyle;

#[derive(Debug, Clone, Parser)]
pub struct Args {
    #[command(flatten)]
    task_runner: gorgond_tasks::tasks::TaskRunnerArgs,
    #[command(flatten)]
    all_tasks: gorgond_tasks::tasks::AllTaskArgs,

    #[command(flatten)]
    db: gorgond_diesel::Args,
    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

#[instrument(name = "ctrl_c", skip_all, fields(indicatif.pb_hide = Empty))]
async fn wait_for_ctrlc(token: CancellationToken) -> Result<()> {
    debug!("Waiting for Ctrl+C...");
    token
        .run_until_cancelled(tokio::signal::ctrl_c())
        .await
        .transpose()
        .into_diagnostic()
        .context("Couldn't wait for Ctrl+C")?;
    info!("Ctrl+C pressed; shutting down...");
    token.cancel();
    Ok(())
}

async fn run(args: Args) -> Result<()> {
    let token = CancellationToken::new();
    tokio::task::spawn(wait_for_ctrlc(token.clone()));

    let pool = args.db.connect().await?;
    args.task_runner
        .run_all(&token, &pool, &args.helpers, args.all_tasks)
        .await
}

fn main() {
    let args = Args::parse();

    use tracing_subscriber::prelude::*;
    let pb_footer =
        ProgressStyle::with_template("...and {pending_progress_bars} more not shown above.")
            .unwrap();
    let pb = tracing_indicatif::IndicatifLayer::new()
        .with_progress_style(gorgon_util_cli::PB_STYLE_SPIN.clone())
        .with_max_progress_bars(20, Some(pb_footer));
    let (w, _w) = tracing_appender::non_blocking(pb.get_stderr_writer());
    (gorgon_util_cli::logger(args.common).build())
        .with(tracing_subscriber::fmt::layer().with_writer(w))
        .with(pb.with_filter(tracing_indicatif::filter::IndicatifFilter::new(true)))
        .init();

    gorgon_util_cli::main_async(args.common, run(args));
}
