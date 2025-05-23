-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

CREATE TABLE repositories (
    id          UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    kind        TEXT NOT NULL    CHECK (kind <> ''),
    ident       TEXT NOT NULL    CHECK (ident <> ''),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (kind, ident)
);

SELECT diesel_manage_updated_at('repositories');

---

CREATE TABLE merge_requests (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    repository_id  UUID NOT NULL    REFERENCES repositories (id) ON DELETE CASCADE,
    ident          TEXT NOT NULL    CHECK (ident <> ''),
    title          TEXT NOT NULL    CHECK (title <> ''),
    description    TEXT NOT NULL    CHECK (description <> ''),

    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (repository_id, ident)
);

SELECT diesel_manage_updated_at('merge_requests');

---

CREATE TABLE merge_request_versions (
    id                UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    merge_request_id  UUID NOT NULL    REFERENCES merge_requests (id) ON DELETE CASCADE,
    url               TEXT NOT NULL    CHECK (url <> ''),

    version_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (merge_request_id, url)
);

SELECT diesel_manage_updated_at('merge_request_versions');
