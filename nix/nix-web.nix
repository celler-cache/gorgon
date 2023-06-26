# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>

{ pkgs ? import ./nixpkgs.default.nix
, lib ? pkgs.lib
, nix ? pkgs.nix
, srcOverride ? builtins.fetchGit ../.
}:
let
  cargoPackageFlags = [ "-p" "nix-web" ];
in
pkgs.rustPlatform.buildRustPackage {
  pname = "nix-web";
  version = let
    head = lib.last (lib.splitString ": " (lib.removeSuffix "\n" (builtins.readFile ../.git/HEAD)));
  in lib.removeSuffix "\n" (builtins.readFile (../.git + "/${head}"));

  src = srcOverride;
  cargoLock.lockFile = ../Cargo.lock;

  postPatch = ''
    substituteInPlace nix-web/src/nix_cli.rs \
      --replace 'pub const NIX_CLI_PATH: &str = "nix"' \
                'pub const NIX_CLI_PATH: &str = "${nix}/bin/nix"'
    substituteInPlace nix-web/nix-web.service \
      --replace 'ExecStart=nix-web' "ExecStart=$out/bin/nix-web"
  '';
  postInstall = ''
    install -m 644 -D nix-web/nix-web.service $out/lib/systemd/system/nix-web.service
  '';

  cargoBuildFlags = cargoPackageFlags;
  cargoTestFlags = cargoPackageFlags;
}
