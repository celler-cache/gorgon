# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>
# SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>

{ pkgs ? import ../nix/nixpkgs.default.nix
, lib ? pkgs.lib
, nix-supervisor ? import ./. { inherit pkgs; }
, cargo ? pkgs.cargo
, hello ? pkgs.hello
}:

let
  # Version support:
  # - Nix 2.23: nix-build sends Op(0) for some reason??
  supportedNixVersions = with pkgs.lib;
    (flip filterAttrs pkgs.nixVersions (attr: pkg: hasPrefix "nix_" attr
      && versionAtLeast pkg.version "2.18" && versionOlder pkg.version "2.23")) //
    (flip filterAttrs pkgs.lixVersions (attr: pkg: hasPrefix "lix_" attr));

  # Build nix-daemon's `nix` integration tests as a standalone binary.
  # We want to run these tests against all supported Nix/Lix versions.
  # This should maybe be moved to `nix-daemon/tests.nix`.
  nix-daemon-nix-integration =
    pkgs.rustPlatform.buildRustPackage {
      name = "nix-daemon-nix-integration";
      inherit (nix-supervisor) src buildInputs nativeBuildInputs;
      cargoDepsName = "nix-supervisor";
      cargoLock.lockFile = ../Cargo.lock;

      cargoBuildFlags = [ "-p=nix-daemon" "--test=nix" ];
      dontCargoCheck = true;
      installPhase = ''
        mkdir -p $out/bin
        bin=$(find target/*/release/deps/ -type f -executable | grep -E 'nix-[0-9a-z]+$')
        echo "found test runner binary: $bin"
        cp $bin $out/bin/nix-integration
      '';
    };
in

pkgs.testers.nixosTest ({ lib, ... }: {
  name = "nix-supervisor-test";

  nodes.machine = { modulesPath, ... }: {
    imports = [
      "${modulesPath}/installer/cd-dvd/channel.nix"
    ];

    system.extraDependencies = [
      hello hello.drvPath
    ];

    systemd.services.nix-supervisor = {
      after = [ "network.target" "nix-daemon.service" ];
      serviceConfig = {
        ExecStart = ''${nix-supervisor}/bin/nix-supervisor \
          --sock=/tmp/nix-supervisor.sock'';
        UMask = "0000";
      };
      wantedBy = [ "multi-user.target" ];
    };

    users.users.testuser = {
      isNormalUser = true;
    };

    # Offline binary cache used for some nix-daemon tests.
    nix.settings.substituters = lib.mkForce [ "file:///tmp/subst" ];

    specialisation = lib.mapAttrs (attr: pkg: {
      configuration.nix.package = pkg;
    }) supportedNixVersions;
  };

  testScript = { nodes, ... }: let
    specialisations = "${nodes.machine.system.build.toplevel}/specialisation";
  in ''
    from os import listdir

    for specialisation in listdir('${specialisations}'):
        machine.succeed(f'${specialisations}/{specialisation}/bin/switch-to-configuration test')
        machine.wait_for_unit("nix-supervisor")

        # Check the current nix version.
        print(machine.succeed("nix --version"));

        # Copy nix to the substitute store. Needed for some nix-daemon tests.
        machine.succeed("nix --extra-experimental-features nix-command copy -v --to file:///tmp/subst $(type -p nix)")
        # Run cargo integration tests, with and without the supervisor in the middle.
        machine.succeed("sudo -u testuser env -C $(sudo -u testuser mktemp -d) ${nix-daemon-nix-integration}/bin/nix-integration")

        # Regular, old-fashioned CLI.
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix-build '<nixpkgs>' -A hello >&2")

        # Newfangled, experimental CLI.
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' run -f '<nixpkgs>' hello >&2")
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' build -f '<nixpkgs>' hello >&2")

        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' show-derivation -f '<nixpkgs>' hello >&2")
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' path-info -f '<nixpkgs>' hello >&2")

        # REPL commands.
        machine.succeed("echo 'hello' | sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' repl -f '<nixpkgs>' >&2")
        machine.succeed("echo ':b hello' | sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' repl -f '<nixpkgs>' >&2")
        machine.succeed("echo ':p hello' | sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' repl -f '<nixpkgs>' >&2")

        # Shells.
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' shell -f '<nixpkgs>' hello -c hello >&2")
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix-shell -p hello --command 'hello' >&2")

        # For some reason, these hang forever when ran in test cases.
        # machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' why-depends -f '<nixpkgs>' hello hello >&2")
        # machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' develop -f '<nixpkgs>' hello -c hello >&2")
  '';
})
