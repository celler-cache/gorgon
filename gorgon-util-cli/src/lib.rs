// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{borrow::Cow, path::PathBuf};

use clap::Parser;
use gorgon_build_helper::helper::{Helper, HelperCaller, HelperContext, HelperFinder};
use miette::{miette, Context, IntoDiagnostic, Result};
use tracing_subscriber::filter::Targets;

#[derive(Debug, Clone, Parser)]
#[group(id = "common")]
#[command(next_help_heading = "Logging")]
pub struct Args {
    /// Increase log level.
    #[arg(long, short, env = "GORGON_VERBOSE", action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, env = "GORGON_QUIET", action=clap::ArgAction::Count)]
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
}

impl HelperContext for &Args {
    fn apply(&self, mut helper: Helper) -> Helper {
        if self.verbose > 0 {
            helper = helper.with_env("GORGON_VERBOSE", self.verbose.to_string());
        }
        if self.quiet > 0 {
            helper = helper.with_env("GORGON_QUIET", self.quiet.to_string());
        }
        helper
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
    pub fn caller(&self) -> Result<impl HelperCaller + use<'_>> {
        let current_dir = std::env::current_dir()
            .into_diagnostic()
            .context("Couldn't get current working directory")?;

        let path = match &self.helper_path[..] {
            [] => Cow::Owned(
                std::env::var_os("PATH")
                    .ok_or_else(|| miette!("Neither $PATH nor --search-path is set"))?,
            ),
            [path] => Cow::Borrowed(path.as_ref()),
            paths => Cow::Owned(
                std::env::join_paths(paths)
                    .into_diagnostic()
                    .context("Couldn't join --search-path")?,
            ),
        };

        let mut caller = HelperFinder::new(current_dir.into(), path);
        if self.helper_dev {
            caller.with_dev(
                self.helper_dev_repo
                    .as_ref()
                    .map(|p| Cow::Borrowed(p.as_ref())),
            );
        }
        Ok(caller)
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
