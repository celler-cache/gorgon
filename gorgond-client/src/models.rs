// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap, fmt::Debug};
use uuid::Uuid;
use yoke::Yokeable;

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Project<'a> {
    pub id: Uuid,
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Worker<'a> {
    pub id: Uuid,
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BuildResult {
    Success,
    Failure,
    WorkerLost,
}

#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Build<'a> {
    pub id: Uuid,
    pub project_id: Uuid,
    #[serde(borrow)]
    pub kind: Cow<'a, str>,
    #[serde(borrow)]
    pub inputs: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
    #[serde(borrow)]
    pub source: Cow<'a, str>,
    #[serde(borrow)]
    pub expr: Cow<'a, str>,

    pub worker_id: Option<Uuid>,
    pub result: Option<BuildResult>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub allocated_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// A task performed as part of a Build.
#[derive(Debug, Serialize, Deserialize, Yokeable)]
pub struct Task<'a> {
    /// ID of the parent Build.
    pub build_id: Uuid,
    /// Sub-ID of this task. Unique together with build_id, but may be reused between builds.
    /// This may be a UUIDv8 with a build system specific ID unceremoniously shoved into it.
    pub task_sid: Uuid,
    /// Sub-ID of the parent task, if any. The parent task must have the same build_id.
    pub parent_task_sid: Uuid,

    #[serde(borrow)]
    pub name: Cow<'a, str>,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}
