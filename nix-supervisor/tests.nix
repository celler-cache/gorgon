# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{ pkgs ? import ../nix/nixpkgs.default.nix
, nix-supervisor ? import ./. { inherit pkgs; }
, hello ? pkgs.hello
}:

let
  # supportedNixVersions = with pkgs.lib; flip filterAttrs pkgs.nixVersions
  #   (attr: pkg: hasPrefix "nix_" attr && versionAtLeast pkg.version "2.18");

  # FIXME: Handshakes are currently broken with Nix 2.20.
  supportedNixVersions = { inherit (pkgs.nixVersions) nix_2_18 nix_2_19; };
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
        machine.succeed("nix --version");

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
