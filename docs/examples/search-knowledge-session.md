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

For an artifact containing immutable Git-tree sources, repository and revision
filters pin retrieval to one exact snapshot:

```json
{"query":"imu_read_callback","mode":"lexical","filters":{"repository":"vesc","revision":"0123456789abcdef0123456789abcdef01234567"}}
```

Git-tree ingestion is additive to the compatibility corpus. The optional
`git-corpus` build feature consumes an already-managed repository plus an exact
commit ID, reads blobs without a checkout, and records repository, revision,
path, media type, trust, license, digest, and source span. Repository acquisition
and active-generation selection remain separate lifecycle steps.

## Source read and citation

1. Read `vesc://knowledge/chunk/{id}` from a returned `resource_uri` for the
   bounded passage and source span.
2. Read `vesc://knowledge/document/{id}` from `document_uri` when the complete
   normalized document is required.
3. Cite the returned `source.repo`, `source.path`, `source.line`, and optional
   `source.revision`.

## Complete example request

Ask the connected assistant:

> Search VESC knowledge for `lbm_add_extension`. Return at most three lexical
> results, and read the top result's chunk resource when `resource_uri` is
> present. Otherwise, use its bounded returned passage or `document_uri`. Cite
> its repository, path, and line when available.

This keeps the search bounded and makes the provenance visible before the
passage is used.
