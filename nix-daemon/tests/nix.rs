// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

mod utils;

use nix_daemon::{
    nix::DaemonStore, BuildMode, ClientSettings, Progress, ProgressExt, Stderr, Store, Verbosity,
};
use std::io::Write;
use std::{collections::HashMap, process::Stdio};
use tokio_test::io::Builder;
use utils::init_logging;

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
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
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
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
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
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
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
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
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
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
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

#[tokio::test]
async fn test_add_to_store() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .add_to_store(
            "test_AddToStore",
            "fixed:r:sha256",
            Vec::<String>::new(),
            false,
            // $ echo -n "DaemonStore::add_to_store()" > test_AddToStore
            // $ nix-store --dump (nix-store --add test_AddToStore) | xxd -i
            Builder::new()
                .read(&[
                    0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6e, 0x69, 0x78, 0x2d, 0x61,
                    0x72, 0x63, 0x68, 0x69, 0x76, 0x65, 0x2d, 0x31, 0x00, 0x00, 0x00, 0x01, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x74, 0x79, 0x70, 0x65,
                    0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x72,
                    0x65, 0x67, 0x75, 0x6c, 0x61, 0x72, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x73, 0x1b, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x44, 0x61, 0x65, 0x6d, 0x6f, 0x6e, 0x53, 0x74,
                    0x6f, 0x72, 0x65, 0x3a, 0x3a, 0x61, 0x64, 0x64, 0x5f, 0x74, 0x6f, 0x5f, 0x73,
                    0x74, 0x6f, 0x72, 0x65, 0x28, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00,
                ])
                .build(),
        )
        .await
        .expect("IsValidPath failed")
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);

    let (name, pi) = r.expect("Progress");
    assert_eq!(
        "/nix/store/rplkfskrgxcfm49953si4jbinw9fg8sm-test_AddToStore".to_string(),
        name
    );
    assert_eq!(pi.deriver, None);
    assert_eq!(pi.references, Vec::<String>::new());
    assert_eq!(
        pi.nar_hash,
        "3c126cf4c0fec8c85cf9791ccdaf670877f9f9faf46b5d1991523d509b341d9e".to_string()
    );
    assert_eq!(pi.nar_size, 144);
    assert_eq!(pi.ultimate, false);
    assert_eq!(pi.signatures, Vec::<String>::new());
    assert_eq!(
        pi.ca,
        Some("fixed:r:sha256:17hx6jdm0gajj4cmsszlzbwzjxq8cypws73rz5fcij7yq3s6q4iw".to_string())
    );
}

#[tokio::test]
async fn test_query_missing_valid() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let missing = store
        .query_missing(&[create_known_test_file()][..])
        .await
        .expect("QueryMissing failed")
        .result()
        .await
        .expect("Progress");
    assert_eq!(
        missing,
        nix_daemon::QueryMissing {
            will_build: Vec::new(),
            will_substitute: Vec::new(),
            unknown: Vec::new(),
            download_size: 0,
            nar_size: 0,
        }
    );
}

#[tokio::test]
async fn test_build_paths() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");

    // Random string our built derivation should output. This test expects to
    // actually build something, so it needs a derivation with a known output,
    // which is incredibly unlikely to already exist in the user's store.
    let cookie = {
        use rand::distributions::{Alphanumeric, DistString};
        Alphanumeric.sample_string(&mut rand::thread_rng(), 16)
    };

    // Instantiate a derivation.
    let mut nix_instantiate = std::process::Command::new("nix-instantiate")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Couldn't spawn nix-instantiate");
    std::thread::spawn({
        let mut stdin = nix_instantiate.stdin.take().unwrap();
        let input = format!(
            "derivation {{
                    name = \"test_build_paths_{}\";
                    builder = \"/bin/sh\";
                    args = [ \"-c\" \"echo -n $name > $out\" ];
                    system = builtins.currentSystem;
                }}",
            cookie,
        );
        move || stdin.write_all(input.as_bytes())
    });
    let nix_instantiate_output = nix_instantiate
        .wait_with_output()
        .expect("nix-instantiate failed");
    let drv_path_ = String::from_utf8(nix_instantiate_output.stdout).unwrap();
    let drv_path = drv_path_.trim();
    let drv_output = format!("{}!out", drv_path);

    // Double check that it's not already built.
    let missing = store
        .query_missing(&[&drv_output][..])
        .await
        .expect("QueryMissing failed")
        .result()
        .await
        .expect("QueryMissing Progress");
    assert_eq!(
        nix_daemon::QueryMissing {
            will_build: vec![drv_path.to_string()],
            will_substitute: vec![],
            unknown: vec![],
            download_size: 0,
            nar_size: 0,
        },
        missing
    );

    // Build it!
    store
        .build_paths(&[&drv_output][..], BuildMode::Normal)
        .await
        .expect("BuildPaths failed")
        .result()
        .await
        .expect("BuildPaths Progress");

    let nix_store_query = std::process::Command::new("nix-store")
        .arg("--query")
        .arg(&drv_path)
        .output()
        .expect("Couldn't create known test derivation");
    let out_path =
        std::path::PathBuf::from(String::from_utf8(nix_store_query.stdout).unwrap().trim());

    let content = std::fs::read_to_string(out_path).expect("Couldn't read output");
    assert_eq!(format!("test_build_paths_{}", cookie), content);
}
