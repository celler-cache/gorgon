-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

DROP TABLE bisection_step_commits;
DROP TABLE bisection_steps;
DROP TABLE bisections;

DROP TABLE rollup_batch_inputs;
DROP TABLE rollup_batches;
DROP TABLE rollup_inputs;
DROP TABLE rollups;

-- Recreate the old rollups tables from ../2025-06-30-153110_inputs/up.sql

create table rollups (
    id             uuid primary key check (id <> '00000000-0000-0000-0000-000000000000'),
    parent_id      uuid          references rollups (id)               on delete cascade,
    input_id       uuid not null references build_template_inputs (id) on delete cascade,
    commit_id      uuid not null references commits (id)               on delete cascade,
    build_input_id uuid          references build_inputs (id)          on delete cascade,

    updated_at  timestamptz not null default now(),
    created_at  timestamptz not null default now()
);
create table rollup_commits (
    rollup_id      uuid not null references rollups (id)        on delete cascade,
    commit_id      uuid not null references commits (id)        on delete cascade,
    primary key (rollup_id, commit_id)
);

select diesel_manage_updated_at('rollups');

comment on column rollups.parent_id is
    'if this rollup is part of a bisection, the parent rollup that was split to create it.';
comment on column rollups.build_input_id is
    'set by gorgond when "build_inputs" is created from "input_id" and this rollup. a rollup is considered pending if this is not set, and will be picked up in a future round of builds.';

