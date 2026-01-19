# Projects
A "project" is the top-level container for anything that is "user-scoped", rather than global - a "namespace" was another contender for what to name this. Once we have authentication and authorization, this will be the primary permission boundary.

Aside from being a container for other things, a project has no inherent functionality.

## Builds
The main unit of work in Gorgon is the humble Build. This is a self-contained bundle of work, which is performed by a Worker.

By design, a Build is very stupid - it has a builder ID (like "nix" or "meson"), an "expression" string for said builder to evaluate, and a set of inputs - names and Fetcher URLs. It doesn't know what nix or meson is, or the syntax of their expressions; all it does is tell a Worker: here are some instructions, I hope they mean something to you.

Builds also never actions any other part of the system - other things (eg. bisections) interact with them, but never the other way around. They're always at the "bottom of the stack".

The lifecycle of a build looks like this:

1. A Build is created.
2. A Worker requests a Build.
   1. If there's an existing build with a `worker_id` matching the Worker, that's returned again. This means that if a worker crashes for any reason, it will resume the last build assigned to it.
   2. If not, one is assigned. `worker_id` and `allocated_at` are updated.
3. The Worker performs the Build.
   1. At regular intervals, the Worker sends a "heartbeat". This updates `heartbeat_at`.
   2. Logs are also asynchronously streamed to storage.
4. Once the build finishes, the worker reports back.
   1. Any pending log uploads are finalized.
   2. The worker calls back to `gorgond`; `result` and `ended_at` are updated.

# Repositories
Repositories are tracked globally, and have two parts: upstream and storage.

The upstream repository is set by `kind` and `ident`, both opaque to `gorgond` itself - how to synchronise repositories is left to separate integrations; `gorgond-github` monitors Repositories with `kind` = `gh` and `ident` set to `owner/repo` (eg. `NixOS/nixpkgs`).

The storage repository is a local(ish) cache of interesting data. Workers only pull from the storage repository, which avoids relying on upstreams to be available during builds, and means we can control our own data retention.

## MergeRequests and Versions
These represent a single "merge request", and versions thereof, for remote repositories that use this style of workflow (eg. Github, Gitlab).

(In the future, we will also have abstractions for Gerrit-style Patchsets, as well as LKML-style mailing lists.)

## Commits and CommitSources
A Commit is a reference to a *local* commit, one which has been resolved and stored in a Repository's `storage` repo. Commit objects are immutable, are not expected to ever be deleted unless the underlying storage is pruned, and mainly exists to give a smaller, more foreign-key friendly UUID identifier to an underlying commit hash.

Meanwhile, a *remote* commit, which hasn't yet been resolved or stored, is referenced by a CommitSource. Remote commits are described by...

### Fetcher URLs
This is a unified way to refer to any kind of remote object, which can be "fetched" in some way. The scheme of the URL tells you what kind of "fetcher" would be used for this URL, and is either in the form `fetcher:` or `fetcher+scheme:`. Additional arguments can be passed as query parameters.

There are two kinds of Fetcher URLs: "floating" and "fixed". A floating URL, like `git+https://github.com/NixOS/nixpkgs` or `git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable`, is one whose meaning can change over time. A fixed one, like `git+https://github.com/NixOS/nixpkgs?ref=f61125a668a320878494449750330ca58b78c557`, is one that will always refer to the same commit.

#### Git URLs
A Git URL is something which, absent the `git://` scheme or `git+` prefix and query parameters, would be accepted by `git clone`.

It supports the following arguments:

- `ref=` to fetch a specific "ref", such as a branch or tag.
- `rev=` to fetch a specific revision, eg. commit hash - this makes the URL "fixed".

Note that specifying `rev=` without also specifying a `ref=` containing said revision, is only allowed if the remote repository has enabled support for it, as this causes some additional load on the server. Don't rely on this if possible.

(The `gorgond-github` integration does rely on this, for the simple reason that I wrote that code before finding this out. This will be changed at some point, but Github making unannounced, breaking changes to their API right before the deadline for this to be done, rather ended up taking priority.)

## Rollups and RollupBatches
A Rollup is a request to take a set of patches (Commits or CommitSources), and "roll up" the changes into one or more batches of non-conflicting changes. We can then test a potentially very large number of patches in one go, on the thesis that most won't break things, and deal with failures by way of...

### Bisections and BisectionSteps
A Rollup is normally built as part of a Bisection. This is a binary search process, analogous to (and named after) the `git bisect` command.

We take a RollupBatch, and try to build it as-is. If it succeeds, we assume that all commits inside are fine; if not, we split the batch up into two or more smaller batches, and repeat the process. Eventually, we'll be down to batches of one commit each, and then we'll know the culprit(s).

Right now, the splits are done manually. In the future, there are a few ways we could make this a bit more intelligent.
