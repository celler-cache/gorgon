-- SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
--
-- SPDX-License-Identifier: EUPL-1.2

ALTER TABLE pull_requests DROP COLUMN synced_at;
ALTER TABLE pull_requests DROP COLUMN title;
ALTER TABLE pull_requests DROP COLUMN body;
ALTER TABLE pushes DROP COLUMN synced_at;
ALTER TABLE events DROP COLUMN synced_at;
