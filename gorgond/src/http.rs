// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use axum::{
    extract::{rejection::JsonRejection, FromRequest},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Router,
};
use axum_extra::response::ErasedJson;
use diesel_async::{pooled_connection::deadpool::Pool, AsyncPgConnection};
use gorgond_client::{builds, ApiError, ApiErrorKind, RootResponse, Url, Uuid};
use miette::Diagnostic;
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::{debug, info, instrument};

use crate::Args;

pub type Result<T, E = ApiErrorAdapter> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error(transparent)]
#[diagnostic(transparent)]
pub struct ApiErrorAdapter(#[from] ApiError);

impl From<JsonRejection> for ApiErrorAdapter {
    fn from(rej: JsonRejection) -> Self {
        let (status, body_text) = (rej.status(), rej.body_text());
        Self(
            ApiError::new_err(ApiErrorKind::RequestRejected, rej)
                .with_status(status)
                .with_detail(body_text),
        )
    }
}

impl IntoResponse for ApiErrorAdapter {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            ErasedJson::pretty(self.0),
        )
            .into_response()
    }
}

#[derive(Debug, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiErrorAdapter))]
pub struct ApiJson<T>(pub T);

impl<T: Serialize> IntoResponse for ApiJson<T> {
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
    Err(ApiErrorAdapter::from(ApiError::from(
        ApiErrorKind::NoSuchRoute {
            route: req.uri().path().to_string(),
        },
    )))
}

pub async fn error_method_not_allowed(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::from(
        ApiErrorKind::MethodNotAllowed {
            method: req.method().to_string(),
            route: req.uri().path().to_string(),
        },
    )))
}

pub async fn root() -> ApiJson<RootResponse> {
    ApiJson(RootResponse {})
}

pub async fn builds_allocate(
    ApiJson(_req): ApiJson<builds::BuildAllocateRequest<'static>>,
) -> Result<ErasedJson> {
    let rsp = builds::BuildAllocateResponse {
        build: builds::Build {
            id: Uuid::now_v7(),
            kind: "nix".into(),
            source: parse_url("git://codeberg.org/gorgon/test-hello")?,
        },
    };
    Ok(ErasedJson::pretty(rsp))
}

fn parse_url<S: AsRef<str> + ToString>(s: S) -> Result<Url, ApiError> {
    Url::parse(s.as_ref())
        .map_err(|err| ApiError::new_err(ApiErrorKind::BadDataUrl { url: s.to_string() }, err))
}
