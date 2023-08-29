# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{ pkgs ? import ../nix/nixpkgs.default.nix
, nix-supervisor ? import ./. { inherit pkgs; }
, hello ? pkgs.hello
}:

let
  supportedNixVersions = with pkgs.lib; flip filterAttrs pkgs.nixVersions
    (attr: pkg: hasPrefix "nix_" attr && versionAtLeast pkg.version "2.13");
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

        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix-build '<nixpkgs>' -A hello >&2")
        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' path-info ./result >&2")

        machine.succeed("sudo -u testuser env NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix-shell -p hello --command 'hello' >&2")
  '';
})
