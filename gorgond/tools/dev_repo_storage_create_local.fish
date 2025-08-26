#!/usr/bin/env fish

# Move to the repo root.
cd (dirname (dirname (status dirname)))

# Build and find the gorgonctl binary.
set -l gorgonctl (
    cargo build --message-format=json-render-diagnostics -p gorgonctl |
    jq -sr '[.[] | select(.reason=="compiler-artifact" and .executable != null) | .executable] | last'
)

# Find repositories that don't have storage assigned.
for json in ($gorgonctl -f json repo list | jq -c '.repositories.[] | select(.storage == "")')
    set -l kind (echo "$json" | jq -r .kind)
    set -l ident (echo "$json" | jq -r .ident)

    # Create storage, eg. ~/src/gorgon/gorgon/data/repos/github/NixOS/nixpkgs.git
    set -l storage $PWD/data/repos/$kind/$ident.git
    echo $kind:$ident '->' $storage

    # Create a bare git repo that we can pull from/push to.
    mkdir -p $storage
    git init --bare $storage

    # Update it on the server.
    $gorgonctl -f json repo kind $kind ident $ident set-storage file://$storage
end
