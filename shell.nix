# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
#
# SPDX-License-Identifier: EUPL-1.2

{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  buildInputs = with pkgs; [
    # build dependencies
    stdenv # C/C++ compiler and deps for crates with bindings

    # sea-orm-cli
    pkg-config openssl

    # repo tools
    reuse # check license compliance
    commitlint # check commit messages
    woodpecker-cli

    # test dependencies
    hello
  ];
}
