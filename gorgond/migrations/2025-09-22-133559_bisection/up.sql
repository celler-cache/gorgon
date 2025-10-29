-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

DROP TABLE rollup_commits;
DROP TABLE rollups;

---

CREATE TABLE rollups (
    id                UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    repository_id     UUID  NOT NULL   REFERENCES repositories (id),
    idempotency_hash  BYTEA NOT NULL   CHECK (idempotency_hash <> ''),
    UNIQUE (repository_id, idempotency_hash),

    rolled_at   TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE rollup_inputs (
    rollup_id      UUID NOT NULL REFERENCES rollups (id) ON DELETE CASCADE,
    commit_id      UUID NOT NULL REFERENCES commits (id) ON DELETE CASCADE,
    PRIMARY KEY (rollup_id, commit_id)
);

SELECT diesel_manage_updated_at('rollups');

COMMENT ON TABLE rollups IS 'A set of commits "rolled up" into one or more merge commits.';
COMMENT ON TABLE rollup_inputs IS 'Input commits to be included in this rollup.';
COMMENT ON COLUMN rollups.idempotency_hash IS 'SHA256 of input commit UUIDs (sorted, binary).';
COMMENT ON COLUMN rollups.rolled_at IS 'na-naa na-na-na nana-na-na katamari damacy';

---

CREATE TABLE rollup_batches (
    id                UUID  PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    rollup_id         UUID  NOT NULL    REFERENCES rollups (id) ON DELETE CASCADE,
    commit_id         UUID  NOT NULL    REFERENCES commits (id) ON DELETE CASCADE,
    UNIQUE (rollup_id, commit_id),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE rollup_batch_inputs (
    batch_id   UUID NOT NULL REFERENCES rollup_batches (id) ON DELETE CASCADE,
    commit_id  UUID NOT NULL REFERENCES commits (id)        ON DELETE CASCADE,
    PRIMARY KEY (batch_id, commit_id)
);
COMMENT ON TABLE rollup_batches IS 'Merged batches of commits, resulting from a rollup.';
COMMENT ON TABLE rollup_batch_inputs IS 'Commits included in a batch.';

---

CREATE TABLE bisections (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    rollup_id      UUID NOT NULL    REFERENCES rollups (id),

    template_id    UUID             REFERENCES build_templates (id)       ON DELETE SET NULL,
    template_kind  TEXT NOT NULL    CHECK (template_kind <> ''),
    template_expr  TEXT NOT NULL    CHECK (template_expr <> ''),

    input_id       UUID             REFERENCES build_template_inputs (id) ON DELETE SET NULL,
    input_key      TEXT NOT NULL    CHECK (input_key <> ''),

    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (rollup_id, template_id, input_id)
);

SELECT diesel_manage_updated_at('bisections');

---

CREATE TABLE bisection_steps (
    id            UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    bisection_id  UUID NOT NULL    REFERENCES bisections (id)     ON DELETE CASCADE,
    batch_id      UUID NOT NULL    REFERENCES rollup_batches (id) ON DELETE CASCADE,
    build_id      UUID             REFERENCES builds (id)         ON DELETE SET NULL,
    result        build_result,

    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (bisection_id, batch_id)
);
CREATE TABLE bisection_step_commits (
    step_id    UUID REFERENCES bisection_steps (id) ON DELETE CASCADE,
    commit_id  UUID REFERENCES commits (id),
    PRIMARY KEY (step_id, commit_id)
);

COMMENT ON COLUMN bisection_steps.build_id IS 'Build; if NULL, it will be created.';

SELECT diesel_manage_updated_at('bisection_steps');

---
