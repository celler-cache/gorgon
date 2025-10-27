-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

DROP TABLE rollup_commits;
DROP TABLE rollups;

---

CREATE TABLE bisections (
    id                UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    idempotency_hash  BYTEA NOT NULL   UNIQUE CHECK (idempotency_hash <> ''),
    repository_id     UUID             REFERENCES repositories (id),

    template_id       UUID             REFERENCES build_templates (id) ON DELETE SET NULL,
    template_kind     TEXT NOT NULL    CHECK (template_kind <> ''),
    template_expr     TEXT NOT NULL    CHECK (template_expr <> ''),

    input_id          UUID             REFERENCES build_template_inputs (id) ON DELETE SET NULL,
    input_key         TEXT NOT NULL    CHECK (input_key <> ''),

    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE bisection_commits (
    bisection_id  UUID NOT NULL REFERENCES bisections (id) ON DELETE CASCADE,
    commit_id     UUID NOT NULL REFERENCES commits (id),
    PRIMARY KEY (bisection_id, commit_id)
);

COMMENT ON COLUMN bisections.idempotency_hash IS 'SHA256 of input commit UUIDs (sorted, binary).';

SELECT diesel_manage_updated_at('bisections');

---

CREATE TABLE bisection_steps (
    id                UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    idempotency_hash  BYTEA NOT NULL   UNIQUE CHECK (idempotency_hash <> ''),
    bisection_id      UUID NOT NULL    REFERENCES bisections (id) ON DELETE CASCADE,
    commit_id         UUID             REFERENCES commits (id)    ON DELETE SET NULL,
    build_id          UUID             REFERENCES builds (id)     ON DELETE SET NULL,
    result            build_result,

    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE bisection_step_commits (
    step_id    UUID REFERENCES bisection_steps (id) ON DELETE CASCADE,
    commit_id  UUID REFERENCES commits (id),
    PRIMARY KEY (step_id, commit_id)
);

COMMENT ON COLUMN bisection_steps.idempotency_hash IS 'SHA256 of input commit UUIDs (sorted, binary).';
COMMENT ON COLUMN bisection_steps.commit_id IS 'Rollup commit; if NULL, it will be created.';
COMMENT ON COLUMN bisection_steps.build_id IS 'Build; if NULL, it will be created.';

SELECT diesel_manage_updated_at('bisection_steps');

---
