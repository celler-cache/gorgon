// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::Parser;
use fetchers::Fetcher;
use gorgond_client::{builds, Client};
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::{collections::BTreeMap, path::PathBuf};
use tracing::{debug, debug_span, info, instrument};

pub mod fetchers;

#[derive(Debug, Clone, Parser)]
pub struct Args {
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
    base_url: reqwest::Url,

    /// Paths to search for scripts.
    #[arg(
        short = 'S',
        long,
        env = "GORGON_BUILD_SCRIPTS_PATH",
        default_value = "./gorgon-build/scripts"
    )]
    scripts_path: Vec<PathBuf>,

    /// Arbitrary ID of this builder.
    #[arg(short, long, env = "GORGON_BUILD_ID")]
    id: String,
}

impl Args {
    #[instrument(skip_all)]
    pub fn resolve_script_path(&mut self) -> Result<()> {
        debug!("Resolving script paths...");
        for path in self.scripts_path.iter_mut().filter(|p| !p.is_absolute()) {
            let _enter = debug_span!("path", ?path).entered();
            *path = path
                .canonicalize()
                .into_diagnostic()
                .with_context(|| format!("Failed to canonicalize path: {path:?}"))?;
            debug!(?path, "Resolved!");
        }
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Config {
    pub fetchers: BTreeMap<String, FetcherConfig>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            fetchers: BTreeMap::from_iter([(
                "git".to_string(),
                FetcherConfig {
                    command: "gorgon-build-fetch-git".into(),
                },
            )]),
        }
    }
}

impl Config {
    #[instrument(skip_all)]
    pub fn resolve_fetcher_commands(&mut self, args: &Args) -> Result<()> {
        debug!("Resolving fetcher command paths...");
        let scripts_path_os = std::env::join_paths(&args.scripts_path)
            .into_diagnostic()
            .context("Couldn't build scripts path")?;
        for (name, fcfg) in self
            .fetchers
            .iter_mut()
            .filter(|(_, c)| !c.command.is_absolute())
        {
            let _enter = debug_span!("fetcher", name, command = ?fcfg.command).entered();
            fcfg.command = which::WhichConfig::new()
                .custom_path_list(scripts_path_os.clone())
                .binary_name(fcfg.command.clone().into_os_string())
                .first_result()
                .into_diagnostic()
                .with_context(|| {
                    format!(
                        "Couldn't resolve command for fetcher '{name}': `{}`",
                        fcfg.command.display()
                    )
                })?;
            debug!(command = ?fcfg.command, "Resolved!");
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct FetcherConfig {
    pub command: PathBuf,
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
    Ok(Client::new(args.base_url.clone())?)
}

#[instrument(skip_all)]
async fn request_build(
    args: &Args,
    _cfg: &Config,
    client: &Client,
) -> Result<builds::Build<'static>> {
    info!("Requesting something to build...");
    Ok(client.builds().allocate(&args.id).await?.build)
}

#[instrument(skip_all)]
fn fetch_build_source(
    _args: &Args,
    cfg: &Config,
    build: &builds::Build<'static>,
    termwidth: usize,
) -> Result<tempfile::TempDir> {
    info!(name = build.source.name.as_ref(), "Fetching source tree...");
    let fetcher = Fetcher::for_source(&cfg.fetchers, &build.source)?;
    let tmpdir = tempfile::Builder::new()
        .prefix(&format!("gorgon-build.{}.", build.name))
        .suffix(".d")
        .tempdir()
        .into_diagnostic()
        .context("Couldn't create temporary directory")?;

    debug!(
        command = ?fetcher.cfg.command,
        args = ?fetcher.args,
        cwd = ?tmpdir.path(),
        "Running fetcher..."
    );
    eprintln!(
        "{}",
        format!("{:─^termwidth$}", format!(" {} ", fetcher.name)).dimmed()
    );
    fetcher.fetch(tmpdir.path())?;
    eprintln!("{}", format!("{:─^termwidth$}", "").dimmed());
    Ok(tmpdir)
}

async fn run(mut args: Args) -> miette::Result<()> {
    init_logging(&args);
    let termwidth = terminal_size::terminal_size_of(std::io::stderr())
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);

    args.resolve_script_path()?;

    let mut cfg = Config::default();
    cfg.resolve_fetcher_commands(&args)?;

    let client = init_client(&args).context("Couldn't build gorgond client")?;
    let build = request_build(&args, &cfg, &client)
        .await
        .context("Couldn't get something to build; lost and without purpose")?;
    info!(name = build.name.as_ref(), id = %build.id, " -> Got a task!");

    let tmpdir =
        fetch_build_source(&args, &cfg, &build, termwidth).context("Couldn't fetch source tree")?;
    for entry in tmpdir.path().read_dir().into_diagnostic()? {
        info!("- {:?}", entry.into_diagnostic()?.file_name());
    }

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

    if let Err(err) = run(Args::parse()).await {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
