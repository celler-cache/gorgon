-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

CREATE TABLE commit_sources (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    repository_id  UUID NOT NULL    REFERENCES repositories (id) ON DELETE CASCADE,
    commit_id      UUID             REFERENCES commits (id)      ON DELETE SET NULL,
    source         TEXT NOT NULL    CHECK (source <> ''),

    fetched_at     TIMESTAMPTZ,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (repository_id, source)
);

-- Note that the data migrations in this file will "steal" the ID of the row where the
-- source URL was originally from, because Postgres' UUIDv7 support is only in v18+.
--
-- If we ever write a data migration that affects anybody but me, we'll do this properly.

-- Drop support for pinning an input for now - lossy, but again, this only affects me.
ALTER TABLE build_template_inputs DROP COLUMN source;
ALTER TABLE build_template_inputs ALTER COLUMN repository_id SET NOT NULL;

-- Migrate repository_merge_request_versions.
ALTER TABLE repository_merge_request_versions
    ADD COLUMN source_id UUID REFERENCES commit_sources (id) ON DELETE CASCADE;

INSERT INTO commit_sources (id, repository_id, source, commit_id)
    SELECT DISTINCT ON (repository_id, source) versions.id, repository_id, source, commit_id
        FROM repository_merge_request_versions AS versions
        INNER JOIN repository_merge_requests AS merges ON versions.merge_request_id = merges.id
    ON CONFLICT (repository_id, source) DO NOTHING;

UPDATE repository_merge_request_versions
    SET source_id = commit_sources.id
    FROM commit_sources
    WHERE commit_sources.source = repository_merge_request_versions.source;

ALTER TABLE repository_merge_request_versions ALTER COLUMN source_id SET NOT NULL; 
ALTER TABLE repository_merge_request_versions DROP COLUMN source;

-- Migrate repository_merge_requests.
ALTER TABLE repository_merge_requests
    ADD COLUMN base_id UUID REFERENCES commit_sources (id) ON DELETE CASCADE;

INSERT INTO commit_sources (id, repository_id, source)
    SELECT DISTINCT ON (repository_id, source) id, repository_id, base AS source
        FROM repository_merge_requests
    ON CONFLICT (repository_id, source) DO NOTHING;

UPDATE repository_merge_requests
    SET base_id = commit_sources.id
    FROM commit_sources
    WHERE commit_sources.source = repository_merge_requests.base;

ALTER TABLE repository_merge_requests ALTER COLUMN base_id SET NOT NULL; 
ALTER TABLE repository_merge_requests DROP COLUMN base;
