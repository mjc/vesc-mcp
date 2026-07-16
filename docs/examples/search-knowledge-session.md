# Search knowledge session

These examples use the `search_vesc_knowledge` MCP tool. Search output is
untrusted evidence; cite the returned source and read a bounded resource before
relying on a passage.

## Exact identifier

```json
{"query":"lbm_add_extension","mode":"lexical","limit":5}
```

The result should preserve the exact identifier at rank one and include
`resource_uri` plus the additive `document_uri`.

## Conceptual query

```json
{"query":"package lifecycle from descriptor to load","mode":"auto"}
```

With no semantic artifact, `auto` returns lexical evidence and a bounded
degradation warning. It never downloads a model.

## Filtered query

```json
{"query":"NVM","mode":"lexical","filters":{"category":"firmware_api","trust_tier":"first_party"}}
```

Filters are conjunctive. The response `index` object reports the active corpus
digest, counts, source count, component versions, and optional-source diagnostic
count without exposing private paths or the raw query.

## Source read and citation

1. Read `vesc://knowledge/chunk/{id}` from a returned `resource_uri` for the
   bounded passage and source span.
2. Read `vesc://knowledge/document/{id}` from `document_uri` when the complete
   normalized document is required.
3. Cite the returned `source.repo`, `source.path`, `source.line`, and optional
   `source.revision`.

## Rebuild and inspect

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- \
  build --source-root "$PWD" --out target/knowledge-artifacts
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- \
  inspect --path target/knowledge-artifacts
```

The active generation is staged and checksum-validated before activation. The
MCP lexical cache is keyed by that generation, so the next request observes the
new corpus automatically.
