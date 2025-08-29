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
        gorgond.build_template_input_commits (input_id, commit_id) {
            input_id -> Uuid,
            commit_id -> Uuid,
            result -> Bool,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.build_template_inputs (id) {
            id -> Uuid,
            template_id -> Uuid,
            key -> Text,
            repository_id -> Uuid,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.build_templates (id) {
            id -> Uuid,
            project_id -> Uuid,
            slug -> Text,
            kind -> Text,
            expr -> Text,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
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
            allocated_at -> Nullable<Timestamptz>,
            heartbeat_at -> Nullable<Timestamptz>,
            started_at -> Nullable<Timestamptz>,
            ended_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.commit_sources (id) {
            id -> Uuid,
            repository_id -> Uuid,
            commit_id -> Nullable<Uuid>,
            source -> Text,
            fetched_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.commits (id) {
            id -> Uuid,
            repository_id -> Uuid,
            hash -> Bytea,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.projects (id) {
            id -> Uuid,
            slug -> Text,
            name -> Text,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.repositories (id) {
            id -> Uuid,
            kind -> Text,
            ident -> Text,
            storage -> Text,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.repository_merge_request_versions (id) {
            id -> Uuid,
            merge_request_id -> Uuid,
            ident -> Text,
            commit_id -> Nullable<Uuid>,
            current_at -> Timestamptz,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
            source_id -> Uuid,
        }
    }

    diesel::table! {
        gorgond.repository_merge_requests (id) {
            id -> Uuid,
            repository_id -> Uuid,
            ident -> Text,
            title -> Text,
            description -> Text,
            closed_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
            base_id -> Uuid,
        }
    }

    diesel::table! {
        gorgond.rollup_commits (rollup_id, commit_id) {
            rollup_id -> Uuid,
            commit_id -> Uuid,
        }
    }

    diesel::table! {
        gorgond.rollups (id) {
            id -> Uuid,
            parent_id -> Nullable<Uuid>,
            input_id -> Uuid,
            commit_id -> Uuid,
            build_input_id -> Nullable<Uuid>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.workers (id) {
            id -> Uuid,
            slug -> Text,
            name -> Text,
            seen_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::joinable!(build_inputs -> builds (build_id));
    diesel::joinable!(build_template_input_commits -> build_template_inputs (input_id));
    diesel::joinable!(build_template_input_commits -> commits (commit_id));
    diesel::joinable!(build_template_inputs -> build_templates (template_id));
    diesel::joinable!(build_template_inputs -> repositories (repository_id));
    diesel::joinable!(build_templates -> projects (project_id));
    diesel::joinable!(builds -> projects (project_id));
    diesel::joinable!(builds -> workers (worker_id));
    diesel::joinable!(commit_sources -> commits (commit_id));
    diesel::joinable!(commit_sources -> repositories (repository_id));
    diesel::joinable!(commits -> repositories (repository_id));
    diesel::joinable!(repository_merge_request_versions -> commit_sources (source_id));
    diesel::joinable!(repository_merge_request_versions -> commits (commit_id));
    diesel::joinable!(repository_merge_request_versions -> repository_merge_requests (merge_request_id));
    diesel::joinable!(repository_merge_requests -> commit_sources (base_id));
    diesel::joinable!(repository_merge_requests -> repositories (repository_id));
    diesel::joinable!(rollup_commits -> commits (commit_id));
    diesel::joinable!(rollup_commits -> rollups (rollup_id));
    diesel::joinable!(rollups -> build_inputs (build_input_id));
    diesel::joinable!(rollups -> build_template_inputs (input_id));
    diesel::joinable!(rollups -> commits (commit_id));

    diesel::allow_tables_to_appear_in_same_query!(
        build_inputs,
        build_template_input_commits,
        build_template_inputs,
        build_templates,
        builds,
        commit_sources,
        commits,
        projects,
        repositories,
        repository_merge_request_versions,
        repository_merge_requests,
        rollup_commits,
        rollups,
        workers,
    );
}
