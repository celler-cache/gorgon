<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is> -->

# nix-web

A web interface for the Nix store.

![A screenshot of nix-web, showing its own derivation, including
outputs, sources, inputs, builder, args, and
environment.](screenshot.png)

## Try it

```sh
nix --extra-experimental-features 'nix-command flakes' run 'git+https://codeberg.org/gorgon/gorgon#nix-web'
```

## Install it

### NixOS

```nix
{ ... }:

{
  systemd.packages = [
    (import ((builtins.fetchTarball {
      url = "https://codeberg.org/gorgon/gorgon/archive/<commit>.tar.gz";
      sha256 = ...;
    }) + "/nix-web") {})
  ];

  systemd.sockets.nix-web = {
    socketConfig.ListenStream = "[::1]:8649";
    wantedBy = [ "sockets.target" ];
  };
}
```

## Contributing

Open pull requests at <https://codeberg.org/gorgon/gorgon/pulls>.

Please sign off your commits (`git commit -s`) to indicate your
certification of the [Developer Certificate of
Origin](../DCO-1.1.txt).
