# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
#
# SPDX-License-Identifier: EUPL-1.2

{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  buildInputs = with pkgs; [
    # build dependencies
    stdenv # C/C++ compiler and deps for crates with bindings

    # repo tools
    reuse # check license compliance
  ];
}
