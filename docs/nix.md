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

The packaged build includes the generated knowledge artifact and optional
semantic runtime:

```bash
nix run
```

## Develop and test

```bash
nix develop -c make check
nix develop -c cargo nextest run --workspace
nix build
```

Knowledge and coverage commands use the same shell:

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
nix develop -c make coverage
nix develop -c make coverage-summary
```

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
