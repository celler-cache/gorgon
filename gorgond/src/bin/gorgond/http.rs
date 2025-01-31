// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use anyhow::Result;
use axum::{Json, Router, routing::get};
use diesel_async::{AsyncPgConnection, pooled_connection::deadpool::Pool};
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

pub async fn root() -> Json<gorgond::RootResponse> {
    Json(gorgond::RootResponse {})
}
