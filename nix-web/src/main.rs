// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderName, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use clap::Parser;
use hyper::header;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tracing::info;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error wrapper. Can be returned from a handler and turned into a 500 Internal Server Error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("nix command exited with code {0}")]
    NixCommandExitCode(i32),

    #[error(transparent)]
    Askama(#[from] askama::Error),
    #[error(transparent)]
    JSON(#[from] serde_json::Error),
    #[error(transparent)]
    Hyper(#[from] hyper::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", self)).into_response()
    }
}

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Path to the nix database.
    #[arg(long, default_value = "/nix/var/nix/db/db.sqlite")]
    nix_db_path: PathBuf,

    /// Path to the nix store.
    #[arg(long, default_value = "/nix/store")]
    nix_store_path: PathBuf,

    /// Increase log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log level.
    #[arg(long, short, action=clap::ArgAction::Count)]
    quiet: u8,

    /// Listen address.
    #[arg(long, short = 'L', default_value = "[::]:8000")]
    http_listen: std::net::SocketAddr,
}

fn init_logging(args: &Args) {
    tracing_subscriber::fmt()
        .with_max_level(match 2 + args.verbose - args.quiet {
            0 => tracing::Level::ERROR,
            1 => tracing::Level::WARN,
            2 => tracing::Level::INFO,
            3 => tracing::Level::DEBUG,
            4.. => tracing::Level::TRACE,
        })
        .init();
}

#[derive(Debug, Clone)]
struct AppState {
    args: Args,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args);
    let state = Arc::new(AppState { args: args.clone() });

    let app = axum::Router::new()
        .route("/", get(get_index))
        .route("/style.css", get(get_style_css))
        .route("/goto", get(get_goto))
        .route("/nix/store/*path", get(get_derivation))
        .with_state(state)
        .layer(tower_http::trace::TraceLayer::new_for_http());

    info!(addr = ?&args.http_listen, "Listening!");
    axum::Server::bind(&args.http_listen)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    blah: &'a str,
}

async fn get_index() -> Result<IndexTemplate<'static>> {
    Ok(IndexTemplate { blah: "im gay" })
}

async fn get_style_css() -> ([(HeaderName, &'static str); 1], &'static str) {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/style.css"),
    )
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct GotoQuery {
    path: String,
}

async fn get_goto(query: Query<GotoQuery>) -> Response {
    Redirect::to(&query.path).into_response()
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathInfo {
    pub ca: String,
    #[serde(rename = "narHash")]
    pub nar_hash: String,
    #[serde(rename = "narSize")]
    pub nar_size: usize,
    pub path: String,
    pub references: Vec<String>,
    #[serde(rename = "registrationTime")]
    pub registration_time: u64, // TODO: Parse timestamp.
}

#[derive(Template)]
#[template(path = "derivation.html")]
struct DerivationTemplate<'a> {
    roots: &'a Vec<&'a PathInfo>,
    root_tmpls: Vec<DerivationItemTemplate<'a>>,
}

#[derive(Template)]
#[template(path = "_derivation_item.html")]
struct DerivationItemTemplate<'a> {
    path_info: &'a PathInfo,
    ref_tmpls: Vec<DerivationItemTemplate<'a>>,
    is_seen_before: bool,
}

impl<'a> DerivationItemTemplate<'a> {
    fn build(
        path_infos: &'a HashMap<String, PathInfo>,
        path: &'a str,
        mut seen: HashSet<&'a str>,
    ) -> (Self, HashSet<&'a str>) {
        let path_info = &path_infos[path];

        let is_seen_before = seen.contains(path);
        let ref_tmpls = if !is_seen_before {
            seen.insert(path);
            let mut ref_tmpls = Vec::with_capacity(path_info.references.len());
            for rf in path_info.references.iter() {
                let (tmpl, seen_) = Self::build(path_infos, rf, seen);
                ref_tmpls.push(tmpl);
                seen = seen_;
            }
            ref_tmpls
        } else {
            Vec::new()
        };
        (
            Self {
                path_info,
                ref_tmpls,
                is_seen_before,
            },
            seen,
        )
    }
}

async fn get_derivation(
    State(state): State<Arc<AppState>>,
    Path(store_path): Path<String>,
) -> Result<Html<String>> {
    let output = Command::new("nix")
        .arg("--experimental-features")
        .arg("nix-command")
        .arg("path-info")
        .arg("--offline")
        .arg("--json")
        .arg("-r")
        .arg("--derivation")
        .arg(state.args.nix_store_path.join(store_path))
        .output()
        .await?;
    if !output.status.success() {
        return Err(Error::NixCommandExitCode(output.status.code().unwrap_or(0)));
    }
    let path_infos: HashMap<String, PathInfo> =
        serde_json::from_slice::<Vec<PathInfo>>(&output.stdout)?
            .iter()
            .map(|pi| (pi.path.clone(), pi.clone()))
            .collect();
    let roots = find_tree_roots(&path_infos);
    Ok(Html(
        DerivationTemplate {
            roots: &roots,
            root_tmpls: roots
                .iter()
                .map(|root| {
                    DerivationItemTemplate::build(&path_infos, &root.path, HashSet::new()).0
                })
                .collect(),
        }
        .render()?,
    ))
}

fn find_tree_roots<'a>(nodes: &'a HashMap<String, PathInfo>) -> Vec<&'a PathInfo> {
    let mut root_set = HashSet::<&'a str>::from_iter(nodes.keys().map(String::as_str));
    for (_, n) in nodes {
        for r in &n.references {
            root_set.remove(r.as_str());
        }
    }
    root_set.iter().map(|&path| &nodes[path]).collect()
}
