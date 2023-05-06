// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use askama::Template;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use tracing::info;

pub type Result<T, E = Error> = std::result::Result<T, E>;

// Error wrapper. Can be returned from a handler and turned into a 500 Internal Server Error.
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

#[derive(Debug, Parser)]
struct Args {
    /// Path to the nix database.
    #[arg(long, short, default_value = "/nix/var/nix/db/db.sqlite")]
    db_path: std::path::PathBuf,

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args);

    let app = axum::Router::new()
        .route("/", get(get_index))
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
