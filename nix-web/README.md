<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is> -->
<!-- SPDX-FileCopyrightText: 2023 embr <git@liclac.eu> -->

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

Contributing
------------

Please see the [main README](..#contributing).

---

[<img src="https://nlnet.nl/logo/banner.svg" width="200" alt="NLNet Foundation logo" />](https://nlnet.nl/)
[<img src="https://nlnet.nl/image/logos/NGI0Entrust_tag.svg" width="200" alt="NGI Zero Entrust logo" />](https://nlnet.nl/NGI0/)

This project was funded through the NGI0 Entrust Fund, a fund established by NLnet with financial support from the European Commission's Next Generation Internet programme, under the aegis of DG Communications Networks, Content and Technology under grant agreement Nº 101069594.
