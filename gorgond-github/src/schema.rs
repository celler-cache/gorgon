// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// @generated automatically by Diesel CLI.

pub mod gorgond_github {
    pub mod sql_types {
        #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
        #[diesel(postgres_type(name = "event_kind", schema = "gorgond_github"))]
        pub struct EventKind;

        #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
        #[diesel(postgres_type(name = "event_pull_request_action", schema = "gorgond_github"))]
        pub struct EventPullRequestAction;
    }

    diesel::table! {
        gorgond_github.actors (id) {
            id -> Int8,
            login -> Text,
            display_login -> Text,
            avatar_url -> Text,
            current_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use super::sql_types::EventKind;
        use super::sql_types::EventPullRequestAction;

        gorgond_github.events (id) {
            id -> Int8,
            repo_id -> Int8,
            actor_id -> Int8,
            org_id -> Nullable<Int8>,
            kind -> EventKind,
            public -> Bool,
            payload -> Jsonb,
            emitted_at -> Timestamptz,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
            synced_at -> Nullable<Timestamptz>,
            pull_request_id -> Nullable<Int8>,
            pull_request_action -> Nullable<EventPullRequestAction>,
            ref_id -> Nullable<Uuid>,
            head -> Bytea,
        }
    }

    diesel::table! {
        gorgond_github.orgs (id) {
            id -> Int8,
            login -> Text,
            avatar_url -> Text,
            current_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        gorgond_github.pull_requests (id) {
            id -> Int8,
            repo_id -> Int8,
            number -> Int4,
            ref_id -> Uuid,
            merge_request_id -> Nullable<Uuid>,
            title -> Text,
            body -> Text,
            current_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
            synced_at -> Nullable<Timestamptz>,
        }
    }

    diesel::table! {
        gorgond_github.refs (id) {
            id -> Uuid,
            repo_id -> Int8,
            ref_name -> Text,
        }
    }

    diesel::table! {
        gorgond_github.repos (id) {
            id -> Int8,
            name -> Text,
            repository_id -> Nullable<Uuid>,
            polled_at -> Nullable<Timestamptz>,
            current_at -> Nullable<Timestamptz>,
            updated_at -> Timestamptz,
            created_at -> Timestamptz,
        }
    }

    diesel::joinable!(events -> actors (actor_id));
    diesel::joinable!(events -> orgs (org_id));
    diesel::joinable!(events -> pull_requests (pull_request_id));
    diesel::joinable!(events -> refs (ref_id));
    diesel::joinable!(events -> repos (repo_id));
    diesel::joinable!(pull_requests -> refs (ref_id));
    diesel::joinable!(pull_requests -> repos (repo_id));
    diesel::joinable!(refs -> repos (repo_id));

    diesel::allow_tables_to_appear_in_same_query!(
        actors,
        events,
        orgs,
        pull_requests,
        refs,
        repos,
    );
}
