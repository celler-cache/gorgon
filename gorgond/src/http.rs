// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::Result;
use axum::{routing::get, Json, Router};
use diesel_async::{pooled_connection::deadpool::Pool, AsyncPgConnection};
use tokio::net::TcpListener;
use tracing::{debug, info, instrument};

use crate::Args;

#[instrument(level = "INFO", skip_all)]
pub async fn run(args: &Args, pool: Pool<AsyncPgConnection>) -> Result<()> {
    let app = Router::new().route("/", get(root)).with_state(pool);

    debug!(http_addr = args.http_addr, "Binding HTTP listener...");
    let listener = TcpListener::bind(&args.http_addr).await?;
    info!("Running on: http://{}/", listener.local_addr()?);
    Ok(axum::serve(listener, app).await?)
}

pub async fn root() -> Json<gorgond_client::RootResponse> {
    Json(gorgond_client::RootResponse {})
}
