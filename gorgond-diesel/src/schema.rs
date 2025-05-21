// @generated automatically by Diesel CLI.

pub mod gorgond {
    pub mod sql_types {
        #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
        #[diesel(postgres_type(name = "build_result", schema = "gorgond"))]
        pub struct BuildResult;
    }

    diesel::table! {
        gorgond.build_inputs (id) {
            id -> Uuid,
            build_id -> Uuid,
            key -> Text,
            source -> Text,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use super::sql_types::BuildResult;

        gorgond.builds (id) {
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
        gorgond.projects (id) {
            id -> Uuid,
            slug -> Text,
            name -> Text,
            created_at -> Timestamptz,
            updated_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.workers (id) {
            id -> Uuid,
            slug -> Text,
            name -> Text,
            created_at -> Timestamptz,
            updated_at -> Timestamptz,
            seen_at -> Nullable<Timestamptz>,
        }
    }

    diesel::joinable!(build_inputs -> builds (build_id));
    diesel::joinable!(builds -> projects (project_id));
    diesel::joinable!(builds -> workers (worker_id));

    diesel::allow_tables_to_appear_in_same_query!(
        build_inputs,
        builds,
        projects,
        workers,
    );
}
