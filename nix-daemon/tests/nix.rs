// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use nix_daemon::{
    nix::DaemonStore, ClientSettings, Progress, ProgressExt, Stderr, Store, Verbosity,
};
use std::collections::HashMap;

const INVALID_STORE_PATH: &'static str =
    "/nix/store/ffffffffffffffffffffffffffffffff-invalid-1.0.0";

// Find the store path for the system's `nix` command.
fn find_nix_derivation() -> String {
    for dir in std::env::var("PATH").unwrap().split(":") {
        let path = std::path::Path::new(dir).join("nix");
        if path.try_exists().unwrap() {
            return path
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
    panic!("No `nix` command in $PATH");
}

// Instantiates a derivation that creates a known derivation in the store.
fn create_known_test_file() -> String {
    let out = std::process::Command::new("nix-instantiate")
        .arg("-E")
        .arg(
            "derivation {
                name = \"nix-daemon-fixed-test-file\";
                builder = \"/bin/sh\";
                system = builtins.currentSystem;
            }",
        )
        .output()
        .expect("Couldn't create known test derivation");
    String::from_utf8(out.stdout)
        .expect("Invalid nix-instantiate output")
        .trim()
        .to_owned()
}

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
        .is_valid_path(INVALID_STORE_PATH)
        .await
        .expect("IsValidPath failed")
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(false, r.unwrap());
}
#[tokio::test]
async fn test_is_valid_path_true() {
    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .is_valid_path(find_nix_derivation())
        .await
        .expect("IsValidPath failed")
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(true, r.unwrap());
}

#[tokio::test]
async fn test_query_pathinfo_none() {
    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .query_pathinfo(INVALID_STORE_PATH)
        .await
        .expect("QueryPathInfo failed")
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(None, r.unwrap());
}
#[tokio::test]
async fn test_query_pathinfo_some() {
    let mut store = DaemonStore::connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .query_pathinfo(create_known_test_file())
        .await
        .expect("QueryPathInfo failed")
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);

    // We can't check the timestamp, so we have to compare the other fields one-by-one.
    let pi = r.expect("Error").expect("No PathInfo");
    assert_eq!(pi.deriver, None);
    assert_eq!(pi.references, Vec::<String>::new());
    assert_eq!(
        pi.nar_hash,
        "cb8becf17ebe664fecd6769b0384ce1a70830a1ac1f434e227c01ed774f91059".to_string()
    );
    assert_eq!(pi.nar_size, 416);
    assert_eq!(pi.ultimate, false);
    assert_eq!(pi.signatures, Vec::<String>::new());
    assert_eq!(
        pi.ca,
        Some("text:sha256:0h9xd0y2mzqnc73x9xnkkkqgi7rvya2b7ksdd0zdczjqsvhf4cpl".to_string())
    );
}
