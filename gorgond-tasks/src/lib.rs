// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::sync::LazyLock;

use directories::ProjectDirs;

pub mod tasks;

pub static DIRS: LazyLock<ProjectDirs> = LazyLock::new(|| {
    ProjectDirs::from("org", "Gorgon", "gorgond").expect("Couldn't find project directories!?")
});
