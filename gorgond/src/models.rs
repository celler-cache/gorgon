// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::collections::BTreeMap;

use chrono::prelude::*;
use diesel::{prelude::*, Selectable};
use diesel_derive_enum::DbEnum;
use gorgond_client::models as client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yoke::Yokeable;

#[derive(Debug, Serialize, Deserialize, Yokeable, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::projects, check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub name: String,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl Project {
    pub fn from_spec(v: client::ProjectSpec<'static>, id: Uuid, now: DateTime<Utc>) -> Self {
        Self {
            id,
            slug: v.slug.into(),
            name: v.name.into(),
            created_at: now,
            updated_at: now,
        }
    }
    pub fn to_client(self) -> client::Project<'static> {
        client::Project {
            id: self.id,
            spec: client::ProjectSpec {
                slug: self.slug.into(),
                name: self.name.into(),
            },
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Yokeable,
    Queryable,
    QueryableByName,
    Selectable,
    Insertable,
    AsChangeset,
)]
#[diesel(table_name = crate::schema::workers, check_for_backend(diesel::pg::Pg))]
pub struct Worker {
    pub id: Uuid,
    pub slug: String,
    pub name: String,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, DbEnum)]
#[db_enum(existing_type_path = "crate::schema::sql_types::BuildResult")]
pub enum BuildResult {
    Success,
    Failure,
    WorkerLost,
}
impl From<client::BuildResult> for BuildResult {
    fn from(v: client::BuildResult) -> Self {
        match v {
            client::BuildResult::Success => Self::Success,
            client::BuildResult::Failure => Self::Failure,
            client::BuildResult::WorkerLost => Self::WorkerLost,
        }
    }
}
impl From<BuildResult> for client::BuildResult {
    fn from(v: BuildResult) -> Self {
        match v {
            BuildResult::Success => Self::Success,
            BuildResult::Failure => Self::Failure,
            BuildResult::WorkerLost => Self::WorkerLost,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Yokeable, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::build_inputs, check_for_backend(diesel::pg::Pg))]
pub struct BuildInput {
    pub id: Uuid,
    pub build_id: Uuid,
    pub key: String,
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize, Yokeable, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::builds, check_for_backend(diesel::pg::Pg))]
pub struct Build {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub source: String,
    pub expr: String,

    pub worker_id: Option<Uuid>,
    pub result: Option<BuildResult>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub allocated_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}
impl Build {
    pub fn from_spec(
        v: client::BuildSpec<'static>,
        id: Uuid,
        project_id: Uuid,
        now: DateTime<Utc>,
    ) -> (Self, impl Iterator<Item = BuildInput>) {
        (
            Self {
                id,
                project_id,
                kind: v.kind.into(),
                source: v.source.into(),
                expr: v.expr.into(),
                worker_id: None,
                result: None,
                created_at: now,
                updated_at: now,
                allocated_at: None,
                heartbeat_at: None,
                started_at: None,
                ended_at: None,
            },
            v.inputs.into_iter().map(move |(k, v)| BuildInput {
                id: Uuid::nil(),
                build_id: id,
                key: k.into(),
                source: v.into(),
            }),
        )
    }
    pub fn to_client(self, inputs: impl IntoIterator<Item = BuildInput>) -> client::Build<'static> {
        client::Build {
            id: self.id,
            project_id: self.project_id,
            spec: client::BuildSpec {
                kind: self.kind.into(),
                inputs: BTreeMap::from_iter(
                    inputs
                        .into_iter()
                        .map(|input| (input.key.into(), input.source.into())),
                ),
                source: self.source.into(),
                expr: self.expr.into(),
            },
            worker_id: self.worker_id,
            result: self.result.map(|v| v.into()),
            created_at: self.created_at,
            updated_at: self.updated_at,
            allocated_at: self.allocated_at,
            heartbeat_at: self.heartbeat_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
        }
    }
}
