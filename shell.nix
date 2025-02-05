# SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
#
# SPDX-License-Identifier: EUPL-1.2

{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  buildInputs = with pkgs; [
    # build dependencies
    stdenv # C/C++ compiler and deps for crates with bindings
    pkg-config
    openssl

    # script dependencies
    fish

    # repo tools
    reuse # check license compliance
    commitlint # check commit messages
    woodpecker-cli

    # test dependencies
    hello
  ] ++ (lib.optionals stdenv.isDarwin [
    iconv
    darwin.apple_sdk.frameworks.SystemConfiguration
  ]);

  DIESEL = "${pkgs.diesel-cli}/bin/diesel";
}
