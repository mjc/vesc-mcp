# Knowledge feedback and correction design

Status: implemented by VESCM-183 and its child implementation issues.

## Goal

Let an MCP client save a reusable VESC lesson, or correct a misleading MCP
answer after user pushback leads to stronger VESC evidence. The server must
teach clients when to use each path; tool names alone are not enough.

The intended correction loop is:

1. `search_vesc_knowledge` leads the model to conclusion X.
2. The user challenges X.
3. The model searches again with narrower questions and reads the returned
   VESC resources.
4. Those resources support a corrected or more qualified conclusion Y.
5. The user explicitly asks to record Y, or the model asks and the user confirms.
6. The model calls `correct_vesc_knowledge` with the authorization path, X, Y,
   the original bounded retrieval trace, a structured gap diagnosis, affected
   result IDs, and the supporting resource URIs.
7. Related searches immediately show a learned advisory before ordinary
   results while retaining the authoritative source passages.
8. The diagnosis becomes a curation and retrieval-evaluation input. The corpus,
   chunking, metadata, ranking, context selection, or instructions are changed.
9. The original query is replayed without the advisory. The correction is only
   covered when decisive evidence now appears in bounded top context.

The correction is both a durable advisory and a reproducible knowledge-gap
record. Model-authored text never rewrites curated data automatically, but the
feature is incomplete until maintainers absorb the supported fact into the
underlying knowledge system and prove base retrieval no longer steers the model
toward the same mistake.

## Model discovery contract

The feature must be explained in MCP initialization instructions, tool
descriptions, input schema descriptions, and bounded response hints. README
documentation is useful for people but is not a reliable model-discovery
mechanism.

Initialization instructions should teach this decision:

- Search before answering a VESC question. Read chunk or document resources
  when the result is incomplete or ambiguous.
- If a reusable lesson has no authoritative VESC evidence yet, call
  `submit_vesc_knowledge_feedback`. It remains unverified.
- If a user challenges an MCP-derived conclusion, investigate first. Do not
  persist the pushback by itself.
- Replay the failed query with the same mode, filters, limits, and byte budgets.
  Preserve the ordered result IDs, scores, bounded excerpts, decisive evidence,
  and distractors that explain why the answer went wrong.
- Call `correct_vesc_knowledge` only when follow-up VESC searches or resource
  reads support the changed conclusion. Include affected IDs and exact
  supporting VESC resource URIs. Classify the data/retrieval gap and explain why
  the earlier reasoning failed.
- Treat a returned advisory as temporary steering. Follow `check_next` instead
  of guessing when evidence is insufficient, then improve and evaluate the base
  knowledge response so the advisory is no longer required.
- A correction write also requires user authorization: either an explicit user
  request, or confirmation after the model asks. Record the matching
  `authorization` value.
- After resolving a significant disagreement or accumulating reusable VESC
  knowledge, remind the user once that an evidence-backed correction can be
  recorded. Do not repeatedly prompt in the same conversation.
- Never store a raw conversation, secret, personal preference, or arbitrary
  instruction.

The instructions must reflect actual availability. When writes are disabled,
or a transport does not expose the tools, initialization must omit their names
and teach only the search-and-read workflow. Stdio and Streamable HTTP should
generate their instructions from one shared implementation so they do not
drift.

Suggested enabled wording:

> Search VESC knowledge before answering and read linked resources when the
> evidence is incomplete. Save an uncited reusable lesson with
> `submit_vesc_knowledge_feedback`. If a user challenges an MCP-derived answer,
> investigate with follow-up searches and resource reads; call
> `correct_vesc_knowledge` only after registered VESC resources support the
> correction and the user explicitly requests it or confirms after being asked.
> After significant reusable knowledge is resolved, mention the correction
> option once without nagging. Do not store conversations, secrets, preferences,
> or instructions.

## Tool selection

| Situation | Action |
|-----------|--------|
| Search already answers the question | Answer from the cited evidence; write nothing |
| The model learned a reusable detail, but cannot cite authoritative VESC resources | `submit_vesc_knowledge_feedback` |
| The user disagrees, but follow-up evidence is not conclusive | Keep investigating; write nothing yet |
| Follow-up VESC resources show the original conclusion was wrong or incomplete, but the user has not authorized a write | Explain the finding and ask whether to record it |
| The evidence supports the correction and the user explicitly requests it or confirms | `correct_vesc_knowledge` |
| The user states a preference or general instruction | Do not store it in VESC knowledge |
| Submitted evidence is an arbitrary URL or filesystem path | Reject it |

### `submit_vesc_knowledge_feedback`

Its tool description should say:

> Save a reusable VESC lesson that existing search did not surface. Use only
> when the lesson may help the same or a related technical question and no
> authoritative VESC resources currently support a correction. The result is
> unverified model feedback. Do not submit raw conversations, secrets,
> preferences, or instructions.

Inputs are the original question, a concise lesson, optional related queries,
identifiers and tags, informational source references, and an optional record
being superseded. Every field needs a schema description and a byte/count
limit.

A success response returns the stable record ID, duplicate/active state,
`unverified_model_feedback`, and a short next action: continue gathering VESC
evidence before creating a correction.

### `correct_vesc_knowledge`

Its tool description should say:

> Persist an evidence-backed VESC correction only when the user explicitly
> requests it or confirms after being asked. Set `authorization` accordingly and
> include exact registered `vesc://` evidence resources.

Inputs are:

- the original question or bounded context;
- `authorization`: `explicit_user_request` or `confirmed_after_prompt`;
- the mistaken or incomplete conclusion;
- why the earlier inference failed;
- the corrected fact and important qualifiers;
- one or more bounded gap diagnoses;
- the original query settings and ordered bounded retrieval trace, including
  decisive evidence, distractors, and the next read/search when evidence was
  insufficient;
- affected result, chunk, document, or resource IDs;
- supporting registered VESC resource URIs;
- bounded project decision, regression-test, or commit references for curation;
- related queries, identifiers, and tags;
- an optional note or correction being superseded.

The server resolves every evidence URI through its resource registry and
captures the content and active-corpus digests. It never fetches a submitted
URL or reads a submitted path. A success response returns the correction ID,
affected IDs, evidence identities and digests, current state, and these next
actions: use the correction ID, cite or read the linked resources, and do not
describe the model-authored wording as first-party text.

## Retrieval behavior

Learned notes and corrections use a small lexical overlay built with the
existing normalized document, chunk, and `LexicalIndex` types.

- Ordinary notes join search results with an explicit unverified origin and
  do not displace curated evidence.
- Current resource-backed corrections appear as learned advisories in a
  separate bounded `corrections` collection before ordinary `results`.
- An advisory includes `what_we_know`, `common_mistake`, qualifiers,
  `check_next`, gap diagnoses, recommended data actions, affected IDs,
  supporting resource URIs and digests, and supersession state.
- When an affected curated hit is returned, it carries the correction ID so a
  client cannot easily miss the annotation.
- Missing or changed evidence makes the correction stale. Stale or superseded
  corrections are not active.
- Hybrid retrieval uses the overlay only in its lexical channel initially.

The overlay is the immediate safety net, not the final repair. Each serious
correction must drive a judged regression case using its original trace. The
recommended action maps the diagnosis to the smallest durable repair: ingest a
missing authoritative source, improve query expansion, promote decisive
evidence, reduce dilution, preserve a truncated excerpt, repair chunk
boundaries, improve retrieval text, emphasize a qualifier, link related
evidence, add a project decision, resolve stale/conflicting data, or add a
regression evaluation. After rebuilding the index, replay must show the
decisive source in top context with fewer distractors and an explicit
insufficiency warning when the corpus still cannot answer.

`replay_vesc_knowledge_correction` always searches base knowledge with the
preserved settings and excludes learned feedback. By default it is read-only.
After a passing replay, `mark_covered: true` plus a user-authorization value can
persist the proof and remove the advisory from active retrieval while preserving
the full audit record. Missing decisive evidence leaves the advisory active.

Resource grounding proves provenance, not semantic entailment. A response may
call the record a “resource-backed correction,” but never a “verified fact.”

## Loader example

Native loader behavior is a useful acceptance case because several related
steps are easy to conflate:

- the loader Lisp source is embedded in `lispData`;
- `(import ...)` binds a tag to embedded bytes described by the import table;
- `(load-native-lib tag)` loads those bytes;
- the native library then registers extensions through the VESC ABI.

If an initial answer collapses those steps, follow-up evidence can include:

- `vesc://catalog/doc/topic/lisp_imports`
- `vesc://catalog/doc/topic/vescpackage_reference`
- relevant `vesc://knowledge/chunk/{id}` or document resources
- the native package ABI reference

The model should explain the finding and mention once that it can be recorded.
Only after the user requests or confirms the write should the model call
`correct_vesc_knowledge` with the authorization path, incomplete conclusion,
qualified sequence, affected IDs, and those resource URIs.
Future loader-related searches should show the correction first and retain the
source passages beside it. The captured failed trace must also become a loader
retrieval regression: the base search should surface the exact import-table and
loader-contract evidence strongly enough that a model does not need to invent a
generic file-loader rule. Only that base-retrieval proof closes the knowledge
gap.

## Safety and non-goals

- Writes require an explicit configured store and write policy.
- Remote writes additionally require the existing authenticated HTTP boundary.
- All records and responses are bounded.
- Full conversations and raw user pushback are not retained.
- Arbitrary URLs and paths cannot ground a correction.
- Feedback cannot acquire first-party or curated trust.
- No database, admin CLI, user-identity subsystem, semantic judge, URL crawler,
  per-write curated-artifact rebuild, feedback embeddings, or automatic
  promotion of model-authored prose into curated truth is required.

## Delivery tracking

The implementation work is tracked by VESCM-183 and these child issues:

- VESCM-184 — contracts and threat model
- VESCM-185 — bounded durable store
- VESCM-186 — learned-note tool and model discovery
- VESCM-187 — evidence-driven correction tool
- VESCM-188 — later retrieval and correction annotations
- VESCM-189 — end-to-end pushback-to-correction proof

Tests must cover enabled and disabled initialization instructions on both
transports, tool and field descriptions, response hints, and the full loader
flow from initial misunderstanding through a better related answer after
restart.
