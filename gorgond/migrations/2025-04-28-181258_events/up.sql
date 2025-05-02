-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

CREATE TYPE build_task_kind AS ENUM (
    'realise',
    'builds',
    'build',
    'post_build_hook',
    'build_waiting',
    'substitute',
    'copy_paths',
    'copy_path',
    'file_transfer',
    'optimise_store',
    'verify_paths',
    'query_path_info'
);

CREATE TYPE build_task_level AS ENUM (
    'error',
    'warn',
    'notice',
    'info',
    'talkative',
    'chatty',
    'debug',
    'vomit'
);

CREATE TABLE build_tasks (
    id         UUID          PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    parent_id  UUID,
    build_id   UUID NOT NULL REFERENCES builds (id) ON DELETE CASCADE,

    name       TEXT             NOT NULL,
    kind       build_task_kind,
    level      build_task_level NOT NULL,

    created_at TIMESTAMPTZ      NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ      NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    ended_at   TIMESTAMPTZ
);

SELECT diesel_manage_updated_at('build_tasks');

---

CREATE TYPE build_event_kind AS ENUM (
    'started', 'stopped', 'file_linked', 'untrusted_path', 'corrupted_path', 'set_phase', 'progress'
);

CREATE TABLE build_events (
    id       UUID NOT NULL PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    build_id UUID NOT NULL REFERENCES builds (id),
    task_id  UUID          REFERENCES build_tasks (id),

    kind     build_event_kind NOT NULL,

    s0 TEXT NOT NULL, -- | set_phase: phase    |                    |
    i0 BIGINT,        -- | progress:  done     | file_linked: bytes |
    i1 BIGINT,        -- | progress:  expected |                    |
    i2 BIGINT,        -- | progress:  running  |                    |
    i3 BIGINT,        -- | progress:  failed   |                    |

    created_at TIMESTAMPTZ      NOT NULL DEFAULT NOW(),
    timestamp  TIMESTAMPTZ      NOT NULL
);

---

CREATE TYPE build_log_kind AS ENUM ('build', 'post_build', 'message');

CREATE TABLE build_logs (
    id       UUID NOT NULL PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    build_id UUID NOT NULL REFERENCES builds (id),
    task_id  UUID          REFERENCES build_tasks (id),

    level      build_task_level NOT NULL,
    kind       build_log_kind   NOT NULL,
    file       TEXT             NOT NULL,
    col        BIGINT,
    line       BIGINT,
    txt        TEXT             NOT NULL CHECK (txt <> ''),

    created_at TIMESTAMPTZ      NOT NULL DEFAULT NOW(),
    timestamp  TIMESTAMPTZ      NOT NULL
);
