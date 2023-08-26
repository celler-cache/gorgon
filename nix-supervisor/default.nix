# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>

{ pkgs ? import ../nix/nixpkgs.default.nix
, lib ? pkgs.lib
, nix ? pkgs.nix
, srcOverride ? import ../nix/src.nix { inherit lib; }
}:
let
  cargoPackageFlags = [ "-p" "nix-supervisor" ];
in
pkgs.rustPlatform.buildRustPackage {
  pname = "nix-supervisor";
  inherit ((lib.importTOML ./Cargo.toml).package) version;

  src = srcOverride;
  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = cargoPackageFlags;
  cargoTestFlags = cargoPackageFlags;
}
