// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::fmt::Debug;

use axum::{
    extract::{FromRequest, Path, State},
    response::IntoResponse,
    routing::{get, post, put},
    Router,
};
use axum_extra::response::ErasedJson;
use chrono::prelude::*;
use diesel::prelude::*;
use diesel_async::{
    pooled_connection::deadpool::Pool, AsyncConnection, AsyncPgConnection, RunQueryDsl,
};
use gorgond_client::{
    api::{self, ApiError},
    Uuid, Yoke, Yokeable,
};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, instrument, warn};

use crate::{models, schema, Args};

pub type Result<T, E = ApiErrorAdapter> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error(transparent)]
#[diagnostic(transparent)]
pub struct ApiErrorAdapter(ApiError);
impl IntoResponse for ApiErrorAdapter {
    fn into_response(self) -> axum::response::Response {
        (
            self.0.status(),
            ErasedJson::pretty(api::ApiErrorWrapper::from(self.0)),
        )
            .into_response()
    }
}
impl<E: Into<ApiError>> From<E> for ApiErrorAdapter {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[derive(Debug)]
pub struct JsonRequest<T: for<'y> Yokeable<'y, Output: Debug + Deserialize<'y>>>(pub Yoke<T>);

impl<T: for<'y> Yokeable<'y, Output: Debug + Deserialize<'y>>, S: Send + Sync> FromRequest<S>
    for JsonRequest<T>
{
    type Rejection = ApiErrorAdapter;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(Yoke::try_attach_to_cart(
            String::from_request(req, state)
                .await
                .map_err(|err| ApiError::BadRequestBody {
                    error: ApiError::wrap_err(&err),
                })?,
            |src| {
                serde_json::from_str(src).map_err(|err| ApiError::BadRequestJson {
                    error: ApiError::wrap_err(&err),
                })
            },
        )?))
    }
}

#[derive(Debug)]
pub struct JsonResponse<T: Debug + Serialize>(pub T);
impl<T: Debug + Serialize> IntoResponse for JsonResponse<T> {
    fn into_response(self) -> axum::response::Response {
        ErasedJson::pretty(self.0).into_response()
    }
}

#[derive(Clone)]
struct S {
    pool: Pool<AsyncPgConnection>,
}
macro_rules! transaction {
    ($state:ident, $block:expr) => {
        $state
            .pool
            .get()
            .await?
            .transaction(move |conn| Box::pin(async move { ($block)(conn).await }))
    };
}

#[instrument(level = "INFO", skip_all)]
pub async fn run(args: &Args, pool: Pool<AsyncPgConnection>) -> crate::Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/projects", post(project_create))
        .route("/projects/{project}", get(project_get))
        .route("/projects/{project}/builds", post(project_build_create))
        .route(
            "/workers/{worker}/internal/allocate-build",
            put(worker_internal_allocate_build),
        )
        .fallback(error_not_found)
        .method_not_allowed_fallback(error_method_not_allowed)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(S { pool });

    debug!(http_addr = args.http_addr, "Binding HTTP listener...");
    let listener = TcpListener::bind(&args.http_addr).await?;
    info!("Running on: http://{}/", listener.local_addr()?);
    Ok(axum::serve(listener, app).await?)
}

async fn error_not_found(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::NoSuchRoute {
        method: req.method().to_string(),
        route: req.uri().path().to_string(),
    }))
}

async fn error_method_not_allowed(req: axum::extract::Request) -> Result<()> {
    Err(ApiErrorAdapter::from(ApiError::MethodNotAllowed {
        method: req.method().to_string(),
        route: req.uri().path().to_string(),
    }))
}

async fn root() -> JsonResponse<api::RootResponse> {
    JsonResponse(api::RootResponse {})
}

async fn project_create(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::ProjectCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectCreateResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let project = models::Project::from_spec(req.get().project.to_owned(), Uuid::now_v7(), now);
        let project = {
            use crate::schema::projects::dsl::*;
            diesel::insert_into(projects)
                .values(&project)
                .get_result::<models::Project>(conn)
                .await?
        };
        Ok(JsonResponse(api::ProjectCreateResponse {
            project: project.to_client(),
        }))
    })
    .await
}

async fn project_get(
    State(state): State<S>,
    Path(project): Path<String>,
) -> Result<JsonResponse<api::ProjectGetResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        Ok(JsonResponse(api::ProjectGetResponse {
            project: models::Project::get(conn, &project).await?.to_client(),
        }))
    })
    .await
}

async fn project_build_create(
    State(state): State<S>,
    Path(project): Path<String>,
    JsonRequest(req): JsonRequest<api::ProjectBuildCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectBuildCreateResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        let now = Utc::now();
        let project_id = models::Project::get_id(conn, &project).await?;

        let (build, inputs) =
            models::Build::from_spec(req.get().build.to_owned(), Uuid::now_v7(), project_id, now);
        let build = {
            use schema::builds;
            diesel::insert_into(builds::table)
                .values(&build)
                .get_result::<models::Build>(conn)
                .await?
        };
        let inputs = {
            use schema::build_inputs;
            diesel::insert_into(build_inputs::table)
                .values(inputs.collect::<Vec<_>>())
                .get_results::<models::BuildInput>(conn)
                .await?
        };
        Ok(JsonResponse(api::ProjectBuildCreateResponse {
            build: build.to_client(inputs),
        }))
    })
    .await
}

async fn worker_internal_allocate_build(
    State(state): State<S>,
    Path(worker): Path<String>,
    JsonRequest(_req): JsonRequest<api::WorkerInternalAllocateBuildRequest>,
) -> Result<JsonResponse<api::WorkerInternalAllocateBuildResponse<'static>>> {
    transaction!(state, async |conn: &mut _| {
        use schema::{build_inputs, builds, workers};
        let now = Utc::now();

        // Resolve the worker slug to an ID, or create it if it doesn't exist.
        let worker_id = {
            let default_worker = models::Worker {
                id: Uuid::now_v7(),
                slug: worker.clone(),
                name: worker.clone(),
                created_at: now,
                updated_at: now,
            };
            let worker = diesel::insert_into(workers::table)
                .values(&default_worker)
                .returning(models::Worker::as_returning())
                .on_conflict(workers::slug)
                .do_update()
                .set(&default_worker)
                .get_result::<models::Worker>(conn)
                .await?;
            if worker.id == default_worker.id {
                info!("New worker node registered!");
            }
            worker.id
        };

        // If worker has an unfinished task, remind it to get back on it.
        // This is also our idempotency mechanism for this endpoint.
        let build = if let Some(build) = builds::table
            .limit(1)
            .filter(builds::worker_id.eq(worker_id))
            .get_result::<models::Build>(conn)
            .await
            .optional()?
        {
            info!("Worker has an unfinished build, reminding it");
            build
        } else {
            // If not, try to find an unallocated task...
            let build_id = if let Some(build_id) = builds::table
                .filter(builds::worker_id.is_null())
                .select(builds::id)
                .limit(1)
                .order_by(builds::created_at.asc())
                .for_no_key_update() // No race conditions pls.
                .get_result::<Uuid>(conn)
                .await
                .optional()?
            {
                info!("Found an unallocated build, allocating it");
                build_id
            } else {
                warn!("No work available!");
                return Err(ApiError::NoWorkAvailable.into());
            };

            // ...and allocate it to this worker.
            debug!("Allocating build...");
            diesel::update(builds::table)
                .filter(builds::id.eq(build_id))
                .set(builds::worker_id.eq(worker_id))
                .returning(models::Build::as_returning())
                .get_result(conn)
                .await?
        };

        let inputs = build_inputs::table
            .filter(build_inputs::build_id.eq(build.id))
            .get_results::<models::BuildInput>(conn)
            .await?;
        Ok(JsonResponse(api::WorkerInternalAllocateBuildResponse {
            build: build.to_client(inputs),
        }))
    })
    .await
}
