// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{ffi::OsStr, path::PathBuf};

use clap::Parser;
use gorgon_build_helper::helper::Helper;
use miette::{miette, Context, IntoDiagnostic, Result};
use tracing::{debug, debug_span, instrument};
use tracing_subscriber::filter::Targets;

#[derive(Debug, Clone, Parser)]
#[group(id = "common")]
#[command(next_help_heading = "Logging")]
pub struct Args {
    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,
}

impl Args {
    pub fn level(&self) -> tracing::Level {
        match self.verbose as i8 - self.quiet as i8 {
            ..=-2 => tracing::Level::ERROR,
            -1 => tracing::Level::WARN,
            0 => tracing::Level::INFO,
            1 => tracing::Level::DEBUG,
            2.. => tracing::Level::TRACE,
        }
    }

    pub fn level_flag(&self) -> Option<&str> {
        match self.level() {
            tracing::Level::ERROR => Some("-qq"),
            tracing::Level::WARN => Some("-q"),
            tracing::Level::INFO => None,
            tracing::Level::DEBUG => Some("-v"),
            tracing::Level::TRACE => Some("-vv"),
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[group(id = "helpers")]
#[command(next_help_heading = "Helpers")]
pub struct HelperArgs {
    /// Path(s) searched for helpers. Default: $PATH
    #[arg(long)]
    helper_path: Vec<PathBuf>,

    /// Run helpers from source. Requires `cargo`.
    #[arg(long)]
    helper_dev: bool,

    /// With --helper-dev, build from this repository.
    #[arg(long)]
    helper_dev_repo: Option<PathBuf>,
}

impl HelperArgs {
    #[instrument(skip_all, fields(name=?name.as_ref()))]
    pub fn helper(&self, name: impl AsRef<OsStr>) -> Result<Helper> {
        let cwd = std::env::current_dir()
            .into_diagnostic()
            .context("Couldn't get current working directory")?;

        // If --helper-dev is set, try to build the helper from source.
        if self.helper_dev {
            let _enter = debug_span!("--helper-dev").entered();
            let repo_path = self.helper_dev_repo.as_ref().unwrap_or(&cwd);
            let src_path = repo_path.join(PathBuf::from(&name));
            match src_path.try_exists() {
                Ok(true) => {
                    return Helper::build_dev(&name, repo_path, &cwd)
                        .with_context(|| format!("Couldn't build helper: `{:?}`", name.as_ref()))
                }
                Ok(false) => debug!(?src_path, "Path does not exist"),
                Err(err) => debug!(?src_path, %err, "Couldn't check if path exists"),
            }
        }

        // Try to find the helper in either --helper-path or $PATH.
        match &self.helper_path[..] {
            [] => Helper::find(
                &name,
                std::env::var_os("PATH")
                    .ok_or_else(|| miette!("Neither $PATH nor --search-path is set"))?,
                &cwd,
            ),
            [path] => Helper::find(&name, path, &cwd),
            paths => Helper::find(
                &name,
                std::env::join_paths(paths)
                    .into_diagnostic()
                    .context("Couldn't join --search-path")?,
                &cwd,
            ),
        }
        .with_context(|| format!("Couldn't find helper: `{:?}`", name.as_ref()))
    }
}

#[must_use]
pub struct LoggerBuilder {
    targets: Targets,
}

impl LoggerBuilder {
    pub fn with_targets<F: FnOnce(Targets) -> Targets>(mut self, f: F) -> Self {
        self.targets = f(self.targets);
        self
    }

    pub fn init(self) {
        use tracing_subscriber::prelude::*;

        tracing_subscriber::Registry::default()
            .with(self.targets)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    }
}

pub fn logger(level: tracing::Level) -> LoggerBuilder {
    LoggerBuilder {
        targets: tracing_subscriber::filter::Targets::new().with_default(level),
    }
}

pub fn main<A: Parser, E: Into<miette::Report>, F: FnOnce(A) -> Result<(), E>>(f: F) {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .expect("Couldn't install miette hook");

    if let Err(err) = f(A::parse()).map_err(|err| err.into()) {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
