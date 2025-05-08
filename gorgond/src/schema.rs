// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "build_event_kind"))]
    pub struct BuildEventKind;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "build_log_kind"))]
    pub struct BuildLogKind;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "build_result"))]
    pub struct BuildResult;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "build_task_kind"))]
    pub struct BuildTaskKind;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "build_task_level"))]
    pub struct BuildTaskLevel;
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::BuildEventKind;

    build_events (id) {
        id -> Uuid,
        build_id -> Uuid,
        task_id -> Nullable<Uuid>,
        kind -> BuildEventKind,
        s0 -> Text,
        i0 -> Nullable<Int8>,
        i1 -> Nullable<Int8>,
        i2 -> Nullable<Int8>,
        i3 -> Nullable<Int8>,
        created_at -> Timestamptz,
        timestamp -> Timestamptz,
    }
}

diesel::table! {
    build_inputs (id) {
        id -> Uuid,
        build_id -> Uuid,
        key -> Text,
        source -> Text,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::BuildTaskLevel;
    use super::sql_types::BuildLogKind;

    build_logs (id) {
        id -> Uuid,
        build_id -> Uuid,
        task_id -> Nullable<Uuid>,
        level -> BuildTaskLevel,
        kind -> BuildLogKind,
        file -> Text,
        col -> Nullable<Int8>,
        line -> Nullable<Int8>,
        txt -> Text,
        created_at -> Timestamptz,
        timestamp -> Timestamptz,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::BuildTaskKind;
    use super::sql_types::BuildTaskLevel;

    build_tasks (id) {
        id -> Uuid,
        parent_id -> Nullable<Uuid>,
        build_id -> Uuid,
        name -> Text,
        kind -> Nullable<BuildTaskKind>,
        level -> BuildTaskLevel,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::BuildResult;

    builds (id) {
        id -> Uuid,
        project_id -> Uuid,
        kind -> Text,
        expr -> Text,
        worker_id -> Nullable<Uuid>,
        result -> Nullable<BuildResult>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        allocated_at -> Nullable<Timestamptz>,
        heartbeat_at -> Nullable<Timestamptz>,
        started_at -> Nullable<Timestamptz>,
        ended_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    projects (id) {
        id -> Uuid,
        slug -> Text,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workers (id) {
        id -> Uuid,
        slug -> Text,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        seen_at -> Nullable<Timestamptz>,
    }
}

diesel::joinable!(build_events -> build_tasks (task_id));
diesel::joinable!(build_events -> builds (build_id));
diesel::joinable!(build_inputs -> builds (build_id));
diesel::joinable!(build_logs -> build_tasks (task_id));
diesel::joinable!(build_logs -> builds (build_id));
diesel::joinable!(build_tasks -> builds (build_id));
diesel::joinable!(builds -> projects (project_id));
diesel::joinable!(builds -> workers (worker_id));

diesel::allow_tables_to_appear_in_same_query!(
    build_events,
    build_inputs,
    build_logs,
    build_tasks,
    builds,
    projects,
    workers,
);
