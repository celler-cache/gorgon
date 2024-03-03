// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use nix_daemon::{ClientSettings, DaemonStore, Stderr, Store, Verbosity};
use std::collections::HashMap;

#[tokio::test]
async fn test_set_options() {
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

#[tokio::test]
async fn test_is_valid_path_false() {
    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .is_valid_path("/nix/store/ffffffffffffffffffffffffffffffff-invalid-1.0.0")
        .await
        .expect("IsValidPath failed")
        .collect()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(false, r.unwrap());
}

#[tokio::test]
async fn test_is_valid_path_true() {
    // Find the nix command.
    let mut nix_path = String::new();
    for dir in std::env::var("PATH").unwrap().split(":") {
        let path = std::path::Path::new(dir).join("nix");
        if path.try_exists().unwrap() {
            nix_path = path
                // /run/current-system/sw/bin/nix
                .canonicalize()
                .unwrap()
                // /nix/store/dsqs4fpljrws4ikzfriyixcp0n7kxrmk-nix-2.18.1/bin/nix
                .parent()
                .unwrap()
                // /nix/store/dsqs4fpljrws4ikzfriyixcp0n7kxrmk-nix-2.18.1/bin
                .parent()
                .unwrap()
                // /nix/store/dsqs4fpljrws4ikzfriyixcp0n7kxrmk-nix-2.18.1
                .to_str()
                .expect("invalid path")
                .to_owned();
        }
    }

    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .is_valid_path(&nix_path)
        .await
        .expect("IsValidPath failed")
        .collect()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(true, r.unwrap());
}
