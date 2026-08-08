# VESCM-257 symbol retrieval replay

The preserved query from Codex session
`019fc3f6-aa8e-7290-9c87-4419d9c90981` is in `queries.json`. The calling
session had selected `vesc-rust-poc`; the replay therefore calls
`set_current_repository` before searching. Both modes used `limit = 5`, an
8,192-byte context budget, and a 16,384-byte response budget.

## What the branch-added tests cover

- `symbol_question_returns_definitions_and_caller_from_bounded_context`
  builds a Git-backed fixture containing the trait, implementation, and caller,
  then requires all decisive paths and source spans in five results.
- `any_exact_identifier_in_a_symbol_query_is_promoted` proves that punctuation
  from the preserved compound query cannot prevent exact symbol promotion.
- `prose_terms_do_not_become_exact_symbol_matches` prevents ordinary prose
  containing the same words from receiving symbol priority.
- `compact_symbol_rows_center_exact_identifier_in_code` puts an unrelated
  snake-case symbol before the target and proves the bounded MCP excerpt still
  centers the exact query identifier.
- The production fixture additionally asserts the selected revision on every
  decisive result, so evidence cannot silently come from another Git era.

## Production replay

Snapshot `f9e06df304445fd4db089bb79a7adcc2a053a4ff21ffb1864c81ee86d4533008`
contains `vesc-rust-poc` at `00aa861ea5ab825c05ae423848681bc4164d8c73`.
Its vector artifact is 1,124,212,824 bytes. The build reused 302,442 vectors
and embedded 55,132; the 2 GiB artifact bound is required to admit the result.

Lexical and hybrid replay return the same decisive bounded context. The first
five results are:

| Rank | Evidence | Source |
| ---: | --- | --- |
| 1 | `chunk-df9aa4bc927ee186d4b3e9c81db0ef9e950915852acb7b580d113d3877ad3973` | `vesc-rust-poc:crates/vescpkg-rs/src/motor.rs:1775` |
| 2 | `chunk-89a3194d3fba2b61babf90a615b24de205e2698f887f49958d80ea16b628f80d` | `vesc-rust-poc:crates/vescpkg-rs/src/motor.rs:1708` |
| 3 | `chunk-cd285d42a66262cf6789fbc4f1973f93c02a7ed4927004c4ac3fbc283f2e162d` | `vesc-rust-poc:crates/vescpkg-rs/src/motor.rs:1741` |
| 4 | `chunk-b57956a9ce688b8ed61094d72cfae5d8f6d80adab7c7675e16d064fb71ee89ab` | `vesc-rust-poc:crates/vescpkg-rs/src/motor.rs:429` |
| 5 | `chunk-945e90ced65ef790f7195b9491b55197243a6db32eaa8e09aafa5700bbab979c` | `vesc-rust-poc:crates/vescpkg-rs/src/motor.rs:1750` |

These excerpts include the defining `MotorControlBindings` declaration, the
public `update_pid_position_offset` forwarding methods, the implementation
call, `PidPosition`, persistence behavior, and repository/path provenance. No
additional source read is needed; a targeted chunk/document read remains
available through the returned evidence ID if a caller needs more surrounding
code.

## Before and after

The observed session trace performed two MCP searches and then six manual file
read actions (`build.rs`, `raw.rs`, `motor.rs`, `motor_control.rs`, and two more
`motor.rs` reads). Replaying its final query against Mika's old selected
snapshot returned five irrelevant BLDC/Refloat results in 300.669 ms and 3,027
HTTP response bytes because that snapshot omitted the Rust source corpus.

| Mode | Results | HTTP bytes | HTTP latency | Manual fallback reads |
| --- | ---: | ---: | ---: | ---: |
| Before lexical | 5 irrelevant | 3,027 | 300.669 ms | 6 observed |
| Before hybrid | 5 irrelevant | 3,435 | 5,261.563 ms | 6 observed |
| After lexical | 5 decisive | 3,681 | 157.365 ms | 0 required by evaluation |
| After hybrid | 5 decisive | 3,680 | 747.383 ms | 0 required by evaluation |

`queries.json` preserves the request settings, both before-mode measurements,
the ordered before and after result identities, revisions and byte/line spans,
the six observed reads, decisive after text, and chunk/document follow-up URI
shapes. The after fallback count is an evaluation of the bounded response, not
a new caller interaction trace.

The checked-in in-process reports use one warm-up and five measured queries.
Lexical p50 is 5.570 ms at 6,230 bytes; hybrid p50 is 209.349 ms at 6,229
bytes. They measure the handler plus JSON serialization and exclude HTTP/MCP
transport. Hybrid retained RSS fell by 175,570,944 bytes across measured
queries; lexical retained 421,888 bytes.

The MIGraphX preparation process aborted during provider teardown with
`corrupted double-linked list` only after publishing and validating the ready
snapshot. Read-only lexical and hybrid serving then completed cleanly against
the published generation. Provider teardown remains a separate defect; it did
not corrupt the artifact or the replay.
