# Deterministic CodeGraphRAG design

Status: accepted design for VESCM-179. The graph augments the existing BM25,
dense-vector, reciprocal-rank fusion, history, and adjacent-context pipeline.
It does not replace any direct retriever and is not required to build a useful
index.

## 1. LightRAG findings

LightRAG's useful idea is dual-level retrieval: begin with query-relevant
entities/relationships for local detail, or relationship/community-like
context for broader questions, then combine the retrieved graph context with
source passages. Its paper and implementation also use LLM-generated entities,
relations, summaries, and query keywords plus mutable graph/vector/key-value
stores. Those mechanisms target prose and are neither deterministic nor
revision-safe enough to become this system's indexing foundation.

CodeGraphRAG transfers only three ideas:

1. use typed relationships as an additional candidate generator;
2. distinguish bounded local structure from subsystem-spanning structure;
3. expose the path that caused indirect evidence to be retrieved.

The design deliberately rejects LightRAG's extraction and storage machinery.
Primary references: the [LightRAG paper](https://arxiv.org/abs/2410.05779) and
the [official implementation](https://github.com/HKUDS/LightRAG).

## 2. Deterministic local and global semantics

`Local` is one hop from a directly retrieved seed through declaration,
definition, call, include/import, generated-source, or adjacent-chunk edges.
It answers questions such as “where is this symbol implemented?” and “who
invokes it?”. `Global` is still bounded traversal, not community summarization:
it uses command-handler, protocol-implementation, QML-backend,
package-firmware-API, configuration-consumer, parser-artifact, subsystem, and
history edges. It answers questions such as “trace this command from UI to
firmware” and “which tagged versions contain this behavior?”.

Rust rules route a query to an edge allowlist. Exact identifiers select local
edges; protocol/command/package/configuration vocabulary selects the matching
global family; historical wording enables occurrence/change edges. Unknown
queries get direct retrieval plus adjacency only. Routing cannot remove direct
lexical or semantic candidates, and graph traversal never runs without a
direct seed.

Default retrieval bounds are eight direct seeds, one graph hop, four outgoing
edges of any one type per seed, 24 graph-derived candidates total, and the
existing two-round investigation limit. A specifically classified trace may
use two hops, but `RoundBudget.max_graph_hops` remains the hard ceiling. Node
IDs in the current path are tracked to break cycles. Expansion stops when the
candidate budget, context-byte budget, per-facet quota, or latency deadline is
reached. High-degree nodes are sorted by confidence, revision match, source
location, then stable edge ID before their fan-out cap is applied.

## 3. Graph schema and extraction matrix

Every node is `(repository, revision, normalized path, anchor, kind)`. Kinds
are file, chunk, symbol, command, protocol, package, configuration, parser,
artifact, subsystem, and revision occurrence. Stable node IDs are SHA-256 of
that canonical tuple. Every edge stores its kind, source and target IDs,
extractor, source span, confidence class, verification state, and revision.
Cross-revision edges name both immutable revisions.

| Edge | Direction / multiplicity | Deterministic source and precedence | Retrieval benefit | False positives / rejection |
|---|---|---|---|---|
| declaration → definition | one-to-many across conditional builds | compiler metadata, then tree-sitter signature match, then exact pattern | jump from API to implementation | overload or macro ambiguity; reject unless language, name, and compatible signature agree |
| caller → callee | many-to-many | compiler call graph, then tree-sitter resolved lexical scope | implementation path and consumers | function pointers/macros; retain only as `candidate` until symbol-table or exact target verification |
| include/import → imported file | many-to-many | compiler dependency output, tree-sitter, exact normalized include resolution | surrounding declarations and dependencies | generated/include-path ambiguity; reject unresolved or escaping paths |
| command ID → handler | usually one-to-many by revision | existing command/domain parser, then exact switch/table pattern | protocol dispatch trace | reused numeric IDs or conditional handlers; require enum/table provenance in same revision |
| protocol → implementation | many-to-many | domain parser plus verified command/handler edges | cross-layer protocol trace | prose mention; never infer from comments alone |
| QML → backend | many-to-many | QML parser property/signal/slot plus C++ meta-object declaration | UI-to-native trace | same property names; require registered type/context and compatible signal/slot |
| package → firmware API | many-to-many | pkgdesc/Lisp/native-library parsers plus exact imported symbol | package capability and loader path | string mentions; require parsed import or resolved ABI symbol |
| configuration → runtime consumer | one-to-many | typed config/domain parser, exact key lookup and access site | effect of a setting | generic string constants; require normalized key and supported access API |
| parser → produced artifact | one-to-many | build recipe/domain parser and explicit output declaration | source-to-wire/generated trace | incidental writes; require declared artifact or validated magic/schema |
| generated → source file | many-to-many | compiler depfile, generator manifest, deterministic source annotation | recover authoritative source | stale comments; require build metadata or content-checked generator marker |
| adjacent chunk ↔ adjacent chunk | at most two per chunk | structural chunker ordinals in one document/revision | local context continuity | none after artifact adjacency validation |
| symbol/relation → revision occurrence | one per observed revision | tagged-history exact identity/occurrence builder | first/last/change evidence | fuzzy similarity; identity and content digests must validate |
| old occurrence → new occurrence | one-to-many across selected revisions | deterministic history diff and rename evidence | behavior evolution | rename guesses; unverified similarity remains outside the published graph |

Compiler metadata outranks symbol tables, which outrank existing domain
parsers, tree-sitter, and deterministic patterns. Duplicate claims collapse
only when kind, endpoints, and revisions match. The highest-precedence
extractor becomes primary and all agreeing extractors remain provenance.
Contradictory endpoints are both retained as candidates only when language
semantics allow multiplicity; otherwise the edge is rejected with the conflict
and extractor evidence recorded. Confidence is an enum (`verified`,
`structural`, `candidate`), never an opaque floating-point LLM score.

## 4. Optional offline LLM proposals

The no-LLM graph includes every edge above and is complete enough for the
baseline. An offline model may propose only a missing relationship between
already indexed nodes. A proposal contains exact node IDs, edge kind, and
source spans. Rust accepts it only if the edge-specific verifier in the matrix
can independently reconstruct the relationship from immutable source,
compiler, parser, or symbol evidence. Accepted edges are serialized as normal
verified edges with both proposal-model and verifier provenance; rejected
proposals are excluded and counted by reason. Model ID, artifact hash, runtime
commit, prompt schema, seed, and proposal bytes are recorded for reproduction.
No proposal can create nodes, assert semantic equivalence, weaken a conflict,
or bypass the deterministic verifier.

## 5. Immutable artifact

The published artifact is a versioned little-endian binary with a small JSON
manifest:

```text
header | sorted node table | forward CSR offsets | forward edges
       | optional reverse CSR offsets | reverse edge ordinals | string table
```

Nodes sort by stable ID. Edges sort by `(source, kind, target, revision,
extractor)`. `offsets[node_count + 1]` delimit packed fixed-width edges, so a
neighborhood is one contiguous slice. Edge kinds and confidence use `u16` and
`u8`; node ordinals use `u32` until the corpus exceeds that verified bound.
Paths, anchors, extractor names, and revision strings are offsets into a
deduplicated UTF-8 table. Reverse CSR is emitted only for edge families whose
query rules traverse backward.

The manifest pins schema, graph digest, corpus digest, repository/revision
inventory, extractor versions, node/edge counts, section offsets and checksums,
endianness, and required reader version. Loading validates magic, checked
section arithmetic, sorted IDs/adjacency, valid ordinals, UTF-8 ranges,
revision membership, and every checksum before exposing a slice. The runtime
memory-maps or reads the immutable bytes and does no graph mutation.

Incremental builds may cache per-revision extractor outputs by content digest,
but publication always performs the same global sort, validation, and complete
artifact serialization. A changed revision produces a new generation; the
active-generation pointer is switched atomically only after validation.

## 6. Memory and cache locality

For 250,000 nodes and 1,500,000 directed edges, a compact estimate is:

| Layout | Estimated core bytes | Access behavior | Decision |
|---|---:|---|---|
| CSR: 4-byte offsets + 16-byte packed edges | ~25 MB plus strings/nodes | one offset lookup and sequential edge scans | publish/runtime format |
| per-node `Vec<Edge>` | ~24 MB edge payload + ~6 MB vector headers + allocator slack | extra pointer chase/allocation per neighborhood | builder convenience only |
| `HashMap<NodeId, Vec<Edge>>` | commonly >60 MB before strings/nodes | hashing, buckets, pointers, poor locality | reject at runtime |
| graph database | process/index/cache overhead typically dominates this corpus | IPC/query planning and non-deterministic cache state | reject |

An optional reverse CSR adds roughly 7 MB (`u32` offsets and edge ordinals) in
this example. Exact size is a benchmark output, not a promise. The acceptance
gate is no more than 32 bytes of graph core data per directed edge including
amortized offsets, and no more than 20% runtime RSS growth over the same loaded
corpus without graph sections. Bounded traversal should touch tens of
contiguous edges, allowing the offsets and hot adjacency slices to remain in
cache; hash-map or database representations defeat that access pattern.

## 7. Candidate generation, fusion, and ordering

1. Produce lexical and semantic top lists under their existing limits.
2. Fuse them and select up to eight stable direct seeds. Exact-identifier
   lexical hits are protected.
3. For each seed, traverse only routed edge kinds within hop/fan-out/global
   budgets. Record one best path per `(candidate, seed, edge family)` and keep
   alternate paths only as provenance.
4. Add structurally adjacent chunks under the separate adjacency budget.
5. Deduplicate by immutable chunk ID. A direct match remains direct even if a
   graph path also reaches it.
6. Compute graph contribution as a small rank term based on seed rank, hop
   count, verified confidence, and edge-family priority. It cannot lift an
   indirect candidate above an exact-identifier hit and is capped below one
   direct reciprocal-rank contribution.
7. Rerank only the already bounded candidate set, retain per mandatory facet,
   and use stable chunk ID as the last tie-breaker.

There are no floating-point graph walks, embeddings of graph summaries,
community detection, or runtime writes. The same artifact, query, config, and
hardware-independent ordering produce the same candidate identities.

## 8. Result provenance

Every result serializes:

```json
{
  "chunk_id": "chunk-…",
  "repository": "vesc_firmware",
  "revision": "c835e9f…",
  "direct": [{"retriever": "lexical", "rank": 2, "score": 8.4}],
  "graph": [{
    "seed_chunk_id": "chunk-…",
    "path": [{
      "edge_id": "edge-…", "kind": "command_handler",
      "extractor": "vesc-command-parser@1", "source_path": "…",
      "source_span": {"start": 120, "end": 127},
      "revision": "c835e9f…", "verification": "verified"
    }],
    "score_contribution": 0.0125
  }],
  "adjacent": null
}
```

Adjacent provenance instead names the seed, document, ordinal delta, and
revision. User-visible explanations render concise forms such as “direct
symbol match”, “handler reached from command ID”, or “runtime consumer reached
from configuration key”; the machine record remains complete. Coverage audits
retain the repository, revision/era, path/stage, relationship, direct ranks,
rerank decision, graph path, and rejection/verification state.

## 9. Evaluation and go/no-go gates

The locked VESC suite includes exact-symbol, command-to-handler,
protocol-to-implementation, QML-to-backend, package-to-firmware-API,
configuration-to-consumer, parser-to-artifact, generated-to-source, and tagged
history questions. Every query has decisive chunk IDs and required graph paths.
Use the same saved corpus generation and query budgets for all ablations:

1. hybrid lexical + semantic baseline;
2. baseline + adjacency;
3. baseline plus each major edge family independently;
4. baseline + the bounded multi-edge graph;
5. verified offline-model edges, only if any survive verification.

Each generated release report records recall@5/10/20, MRR@10, nDCG@10,
mandatory-facet/path completeness, provenance edge/path precision,
FrontierShortcutRate, artifact bytes, bytes per node/edge, build peak RSS,
runtime retained and peak RSS, cold-load latency, warm query p50/p95, graph-only
expansion p50/p95, candidates expanded, and byte-for-byte rebuild digest.

An edge family ships only if it adds at least one locked decisive result or
mandatory relationship without lowering direct retrieval metrics, maintains
100% provenance correctness and reproducibility, and keeps graph expansion
p95 below 2 ms on the Ryzen 5 8600G and Apple M1 profiles. The whole graph must
stay within 10% of lookup p95, 20% of loaded-corpus RSS, 32 core bytes per edge,
and the configured candidate/context limits. Otherwise the family is rejected
or deferred. Optional LLM proposals additionally must outperform the same
deterministic graph and have zero unverifiable accepted edges.

## 10. Architecture comparison

| Property | Traditional GraphRAG | LightRAG | Hybrid BM25 + vector | CodeGraphRAG |
|---|---|---|---|---|
| extraction | often LLM/entity pipeline | LLM entities/relations | none | compiler/parser/tree-sitter/pattern evidence |
| determinism | low to medium | low | high | high, byte-reproducible |
| code/revision identity | usually weak | document-oriented | chunk metadata | first-class node/edge identity |
| indexing cost | high | high | low/medium | medium, cacheable offline |
| runtime latency | graph/store dependent | multiple store retrieval | lowest | near hybrid via bounded CSR slices |
| memory/operations | graph DB/vector stores | graph + vector + KV stores | simple artifacts | one additional immutable artifact |
| explainability | graph paths, sometimes generated | entity/relation context | direct ranks | exact seed, edge, extractor, span, revision |
| local/embedded fit | poor | poor | strong | strong if gates pass |

Ordinary hybrid retrieval remains the fallback and control. CodeGraphRAG is
valuable only for relationships direct text similarity misses.

## 11. Explicit rejection list

- **LLM entity/relation extraction:** non-reproducible and cannot establish
  symbol or revision identity. Offline proposals are verifier-gated only.
- **Graph databases:** unnecessary for immutable bounded adjacency, add memory,
  deployment, query-planning, and cache-state complexity.
- **Multiple storage backends:** create consistency and generation-skew risks;
  one manifest binds lexical, vectors, history, and graph artifacts.
- **Document summaries:** discard exact anchors and introduce unsupported text.
- **LLM keyword generation:** Rust classification and exact identifiers are
  reproducible, cheaper, and inspectable.
- **Runtime graph mutation:** breaks immutable generation identity and concurrent
  reproducibility; rebuild and atomically publish instead.
- **Complex orchestration/community layers:** no measured code-retrieval value;
  the bounded staged pipeline is sufficient.

## 12. Phased implementation

1. Characterize the hybrid + adjacency baseline and lock relationship queries.
2. Implement artifact IDs, manifest, validation, and adjacency CSR with no
   ranking change; require byte-identical rebuilds.
3. Add high-confidence domain edges (command, package/API, parser/artifact,
   configuration) one family at a time and apply the quality/latency gates.
4. Add compiler/tree-sitter declaration, call, import, QML/backend, and
   generated/source edges only where the toolchain can verify endpoints.
5. Integrate history occurrence/change edges and the fail-closed investigation
   coverage pipeline.
6. Consider optional offline proposals only after deterministic ablations leave
   a measured relationship gap.

Each phase is independently removable. Failure of a gate leaves BM25, dense,
fusion, adjacency, and previously accepted edge families unchanged. This keeps
offline operation, simple deployment, deterministic indexing, low memory, and
low latency as hard properties rather than aspirations.
