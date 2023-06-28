# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

{ lib, src ? builtins.fetchGit { url = ../.; submodules = true; } }:

with lib;

cleanSourceWith {
  filter = path: type: (type == "regular" -> !hasSuffix ".nix" path);
  src = cleanSource src;
}
