// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like gorgond_client::Error, and puts a warning on *everything*
// returning Error if it's not silenced like this.
#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use gorgond_client::{api, models, Client};
use miette::{miette, IntoDiagnostic, NamedSource, Result};
use std::{collections::BTreeMap, fmt::Debug, io::Write};

#[derive(Debug, Clone, Parser)]
struct AllArgs {
    #[command(flatten)]
    args: Args,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Parser)]
struct Args {
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
    base_url: url::Url,
}

impl Args {
    fn level(&self) -> tracing::Level {
        match self.verbose as i8 - self.quiet as i8 {
            ..=-2 => tracing::Level::ERROR,
            -1 => tracing::Level::WARN,
            0 => tracing::Level::INFO,
            1 => tracing::Level::DEBUG,
            2.. => tracing::Level::TRACE,
        }
    }

    fn new_client(&self) -> Result<Client> {
        Ok(Client::new(self.base_url.clone())?)
    }

    fn dump<V: serde::Serialize + Debug>(&self, v: V) -> Result<()> {
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("Serializing output failed")]
        struct DumpJsonError(
            #[source] serde_json::Error,
            #[source_code] NamedSource<String>,
        );
        let mut stdout = std::io::stdout();
        colored_json::write_colored_json(&v, &mut stdout)
            .map_err(|err| DumpJsonError(err, NamedSource::new("", format!("{v:#?}"))))?;
        stdout.write_all(b"\n").into_diagnostic()
    }
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Test your connection to gorgond.
    Ping(Ping),
    /// Manage Projects.
    Projects {
        #[command(subcommand)]
        cmd: ProjectsCommand,
    },
    /// Manage a Project.
    Project {
        id: String,
        #[command(subcommand)]
        cmd: ProjectCommand,
    },
}
impl Command {
    pub async fn run(self, args: &Args, client: Client) -> Result<()> {
        match self {
            Command::Ping(cmd) => cmd.run(args, client).await,
            Command::Projects { cmd } => cmd.run(args, client.projects()).await,
            Command::Project { id, cmd } => cmd.run(args, client.projects().id(&id)).await,
        }
    }
}

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    pub async fn run(self, args: &Args, client: Client) -> Result<()> {
        args.dump(client.root(api::RootRequest {}).await?.get())
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectsCommand {
    /// Create a new project.
    Create(ProjectCreateCommand),
}
impl ProjectsCommand {
    pub async fn run(self, args: &Args, projects: api::ProjectsClient<'_>) -> Result<()> {
        match self {
            Self::Create(cmd) => cmd.run(args, projects).await,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectCommand {
    /// Get a new project.
    Get,
    /// Manage builds for this projects.
    Builds {
        #[command(subcommand)]
        cmd: ProjectBuildCommand,
    },
}
impl ProjectCommand {
    pub async fn run(self, args: &Args, project: api::ProjectClient<'_>) -> Result<()> {
        match self {
            Self::Get => args.dump(project.get(api::ProjectGetRequest {}).await?.get()),
            Self::Builds { cmd } => cmd.run(args, project.builds()).await,
        }
    }
}

#[derive(Debug, Clone, Parser)]
struct ProjectCreateCommand {
    pub slug: String,
    pub name: String,
}
impl ProjectCreateCommand {
    pub async fn run(self, args: &Args, projects: api::ProjectsClient<'_>) -> Result<()> {
        args.dump(
            projects
                .create(api::ProjectCreateRequest {
                    project: models::ProjectSpec {
                        slug: self.slug.as_str().into(),
                        name: self.name.as_str().into(),
                    },
                })
                .await?
                .get(),
        )
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectBuildCommand {
    Create(ProjectBuildCreateCommand),
}
impl ProjectBuildCommand {
    pub async fn run(self, args: &Args, builds: api::ProjectsBuildsClient<'_>) -> Result<()> {
        match self {
            Self::Create(cmd) => cmd.run(args, builds).await,
        }
    }
}

/// Create a new project.
#[derive(Debug, Clone, Parser)]
struct ProjectBuildCreateCommand {
    #[arg(short, long)]
    pub input: Vec<String>,
    #[arg(short, long, default_value = "nix")]
    pub kind: String,

    pub source: String,
    pub expr: String,
}
impl ProjectBuildCreateCommand {
    pub async fn run(self, args: &Args, builds: api::ProjectsBuildsClient<'_>) -> Result<()> {
        let mut inputs = BTreeMap::new();
        for input in self.input {
            let (k, v) = input.split_once("=").ok_or(miette!("-i/--input=key=url"))?;
            inputs.insert(k.to_string().into(), v.to_string().into());
        }
        args.dump(
            builds
                .create(api::ProjectBuildCreateRequest {
                    build: models::BuildSpec {
                        kind: self.kind.into(),
                        inputs,
                        source: self.source.into(),
                        expr: self.expr.into(),
                    },
                })
                .await?
                .get(),
        )
    }
}

async fn run(args: AllArgs) -> Result<()> {
    args.command
        .run(&args.args, args.args.new_client()?)
        .await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = AllArgs::parse();

    use tracing_subscriber::prelude::*;
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::Targets::new().with_default(args.args.level()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .context_lines(6)
                .build(),
        )
    }))
    .unwrap();

    if let Err(err) = run(args).await {
        eprintln!("\n{err:?}");
        std::process::exit(1);
    }
}
