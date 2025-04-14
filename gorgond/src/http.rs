// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::collections::BTreeMap;

use axum::{
    extract::FromRequest,
    response::IntoResponse,
    routing::{get, put},
    Router,
};
use axum_extra::response::ErasedJson;
use chrono::prelude::*;
use diesel_async::{pooled_connection::deadpool::Pool, AsyncPgConnection};
use gorgond_client::{
    api::{self, ApiError},
    models, Uuid, Yoke, Yokeable,
};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, instrument};

use crate::Args;

pub type Result<T, E = ApiErrorAdapter> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error(transparent)]
#[diagnostic(transparent)]
pub struct ApiErrorAdapter(#[from] api::ApiError);
impl IntoResponse for ApiErrorAdapter {
    fn into_response(self) -> axum::response::Response {
        (
            self.0.status(),
            ErasedJson::pretty(api::ApiErrorWrapper::from(self.0)),
        )
            .into_response()
    }
}

pub struct JsonRequest<T: for<'y> Yokeable<'y>>(pub Yoke<T>);
impl<T: for<'y> Yokeable<'y>> std::fmt::Debug for JsonRequest<T>
where
    for<'y> <T as Yokeable<'y>>::Output: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("JsonRequest").field(&self.0).finish()
    }
}

impl<T: for<'y> Yokeable<'y>, S: Send + Sync> FromRequest<S> for JsonRequest<T>
where
    for<'y> <T as Yokeable<'y>>::Output: Deserialize<'y>,
{
    type Rejection = ApiErrorAdapter;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(Yoke::try_attach_to_cart(
            String::from_request(req, state)
                .await
                .map_err(|err| ApiError::BadRequestBody {
                    error: err.to_string(),
                })?,
            |src| {
                serde_json::from_str(src).map_err(|err| ApiError::BadRequestJson {
                    error: err.to_string(),
                })
            },
        )?))
    }
}

#[derive(Debug)]
pub struct JsonResponse<T>(pub T);
impl<T: Serialize> IntoResponse for JsonResponse<T> {
    fn into_response(self) -> axum::response::Response {
        ErasedJson::pretty(self.0).into_response()
    }
}

#[instrument(level = "INFO", skip_all)]
pub async fn run(args: &Args, pool: Pool<AsyncPgConnection>) -> crate::Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/builds/allocate", put(builds_allocate))
        .fallback(error_not_found)
        .method_not_allowed_fallback(error_method_not_allowed)
        .with_state(pool);

    debug!(http_addr = args.http_addr, "Binding HTTP listener...");
    let listener = TcpListener::bind(&args.http_addr).await?;
    info!("Running on: http://{}/", listener.local_addr()?);
    Ok(axum::serve(listener, app).await?)
}

pub async fn error_not_found(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::NoSuchRoute {
        route: req.uri().path().to_string(),
    }))
}

pub async fn error_method_not_allowed(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::MethodNotAllowed {
        method: req.method().to_string(),
        route: req.uri().path().to_string(),
    }))
}

pub async fn root() -> JsonResponse<api::RootResponse> {
    JsonResponse(api::RootResponse {})
}

pub async fn builds_allocate(
    JsonRequest(_req): JsonRequest<api::BuildAllocateRequest<'static>>,
) -> Result<JsonResponse<api::BuildAllocateResponse<'static>>> {
    Ok(JsonResponse(api::BuildAllocateResponse {
        build: models::Build {
            id: Uuid::now_v7(),
            project_id: Uuid::nil(),
            kind: "nix".into(),
            inputs: BTreeMap::from_iter([(
                "nixpkgs".into(),
                concat!(
                    "git+https://github.com/NixOS/nixpkgs",
                    "?rev=b75693fb46bfaf09e662d09ec076c5a162efa9f6"
                )
                .into(),
            )]),
            source: concat!(
                "git+https://codeberg.org/gorgon/test-timestamp",
                "?rev=73d6baaddf2a8972e4a24c1867a89874efee8a24"
            )
            .into(),
            expr: r#"{ source, nixpkgs }: (import nixpkgs {}).callPackage source {}"#.into(),
            worker_id: None,
            result: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            allocated_at: None,
            heartbeat_at: None,
            started_at: None,
            ended_at: None,
        },
    }))
}
