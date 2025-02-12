// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::Parser;
use gorgond_client::{builds, Client};
use miette::{miette, Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::{cell::LazyCell, collections::BTreeMap, path::PathBuf};
use tracing::{debug, debug_span, info, instrument};

pub mod helpers;

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

    /// Run builders from source.
    #[arg(long)]
    dev: bool,

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

#[derive(Debug, Eq, PartialEq)]
pub struct Config {
    pub fetchers: BTreeMap<String, FetcherConfig>,
    pub builders: BTreeMap<String, BuilderConfig>,
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
            builders: BTreeMap::from_iter([(
                "nix".to_string(),
                BuilderConfig {
                    command: "gorgon-build-nix".into(),
                    args: vec![],
                    current_dir: None,
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

    #[instrument(skip_all)]
    pub fn resolve_builder_commands(&mut self, args: &Args) -> Result<()> {
        let repo_path =
            LazyCell::new(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../")));

        debug!("Resolving builder command paths...");
        for (name, bcfg) in self
            .builders
            .iter_mut()
            .filter(|(_, c)| !c.command.is_absolute())
        {
            let _enter = debug_span!("fetcher", name, command = ?bcfg.command).entered();
            if args.dev {
                info!("--dev: using development versions of in-tree builders...");
                let dev_path = (*repo_path).join(&bcfg.command);
                if dev_path
                    .try_exists()
                    .into_diagnostic()
                    .with_context(|| "Couldn't check if directory exists: {dev_path:?}")?
                {
                    info!(" -> 🚧 Running builder from source");
                    *bcfg = BuilderConfig {
                        command: "cargo".into(),
                        args: vec![
                            "run".into(),
                            "-p".into(),
                            bcfg.command.to_string_lossy().into(),
                            "--".into(),
                        ],
                        current_dir: Some(repo_path.clone()),
                    };
                } else {
                    info!(" -> ❔ Not found in tree, running from path");
                }
            }

            bcfg.command = which::WhichConfig::new()
                .binary_name(bcfg.command.clone().into_os_string())
                .first_result()
                .into_diagnostic()
                .with_context(|| {
                    format!(
                        "Couldn't resolve command for builder '{name}': `{}`",
                        bcfg.command.display()
                    )
                })?;
            debug!(command = ?bcfg.command, "Resolved!");
        }
        Ok(())
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

async fn run(mut args: Args) -> miette::Result<()> {
    init_logging(&args);
    let termwidth = terminal_size::terminal_size_of(std::io::stderr())
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);

    args.resolve_script_path()?;

    let mut cfg = Config::default();
    cfg.resolve_fetcher_commands(&args)?;
    cfg.resolve_builder_commands(&args)?;

    // Ask gorgond for something to do.
    let client = init_client(&args).context("Couldn't build gorgond client")?;
    let build = request_build(&args, &cfg, &client)
        .await
        .context("Couldn't get something to build; lost and without purpose")?;
    info!(name = build.name.as_ref(), id = %build.id, " -> Got a task!");

    // Check that fetcher and builder are valid.
    let mut fetcher =
        helpers::fetcher(&cfg.fetchers, &build.source).context("Couldn't get fetcher")?;
    let mut builder = helpers::builder(&cfg.builders, &build).context("Couldn't get builder")?;

    let tmpdir = tempfile::Builder::new()
        .prefix(&format!("gorgon-build.{}.", build.name))
        .suffix(".d")
        .tempdir()
        .into_diagnostic()
        .context("Couldn't create temporary directory")?;
    fetcher.current_dir = Some(tmpdir.path().into());
    builder.args.push(
        tmpdir
            .path()
            .to_str()
            .ok_or_else(|| miette!("tmpdir is not valid UTF-8"))?
            .into(),
    );

    // Fetch sources.
    {
        let _enter = debug_span!("fetch").entered();
        debug!(
            command = ?fetcher.command,
            args = ?fetcher.args,
            current_dir = ?fetcher.current_dir,
            "Running fetcher..."
        );
        eprintln!(
            "{}",
            format!("{:─^termwidth$}", format!(" {} ", fetcher.name)).dimmed()
        );
        fetcher.run()?;
        eprintln!("{}", format!("{:─^termwidth$}", "").dimmed());
    }

    // Start build.
    {
        let _enter = debug_span!("build").entered();
        debug!(
            command = ?builder.command,
            args = ?builder.args,
            current_dir = ?builder.current_dir,
            "Running builder..."
        );
        eprintln!(
            "{}",
            format!("{:─^termwidth$}", format!(" {} ", builder.name)).dimmed()
        );
        builder.run()?;
        eprintln!("{}", format!("{:─^termwidth$}", "").dimmed());
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
