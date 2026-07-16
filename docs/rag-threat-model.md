# Knowledge retrieval threat model

The retrieval corpus is local, allowlisted, and untrusted evidence. Retrieved
text is data; it is never treated as an MCP instruction or configuration.

| Boundary | Risk | Mitigation | Evidence |
|----------|------|------------|----------|
| Source ingestion | traversal, symlink escape, arbitrary dotfiles, build output, or canary secrets enter the corpus | canonicalize the approved root, require repo-relative allowlisted `SourceSpec` paths, reject escapes before reading, and report optional-source failures | `tests/ingestion.rs` |
| Source content | prompt-like text, malformed UTF-8, oversized files, or unsupported attribution | bounded metadata/read, LF normalization, typed rejection, trust/license fields, and content-only chunking | ingestion/chunking contract tests |
| Corpus artifacts | stale, truncated, tampered, incompatible, or path-leaking artifacts | versioned schemas, portable IDs, checked lengths, SHA-256 vector checksum, repo-relative paths, and validation before activation | corpus/semantic/lexical tests |
| Query boundary | parser injection, oversized input, candidate explosion, and response amplification | programmatic Tantivy queries, 4 KiB query bound, 50-result cap, bounded candidate count, 8 KiB passage and 64 KiB response budgets | lexical and MCP tool tests |
| Retrieval output | private path disclosure, untrusted text interpreted as instructions, or unstable citations | stable document/chunk IDs, bounded provenance, trust tier, resource URI, deterministic ordering, and ordinary JSON strings | MCP provenance/resource test |
| Optional semantics | model download, runtime supply chain, NaN vectors, or missing capability | provider boundary, no startup download, explicit artifact validation, fake provider for offline tests, lexical fallback in `auto` | semantic artifact tests and rollout docs |
| Device safety | search work weakens flash/upload gates | retrieval is read-only and does not touch package/device gate code | existing flash-gate tests |

## Accepted risks and deferred work

- A keyword allowlist is not proof that a source contains no secrets; the
  primary control is the approved-root/allowlist boundary.
- The FastEmbed/ONNX adapter is present behind `semantic-fastembed`, but model
  files, license, Nix closure, and semantic quality proof remain operator-owned
  rollout gates; the default build and server do not load or download a model.
- Fuzzy near-duplicate suppression is deferred; identical normalized content
  digests are suppressed today.

## Operator rules

Keep `VESC_PACKAGE_ROOTS` explicit for live package work. Prefer fixtures for
offline searches. Do not add arbitrary runtime crawling or user-selected roots
to the corpus builder without a new threat-model review.
