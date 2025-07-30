// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like gorgond_client::Error, and puts a warning on *everything*
// returning Error if it's not silenced like this.
#![allow(clippy::result_large_err)]

use std::{borrow::Cow, fmt::Debug, io::Read};

use clap::{Parser, Subcommand};
use gorgonctl::output;
use gorgond_client::{api, events::BuildEvent, models, Client, Uuid};
use miette::{miette, Context, IntoDiagnostic, Result};
use tracing::debug;
use url::Url;

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
    /// Manage Repositories.
    Repo {
        #[command(subcommand)]
        cmd: RepositoriesCommand,
    },
    /// Manage Projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectsCommand,
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
            Command::Repo { cmd } => cmd.run(args, client.repositories()).await,
            Command::Project { cmd } => cmd.run(args, client.projects()).await,
            Command::Build { cmd } => cmd.run(args, client.builds()).await,
        }
    }
}

// -- Root -- //

#[derive(Debug, Clone, Parser)]
struct Ping;
impl Ping {
    pub async fn run(self, args: &Args, client: Client) -> Result<()> {
        args.out.dump(client.root(api::RootRequest {}).await?.get())
    }
}

// -- Repositories -- //

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Repository")]
struct RepositoryArgs {
    pub kind: String,
    pub ident: String,
}
impl From<RepositoryArgs> for models::RepositorySpec<'static> {
    fn from(args: RepositoryArgs) -> Self {
        Self {
            kind: args.kind.into(),
            ident: args.ident.into(),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum RepositoriesCommand {
    /// Manage a repository by ID.
    Id {
        id: Uuid,
        #[command(subcommand)]
        command: RepositoryCommand,
    },
    /// Manage repositories by Kind.
    Kind {
        kind: String,
        #[command(subcommand)]
        command: RepositoriesKindCommand,
    },
    /// Create a new repository.
    Create {
        #[command(flatten)]
        spec: RepositoryArgs,
    },
    /// List repositories.
    List,
}
impl RepositoriesCommand {
    pub async fn run(self, args: &Args, repos: api::RepositoriesClient<'_>) -> Result<()> {
        match self {
            Self::Id { id, command } => command.run(args, repos.id(id)).await,
            Self::Kind { kind, command } => command.run(args, repos.kind(&kind)).await,
            Self::Create { spec } => args.out.with(
                |r| output::repo_table([&r.repository]),
                repos
                    .create(api::RepositoryCreateRequest {
                        repository: spec.into(),
                    })
                    .await?
                    .get(),
            ),
            Self::List => args.out.with(
                |r| output::repo_table(r.repositories.iter()),
                repos.list(api::RepositoriesListRequest {}).await?.get(),
            ),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum RepositoriesKindCommand {
    /// Manage a repository by Kind + Ident.
    Ident {
        ident: String,
        #[command(subcommand)]
        command: RepositoryCommand,
    },
    /// List repositories by Kind.
    List,
}
impl RepositoriesKindCommand {
    pub async fn run(self, args: &Args, repos: api::RepositoriesKindClient<'_>) -> Result<()> {
        match self {
            Self::Ident { ident, command } => command.run(args, repos.ident(&ident)).await,
            Self::List => args.out.with(
                |r| output::repo_table(r.repositories.iter()),
                repos.list(api::RepositoriesListRequest {}).await?.get(),
            ),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum RepositoryCommand {
    /// Get this repository.
    Get,
    /// Manage Merge Requests for this Repository.
    Merges {
        #[command(subcommand)]
        command: RepositoryMergeRequestsCommand,
    },
}
impl RepositoryCommand {
    pub async fn run(self, args: &Args, repo: api::RepositoryClient<'_>) -> Result<()> {
        match self {
            Self::Get => args.out.with(
                |r| output::repo_table([&r.repository]),
                repo.get(api::RepositoryGetRequest {}).await?.get(),
            ),
            Self::Merges { command } => command.run(args, repo.merges()).await,
        }
    }
}

// -- Merge Requests -- //

#[derive(Debug, Clone, Subcommand)]
enum RepositoryMergeRequestsCommand {
    /// List merge requests.
    List,
    /// Manage a merge requests by ident.
    Ident {
        ident: String,
        #[command(subcommand)]
        command: RepositoryMergeRequestCommand,
    },
}
impl RepositoryMergeRequestsCommand {
    pub async fn run(
        self,
        args: &Args,
        merges: api::RepositoryMergeRequestsClient<'_>,
    ) -> Result<()> {
        match self {
            Self::List => args.out.with(
                |r| {
                    output::merge_table(
                        r.merge_requests
                            .iter()
                            .map(|m| (m, r.repositories.get(&m.repository_id))),
                    )
                },
                merges
                    .list(api::RepositoryMergeRequestsListRequest {})
                    .await?
                    .get(),
            ),
            Self::Ident { ident, command } => command.run(args, merges.ident(&ident)).await,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum RepositoryMergeRequestCommand {
    /// Get this merge request.
    Get,
}
impl RepositoryMergeRequestCommand {
    pub async fn run(
        self,
        args: &Args,
        merge: api::RepositoryMergeRequestClient<'_>,
    ) -> Result<()> {
        match self {
            Self::Get => args.out.with(
                |r| output::merge_table([(&r.merge_request, Some(&r.repository))]),
                &merge
                    .get(api::RepositoryMergeRequestGetRequest {})
                    .await?
                    .get(),
            ),
        }
    }
}

// -- Projects -- //

#[derive(Debug, Clone, Parser)]
struct ProjectArgs {
    pub slug: Cow<'static, str>,
    pub name: Cow<'static, str>,
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectsCommand {
    /// Create a project.
    Create {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Manage a project by slug.
    Slug {
        slug: String,
        #[command(subcommand)]
        cmd: ProjectCommand,
    },
}
impl ProjectsCommand {
    pub async fn run(self, args: &Args, projects: api::ProjectsClient<'_>) -> Result<()> {
        match self {
            Self::Create { project } => args.out.dump(
                projects
                    .create(api::ProjectCreateRequest {
                        project: models::ProjectSpec {
                            slug: project.slug,
                            name: project.name,
                        },
                    })
                    .await?
                    .get(),
            ),
            Self::Slug { slug, cmd } => cmd.run(args, projects.slug(&slug)).await,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectCommand {
    /// Get this project.
    Get,
    /// Manage builds.
    Build {
        #[command(subcommand)]
        cmd: ProjectBuildCommand,
    },
    /// Manage build templates.
    BuildTemplate {
        #[command(subcommand)]
        cmd: ProjectBuildTemplatesCommand,
    },
}
impl ProjectCommand {
    pub async fn run(self, args: &Args, project: api::ProjectClient<'_>) -> Result<()> {
        match self {
            Self::Get => args
                .out
                .dump(project.get(api::ProjectGetRequest {}).await?.get()),
            Self::Build { cmd } => cmd.run(args, project.builds()).await,
            Self::BuildTemplate { cmd } => cmd.run(args, project.build_templates()).await,
        }
    }
}

// -- Build Templates -- //

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Build Template")]
struct BuildTemplateArgs {
    /// Builder used for the resulting builds, eg. "nix".
    #[arg(long, short, required = true)]
    pub kind: Option<Cow<'static, str>>,

    /// Builder expression.
    #[arg(long, short, required = true)]
    pub expr: Option<Cow<'static, str>>,

    /// Specify a tracked repository input:
    ///
    /// nixpkgs=github:NixOS/nixpkgs
    #[arg(
        short = 'r', long,
        value_parser = parse_key_value
    )]
    pub input_repo: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    // Specify a static input:
    //
    // nixpkgs=git+https://github.com/NixOS/nixpkgs?rev=cf05413ba4d3a4a26c08aca1fbf30ca9d7f94b29
    //
    // FIXME: Why does this get stuck as required!?
    //
    // #[arg(
    //     short = 'u', long, required = false,
    //     default_values = Vec::<&std::ffi::OsStr>::with_capacity(0),
    //     value_parser = parse_key_value
    // )]
    // pub input_url: BTreeMap<Cow<'static, str>, Cow<'static, str>>,
}
impl TryFrom<BuildTemplateArgs> for models::BuildTemplateSpec<'static> {
    type Error = miette::Report;
    fn try_from(args: BuildTemplateArgs) -> Result<Self> {
        // let input_url = (args.input_url.into_iter())
        //     .map(|(k, u)| (k, models::BuildTemplateInputSource::Url(u)));
        let input_repo = (args.input_repo.into_iter()).map(|(k, spec)| {
            spec.split_once(":")
                .ok_or(miette!("Repo spec not in 'kind:ident' format: {spec}"))
                .map(|(kind, ident)| {
                    (
                        k,
                        models::BuildTemplateInputSource::Repository(models::RepositorySpec {
                            kind: kind.to_string().into(),
                            ident: ident.to_string().into(),
                        }),
                    )
                })
        });
        Ok(models::BuildTemplateSpec {
            kind: args.kind.unwrap_or_default(),
            expr: args.expr.unwrap_or_default(),
            inputs: input_repo.collect::<Result<_>>()?,
        })
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectBuildTemplatesCommand {
    /// List build templates.
    List,
    /// Manage a build template by slug.
    Slug {
        slug: String,
        #[command(subcommand)]
        cmd: ProjectBuildTemplateCommand,
    },
}
impl ProjectBuildTemplatesCommand {
    pub async fn run(
        self,
        args: &Args,
        templates: api::ProjectBuildTemplatesClient<'_>,
    ) -> Result<()> {
        match self {
            Self::List => args.out.dump(
                templates
                    .list(api::BuildTemplatesListRequest {})
                    .await?
                    .get(),
            ),
            Self::Slug { slug, cmd } => cmd.run(args, templates.slug(&slug)).await,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum ProjectBuildTemplateCommand {
    /// Create or update a build template.
    Set {
        #[command(flatten)]
        t: BuildTemplateArgs,
    },
}
impl ProjectBuildTemplateCommand {
    pub async fn run(
        self,
        args: &Args,
        template: api::ProjectBuildTemplateClient<'_>,
    ) -> Result<()> {
        match self {
            Self::Set { t } => args.out.dump(
                template
                    .set(api::BuildTemplateSetRequest {
                        template: t.try_into()?,
                    })
                    .await?
                    .get(),
            ),
        }
    }
}

// -- Builds -- //

#[derive(Debug, Clone, Parser)]
#[command(next_help_heading = "Build")]
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
    pub async fn run(self, args: &Args, builds: api::ProjectBuildsClient<'_>) -> Result<()> {
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
    Events {
        /// Include "Progress" events.
        /// Disabled by default as they make the output very long and hard-to-read.
        #[arg(long, short, default_value = "false")]
        progress: bool,

        id: Uuid,
    },
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
            Self::Events { progress, id } => {
                let rsp_ = builds.id(id).events(api::BuildEventsRequest {}).await?;
                let url = rsp_.get().url.clone();
                debug!(%url, "Fetching events...");
                let events = Self::load_events(&url)
                    .map(|it| {
                        it.filter(|e| e.as_ref().is_ok_and(|e| progress || !e.event.is_progress()))
                    })
                    .with_context(|| url.to_string())?
                    .collect::<Result<Vec<BuildEvent<'static>>>>()?;
                args.out.with(|r| output::event_tree(r), &events)
            }
        }
    }
    fn load_events(url: &Url) -> Result<impl Iterator<Item = Result<BuildEvent<'static>>>> {
        match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| miette!("URL has bad file path"))?;
                std::fs::File::open(&path)
                    .into_diagnostic()
                    .map(Self::read_events)
                    .with_context(|| path.to_string_lossy().to_string())
            }
            _ => Err(miette!("Don't know how to fetch events from: {url}")),
        }
    }
    fn read_events(r: impl Read) -> impl Iterator<Item = Result<BuildEvent<'static>>> {
        let mut r = minicbor_io::Reader::new(zstd::stream::Decoder::new(r).unwrap());
        std::iter::from_fn(move || {
            r.read::<BuildEvent>()
                .map(|o| o.map(|e| e.into_owned()))
                .into_diagnostic()
                .transpose()
        })
    }
}

async fn run(args: AllArgs) -> Result<()> {
    owo_colors::set_override(args.args.out.stdout_color());
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
