-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

ALTER TABLE build_template_inputs ADD COLUMN source TEXT NOT NULL DEFAULT '';
ALTER TABLE build_template_inputs ALTER COLUMN repository_id DROP NOT NULL;

ALTER TABLE repository_merge_request_versions ADD COLUMN source TEXT NOT NULL DEFAULT '';
UPDATE repository_merge_request_versions SET source = commit_sources.source
    FROM commit_sources WHERE commit_sources.id = repository_merge_request_versions.source_id;
ALTER TABLE repository_merge_request_versions ADD CHECK (source <> '');
ALTER TABLE repository_merge_request_versions DROP COLUMN source_id;

ALTER TABLE repository_merge_requests ADD COLUMN base TEXT NOT NULL DEFAULT '';
UPDATE repository_merge_requests SET base = commit_sources.source
    FROM commit_sources WHERE commit_sources.id = repository_merge_requests.base_id;
ALTER TABLE repository_merge_requests ADD CHECK (base <> '');
ALTER TABLE repository_merge_requests DROP COLUMN base_id;

DROP TABLE commit_sources;
