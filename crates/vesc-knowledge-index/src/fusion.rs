//! Deterministic fusion of lexical and semantic retrieval candidates.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::{Chunk, ChunkId, LexicalHit, SemanticHit};

const LEXICAL_FLOOR_DEPTH: usize = 2;

/// Controls reciprocal-rank fusion and result diversity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusionConfig {
    /// Reciprocal-rank smoothing constant.
    pub rrf_k: u32,
    /// Maximum number of passages returned from one source document.
    pub max_per_document: usize,
    /// Maximum number of fused hits.
    pub limit: usize,
    /// Keep lexical candidates ahead of semantic-only fallback candidates.
    ///
    /// This is the safe rollout policy for an uncalibrated local model: RRF
    /// still ranks overlapping candidates, while semantic-only passages fill
    /// lexical gaps instead of displacing trusted lexical evidence.
    pub lexical_floor: bool,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60,
            max_per_document: 2,
            limit: 10,
            lexical_floor: false,
        }
    }
}

/// A candidate after lexical/semantic reciprocal-rank fusion.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    pub chunk: Chunk,
    pub score: f64,
    pub lexical_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub lexical_score: Option<f32>,
    pub semantic_similarity: Option<f32>,
    pub exact_identifier: bool,
}

/// Bounded context assembled around one ranked anchor chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedContext {
    /// Anchor text plus any admitted adjacent passages.
    pub passage: String,
    /// Number of neighboring chunks admitted into `passage`.
    pub neighbor_count: usize,
    /// Stable explanation for why expansion occurred or stopped.
    pub reason: Option<String>,
}

/// Expand one anchor by a bounded number of reciprocal adjacent chunks.
///
/// The anchor is always admitted before neighbors. Traversal follows the
/// stored adjacency handles, verifies document identity, and stops at the
/// byte budget without exposing filesystem paths.
#[must_use]
pub fn expand_adjacent_context(
    anchor: &Chunk,
    chunks: &BTreeMap<ChunkId, Chunk>,
    max_neighbors: usize,
    max_bytes: usize,
) -> ExpandedContext {
    if max_bytes == 0 {
        return ExpandedContext {
            passage: String::new(),
            neighbor_count: 0,
            reason: Some("context budget prevented expansion".into()),
        };
    }

    let mut neighbors = Vec::new();
    let mut seen = BTreeSet::from([anchor.chunk_id.clone()]);
    let mut previous = anchor.previous_chunk.clone();
    let mut previous_chunks = Vec::new();
    for _ in 0..max_neighbors {
        let Some(id) = previous.as_ref() else { break };
        let Some(chunk) = chunks.get(id) else { break };
        if chunk.document_id != anchor.document_id || !seen.insert(id.clone()) {
            break;
        }
        previous.clone_from(&chunk.previous_chunk);
        previous_chunks.push(chunk.clone());
    }
    previous_chunks.reverse();
    neighbors.extend(previous_chunks);

    let mut next = anchor.next_chunk.clone();
    for _ in 0..max_neighbors {
        let Some(id) = next.as_ref() else { break };
        let Some(chunk) = chunks.get(id) else { break };
        if chunk.document_id != anchor.document_id || !seen.insert(id.clone()) {
            break;
        }
        next.clone_from(&chunk.next_chunk);
        neighbors.push(chunk.clone());
    }

    let mut passage = String::new();
    append_bounded_piece(&mut passage, &anchor.text, max_bytes);
    let anchor_truncated = passage.len() < anchor.text.len();
    let mut neighbor_count = 0;
    let mut budget_limited = anchor_truncated;
    for neighbor in neighbors {
        if append_bounded_piece(&mut passage, &neighbor.text, max_bytes) {
            neighbor_count += 1;
        } else {
            budget_limited = true;
            break;
        }
    }
    let reason = if neighbor_count > 0 {
        Some(if budget_limited {
            "adjacent chunks included until the context budget".into()
        } else {
            "adjacent chunks included".into()
        })
    } else if budget_limited {
        Some("context budget prevented adjacent expansion".into())
    } else {
        None
    };
    ExpandedContext {
        passage,
        neighbor_count,
        reason,
    }
}

fn append_bounded_piece(output: &mut String, piece: &str, max_bytes: usize) -> bool {
    let separator = if output.is_empty() { "" } else { "\n\n" };
    let required = separator.len().saturating_add(piece.len());
    let available = max_bytes.saturating_sub(output.len());
    if required <= available {
        output.push_str(separator);
        output.push_str(piece);
        return true;
    }
    if output.is_empty() {
        let mut end = max_bytes.min(piece.len());
        while end > 0 && !piece.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&piece[..end]);
    }
    false
}

/// Fuse ranked candidates with stable ordering and passage diversity.
///
/// Candidates are joined by stable chunk ID. A passage can contribute to both
/// signals once, exact identifier hits are protected ahead of ordinary hits,
/// and identical content digests are emitted only once.
#[must_use]
pub fn fuse_candidates(
    lexical: &[LexicalHit],
    semantic: &[SemanticHit],
    chunks: &BTreeMap<ChunkId, Chunk>,
    config: FusionConfig,
) -> Vec<FusedHit> {
    let k = f64::from(config.rrf_k);
    let mut candidates: BTreeMap<ChunkId, Candidate> = BTreeMap::new();

    for (rank, hit) in lexical.iter().enumerate() {
        let entry = candidates
            .entry(hit.chunk.chunk_id.clone())
            .or_insert_with(|| Candidate::new(hit.chunk.clone()));
        entry.lexical_rank = Some(rank + 1);
        entry.lexical_score = Some(hit.score);
        entry.exact_identifier |= hit.exact_identifier;
        entry.score += 1.0 / (k + f64::from(u32::try_from(rank).unwrap_or(u32::MAX)) + 1.0);
    }

    for (rank, hit) in semantic.iter().enumerate() {
        let Some(chunk) = chunks.get(&hit.chunk_id) else {
            continue;
        };
        let entry = candidates
            .entry(hit.chunk_id.clone())
            .or_insert_with(|| Candidate::new(chunk.clone()));
        entry.semantic_rank = Some(rank + 1);
        entry.semantic_similarity = Some(hit.similarity);
        entry.score += 1.0 / (k + f64::from(u32::try_from(rank).unwrap_or(u32::MAX)) + 1.0);
    }

    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|left, right| {
        right
            .exact_identifier
            .cmp(&left.exact_identifier)
            .then_with(|| {
                if left.exact_identifier && right.exact_identifier {
                    left.lexical_rank.cmp(&right.lexical_rank)
                } else {
                    Ordering::Equal
                }
            })
            // Registered catalog evidence has stable public identifiers and
            // should not be buried beneath anonymous source-code passages.
            .then_with(|| {
                left.chunk
                    .legacy_ids
                    .is_empty()
                    .cmp(&right.chunk.legacy_ids.is_empty())
            })
            .then_with(|| {
                if config.lexical_floor {
                    let left_protected = left
                        .lexical_rank
                        .is_some_and(|rank| rank <= LEXICAL_FLOOR_DEPTH);
                    let right_protected = right
                        .lexical_rank
                        .is_some_and(|rank| rank <= LEXICAL_FLOOR_DEPTH);
                    right_protected.cmp(&left_protected)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| {
                if config.lexical_floor {
                    match (left.lexical_rank, right.lexical_rank) {
                        (Some(left), Some(right))
                            if left <= LEXICAL_FLOOR_DEPTH && right <= LEXICAL_FLOOR_DEPTH =>
                        {
                            left.cmp(&right)
                        }
                        _ => Ordering::Equal,
                    }
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });

    let mut document_counts = BTreeMap::<_, usize>::new();
    let mut content_ids = BTreeSet::new();
    let mut output = Vec::new();
    for candidate in candidates {
        if output.len() >= config.limit.max(1) {
            break;
        }
        if config.max_per_document == 0 {
            break;
        }
        let document_count = document_counts
            .entry(candidate.chunk.document_id.clone())
            .or_default();
        if *document_count >= config.max_per_document {
            continue;
        }
        if !content_ids.insert(candidate.chunk.content_digest.clone()) {
            continue;
        }
        *document_count += 1;
        output.push(candidate.into_hit());
    }
    output
}

struct Candidate {
    chunk: Chunk,
    score: f64,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
    lexical_score: Option<f32>,
    semantic_similarity: Option<f32>,
    exact_identifier: bool,
}

impl Candidate {
    const fn new(chunk: Chunk) -> Self {
        Self {
            chunk,
            score: 0.0,
            lexical_rank: None,
            semantic_rank: None,
            lexical_score: None,
            semantic_similarity: None,
            exact_identifier: false,
        }
    }

    fn into_hit(self) -> FusedHit {
        FusedHit {
            chunk: self.chunk,
            score: self.score,
            lexical_rank: self.lexical_rank,
            semantic_rank: self.semantic_rank,
            lexical_score: self.lexical_score,
            semantic_similarity: self.semantic_similarity,
            exact_identifier: self.exact_identifier,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{NormalizedDocument, RepositoryId, Revision, SourceKind};

    fn chunk(document: &str, ordinal: u32, text: &str, identifier: &str) -> Chunk {
        let mut normalized = NormalizedDocument::new(
            document,
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            format!("docs/{document}.md"),
            "text/markdown",
            text,
        )
        .expect("document");
        normalized.identifiers.insert(identifier.into());
        Chunk::from_document(&normalized, ordinal, text.into(), Vec::new(), None).expect("chunk")
    }

    fn map(chunks: &[Chunk]) -> BTreeMap<ChunkId, Chunk> {
        chunks
            .iter()
            .cloned()
            .map(|chunk| (chunk.chunk_id.clone(), chunk))
            .collect()
    }

    #[test]
    fn overlapping_candidates_get_both_rank_contributions() {
        let first = chunk("first", 0, "same", "first_id");
        let second = chunk("second", 0, "other", "second_id");
        let hits = fuse_candidates(
            &[LexicalHit {
                chunk: first.clone(),
                score: 2.0,
                exact_identifier: false,
            }],
            &[
                SemanticHit {
                    chunk_id: first.chunk_id.clone(),
                    similarity: 0.9,
                },
                SemanticHit {
                    chunk_id: second.chunk_id.clone(),
                    similarity: 0.8,
                },
            ],
            &map(&[first.clone(), second]),
            FusionConfig::default(),
        );
        assert_eq!(hits[0].chunk.chunk_id, first.chunk_id);
        assert_eq!(hits[0].lexical_rank, Some(1));
        assert_eq!(hits[0].semantic_rank, Some(1));
    }

    #[test]
    fn rrf_known_two_list_example() {
        let first = chunk("first", 0, "first", "first_id");
        let second = chunk("second", 0, "second", "second_id");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: first.clone(),
                    score: 10.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: second.clone(),
                    score: 1.0,
                    exact_identifier: false,
                },
            ],
            &[
                SemanticHit {
                    chunk_id: first.chunk_id.clone(),
                    similarity: 0.9,
                },
                SemanticHit {
                    chunk_id: second.chunk_id.clone(),
                    similarity: 0.8,
                },
            ],
            &map(&[first.clone(), second]),
            FusionConfig::default(),
        );
        assert!((hits[0].score - 2.0 / 61.0).abs() < f64::EPSILON);
        assert!((hits[1].score - 2.0 / 62.0).abs() < f64::EPSILON);
        assert_eq!(hits[0].chunk.chunk_id, first.chunk_id);
    }

    #[test]
    fn exact_identifier_is_protected_and_duplicates_are_suppressed() {
        let exact = chunk("exact", 0, "same", "target");
        let duplicate = chunk("duplicate", 0, "same", "other");
        let other = chunk("other", 0, "different", "other_two");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: exact.clone(),
                    score: 0.1,
                    exact_identifier: true,
                },
                LexicalHit {
                    chunk: duplicate.clone(),
                    score: 10.0,
                    exact_identifier: false,
                },
            ],
            &[SemanticHit {
                chunk_id: other.chunk_id.clone(),
                similarity: 1.0,
            }],
            &map(&[exact.clone(), duplicate, other]),
            FusionConfig {
                limit: 10,
                ..FusionConfig::default()
            },
        );
        assert_eq!(hits[0].chunk.chunk_id, exact.chunk_id);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn exact_identifiers_keep_lexical_order_after_fusion() {
        let first = chunk("first", 0, "first", "first_id");
        let second = chunk("second", 0, "second", "second_id");
        let third = chunk("third", 0, "third", "third_id");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: first.clone(),
                    score: 1.0,
                    exact_identifier: true,
                },
                LexicalHit {
                    chunk: second.clone(),
                    score: 1.0,
                    exact_identifier: true,
                },
            ],
            &[
                SemanticHit {
                    chunk_id: second.chunk_id.clone(),
                    similarity: 1.0,
                },
                SemanticHit {
                    chunk_id: third.chunk_id.clone(),
                    similarity: 0.9,
                },
                SemanticHit {
                    chunk_id: first.chunk_id.clone(),
                    similarity: 0.8,
                },
            ],
            &map(&[first.clone(), second, third]),
            FusionConfig::default(),
        );

        assert_eq!(hits[0].chunk.chunk_id, first.chunk_id);
        assert_eq!(
            hits[1].chunk.identifiers.first().map(String::as_str),
            Some("second_id")
        );
    }

    #[test]
    fn registered_evidence_precedes_anonymous_source_passages() {
        let mut registered = chunk("registered", 0, "registered", "registered_id");
        registered.legacy_ids.push("catalog.registered".into());
        let source = chunk("source", 0, "source", "source_id");
        let hits = fuse_candidates(
            &[],
            &[
                SemanticHit {
                    chunk_id: source.chunk_id.clone(),
                    similarity: 1.0,
                },
                SemanticHit {
                    chunk_id: registered.chunk_id.clone(),
                    similarity: 0.9,
                },
            ],
            &map(&[registered.clone(), source]),
            FusionConfig::default(),
        );

        assert_eq!(hits[0].chunk.chunk_id, registered.chunk_id);
    }

    #[test]
    fn document_cap_is_deterministic() {
        let mut document = NormalizedDocument::new(
            "doc",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/doc.md",
            "text/markdown",
            "one two three",
        )
        .expect("document");
        document.identifiers.insert("one".into());
        let first = Chunk::from_document(&document, 0, "one".into(), Vec::new(), None)
            .expect("first chunk");
        let second = Chunk::from_document(&document, 1, "two".into(), Vec::new(), None)
            .expect("second chunk");
        let third = Chunk::from_document(&document, 2, "three".into(), Vec::new(), None)
            .expect("third chunk");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: first.clone(),
                    score: 3.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: second.clone(),
                    score: 2.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: third.clone(),
                    score: 1.0,
                    exact_identifier: false,
                },
            ],
            &[],
            &map(&[first.clone(), second, third]),
            FusionConfig {
                max_per_document: 1,
                ..FusionConfig::default()
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.chunk_id, first.chunk_id);
    }

    #[test]
    fn document_cap_backfills_other_sources() {
        let document = NormalizedDocument::new(
            "first",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/first.md",
            "text/markdown",
            "first third",
        )
        .expect("document");
        let first =
            Chunk::from_document(&document, 0, "first".into(), Vec::new(), None).expect("first");
        let third =
            Chunk::from_document(&document, 1, "third".into(), Vec::new(), None).expect("third");
        let second = chunk("second", 0, "second", "second_id");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: first.clone(),
                    score: 3.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: third.clone(),
                    score: 2.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: second.clone(),
                    score: 1.0,
                    exact_identifier: false,
                },
            ],
            &[],
            &map(&[first.clone(), second.clone(), third]),
            FusionConfig {
                limit: 2,
                max_per_document: 1,
                ..FusionConfig::default()
            },
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.chunk_id, first.chunk_id);
        assert_eq!(hits[1].chunk.chunk_id, second.chunk_id);
    }

    #[test]
    fn one_sided_candidate_is_retained() {
        let lexical_only = chunk("lexical", 0, "lexical", "lexical_id");
        let semantic_only = chunk("semantic", 0, "semantic", "semantic_id");
        let hits = fuse_candidates(
            &[LexicalHit {
                chunk: lexical_only.clone(),
                score: 1.0,
                exact_identifier: false,
            }],
            &[SemanticHit {
                chunk_id: semantic_only.chunk_id.clone(),
                similarity: 0.9,
            }],
            &map(&[lexical_only, semantic_only]),
            FusionConfig::default(),
        );
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|hit| hit.lexical_rank.is_none()));
        assert!(hits.iter().any(|hit| hit.semantic_rank.is_none()));
    }

    #[test]
    fn fusion_tie_breaks_by_chunk_id() {
        let first = chunk("first", 0, "first", "first_id");
        let second = chunk("second", 0, "second", "second_id");
        let hits = fuse_candidates(
            &[
                LexicalHit {
                    chunk: first.clone(),
                    score: 1.0,
                    exact_identifier: false,
                },
                LexicalHit {
                    chunk: second.clone(),
                    score: 1.0,
                    exact_identifier: false,
                },
            ],
            &[
                SemanticHit {
                    chunk_id: second.chunk_id.clone(),
                    similarity: 1.0,
                },
                SemanticHit {
                    chunk_id: first.chunk_id.clone(),
                    similarity: 1.0,
                },
            ],
            &map(&[first.clone(), second.clone()]),
            FusionConfig::default(),
        );
        let expected = std::cmp::min(first.chunk_id, second.chunk_id);
        assert_eq!(hits[0].chunk.chunk_id, expected);
    }

    #[test]
    fn adjacent_expansion_obeys_budget() {
        let document = NormalizedDocument::new(
            "doc",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/doc.md",
            "text/markdown",
            "first passage middle passage last passage",
        )
        .expect("document");
        let mut first =
            Chunk::from_document(&document, 0, "first passage".into(), Vec::new(), None)
                .expect("first");
        let mut middle =
            Chunk::from_document(&document, 1, "middle passage".into(), Vec::new(), None)
                .expect("middle");
        let mut last = Chunk::from_document(&document, 2, "last passage".into(), Vec::new(), None)
            .expect("last");
        first.next_chunk = Some(middle.chunk_id.clone());
        middle.previous_chunk = Some(first.chunk_id.clone());
        middle.next_chunk = Some(last.chunk_id.clone());
        last.previous_chunk = Some(middle.chunk_id.clone());
        let chunks = map(&[first.clone(), middle.clone(), last]);
        let context = expand_adjacent_context(
            &middle,
            &chunks,
            1,
            middle.text.len() + 2 + first.text.len(),
        );
        assert!(context.passage.contains("middle passage"));
        assert!(context.passage.contains("first passage"));
        assert!(!context.passage.contains("last passage"));
        assert_eq!(context.neighbor_count, 1);
        assert!(context.reason.is_some());
    }
}
