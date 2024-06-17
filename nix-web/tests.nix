# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{ pkgs ? import ../nix/nixpkgs.default.nix
, nix-web ? import ./. { inherit pkgs; }
}:

let
  supportedNixVersions = with pkgs.lib; flip filterAttrs pkgs.nixVersions
    (attr: pkg: hasPrefix "nix_" attr && versionAtLeast pkg.version "2.18");

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

    systemd.sockets.nix-web = {
      wantedBy = [ "sockets.target" ];
      socketConfig.ListenStream = "[::1]:1234";
    };

    systemd.packages = [
      (nix-web.overrideAttrs ({ ... }: {
        NIX_WEB_BUILD_NIX_CLI_PATH = "nix";
      }))
    ];

    specialisation = lib.mapAttrs (attr: pkg: {
      configuration = {
        nix.package = pkg;
        systemd.services.nix-web.path = [ pkg ];
      };
    }) supportedNixVersions;
  };

  testScript = { nodes, ... }: let
    specialisations = "${nodes.machine.system.build.toplevel}/specialisation";
  in ''
    from os import listdir

    for specialisation in listdir('${specialisations}'):
        machine.succeed(f'${specialisations}/{specialisation}/bin/switch-to-configuration test')
        machine.wait_for_unit("sockets.target")
        machine.succeed("curl --fail-with-body http://localhost:1234/ >&2")
        machine.succeed("curl --fail-with-body http://localhost:1234${testDrv.drvPath} >&2")
  '';
})
