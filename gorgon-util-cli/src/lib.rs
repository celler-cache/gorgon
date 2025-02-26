// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{borrow::Cow, path::PathBuf};

use clap::Parser;
use colored_json::write_colored_json_with_mode;
use gorgon_build_helper::{
    helper::{Helper, HelperCaller, HelperContext, HelperFinder},
    SerializedError,
};
use miette::{miette, Context, IntoDiagnostic, Result};
use strum::{AsRefStr, Display, EnumString, IntoStaticStr};
use tracing_subscriber::filter::Targets;

#[derive(
    Debug,
    Display,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    EnumString,
    AsRefStr,
    IntoStaticStr,
    clap::ValueEnum,
)]
#[strum(serialize_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Default,
    Json,
}

#[derive(Debug, Clone, Parser)]
#[group(id = "common")]
#[command(next_help_heading = "Logging")]
pub struct Args {
    /// Increase log level.
    #[arg(long, short, env = "GORGON_VERBOSE", action=clap::ArgAction::Count)]
    pub verbose: u8,

    /// Decrease log level.
    #[arg(long, short, env = "GORGON_QUIET", action=clap::ArgAction::Count)]
    pub quiet: u8,

    /// Output format for logs.
    #[arg(long, env = "GORGON_LOG_FORMAT", default_value = "default")]
    pub log_format: OutputFormat,

    /// Output format for output and errors.
    #[arg(long, env = "GORGON_OUTPUT_FORMAT", default_value = "default")]
    pub output_format: OutputFormat,
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
            .with_env("GORGON_LOG_FORMAT", self.log_format.as_ref())
            .with_env("GORGON_OUTPUT_FORMAT", self.output_format.as_ref())
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
    format: OutputFormat,
    targets: Targets,
}

impl LoggerBuilder {
    pub fn with_targets<F: FnOnce(Targets) -> Targets>(mut self, f: F) -> Self {
        self.targets = f(self.targets);
        self
    }
    pub fn with_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }

    pub fn init(self) {
        use tracing_subscriber::{fmt::layer, prelude::*};

        // This is a dumb workaround for some type fuckery.
        let reg = tracing_subscriber::Registry::default().with(self.targets);
        match self.format {
            OutputFormat::Default => reg.with(layer().with_writer(std::io::stderr)).init(),
            OutputFormat::Json => reg.with(layer().json().with_writer(std::io::stderr)).init(),
        }
    }
}

pub fn logger(format: OutputFormat, level: tracing::Level) -> LoggerBuilder {
    LoggerBuilder {
        format,
        targets: tracing_subscriber::filter::Targets::new().with_default(level),
    }
}

pub fn main<E: Into<miette::Report>, F: FnOnce() -> Result<(), E>>(format: OutputFormat, f: F) {
    match format {
        OutputFormat::Default => miette::set_hook(Box::new(|_| {
            Box::new(
                miette::MietteHandlerOpts::new()
                    .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                    .context_lines(6)
                    .build(),
            )
        }))
        .expect("Couldn't install miette hook"),
        OutputFormat::Json => {}
    }

    if let Err(err) = f().map_err(|err| err.into()) {
        match format {
            OutputFormat::Default => eprintln!("\n{err:?}"),
            OutputFormat::Json => {
                write_colored_json_with_mode(
                    &SerializedError::new(AsRef::<dyn miette::Diagnostic>::as_ref(&err)),
                    &mut std::io::stdout(),
                    colored_json::ColorMode::Auto(colored_json::Output::StdOut),
                )
                .expect("Couldn't serialize error");
                println!();
            }
        }
        std::process::exit(1);
    }
}
