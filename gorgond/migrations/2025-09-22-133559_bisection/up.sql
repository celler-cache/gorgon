-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

DROP TABLE rollup_commits;
DROP TABLE rollups;

---

CREATE TABLE rollups (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    repository_id  UUID  NOT NULL   REFERENCES repositories (id) ON DELETE CASCADE,

    rolled_at   TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE rollup_inputs (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    rollup_id      UUID NOT NULL REFERENCES rollups (id)        ON DELETE CASCADE,

    commit_id      UUID          REFERENCES commits (id)        ON DELETE CASCADE,
    source_id      UUID          REFERENCES commit_sources (id) ON DELETE CASCADE,
    CONSTRAINT commit_or_source CHECK (commit_id IS NOT NULL OR source_id IS NOT NULL),

    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX rollup_inputs_rollup_commit_unique
    ON rollup_inputs (rollup_id, commit_id) WHERE commit_id IS NOT NULL;
CREATE UNIQUE INDEX rollup_inputs_rollup_source_unique
    ON rollup_inputs (rollup_id, source_id) WHERE source_id IS NOT NULL;

SELECT diesel_manage_updated_at('rollups');
SELECT diesel_manage_updated_at('rollup_inputs');

COMMENT ON TABLE rollups IS 'A set of commits "rolled up" into one or more merge commits.';
COMMENT ON COLUMN rollups.rolled_at IS 'na-naa na-na-na nana-na-na';

COMMENT ON TABLE rollup_inputs IS 'Commits (or sources) to be included in this rollup.';
COMMENT ON COLUMN rollup_inputs.commit_id IS 'The included commit. If not set, source_id is resolved.';
COMMENT ON COLUMN rollup_inputs.source_id IS 'The source to be resolved to set commit_id.';

---

CREATE TABLE rollup_batches (
    id                UUID  PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    rollup_id         UUID  NOT NULL    REFERENCES rollups (id) ON DELETE CASCADE,
    commit_id         UUID  NOT NULL    REFERENCES commits (id) ON DELETE CASCADE,
    UNIQUE (rollup_id, commit_id),

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE rollup_batch_commits (
    batch_id   UUID NOT NULL REFERENCES rollup_batches (id) ON DELETE CASCADE,
    commit_id  UUID NOT NULL REFERENCES commits (id)        ON DELETE CASCADE,
    PRIMARY KEY (batch_id, commit_id)
);
COMMENT ON TABLE rollup_batches IS 'Merged batches of commits, resulting from a rollup.';
COMMENT ON TABLE rollup_batch_commits IS 'Commits included in a batch.';

---

CREATE TABLE bisections (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    project_id     UUID NOT NULL    REFERENCES projects (id) ON DELETE CASCADE,

    template_id    UUID             REFERENCES build_templates (id)       ON DELETE SET NULL,
    template_kind  TEXT NOT NULL    CHECK (template_kind <> ''),
    template_expr  TEXT NOT NULL    CHECK (template_expr <> ''),

    input_id       UUID             REFERENCES build_template_inputs (id) ON DELETE SET NULL,
    input_key      TEXT NOT NULL    CHECK (input_key <> ''),

    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT diesel_manage_updated_at('bisections');

---

CREATE TABLE bisection_steps (
    id            UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    bisection_id  UUID NOT NULL    REFERENCES bisections (id)     ON DELETE CASCADE,
    batch_id      UUID NOT NULL    REFERENCES rollup_batches (id) ON DELETE CASCADE,
    build_id      UUID NOT NULL    REFERENCES builds (id)         ON DELETE CASCADE,

    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (bisection_id, batch_id)
);

SELECT diesel_manage_updated_at('bisection_steps');

---

CREATE TABLE bisection_rollups (
    bisection_id UUID NOT NULL REFERENCES bisections (id)      ON DELETE CASCADE,
    rollup_id    UUID NOT NULL REFERENCES rollups (id)         ON DELETE CASCADE,
    fork_step_id UUID          REFERENCES bisection_steps (id) ON DELETE CASCADE,
    PRIMARY KEY (bisection_id, rollup_id)
);

---

ALTER TABLE build_inputs ADD COLUMN commit_id UUID;
ALTER TABLE build_inputs ADD FOREIGN KEY (commit_id) REFERENCES commits (id);
ALTER TABLE build_inputs DROP CONSTRAINT build_inputs_source_check;
ALTER TABLE build_inputs ADD CONSTRAINT build_inputs_source_or_commit
    CHECK ((source = '' AND commit_id IS NOT NULL) OR (source <> '' AND commit_id IS NULL));
