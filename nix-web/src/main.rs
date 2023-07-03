// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub mod nix_cli;

use crate::nix_cli::{Derivation, DerivationOutput, DerivationStatus, PathInfo};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderName, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use clap::Parser;
use hyper::header;
use listenfd::ListenFd;
use serde::Deserialize;
use sqlx::{Connection, Row};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, debug_span, info, trace};

pub type Result<T, E = Error> = std::result::Result<T, E>;

// TODO: Is there a way to make this a Path or PathBuf?
const NIX_STORE_PATH: &str = "/nix/store";

/// Error wrapper. Can be returned from a handler and turned into a 500 Internal Server Error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("nix command exited with code {0}: {1}")]
    NixCommandError(i32, String),

    #[error(transparent)]
    Askama(#[from] askama::Error),
    #[error(transparent)]
    JSON(#[from] serde_json::Error),
    #[error(transparent)]
    Hyper(#[from] hyper::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
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
    #[arg(long, short = 'L', default_value = "[::]:8649")]
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
        .route("/static/*path", get(get_static))
        .route("/goto", get(get_goto))
        .route("/nix/store/*path", get(get_derivation))
        .with_state(state)
        .layer(tower_http::trace::TraceLayer::new_for_http());

    // Take an external FD if we were socket activated, else listen normally.
    let mut listen_fds = ListenFd::from_env();
    let srv = if listen_fds.len() > 0 {
        // This unwrap() can only fail if the FD has already been used.
        info!("Socket activated, using external FD 0...");
        axum::Server::from_tcp(listen_fds.take_tcp_listener(0)?.unwrap())?
    } else {
        debug!(addr = ?&args.http_listen, "Binding...");
        axum::Server::try_bind(&args.http_listen)?
    }
    .serve(app.into_make_service());
    info!(addr = ?srv.local_addr(), "Listening!");
    srv.await?;
    Ok(())
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    last_paths: Vec<String>,
}

async fn get_index(State(state): State<Arc<AppState>>) -> Result<IndexTemplate> {
    // Find the last registered paths in the store, sorted by most-recent-first.
    let last_path_paths: Vec<String> = {
        // Open the database as immutable, or we start fighting the daemon over the WAL.
        // This may cause problems if the database changes while the query is running.
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&state.args.nix_db_path)
                .immutable(true),
        )
        .await?;
        sqlx::query("SELECT path FROM ValidPaths ORDER BY registrationTime DESC LIMIT 10")
            .map(|row: sqlx::sqlite::SqliteRow| row.get(0))
            .fetch_all(&mut conn)
            .await?
    };

    Ok(IndexTemplate {
        last_paths: last_path_paths,
    })
}

async fn get_static(
    Path(path): Path<String>,
) -> (StatusCode, [(HeaderName, &'static str); 1], &'static str) {
    match path.as_str() {
        "style.css" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            include_str!("../static/style.css"),
        ),
        "bootstrap.css" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            include_str!("../../vendor/bootstrap/css/bootstrap.css"),
        ),
        "bootstrap.css.map" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            include_str!("../../vendor/bootstrap/css/bootstrap.css.map"),
        ),
        "bootstrap.js" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            include_str!("../../vendor/bootstrap/js/bootstrap.js"),
        ),
        "bootstrap.js.map" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            include_str!("../../vendor/bootstrap/js/bootstrap.js.map"),
        ),
        _ => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "not found",
        ),
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct GotoQuery {
    path: String,
}

async fn get_goto(query: Query<GotoQuery>) -> Response {
    Redirect::to(&query.path).into_response()
}

#[derive(Template)]
#[template(path = "derivation.html")]
struct DerivationTemplate<'a> {
    root: &'a Derivation,
    root_outputs: Vec<(&'a String, &'a String, Option<&'a PathInfo>)>,
    root_input_tmpls: Vec<DerivationItemTemplate<'a>>,
    root_env_html: Vec<(&'a String, String)>,
}

#[derive(Template)]
#[template(path = "_derivation_item.html")]
struct DerivationItemTemplate<'a> {
    drv: &'a Derivation,
    input_tmpls: Vec<DerivationItemTemplate<'a>>,
    is_seen_before: bool,
}

impl<'a> DerivationItemTemplate<'a> {
    // Recursively builds a batch of DerivationItemTemplates, shallow-first.
    fn build_batch<P: Iterator<Item = &'a String>>(
        drvs: &'a BTreeMap<String, Derivation>,
        out_pis: &'a BTreeMap<String, PathInfo>,
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
                input_tmpls: Vec::with_capacity(drv.input_drvs.len()),
                is_seen_before,
            });
        }

        // Fill in references for derivations we haven't seen before.
        for tmpl in batch.iter_mut().filter(|t| !t.is_seen_before) {
            let (tmpls, seen_) = Self::build_batch(drvs, out_pis, tmpl.drv.input_drvs.keys(), seen);
            tmpl.input_tmpls = tmpls;
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
    Path(store_path): Path<String>,
    Query(query): Query<GetDerivationQuery>,
) -> Result<Response> {
    match query.page.unwrap_or(GetDerivationPage::Info) {
        GetDerivationPage::Info => Ok(get_derivation_info(Path(store_path)).await?.into_response()),
        GetDerivationPage::Log => Ok(get_derivation_log(Path(store_path)).await?.into_response()),
    }
}

async fn get_derivation_info(Path(store_path): Path<String>) -> Result<Html<String>> {
    // Collect info from nix.
    let (mut drvs, mut out_pis) = {
        let path_buf = PathBuf::from(NIX_STORE_PATH).join(&store_path);
        let path = path_buf.to_str().unwrap();
        let drvs = nix_cli::Derivation::get(["-r"], [path]);
        let out_pis = nix_cli::PathInfo::get(["-r", "--derivation"], [path]);
        (
            drvs.await?,
            match out_pis.await {
                Ok(v) => v,
                // If this is an invalid path, still render the page.
                Err(Error::NixCommandError(..)) => BTreeMap::new(),
                Err(e) => return Err(e),
            },
        )
    };

    // Correlate Derivations and PathInfos.
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
            trace!(
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
            DerivationStatus::Missing
        };
    }

    // Run a second round of `nix path-info` checks on outputs we're unsure about.
    if need_pathinfo_check.len() > 0 {
        let span = debug_span!("pathinfo2");
        let _enter = span.enter();
        trace!(
            count = need_pathinfo_check.len(),
            "running secondary `nix path-info`"
        );

        for (_, pi) in nix_cli::PathInfo::get(
            ["--"],
            need_pathinfo_check
                .drain()
                .map(|path| format!("{}^*", path)),
        )
        .await?
        {
            if let Some(drv) = drvs.get_mut(&pi.path) {
                trace!(path=?pi.path, "confirmed by secondary `nix path-info` -- marking as success");
                drv.status = DerivationStatus::Success
            }
            if let Some(deriver) = &pi.deriver {
                if let Some(drv) = drvs.get_mut(deriver) {
                    trace!(path=?pi.path, "confirmed by secondary `nix path-info` -- marking as success");
                    drv.status = DerivationStatus::Success
                }
            }
            out_pis.insert(pi.path.clone(), pi);
        }
    }

    let root = &drvs[find_tree_root(&drvs)];
    Ok(Html(
        DerivationTemplate {
            root,
            root_outputs: find_outputs(&out_pis, root),
            root_input_tmpls: DerivationItemTemplate::build_batch(
                &drvs,
                &out_pis,
                root.input_drvs.keys(),
                HashSet::new(),
            )
            .0,
            root_env_html: root
                .env
                .iter()
                .map(|(key, value)| (key, env_value_to_html(&value)))
                .collect(),
        }
        .render()?,
    ))
}

fn env_value_to_html(unescaped_value: &String) -> String {
    // HTML escape the value.
    let mut escaped = {
        use askama_escape::Escaper;
        let mut s = String::new();
        (askama_escape::Html {})
            .write_escaped(&mut s, &unescaped_value)
            .unwrap();
        s
    };

    // Link nix store paths.
    lazy_static::lazy_static! {
        static ref LINK_RE: regex::Regex = regex::Regex::new(
            r"/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[0-9a-zA-Z+._?=-]+").unwrap();
    };
    escaped = LINK_RE
        .replace_all(&escaped, "<a href=\"$0\">$0</a>")
        .into();

    // Wrap multi-line values in <pre> tags.
    if escaped.contains("\n") {
        format!("<pre style=\"white-space: pre-wrap;\">{}</pre>", escaped)
    } else {
        escaped
    }
}

fn find_outputs<'a>(
    out_pis: &'a BTreeMap<String, PathInfo>,
    drv: &'a Derivation,
) -> Vec<(&'a String, &'a String, Option<&'a PathInfo>)> {
    let mut outputs = drv
        .outputs
        .iter()
        .map(|(name, out)| (name, &out.path, out_pis.get(&out.path)))
        .collect::<Vec<_>>();
    outputs.sort_unstable_by_key(|v| v.0);
    outputs
}

fn find_tree_root<'a>(nodes: &'a BTreeMap<String, Derivation>) -> &'a String {
    let mut root_set = HashSet::<&'a String>::from_iter(nodes.keys());
    for (_, n) in nodes {
        for r in n.input_drvs.keys() {
            root_set.remove(r);
        }
    }
    root_set.iter().next().unwrap()
}

async fn get_derivation_log(Path(store_path): Path<String>) -> Result<String> {
    Ok(nix_cli::log(format!("{}/{}", NIX_STORE_PATH, store_path)).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_value_to_html_nop() {
        assert_eq!(env_value_to_html(&"hello world".into()), "hello world");
    }

    #[test]
    fn test_env_value_to_html_sneaky() {
        assert_eq!(
            env_value_to_html(&"<blink>hello world</blink>".into()),
            "&lt;blink&gt;hello world&lt;/blink&gt;"
        );
    }

    #[test]
    fn test_env_value_to_html_multiline() {
        assert_eq!(
            env_value_to_html(&"hello\nworld".into()),
            "<pre style=\"white-space: pre-wrap;\">hello\nworld</pre>"
        );
    }

    #[test]
    fn test_env_value_to_html_store_path() {
        assert_eq!(
            env_value_to_html(&"/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin".into()),
            "<a href=\"/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin\">/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin</a>"
        );
    }

    #[test]
    fn test_env_value_to_html_store_path_subpath() {
        assert_eq!(
            env_value_to_html(&"/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin/bin/sqlite3".into()),
            "<a href=\"/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin\">/nix/store/5k6jjkhrm9xkbnn8ih75gvkibgipai0p-sqlite-3.41.2-bin</a>/bin/sqlite3"
        );
    }
}
