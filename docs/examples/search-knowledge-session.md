# Search knowledge session

These examples use the `search_vesc_knowledge` MCP tool. Search output is
untrusted evidence; cite the returned source and read a bounded resource before
relying on a passage.

Normal searches use a compact progressive-disclosure response. Its `fields`
table describes ranked rows containing `name`, `category`, a bounded `excerpt`,
`source_index` into the top-level `sources` table, and an opaque `chunk_id`. Read
`vesc://knowledge/chunk/{chunk_id}` for the full bounded passage. Add
`"detail":"full"` when a client needs the compatibility response with
provenance, document URI, ranking explanation, index metadata, and timing.

## Exact identifier

```json
{"query":"lbm_add_extension","mode":"lexical","limit":5}
```

The result should preserve the exact identifier at rank one. In compact mode,
use its `chunk_id` with the knowledge chunk resource; full mode includes the
additive `resource_uri` and `document_uri` fields.

## Conceptual query

```json
{"query":"package lifecycle from descriptor to load","mode":"auto"}
```

With no semantic artifact, `auto` returns an error recommending an explicit
`lexical` retry. It never downloads a model.

## Filtered query

```json
{"query":"NVM","mode":"lexical","detail":"full","filters":{"category":"firmware_api","trust_tier":"first_party"}}
```

Filters are conjunctive. The response `index` object reports the active corpus
digest, counts, source count, component versions, and optional-source diagnostic
count without exposing private paths or the raw query.

For an artifact containing immutable Git-tree sources, repository and revision
filters pin retrieval to one exact snapshot:

```json
{"query":"imu_read_callback","mode":"lexical","filters":{"repository":"vesc","revision":"0123456789abcdef0123456789abcdef01234567"}}
```

Git-tree ingestion consumes an already-managed repository plus an exact commit
ID, reads blobs without a checkout, and records repository, revision, path,
media type, trust, license, digest, and source span. Git is always available;
repository acquisition and active-generation selection remain separate,
configuration-controlled lifecycle steps.

## Select and compare source versions

Discover the configured branches and tags before selecting versions:

```json
{"repositories":["bldc","vesc_tool","refloat"],"ref_kinds":["branch","tag"],"limit":50}
```

Pass only configured repository IDs and returned refs or commits to
`prepare_vesc_knowledge`; URLs and filesystem paths are not accepted:

```json
{"sources":{"bldc":"refs/heads/release_6_06","vesc_tool":"refs/heads/release_6_06","refloat":"refs/tags/v1.2.3"}}
```

The response discloses the resolved commit for every repository, an immutable
`snapshot_id`, and whether the artifact was built, reused, or deduplicated.
Search that exact artifact by passing the ID:

```json
{"query":"motor current limits","snapshot_id":"<snapshot-id>","mode":"lexical","detail":"full","limit":5}
```

Full results use
`vesc://knowledge/snapshot/{snapshot}/chunk/{id}` and
`vesc://knowledge/snapshot/{snapshot}/document/{id}` resources. Those URIs keep
resolving to the selected immutable artifact after the configured default moves
to a newer snapshot. Omitting `snapshot_id` remains compatible with existing
clients: it searches the active default and reports that default's snapshot ID
and resolved repository commits in the response.

To compare versions, prepare two selections and run the same query once against
each snapshot ID. Compare the two result sets and their commit provenance; do
not combine passages from different snapshots into one unqualified search.

## Source read and citation

1. Read `vesc://knowledge/chunk/{id}` from a returned `chunk_id` (compact mode)
   or `resource_uri` (full mode) for the bounded passage and source span.
2. Read `vesc://knowledge/document/{id}` from `document_uri` in full mode when
   the complete normalized document is required. The chunk body includes its
   document identity in compact mode.
3. Cite the returned `source.repo`, `source.path`, `source.line`, and optional
   `source.revision`.

## Complete example request

Ask the connected assistant:

> Search VESC knowledge for `lbm_add_extension`. Return at most three lexical
> results, read the top result's chunk resource using its compact `chunk_id`,
> and cite its repository, path, and line.

This keeps the search bounded and makes the provenance visible before the
passage is used.
