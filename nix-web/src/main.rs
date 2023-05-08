// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error wrapper. Can be returned from a handler and turned into a 500 Internal Server Error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Hyper(#[from] hyper::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", self)).into_response()
    }
}

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Path to the nix database.
    #[arg(long, short = 'D', default_value = "/nix/var/nix/db/db.sqlite")]
    nix_db_path: PathBuf,

    /// Path to the nix store.
    #[arg(long, short = 'S', default_value = "/nix/store")]
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
        .route("/nix/store/:id", get(get_derivation))
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

#[derive(Template)]
#[template(path = "derivation.html")]
struct DerivationTemplate {
    path: PathBuf,
    id: String,
}

async fn get_derivation<'a>(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<DerivationTemplate> {
    let path = state.args.nix_store_path.join(&id);
    Ok(DerivationTemplate { path, id })
}
