// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like gorgond_client::Error, and puts a warning on *everything*
// returning Error if it's not silenced like this.
#![allow(clippy::result_large_err)]

use std::{borrow::Cow, fmt::Debug};

use clap::{Parser, Subcommand};
use gorgond_client::{api, models, Client, Uuid};
use miette::Result;

pub mod output;

fn parse_key_value(s: &str) -> Result<(Cow<'static, str>, Cow<'static, str>), clap::Error> {
    s.split_once("=")
        .map(|(a, b)| (a.to_string().into(), b.to_string().into()))
        .ok_or(clap::Error::new(clap::error::ErrorKind::NoEquals))
}

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
    #[arg(long, short, action=clap::ArgAction::Count, global=true)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count, global=true)]
    quiet: u8,

    /// Base URL for the Gorgon API to talk to.
    #[arg(
        long,
        short = 'B',
        env = "GORGON_BASE_URL",
        default_value = "http://localhost:9999/"
    )]
    base_url: url::Url,

    #[clap(flatten)]
    out: output::OutputArgs,
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
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Test your connection to gorgond.
    Ping(Ping),
    /// Manage Projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCommand,
    },
    /// Manage Builds.
    Build {
        #[command(subcommand)]
        cmd: BuildCommand,
    },
}
impl Command {
    pub async fn run(self, args: &Args, client: Client) -> Result<()> {
        match self {
            Command::Ping(cmd) => cmd.run(args, client).await,
            Command::Project { cmd } => cmd.run(args, client.projects()).await,
            Command::Build { cmd } => cmd.run(args, client.builds()).await,
        }
    }
}

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    pub async fn run(self, args: &Args, client: Client) -> Result<()> {
        args.out.dump(client.root(api::RootRequest {}).await?.get())
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectCommand {
    /// Create a new project.
    Create {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Get a new project.
    Get { slug: String },
    /// Manage builds for this projects.
    Builds {
        slug: String,
        #[command(subcommand)]
        cmd: ProjectBuildCommand,
    },
}
impl ProjectCommand {
    pub async fn run(self, args: &Args, projects: api::ProjectsClient<'_>) -> Result<()> {
        match self {
            Self::Create { project } => args.out.dump(
                projects
                    .create(api::ProjectCreateRequest {
                        project: models::ProjectSpec {
                            slug: project.slug.as_str().into(),
                            name: project.name.as_str().into(),
                        },
                    })
                    .await?
                    .get(),
            ),
            Self::Get { slug } => args.out.dump(
                projects
                    .slug(&slug)
                    .get(api::ProjectGetRequest {})
                    .await?
                    .get(),
            ),
            Self::Builds { slug, cmd } => cmd.run(args, projects.slug(&slug).builds()).await,
        }
    }
}

#[derive(Debug, Clone, Parser)]
struct ProjectArgs {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Parser)]
struct BuildArgs {
    #[arg(short, long)]
    pub kind: Cow<'static, str>,
    #[arg(short, long, value_parser = parse_key_value)]
    pub inputs: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    #[arg(short, long)]
    pub expr: Cow<'static, str>,
}
impl From<BuildArgs> for models::BuildSpec<'static> {
    fn from(args: BuildArgs) -> Self {
        Self {
            kind: args.kind,
            inputs: args.inputs.into_iter().collect(),
            expr: args.expr,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectBuildCommand {
    /// List builds.
    List,
    /// Create a new (unallocated) job for this project.
    Create {
        #[command(flatten)]
        build: BuildArgs,
    },
}
impl ProjectBuildCommand {
    pub async fn run(self, args: &Args, builds: api::ProjectsBuildsClient<'_>) -> Result<()> {
        match self {
            Self::List => args.out.with(
                |r| output::build_table(r.builds.iter().map(|b| (b, Some(&r.project), None))),
                builds.list(api::ProjectBuildsRequest {}).await?.get(),
            ),
            Self::Create { build } => args.out.dump(
                builds
                    .create(api::ProjectBuildCreateRequest {
                        build: build.into(),
                    })
                    .await?
                    .get(),
            ),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum BuildCommand {
    /// List builds.
    List,
    /// Get a build.
    Get { id: Uuid },
    /// Get build events.
    Events { id: Uuid },
}
impl BuildCommand {
    pub async fn run(self, args: &Args, builds: api::BuildsClient<'_>) -> Result<()> {
        match self {
            Self::List => args.out.with(
                |r| {
                    output::build_table(r.builds.iter().map(|b| {
                        (
                            b,
                            r.projects.get(&b.project_id),
                            b.worker_id.and_then(|id| r.workers.get(&id)),
                        )
                    }))
                },
                builds.list(api::BuildsRequest {}).await?.get(),
            ),
            Self::Get { id } => args
                .out
                .dump(builds.id(id).get(api::BuildRequest {}).await?.get()),
            Self::Events { id } => args.out.with(
                |r| output::event_tree(&r.events),
                builds
                    .id(id)
                    .events(api::BuildEventsRequest {})
                    .await?
                    .get(),
            ),
        }
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
