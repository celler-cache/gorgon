-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

ALTER TABLE pull_requests ADD COLUMN synced_at TIMESTAMPTZ;
ALTER TABLE pull_requests ADD COLUMN title TEXT NOT NULL DEFAULT '';
ALTER TABLE pull_requests ADD COLUMN body TEXT NOT NULL DEFAULT '';
ALTER TABLE pushes ADD COLUMN synced_at TIMESTAMPTZ;
ALTER TABLE events ADD COLUMN synced_at TIMESTAMPTZ;
