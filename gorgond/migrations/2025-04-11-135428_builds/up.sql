CREATE TABLE projects (
    id          UUID PRIMARY KEY,
    slug        TEXT UNIQUE NOT NULL CHECK (name <> ''),
    name        TEXT NOT NULL CHECK (name <> ''),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('projects');

---

CREATE TABLE workers (
    id          UUID PRIMARY KEY,
    slug        TEXT UNIQUE NOT NULL CHECK (name <> ''),
    name        TEXT NOT NULL CHECK (name <> ''),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('workers');

---

CREATE TYPE build_result AS ENUM ('success', 'failure', 'worker_lost');

CREATE TABLE builds (
    id              UUID PRIMARY KEY,
    project_id      UUID NOT NULL REFERENCES projects (id),
    kind            TEXT NOT NULL CHECK (kind <> ''),
    source          TEXT NOT NULL CHECK (source <> ''),
    expr            TEXT NOT NULL CHECK (expr <> ''),

    worker_id       UUID REFERENCES workers (id),
    result          build_result,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    allocated_at    TIMESTAMPTZ,
    heartbeat_at    TIMESTAMPTZ,
    started_at      TIMESTAMPTZ,
    ended_at        TIMESTAMPTZ
);

SELECT diesel_manage_updated_at('builds');

---

CREATE TABLE build_inputs (
    id          UUID PRIMARY KEY,
    build_id    UUID NOT NULL REFERENCES builds (id) ON DELETE CASCADE,
    key         TEXT NOT NULL CHECK (key <> ''),
    source      TEXT NOT NULL CHECK (source <> ''),

    UNIQUE (build_id, key)
);
