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
