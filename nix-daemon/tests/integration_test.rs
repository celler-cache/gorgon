// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use nix_daemon::{ClientSettings, DaemonStore, Store, Verbosity};
use std::collections::HashMap;

#[tokio::test]
async fn test_basic() {
    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    store
        .set_options(ClientSettings {
            keep_failed: false,
            keep_going: false,
            try_fallback: false,
            verbosity: Verbosity::Vomit,
            max_build_jobs: 2,
            max_silent_time: 60,
            verbose_build: true,
            build_cores: 69,
            use_substitutes: false,
            overrides: HashMap::new(),
        })
        .await
        .expect("SetOptions failed")
        .result()
        .await
        .expect("SetOptions Result failed");
}
