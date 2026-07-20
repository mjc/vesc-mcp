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

The packaged build includes the generated knowledge artifact and the pinned
INT8 `jinaai/jina-embeddings-v2-base-code` query model. `auto` retrieval uses
hybrid search when the semantic runtime is available and falls back to lexical
search with a warning if it is not:

```bash
nix run
```

The previous BGE model remains at
`share/vesc-mcp/models/bge-small-en-v1.5-quantized` inside the package for an
explicit fallback. It must be paired with a compatible BGE vector artifact;
overriding only the model would correctly fail artifact compatibility checks.

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
    refresh = true;
    eagerIndex = true;
    allowOfflineRestart = true;
    timeoutSecs = 900;
  };
};
```

The first start binds HTTP before fetching three bare Git stores and building
the combined default history in the background, so clients can observe
`ping.knowledge` progress throughout the expensive first preparation. Later
starts fetch incrementally with gix, reuse content-addressed passages, and
advance the default alias only after the new snapshot validates. Changing one
ref creates a new artifact without deleting older immutable snapshots.

Bare repositories, manifests, indexes, and temporary same-filesystem staging
live below `/var/lib/vesc-mcp`; disposable caches have the separate
`/var/cache/vesc-mcp` lifecycle. The dynamic service user has no writable home
and `ProtectSystem=strict` prevents writes to the Nix store or project checkout.
With `allowOfflineRestart = true`, a failed refresh retains and serves the last
valid default snapshot with a bounded stale warning. Set `refresh = false` for
an intentionally offline cached restart, or `eagerIndex = false` to defer new
snapshot preparation.

Only credential-free HTTPS repository URLs are accepted in evaluated Nix
configuration. Put bearer tokens or Git credential environment settings in a
root-readable `authTokenFile`/systemd credential source; secrets referenced by
ordinary Nix strings or store paths are world-readable and must not be used.
