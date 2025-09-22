// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{
    str::FromStr,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use clap::Parser;
use directories::ProjectDirs;
use miette::{Context, IntoDiagnostic, Result};
use rayon::iter::{ParallelBridge, ParallelIterator};
use tokio_util::sync::CancellationToken;
use tracing::{debug, field::Empty, info, instrument};
use tracing_indicatif::style::ProgressStyle;

pub mod tasks;

static DIRS: LazyLock<ProjectDirs> = LazyLock::new(|| {
    ProjectDirs::from("org", "Gorgon", "gorgond").expect("Couldn't find project directories!?")
});

#[derive(Debug, Clone, Parser)]
pub struct Args {
    #[arg(long, short, default_value_t = 2)]
    pub threads: usize,

    /// Time between checks for new tasks.
    #[arg(long, default_value = "10m", value_parser = parse_duration)]
    pub poll_interval: chrono::Duration,

    /// Time before refreshing non-fixed refs.
    ///
    /// This means Source URLs that don't refer to a fixed commit, eg.
    /// "git+https://github.com/NixOS/nixpkgs?ref=master".
    #[arg(long, default_value = "1h", value_parser = parse_duration)]
    pub ref_stale: chrono::Duration,

    #[command(flatten)]
    db: gorgond_diesel::Args,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

fn parse_duration(s: &str) -> Result<chrono::Duration, clap::Error> {
    humantime::Duration::from_str(s)
        .map_err(|err| clap::Error::raw(clap::error::ErrorKind::InvalidValue, err))
        .and_then(|d| {
            chrono::Duration::from_std(*d)
                .map_err(|err| clap::Error::raw(clap::error::ErrorKind::InvalidValue, err))
        })
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

fn run(args: Args) -> Result<()> {
    // Set up the Rayon thread pool.
    rayon::ThreadPoolBuilder::new()
        .thread_name(|idx| format!("rayon-{:02}", idx + 1))
        .num_threads(args.threads)
        .build_global()
        .into_diagnostic()
        .context("Couldn't init Rayon thread pool")?;

    // Set up a tokio runtime, but we're handling it manually.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name_fn({
            static THREAD_ID: AtomicUsize = AtomicUsize::new(1);
            move || format!("tokio-{:02}", THREAD_ID.fetch_add(1, Ordering::Relaxed))
        })
        .build()
        .map(Arc::new)
        .into_diagnostic()
        .context("Couldn't init Tokio runtime")?;

    // Connect to the database.
    let pool = rt.block_on(args.db.connect())?;

    // Create a cancellation token, which is cancelled on Ctrl+C.
    let token = CancellationToken::new();
    rt.spawn(wait_for_ctrlc(token.clone()));

    // Find tasks, until Ctrl+C. If this exits for any reason, cancel the token.
    let (send, recv) = crossbeam::channel::unbounded();
    let join_find = rt.spawn({
        let (args, token, pool) = (args.clone(), token.clone(), pool.clone());
        async move {
            let _token = token.drop_guard_ref();
            token
                .run_until_cancelled(tasks::Task::watch(&args, send, pool))
                .await
        }
    });

    // Process tasks in parallel.
    recv.into_iter()
        .par_bridge()
        .try_for_each_with(rt.clone(), |rt, task| task.run(&args, rt, &token, &pool))
        .context("Runner failed")?;
    token.cancel();

    // Check the output of the task finding task.
    rt.block_on(join_find)
        .into_diagnostic()
        .context("Watcher panicked!")?
        .transpose()
        .context("Watcher failed")?;

    Ok(())
}

static PB_STYLE_BLANK: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template(concat!(
        "{span_child_prefix} {span_name} {span_fields}",
        "{wide_msg}",
    ))
    .unwrap()
});
static PB_STYLE_SPIN: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template(concat!(
        "{span_child_prefix}{spinner} {elapsed} {span_name} {span_fields}",
        "{wide_msg}",
    ))
    .unwrap()
});
static PB_STYLE_BAR: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template(concat!(
        "{span_child_prefix}{spinner} {elapsed} {span_name} {span_fields}",
        "{bar} {human_pos}/{human_len} {wide_msg}",
    ))
    .unwrap()
});
static PB_STYLE_BAR_ETA: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template(concat!(
        "{span_child_prefix}{spinner} {elapsed} {span_name} {span_fields}",
        "{bar} {human_pos}/{human_len} {wide_msg} ETA: {eta}",
    ))
    .unwrap()
});

fn main() {
    let args = Args::parse();

    use tracing_subscriber::prelude::*;
    let pb_footer =
        ProgressStyle::with_template("...and {pending_progress_bars} more not shown above.")
            .unwrap();
    let pb = tracing_indicatif::IndicatifLayer::new()
        .with_progress_style(PB_STYLE_SPIN.clone())
        .with_max_progress_bars(20, Some(pb_footer));
    let (w, _w) = tracing_appender::non_blocking(pb.get_stderr_writer());
    (gorgon_util_cli::logger(args.common).build())
        .with(tracing_subscriber::fmt::layer().with_writer(w))
        .with(pb.with_filter(tracing_indicatif::filter::IndicatifFilter::new(true)))
        .init();

    gorgon_util_cli::main(args.common, || run(args));
}
