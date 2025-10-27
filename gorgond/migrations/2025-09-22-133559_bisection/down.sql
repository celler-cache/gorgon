-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

DROP TABLE bisection_step_commits;
DROP TABLE bisection_steps;
DROP TABLE bisection_commits;
DROP TABLE bisections;

-- Recreate the old rollups tables from ../2025-06-30-153110_inputs/up.sql

CREATE TABLE rollups (
    id             UUID PRIMARY KEY CHECK (id <> '00000000-0000-0000-0000-000000000000'),
    parent_id      UUID          REFERENCES rollups (id)               ON DELETE CASCADE,
    input_id       UUID NOT NULL REFERENCES build_template_inputs (id) ON DELETE CASCADE,
    commit_id      UUID NOT NULL REFERENCES commits (id)               ON DELETE CASCADE,
    build_input_id UUID          REFERENCES build_inputs (id)          ON DELETE CASCADE,

    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE rollup_commits (
    rollup_id      UUID NOT NULL REFERENCES rollups (id)        ON DELETE CASCADE,
    commit_id      UUID NOT NULL REFERENCES commits (id)        ON DELETE CASCADE,
    PRIMARY KEY (rollup_id, commit_id)
);

SELECT diesel_manage_updated_at('rollups');

COMMENT ON COLUMN rollups.parent_id IS
    'If this rollup is part of a bisection, the parent rollup that was split to create it.';
COMMENT ON COLUMN rollups.build_input_id IS
    'Set by gorgond when "build_inputs" is created from "input_id" and this rollup. A rollup is considered pending if this is not set, and will be picked up in a future round of builds.';

