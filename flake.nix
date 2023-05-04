# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{
  outputs =
    { self, nixpkgs }:
    nixpkgs.lib.foldAttrs nixpkgs.lib.mergeAttrs {} (map (system: {
      checks.${system}.reuse = import nix/reuse.nix {
        pkgs = nixpkgs.legacyPackages.${system};
        src = self;
      };
    }) [
      "aarch64-linux"
      "armv7l-linux"
      "i686-linux"
      "powerpc64le-linux"
      "riscv64-linux"
      "x86_64-linux"
    ]);
}
