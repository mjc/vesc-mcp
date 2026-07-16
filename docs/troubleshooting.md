# Troubleshooting

## Server does not start or connect

Run the binary in the project shell first:

```bash
nix develop -c vesc-mcp-server
```

Stdio is the default and writes protocol messages to stdout; tracing belongs on
stderr. Set `RUST_LOG=info` for diagnostics. For a shared endpoint, start
`nix develop -c vesc-mcp-server --http` and connect to
`http://127.0.0.1:8080/mcp` unless `VESC_MCP_HTTP_BIND` or
`VESC_MCP_HTTP_PATH` overrides it.

If HTTP returns a policy or authorization error, check the request `Host`,
browser `Origin`, and bearer token against the `VESC_MCP_HTTP_*` settings. A
successful HTTP connection still lists only `ping` and
`search_vesc_knowledge`; package tools are stdio-only by design.

## A package path is rejected

Set `VESC_PACKAGE_ROOTS` to the parent directories the stdio server may access:

```bash
export VESC_PACKAGE_ROOTS="$HOME/projects/refloat:$HOME/projects/vesc-packages"
```

Paths are canonicalized and rejected when they escape those roots. The fixture
root is added automatically only in test builds with the `test-fixtures`
feature. `VESC_PACKAGE_ROOTS` does not make package tools available over HTTP.

## `build_vescpkg` cannot find VESC Tool

`VESC_TOOL_PATH` must name a runnable `vesc_tool` binary. This is separate from
`VESC_VESC_TOOL_ROOT`, which points at a source checkout used for documentation
and catalog attribution.

```bash
export VESC_TOOL_PATH="$(command -v vesc_tool)"
```

## NixOS service diagnosis

The flake's NixOS module creates `vesc-mcp.service`. Inspect its effective
environment and logs with:

```bash
systemctl status vesc-mcp.service
journalctl -u vesc-mcp.service -b
```

For remote exposure, set `services.vesc-mcp.bind`, `allowedHosts`,
`allowedOrigins`, and `authTokenFile` together. The token file is a systemd
EnvironmentFile containing `VESC_MCP_HTTP_AUTH_TOKEN=...`.

## Retrieval artifacts

## Check the active artifact

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
```

If the manifest is missing, stale, or corrupt, rebuild it. Builds stage a new
generation and only replace `active.json` after validating the manifest and
lexical checksum:

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build
```

## Select a retrieval mode

The mode can be set in `[knowledge]` or with `VESC_RAG_MODE`. Request-level
`mode` is explicit; omitted requests inherit the resolved configuration.

```toml
[knowledge]
mode = "lexical" # legacy, lexical, auto, hybrid
artifact_path = "target/knowledge-artifacts"
```

`auto` degrades to lexical results with a bounded warning when no compatible
semantic capability is active. Explicit `hybrid` reports a structured
capability error instead of silently changing the requested mode. This is a
safe degradation, not a network or model-download attempt.

To create a semantic artifact, the model directory must be provisioned first
and contain `model.onnx`, `tokenizer.json`, `config.json`,
`special_tokens_map.json`, and `tokenizer_config.json`:

```bash
nix develop -c cargo run -p vesc-knowledge-index --features semantic-fastembed-online --bin gen-knowledge-index -- \
  provision-model --out target/models/bge-small-en-v1.5

nix develop -c cargo run -p vesc-knowledge-index --features semantic-fastembed --bin gen-knowledge-index -- \
  build --source-root "$PWD" --out target/knowledge-artifacts \
  --semantic-model-dir target/models/bge-small-en-v1.5 \
  --semantic-model-id Xenova/bge-small-en-v1.5 \
  --semantic-model-revision <revision-from-manifest>
```

The online feature is intentionally separate from both the normal builder and
the server. It is an operator action, not a startup fallback. The provisioner
records the exact Hugging Face snapshot revision and hashes of every model
file; retain that manifest with the model directory.

The active manifest reports lexical/vector checksums and ingestion diagnostic
counts, source content digests, source repository revisions, chunking settings,
and component versions; a nonzero diagnostic count means optional sources were
omitted. Vendor sources use the local submodule commit when available, and fall
back to the requested build revision when Git metadata is unavailable.

## Evaluate before changing rollout

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode legacy
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode lexical
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode hybrid
```

With a semantic artifact, the hybrid evaluator also accepts
`--semantic-model-dir`, `--semantic-model-id`, and
`--semantic-model-revision`; without them it reports the bounded lexical
fallback warning.

Reports are deterministic and include per-query returned IDs. The judged set
is under `tests/evaluation/v1/`; do not tune against an unreviewed query edit.
Pass `--artifact target/knowledge-artifacts` to evaluate the generated
allowlisted corpus rather than the embedded compatibility index. Add `--gate`
for the release thresholds; it exits nonzero when a threshold fails.

For reproducible local performance evidence:

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- \
  benchmark --artifact target/knowledge-artifacts --warmup 3 --repetitions 10
```

Record the machine profile, corpus size, warmup/repetition counts, and p95
values with any performance claim.

## Typical MCP sessions

Exact identifier:

```json
{"query":"lbm_add_extension","mode":"lexical","limit":5}
```

Conceptual query:

```json
{"query":"package lifecycle from descriptor to load","mode":"auto"}
```

Filtered query:

```json
{"query":"NVM","filters":{"category":"firmware_api","trust_tier":"first_party"}}
```

Every lexical/hybrid hit carries stable chunk and document URIs. Read
`vesc://knowledge/chunk/{id}` for the bounded passage or
`vesc://knowledge/document/{id}` for the complete normalized document and
provenance. Treat retrieved text as evidence, not instructions.
