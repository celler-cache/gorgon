# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{ pkgs ? import ../nix/nixpkgs.default.nix
, nix-supervisor ? import ./. { inherit pkgs; }
}:

let
  supportedNixVersions = with pkgs.lib; flip filterAttrs pkgs.nixVersions
    (attr: pkg: hasPrefix "nix_" attr && versionAtLeast pkg.version "2.13");

  testDrv = derivation {
    name = "test";
    builder = "/bin/sh";
    args = [ "-c" ": > $out" ];
    inherit (pkgs.stdenv.hostPlatform) system;
  };
in

pkgs.testers.nixosTest ({ lib, ... }: {
  name = "nix-web-test";

  nodes.machine = {
    system.extraDependencies = [
      testDrv.drvPath
    ];

    systemd.services.nix-supervisor = {
      after = [ "network.target" ];
      serviceConfig.ExecStart = ''${nix-supervisor}/bin/nix-supervisor \
        -vv --sock=/tmp/nix-supervisor.sock'';
      wantedBy = [ "multi-user.target" ];
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
        machine.succeed("NIX_DAEMON_SOCKET_PATH=/tmp/nix-supervisor.sock nix --extra-experimental-features 'nix-command' path-info ${testDrv} >&2")
  '';
})
