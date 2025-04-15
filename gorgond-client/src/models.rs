// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use chrono::prelude::*;
use ownable::{IntoOwned, ToOwned};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap, fmt::Debug};
use uuid::Uuid;
use yoke::Yokeable;

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct ProjectSpec<'a> {
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Project<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: ProjectSpec<'a>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct WorkerSpec<'a> {
    #[serde(borrow)]
    pub slug: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Worker<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: WorkerSpec<'a>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToOwned, IntoOwned)]
pub enum BuildResult {
    Success,
    Failure,
    WorkerLost,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct BuildSpec<'a> {
    #[serde(borrow)]
    pub kind: Cow<'a, str>,
    #[serde(borrow)]
    pub inputs: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
    #[serde(borrow)]
    pub source: Cow<'a, str>,
    #[serde(borrow)]
    pub expr: Cow<'a, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Build<'a> {
    #[ownable(clone)]
    pub id: Uuid,
    #[ownable(clone)]
    pub project_id: Uuid,
    #[serde(flatten, borrow)]
    pub spec: BuildSpec<'a>,

    #[ownable(clone)]
    pub worker_id: Option<Uuid>,
    pub result: Option<BuildResult>,

    #[ownable(clone)]
    pub created_at: DateTime<Utc>,
    #[ownable(clone)]
    pub updated_at: DateTime<Utc>,
    #[ownable(clone)]
    pub allocated_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub started_at: Option<DateTime<Utc>>,
    #[ownable(clone)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// A task performed as part of a Build.
#[derive(Debug, Clone, Serialize, Deserialize, Yokeable, ToOwned, IntoOwned)]
pub struct Task<'a> {
    /// ID of the parent Build.
    #[ownable(clone)]
    pub build_id: Uuid,
    /// Sub-ID of this task. Unique together with build_id, but may be reused between builds.
    /// This may be a UUIDv8 with a build system specific ID unceremoniously shoved into it.
    #[ownable(clone)]
    pub task_sid: Uuid,
    /// Sub-ID of the parent task, if any. The parent task must have the same build_id.
    #[ownable(clone)]
    pub parent_task_sid: Uuid,

    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[ownable(clone)]
    pub start: DateTime<Utc>,
    #[ownable(clone)]
    pub end: Option<DateTime<Utc>>,
}
