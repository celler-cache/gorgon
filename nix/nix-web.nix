# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>

{ pkgs ? import ./nixpkgs.default.nix
, lib ? pkgs.lib
, srcOverride ? builtins.fetchGit ../.
}:
pkgs.rustPlatform.buildRustPackage {
  pname = "nix-web";
  version = let
    head = lib.last (lib.splitString ": " (lib.removeSuffix "\n" (builtins.readFile ../.git/HEAD)));
  in lib.removeSuffix "\n" (builtins.readFile (../.git + "/${head}"));

  src = srcOverride;
  cargoLock.lockFile = ../Cargo.lock;
}
