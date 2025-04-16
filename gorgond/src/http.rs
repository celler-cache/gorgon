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
    pooled_connection::{
        deadpool::{Pool, PoolError as DeadpoolPoolError},
        PoolError,
    },
    AsyncConnection, AsyncPgConnection, RunQueryDsl,
};
use gorgond_client::{
    api::{self, ApiError},
    Uuid, Yoke, Yokeable,
};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, instrument};

use crate::{models, schema, Args};

pub type Result<T, E = ApiErrorAdapter> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error(transparent)]
#[diagnostic(transparent)]
pub struct ApiErrorAdapter(#[from] ApiError);
impl IntoResponse for ApiErrorAdapter {
    fn into_response(self) -> axum::response::Response {
        (
            self.0.status(),
            ErasedJson::pretty(api::ApiErrorWrapper::from(self.0)),
        )
            .into_response()
    }
}

impl From<diesel::result::Error> for ApiErrorAdapter {
    fn from(err: diesel::result::Error) -> Self {
        Self(match err {
            diesel::result::Error::NotFound => ApiError::DatabaseNotFound {
                error: err.to_string(),
            },
            diesel::result::Error::DatabaseError(kind, _) => match kind {
                diesel::result::DatabaseErrorKind::UniqueViolation => ApiError::DatabaseDuplicate {
                    error: err.to_string(),
                },
                diesel::result::DatabaseErrorKind::ForeignKeyViolation
                | diesel::result::DatabaseErrorKind::NotNullViolation
                | diesel::result::DatabaseErrorKind::CheckViolation => {
                    ApiError::DatabaseConstraints {
                        error: err.to_string(),
                    }
                }
                _ => ApiError::DatabaseQueryError {
                    error: err.to_string(),
                },
            },
            err => ApiError::DatabaseConnection {
                error: err.to_string(),
            },
        })
    }
}
impl From<PoolError> for ApiErrorAdapter {
    fn from(err: PoolError) -> Self {
        Self(match err {
            PoolError::ConnectionError(err) => ApiError::DatabaseConnection {
                error: err.to_string(),
            },
            PoolError::QueryError(err) => match err {
                diesel::result::Error::NotFound => ApiError::DatabaseNotFound {
                    error: err.to_string(),
                },
                _ => ApiError::DatabaseConnection {
                    error: err.to_string(),
                },
            },
        })
    }
}
impl From<DeadpoolPoolError> for ApiErrorAdapter {
    fn from(err: DeadpoolPoolError) -> Self {
        match err {
            DeadpoolPoolError::Backend(err) => Self::from(err),
            err => Self(ApiError::DatabaseConnection {
                error: err.to_string(),
            }),
        }
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

#[instrument(level = "INFO", skip_all)]
pub async fn run(args: &Args, pool: Pool<AsyncPgConnection>) -> crate::Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/projects", post(project_create))
        .route("/projects/{project_slug}", get(project_get))
        .route(
            "/projects/{project_slug}/builds",
            post(project_build_create),
        )
        .route(
            "/workers/{worker_slug}/internal/allocate-build",
            put(worker_internal_allocate_build),
        )
        .fallback(error_not_found)
        .method_not_allowed_fallback(error_method_not_allowed)
        .with_state(S { pool });

    debug!(http_addr = args.http_addr, "Binding HTTP listener...");
    let listener = TcpListener::bind(&args.http_addr).await?;
    info!("Running on: http://{}/", listener.local_addr()?);
    Ok(axum::serve(listener, app).await?)
}

async fn error_not_found(req: axum::extract::Request) -> Result<()> {
    let (method, route) = (req.method().to_string(), req.uri().path().to_string());
    debug!(method, route, "Not found");

    Err(ApiErrorAdapter::from(ApiError::NoSuchRoute {
        method,
        route,
    }))
}

async fn error_method_not_allowed(req: axum::extract::Request) -> Result<()> {
    let (method, route) = (req.method().to_string(), req.uri().path().to_string());
    debug!(method, route, "Method not allowed");

    Err(ApiErrorAdapter::from(ApiError::MethodNotAllowed {
        method,
        route,
    }))
}

async fn root() -> JsonResponse<api::RootResponse> {
    JsonResponse(api::RootResponse {})
}

async fn project_create(
    State(state): State<S>,
    JsonRequest(req): JsonRequest<api::ProjectCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectCreateResponse<'static>>> {
    let now = Utc::now();
    let project = models::Project::from_spec(req.get().project.to_owned(), Uuid::now_v7(), now);
    let project = {
        use crate::schema::projects::dsl::*;

        let mut conn = state.pool.get().await?;
        diesel::insert_into(projects)
            .values(&project)
            .get_result::<models::Project>(&mut conn)
            .await?
    };
    Ok(JsonResponse(api::ProjectCreateResponse {
        project: project.to_client(),
    }))
}

async fn project_get(
    State(state): State<S>,
    Path(project_slug): Path<String>,
) -> Result<JsonResponse<api::ProjectGetResponse<'static>>> {
    let project = {
        use schema::projects::dsl::*;

        let mut conn = state.pool.get().await?;
        projects
            .filter(schema::projects::slug.eq(&project_slug))
            .first::<models::Project>(&mut conn)
            .await?
    };
    Ok(JsonResponse(api::ProjectGetResponse {
        project: project.to_client(),
    }))
}

async fn project_build_create(
    State(state): State<S>,
    Path(project_slug): Path<String>,
    JsonRequest(req): JsonRequest<api::ProjectBuildCreateRequest<'static>>,
) -> Result<JsonResponse<api::ProjectBuildCreateResponse<'static>>> {
    let now = Utc::now();
    let mut conn = state.pool.get().await?;
    let project_id: Uuid = {
        use schema::projects;
        projects::table
            .select(projects::id)
            .filter(projects::slug.eq(project_slug))
            .first(&mut conn)
            .await?
    };

    let (build, inputs) =
        models::Build::from_spec(req.get().build.to_owned(), Uuid::now_v7(), project_id, now);
    let build = {
        use schema::builds;
        diesel::insert_into(builds::table)
            .values(&build)
            .get_result::<models::Build>(&mut conn)
            .await?
    };
    let inputs = {
        use schema::build_inputs;
        diesel::insert_into(build_inputs::table)
            .values(inputs.collect::<Vec<_>>())
            .get_results::<models::BuildInput>(&mut conn)
            .await?
    };
    Ok(JsonResponse(api::ProjectBuildCreateResponse {
        build: build.to_client(inputs),
    }))
}

async fn worker_internal_allocate_build(
    State(state): State<S>,
    Path(worker_slug): Path<String>,
    JsonRequest(_req): JsonRequest<api::WorkerInternalAllocateBuildRequest>,
) -> Result<JsonResponse<api::WorkerInternalAllocateBuildResponse<'static>>> {
    let now = Utc::now();
    let build = state
        .pool
        .get()
        .await?
        .transaction(|conn| {
            Box::pin(async {
                use schema::{build_inputs, builds, workers};

                let default_worker = models::Worker {
                    id: Uuid::now_v7(),
                    slug: worker_slug.clone(),
                    name: worker_slug.clone(),
                    created_at: now,
                    updated_at: now,
                };
                let worker_id = diesel::insert_into(workers::table)
                    .values(&default_worker)
                    .returning(workers::id)
                    .on_conflict(workers::slug)
                    .do_update()
                    .set(&default_worker)
                    .get_result::<Uuid>(conn)
                    .await?;

                let build_id = builds::table
                    .select(builds::id)
                    .filter(builds::worker_id.is_null())
                    .limit(1)
                    .order_by(builds::created_at.asc())
                    .for_no_key_update()
                    .get_result::<Uuid>(conn)
                    .await?;
                let build = diesel::update(builds::table)
                    .filter(builds::id.eq(build_id))
                    .set(builds::worker_id.eq(worker_id))
                    .returning(models::Build::as_returning())
                    .get_result(conn)
                    .await?;
                let inputs = build_inputs::table
                    .filter(build_inputs::build_id.eq(build.id))
                    .get_results::<models::BuildInput>(conn)
                    .await?;

                Ok::<_, diesel::result::Error>(build.to_client(inputs))
            })
        })
        .await?;

    Ok(JsonResponse(api::WorkerInternalAllocateBuildResponse {
        build,
    }))
}
