# Nix setup

This is an optional alternative to the release archives described in
[installation.md](installation.md). It is also the repository's reproducible
development environment.

## Run the server

From a source checkout:

```bash
nix develop -c vesc-mcp-server
```

For Streamable HTTP:

```bash
nix develop -c vesc-mcp-server --http
```

The packaged build includes the embedded catalog and the pinned INT8
`jinaai/jina-embeddings-v2-base-code` query model. `auto` retrieval uses hybrid
search when its managed snapshot has matching vectors. Managed snapshot
preparation builds those vectors with the packaged model. With managed
repositories disabled, lexical search uses the embedded catalog and `auto` or
`hybrid` returns an explicit lexical retry. With managed repositories enabled,
search never substitutes the embedded catalog or `artifact_path` for an
unavailable managed snapshot:

```bash
nix run
```

## Develop and test

```bash
nix develop -c make check
nix develop .#ci -c make check
nix develop -c cargo nextest run --workspace
nix build
```

The default shell includes profiling, provider, audit, and editor tooling. The
lean `.#ci` shell contains only the pinned Rust toolchain and commands needed by
the required checks and coverage workflow.

Knowledge and coverage commands use the same shell:

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
nix develop -c make coverage
nix develop -c make coverage-summary
```

`make coverage` writes and summarizes `lcov.info`; `make coverage-summary`
reprints that existing report without rerunning tests.

## NixOS service

The flake exports `nixosModules.default`. A minimal local service is:

```nix
services.vesc-mcp = {
  enable = true;
  allowedHosts = [ "localhost" "127.0.0.1" ];
  retrievalMode = "lexical";
};
```

Before allowing remote clients, set `authTokenFile`, the intended bind
address, accepted hosts, accepted origins, and a firewall rule. The token file
is a systemd EnvironmentFile containing `VESC_MCP_HTTP_AUTH_TOKEN=...`.

### Managed repository service

Repositories are declared as a typed attribute set. Nix generates the runtime
TOML in the immutable store, while systemd provides the writable data root at
`/var/lib/vesc-mcp`. Operators do not maintain a second mutable configuration
file.

```nix
services.vesc-mcp = {
  enable = true;
  retrievalMode = "lexical";

  repositories = {
    bldc = {
      url = "https://github.com/vedderb/bldc.git";
      defaultRef = "refs/heads/master";
      include = [ "**/*.c" "**/*.h" "documentation/**" ];
      exclude = [ "build/**" ];
      license = "GPL-3.0-or-later";
      attribution = "VESC Project";
    };
    vesc_tool = {
      url = "https://github.com/vedderb/vesc_tool.git";
      defaultRef = "refs/heads/master";
      include = [ "**/*.cpp" "**/*.h" "**/*.qml" ];
      exclude = [ "build/**" ];
      license = "GPL-3.0-or-later";
      attribution = "VESC Project";
    };
    refloat = {
      url = "https://github.com/lukash/refloat.git";
      defaultRef = "refs/heads/main";
      required = false;
      trustTier = "community";
      include = [ "**/*.c" "**/*.h" "**/*.lisp" "doc/**" ];
      exclude = [ "build/**" ];
      license = "GPL-3.0-or-later";
      attribution = "Refloat contributors";
    };
  };

  # Optional explicit default and historical comparison set.
  defaultVersions.bldc = "refs/heads/release_6_06";
  prewarm = [
    {
      bldc = "refs/heads/release_6_05";
      vesc_tool = "refs/heads/release_6_05";
      refloat = "refs/tags/v1.2.3";
    }
  ];

  startup = {
    cachedOnly = false;
    refresh = true;
    eagerIndex = true;
    allowOfflineRestart = true;
    timeoutSecs = 900; # Bounds the detached refresh/index preparation child.
  };
};
```

The first start binds HTTP before fetching three bare Git stores and building
the combined default history in the background, so clients can observe
`ping.knowledge` progress throughout the expensive first preparation. Later
starts fetch incrementally with gix, reuse content-addressed passages, and
reuse unchanged semantic rows from the prior binary vector artifact. New commits
are chunked and embedded; rebased, force-moved, or deleted refs remove only
evidence that is no longer reachable from any configured ref. Unchanged content
and vectors remain reusable across changed commit IDs. The default alias advances
only after the reconciled snapshot validates; changing one ref does not mutate
older immutable snapshots.

Configuring at least one enabled repository enables managed Git in the generated
runtime configuration. An empty or fully disabled `repositories` set leaves
managed Git disabled.

Bare repositories, manifests, indexes, and temporary same-filesystem staging
live below `/var/lib/vesc-mcp`; disposable caches have the separate
`/var/cache/vesc-mcp` lifecycle. The dynamic service user has no writable home
and `ProtectSystem=strict` prevents writes to the Nix store or project checkout.
With `allowOfflineRestart = true`, a source outage may retain and serve the last
valid default snapshot when its enabled repositories, resolved selections, and
indexing policies still match. Build or configuration failures do not silently
fall back. A cached outage restart and an intentionally skipped refresh publish
`ping.knowledge.state = "stale"`; strict preparation, terminal failure, or an
invalid managed artifact makes search explicitly unavailable. Set
`refresh = false` for an intentionally offline cached restart. Set
`cachedOnly = true` on serving-only hosts to prohibit repository fetches and
document inference. It may still derive a selected lexical/vector artifact from
a compatible distributed complete-history cache; if that cache does not contain
the selected content, search reports unavailable instead of fetching or running
the embedding model. `eagerIndex = false` avoids updating an already compatible
snapshot.

Distribute knowledge through a staging data root outside the live data tree.
Stop the target service, transfer the complete data-root tuple into that staging
directory, install it, and only then restart the service:

```bash
systemctl --user stop vesc-mcp.service
vesc-mcp-server --install-distributed-cache /path/to/vesc-mcp-data.incoming
systemctl --user start vesc-mcp.service
```

The installer validates the snapshot identity, active generation, lexical and
vector checksums, Git history tips, cached origins, and the configured serving
contract before writing live state. It publishes immutable artifact data and
additive Git objects first, then repository refs/catalogs and snapshot pins. The
canonical snapshot manifest is the final commit marker; the default alias moves
afterward. It shares the preparation and repository locks, so a failed or
interrupted install leaves either the prior complete default or the new complete
default. Do not transfer files directly into the live `artifacts`, `repositories`,
or `snapshots` directories; orphan pruning intentionally removes partial tuples.

Only credential-free HTTPS repository URLs are accepted in evaluated Nix
configuration. Put bearer tokens or Git credential environment settings in a
root-readable `authTokenFile`/systemd credential source; secrets referenced by
ordinary Nix strings or store paths are world-readable and must not be used.
