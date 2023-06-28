# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>

self: super: {
  nix-web = super.callPackage ../nix-web {};
}
