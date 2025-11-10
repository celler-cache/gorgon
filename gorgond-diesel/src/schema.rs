// @generated automatically by Diesel CLI.

pub mod gorgond {
    pub mod sql_types {
        #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
        #[diesel(postgres_type(name = "build_result", schema = "gorgond"))]
        pub struct BuildResult;
    }

    diesel::table! {
        gorgond.bisection_step_commits (step_id, commit_id) {
            step_id -> Uuid,
            commit_id -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use super::sql_types::BuildResult;

        gorgond.bisection_steps (id) {
            id -> Uuid,
            bisection_id -> Uuid,
            batch_id -> Uuid,
            build_id -> Nullable<Uuid>,
            result -> Nullable<BuildResult>,
            started_at -> Nullable<Timestamptz>,
            ended_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.bisections (id) {
            id -> Uuid,
            rollup_id -> Uuid,
            template_id -> Nullable<Uuid>,
            template_kind -> Text,
            template_expr -> Text,
            input_id -> Nullable<Uuid>,
            input_key -> Text,
            started_at -> Nullable<Timestamptz>,
            ended_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
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
        gorgond.rollup_batch_inputs (batch_id, commit_id) {
            batch_id -> Uuid,
            commit_id -> Uuid,
        }
    }

    diesel::table! {
        gorgond.rollup_batches (id) {
            id -> Uuid,
            rollup_id -> Uuid,
            commit_id -> Uuid,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond.rollup_inputs (rollup_id, source_id) {
            rollup_id -> Uuid,
            source_id -> Uuid,
        }
    }

    diesel::table! {
        gorgond.rollups (id) {
            id -> Uuid,
            repository_id -> Uuid,
            source_hash -> Bytea,
            rolled_at -> Nullable<Timestamptz>,
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

    diesel::joinable!(bisection_step_commits -> bisection_steps (step_id));
    diesel::joinable!(bisection_step_commits -> commits (commit_id));
    diesel::joinable!(bisection_steps -> bisections (bisection_id));
    diesel::joinable!(bisection_steps -> builds (build_id));
    diesel::joinable!(bisection_steps -> rollup_batches (batch_id));
    diesel::joinable!(bisections -> build_template_inputs (input_id));
    diesel::joinable!(bisections -> build_templates (template_id));
    diesel::joinable!(bisections -> rollups (rollup_id));
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
    diesel::joinable!(rollup_batch_inputs -> commits (commit_id));
    diesel::joinable!(rollup_batch_inputs -> rollup_batches (batch_id));
    diesel::joinable!(rollup_batches -> commits (commit_id));
    diesel::joinable!(rollup_batches -> rollups (rollup_id));
    diesel::joinable!(rollup_inputs -> commit_sources (source_id));
    diesel::joinable!(rollup_inputs -> rollups (rollup_id));
    diesel::joinable!(rollups -> repositories (repository_id));

    diesel::allow_tables_to_appear_in_same_query!(
        bisection_step_commits,
        bisection_steps,
        bisections,
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
        rollup_batch_inputs,
        rollup_batches,
        rollup_inputs,
        rollups,
        workers,
    );
}
