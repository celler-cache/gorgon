// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::entity;
use anyhow::Context;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use sea_orm::{prelude::*, QueryOrder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type Result<T, E = AppError> = std::result::Result<T, E>;

pub struct AppError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct JsonError {
            pub error: String,
            pub cause: Vec<String>,
        }

        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: format!("{}", self.0),
                cause: self.0.chain().map(|err| format!("{}", err)).collect(),
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AppState {
    db: Arc<DatabaseConnection>,
}

pub(crate) fn new(db: Arc<DatabaseConnection>) -> Router<()> {
    Router::new()
        .route("/nix/store/:drv/builds", get(get_drv_builds))
        .with_state(Arc::new(AppState { db }))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

#[derive(Debug, Serialize, Deserialize)]
struct GetDrvBuildsResponse {
    pub builds: Vec<entity::build::Model>,
}

async fn get_drv_builds(
    State(state): State<Arc<AppState>>,
    Path(drv): Path<String>,
) -> Result<Json<GetDrvBuildsResponse>> {
    let builds = entity::build::Entity::find()
        .filter(entity::build::Column::Name.eq(format!("/nix/store/{}", drv)))
        .order_by_desc(entity::build::Column::Start)
        .all(&*state.db)
        .await
        .with_context(|| format!("Querying Builds for: /nix/store/{}", drv))?;
    Ok(Json(GetDrvBuildsResponse { builds }))
}
