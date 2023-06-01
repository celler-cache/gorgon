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
use tokio::{process::Command, task::JoinSet};
use tracing::{debug, debug_span, info, warn};

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
    pub path: String,
    // Only present (as the only item alongside path) for invalid paths.
    #[serde(default = "PathInfo::default_valid")]
    pub valid: bool,

    pub deriver: Option<String>,
    pub ca: Option<String>,
    #[serde(rename = "narHash")]
    pub nar_hash: Option<String>,
    #[serde(rename = "narSize")]
    pub nar_size: Option<usize>,
    pub references: Option<Vec<String>>,
    #[serde(rename = "registrationTime")]
    pub registration_time: Option<u64>, // TODO: Parse timestamp.
    pub signatures: Option<Vec<String>>,
}

impl PathInfo {
    fn default_valid() -> bool {
        true
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum DerivationStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "wait")]
    Wait,
    #[serde(rename = "succ")]
    Success,
    #[serde(rename = "fail")]
    Failure,
}

impl Default for DerivationStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

impl std::fmt::Display for DerivationStatus {
    fn fmt<'w>(&self, w: &mut std::fmt::Formatter<'w>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(w, "unknown"),
            Self::Wait => write!(w, "wait"),
            Self::Success => write!(w, "succ"),
            Self::Failure => write!(w, "fail"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Derivation {
    #[serde(default)]
    pub path: String, // Filled in from the map key.
    #[serde(default)]
    pub status: DerivationStatus,

    // From `nix show-derivation`.
    pub args: Vec<String>,
    pub builder: String,
    pub env: HashMap<String, String>,
    #[serde(rename = "inputDrvs")]
    pub input_drvs: HashMap<String, Vec<String>>,
    #[serde(rename = "inputSrcs")]
    pub input_srcs: Vec<String>,
    pub outputs: HashMap<String, DerivationOutput>,
    pub system: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DerivationOutput {
    pub path: String,
}

#[derive(Template)]
#[template(path = "derivation.html")]
struct DerivationTemplate<'a> {
    roots: &'a Vec<String>,
    root_tmpls: Vec<DerivationItemTemplate<'a>>,
}

#[derive(Template)]
#[template(path = "_derivation_item.html")]
struct DerivationItemTemplate<'a> {
    drv: &'a Derivation,
    ref_tmpls: Vec<DerivationItemTemplate<'a>>,
    is_seen_before: bool,
}

impl<'a> DerivationItemTemplate<'a> {
    // Recursively builds a batch of DerivationItemTemplates, shallow-first.
    fn build_batch<P: Iterator<Item = &'a String>>(
        drvs: &'a HashMap<String, Derivation>,
        paths: P,
        mut seen: HashSet<&'a str>,
    ) -> (Vec<Self>, HashSet<&'a str>) {
        let mut batch = Vec::new();

        // Fill the batch and flag each derivation as seen, shallow-first.
        for path in paths {
            let drv = &drvs[path];
            let is_seen_before = seen.contains(&path.as_str());
            if !is_seen_before {
                seen.insert(path);
            }
            batch.push(Self {
                drv,
                is_seen_before,
                ref_tmpls: Vec::with_capacity(drv.input_drvs.len()),
            });
        }

        // Fill in references for derivations we haven't seen before.
        for tmpl in batch.iter_mut().filter(|t| !t.is_seen_before) {
            let (tmpls, seen_) = Self::build_batch(drvs, tmpl.drv.input_drvs.keys(), seen);
            tmpl.ref_tmpls = tmpls;
            seen = seen_;
        }
        (batch, seen)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub enum GetDerivationPage {
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "log")]
    Log,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct GetDerivationQuery {
    #[serde(rename = "p")]
    page: Option<GetDerivationPage>,
}

async fn get_derivation(
    State(state): State<Arc<AppState>>,
    Path(store_path): Path<String>,
    Query(query): Query<GetDerivationQuery>,
) -> Result<Response> {
    match query.page.unwrap_or(GetDerivationPage::Info) {
        GetDerivationPage::Info => Ok(get_derivation_info(State(state), Path(store_path))
            .await?
            .into_response()),
        GetDerivationPage::Log => Ok(get_derivation_log(State(state), Path(store_path))
            .await?
            .into_response()),
    }
}

async fn get_derivation_info(
    State(state): State<Arc<AppState>>,
    Path(store_path): Path<String>,
) -> Result<Html<String>> {
    // Collect info from nix.
    let (drv_cmd, out_pi_cmd) = {
        let path = state.args.nix_store_path.join(&store_path);
        let drv_cmd = Command::new("nix")
            .arg("--experimental-features")
            .arg("nix-command")
            .arg("show-derivation")
            .arg("--derivation")
            .arg("-r")
            .arg(&path)
            .output();
        let out_pi_cmd = Command::new("nix")
            .arg("--experimental-features")
            .arg("nix-command")
            .arg("path-info")
            .arg("--offline")
            .arg("--json")
            .arg("-r")
            .arg(&path)
            .output();
        (drv_cmd.await?, out_pi_cmd.await?)
    };
    // The only one that has to succeed is the `show-derivation` call.
    if !drv_cmd.status.success() {
        return Err(Error::NixCommandExitCode(
            drv_cmd.status.code().unwrap_or(0),
        ));
    }

    let out_pis: HashMap<String, PathInfo> = if out_pi_cmd.status.success() {
        serde_json::from_slice::<Vec<PathInfo>>(&out_pi_cmd.stdout)?
            .iter()
            .filter(|pi| pi.valid)
            .map(|pi| (pi.path.clone(), pi.clone()))
            .collect()
    } else {
        HashMap::new()
    };

    // Aggregate it all into Derivations.
    let drvs: HashMap<String, Derivation> = {
        let span = debug_span!("drvs");
        let _enter = span.enter();

        let mut drvs: HashMap<String, Derivation> = serde_json::from_slice(&drv_cmd.stdout)?;
        let mut need_pathinfo_check = HashSet::<&String>::new();
        for (path, drv) in drvs.iter_mut() {
            let span = debug_span!("drv", path);
            let _enter = span.enter();

            drv.path = path.clone();

            // If the initial `nix path-info` has one of our outputs, it's a success.
            drv.status = if drv
                .outputs
                .iter()
                .any(|(_, DerivationOutput { path })| out_pis.contains_key(path))
            {
                // There's an output; it's a success.
                debug!(
                    path = drv.path,
                    "confirmed by primary `nix path-info` -- marking as success"
                );
                DerivationStatus::Success
            } else {
                // The initial `nix path-info` didn't include any outputs - this may mean
                // that it failed, that it's still building, or just that it's a build
                // dependency that isn't referenced by the final product.
                //
                // The only way to find out is a second round of `nix path-info`.
                need_pathinfo_check.insert(&path);
                DerivationStatus::Unknown
            };
        }

        // Run a second round of `nix path-info` checks on outputs we're unsure about.
        if need_pathinfo_check.len() > 0 {
            let span = debug_span!("pathinfo2");
            let _enter = span.enter();
            debug!(
                count = need_pathinfo_check.len(),
                "running secondary `nix path-info`"
            );

            let pi_cmd2 = Command::new("nix")
                .arg("--experimental-features")
                .arg("nix-command")
                .arg("path-info")
                .arg("--json")
                .args(need_pathinfo_check)
                .output()
                .await?;
            if !pi_cmd2.status.success() {
                warn!(
                    "second round of `nix path-info` failed with status {:?}",
                    pi_cmd2.status.code(),
                );
            } else {
                for pi in serde_json::from_slice::<Vec<PathInfo>>(&pi_cmd2.stdout)?
                    .iter()
                    .filter(|pi| pi.valid)
                {
                    if let Some(drv) = drvs.get_mut(&pi.path) {
                        debug!(path=?pi.path, "confirmed by secondary `nix path-info` -- marking as success");
                        drv.status = DerivationStatus::Success
                    }
                    if let Some(deriver) = &pi.deriver {
                        if let Some(drv) = drvs.get_mut(deriver) {
                            debug!(path=?pi.path, "confirmed by secondary `nix path-info` -- marking as success");
                            drv.status = DerivationStatus::Success
                        }
                    }
                }
            }
        }

        // Finally, disambiguate failed builds (no outputs, has log) from not yet built.
        {
            let mut log_check_tasks = JoinSet::new();
            for path in drvs
                .iter()
                .filter(|(_, drv)| drv.status == DerivationStatus::Unknown)
                .map(|(path, _)| path.clone())
            {
                let mut process = Command::new("nix")
                    .arg("--experimental-features")
                    .arg("nix-command")
                    .arg("log")
                    .arg(path.clone())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;
                log_check_tasks.spawn(async move { (path, process.wait().await) });
            }
            while let Some(res) = log_check_tasks.join_next().await {
                let (path, status) = res.unwrap();
                if let Some(drv) = drvs.get_mut(&path) {
                    drv.status = if status?.success() {
                        DerivationStatus::Failure
                    } else {
                        DerivationStatus::Wait
                    };
                }
            }
        }

        drvs
    };

    let roots = find_tree_roots(&drvs);
    Ok(Html(
        DerivationTemplate {
            roots: &roots,
            root_tmpls: DerivationItemTemplate::build_batch(&drvs, roots.iter(), HashSet::new()).0,
        }
        .render()?,
    ))
}

fn find_tree_roots<'a>(nodes: &'a HashMap<String, Derivation>) -> Vec<String> {
    let mut root_set = HashSet::<&'a str>::from_iter(nodes.keys().map(String::as_str));
    for (_, n) in nodes {
        for r in n.input_drvs.keys() {
            root_set.remove(r.as_str());
        }
    }
    root_set.iter().map(|&path| path.to_string()).collect()
}

async fn get_derivation_log(
    State(state): State<Arc<AppState>>,
    Path(store_path): Path<String>,
) -> Result<String> {
    let output = Command::new("nix")
        .arg("--experimental-features")
        .arg("nix-command")
        .arg("log")
        .arg(state.args.nix_store_path.join(store_path))
        .output()
        .await?;
    if !output.status.success() {
        return Err(Error::NixCommandExitCode(output.status.code().unwrap_or(0)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into())
}
