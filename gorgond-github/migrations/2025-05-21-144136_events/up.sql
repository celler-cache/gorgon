-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

CREATE TABLE repos (
    id             BIGINT PRIMARY KEY,
    name           TEXT   NOT NULL,
    repository_id  UUID       NULL REFERENCES gorgond.repositories ON DELETE SET NULL,

    polled_at      TIMESTAMPTZ,
    current_at     TIMESTAMPTZ,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('repos');

COMMENT ON COLUMN repos.repository_id IS
    'Set only if the repo corresponds to a repository monitored by gorgond. '
    'We still need to track eg. forks, to be able to follow pull request pushes.';
COMMENT ON COLUMN repos.polled_at IS
    'If this is a repo monitored by gorgond, when we last polled for events.';

---

CREATE TABLE actors (
    id             BIGINT PRIMARY KEY,
    login          TEXT   NOT NULL,
    display_login  TEXT   NOT NULL,
    avatar_url     TEXT   NOT NULL,

    current_at     TIMESTAMPTZ,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('actors');

---

CREATE TABLE orgs (
    id             BIGINT PRIMARY KEY,
    login          TEXT   NOT NULL,
    avatar_url     TEXT   NOT NULL,

    current_at     TIMESTAMPTZ,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('orgs');

---

CREATE TABLE refs (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    repo_id        BIGINT NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    ref_name       TEXT   NOT NULL CHECK (ref_name <> ''),
    UNIQUE (repo_id, ref_name)
);

---

CREATE TABLE pull_requests (
    id                BIGINT PRIMARY KEY,
    repo_id           BIGINT NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    number            INT    NOT NULL,
    head_ref_id       UUID   NOT NULL REFERENCES refs (id) ON DELETE CASCADE,
    base_ref_id       UUID   NOT NULL REFERENCES refs (id) ON DELETE CASCADE,
    merge_request_id  UUID       NULL REFERENCES gorgond.repository_merge_requests ON DELETE SET NULL,

    title             TEXT   NOT NULL DEFAULT '',
    body              TEXT   NOT NULL DEFAULT '',

    current_at        TIMESTAMPTZ     NULL,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    synced_at         TIMESTAMPTZ     NULL
);

SELECT diesel_manage_updated_at('pull_requests');

COMMENT ON COLUMN pull_requests.merge_request_id IS
    'MergeRequests on the gorgond side are created asynchronously, then filled in here.';

---

-- https://docs.github.com/en/rest/using-the-rest-api/github-event-types
CREATE TYPE event_kind AS ENUM ('pull_request', 'push');
CREATE TYPE event_pull_request_action AS ENUM (
    'opened', 'edited', 'closed', 'reopened',
    'assigned', 'unassigned', 'review_requested', 'review_request_removed',
    'labeled', 'unlabeled', 'synchronize'
);

CREATE TABLE events (
    id               BIGINT      PRIMARY KEY,
    repo_id          BIGINT      NOT NULL REFERENCES repos (id)         ON DELETE CASCADE,
    actor_id         BIGINT      NOT NULL REFERENCES actors (id)        ON DELETE CASCADE,
    org_id           BIGINT          NULL REFERENCES orgs (id)          ON DELETE CASCADE,
    kind             event_kind  NOT NULL,
    public           BOOLEAN     NOT NULL,
    payload          JSONB       NOT NULL,

    emitted_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    synced_at        TIMESTAMPTZ     NULL,

    pull_request_id  BIGINT          NULL REFERENCES pull_requests (id) ON DELETE CASCADE,
    pull_request_action event_pull_request_action NULL,

    ref_id           UUID            NULL REFERENCES refs (id)          ON DELETE CASCADE,
    head             BYTEA       NOT NULL
);

SELECT diesel_manage_updated_at('events');

COMMENT ON COLUMN events.pull_request_id IS 'PullRequestEvent';
COMMENT ON COLUMN events.ref_id          IS 'PushEvent';
COMMENT ON COLUMN events.head            IS 'PushEvent';
