//! Fielded lexical retrieval over normalized chunks.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

use crate::corpus::{Chunk, ChunkId, ContentDigest, SourceKind, TrustTier};
use crate::{Category, RepositoryId, Revision};

/// Typed filters applied after Tantivy candidate retrieval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LexicalFilters {
    pub category: Option<Category>,
    pub repository: Option<RepositoryId>,
    pub revision: Option<Revision>,
    pub source_kind: Option<SourceKind>,
    pub trust_tier: Option<TrustTier>,
    pub tags: Vec<String>,
}

impl LexicalFilters {
    /// Returns whether a chunk satisfies every configured filter.
    #[must_use]
    pub fn matches(&self, chunk: &Chunk) -> bool {
        matches_filters(chunk, self)
    }
}

/// A ranked lexical hit with an opaque BM25 score and exact-match marker.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalHit {
    pub chunk: Chunk,
    pub score: f32,
    pub exact_identifier: bool,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LexicalError {
    #[error("failed to create lexical writer: {0}")]
    Writer(#[source] tantivy::TantivyError),
    #[error("failed to commit lexical index: {0}")]
    Commit(#[source] tantivy::TantivyError),
    #[error("failed to build lexical query")]
    EmptyQuery,
    #[error("failed to search lexical index: {0}")]
    Search(#[source] tantivy::TantivyError),
    #[error("lexical document is missing chunk id")]
    MissingChunkId,
    #[error("lexical artifact I/O failed: {0}")]
    Io(String),
    #[error("lexical artifact is invalid: {0}")]
    Artifact(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexicalArtifact {
    schema: u16,
    chunks: Vec<Chunk>,
}

#[derive(Debug, Serialize)]
struct LexicalArtifactRef<'a> {
    schema: u16,
    chunks: Vec<&'a Chunk>,
}

struct DigestingWriter<W> {
    inner: W,
    digest: Sha256,
    bytes: u64,
}

impl<W> DigestingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> (ContentDigest, u64) {
        let digest = self.digest.finalize();
        let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
        encoded.push_str("sha256:");
        for byte in digest {
            encoded.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
            encoded.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
        }
        (
            ContentDigest::try_from(encoded).expect("sha256 digest is valid"),
            self.bytes,
        )
    }
}

impl<W: Write> Write for DigestingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// In-memory fielded lexical index. Artifact persistence is added by the lifecycle phase.
pub struct LexicalIndex {
    index: Index,
    reader: IndexReader,
    fields: LexicalFields,
    chunks: BTreeMap<ChunkId, Chunk>,
}

#[derive(Clone, Copy)]
struct LexicalFields {
    title: Field,
    path: Field,
    identifiers: Field,
    identifiers_raw: Field,
    body: Field,
    tags: Field,
    chunk_id: Field,
    category: Field,
    repository: Field,
    trust_tier: Field,
}

impl LexicalIndex {
    /// Builds an in-memory Tantivy index from chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Writer`] or [`LexicalError::Commit`] when Tantivy
    /// cannot construct or commit the index.
    pub fn build(chunks: &[Chunk]) -> Result<Self, LexicalError> {
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(15_000_000).map_err(LexicalError::Writer)?;
        let mut chunk_map = BTreeMap::new();
        for chunk in chunks {
            add_chunk(&writer, fields, chunk);
            chunk_map.insert(chunk.chunk_id.clone(), chunk.clone());
        }
        writer.commit().map_err(LexicalError::Commit)?;
        let reader = index.reader().map_err(LexicalError::Writer)?;
        Ok(Self {
            index,
            reader,
            fields,
            chunks: chunk_map,
        })
    }

    /// Writes a deterministic, versioned lexical source artifact.
    ///
    /// Tantivy remains an implementation detail and is rebuilt in memory on
    /// load; the compact source artifact makes cache invalidation explicit.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Io`] when the file cannot be written or
    /// [`LexicalError::Artifact`] when a chunk cannot be serialized.
    pub fn write_artifact(&self, path: &Path) -> Result<(), LexicalError> {
        self.write_artifact_with_digest(path).map(|_| ())
    }

    /// Writes the artifact and returns the digest and exact byte length without
    /// rereading the file.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when serialization or writing fails.
    pub fn write_artifact_with_digest(
        &self,
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        Self::write_chunk_refs_artifact_with_digest(self.chunks.values(), path)
    }

    /// Writes chunks as a deterministic lexical source artifact without
    /// constructing the transient Tantivy index used for queries.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when serialization or writing fails.
    pub(crate) fn write_chunks_artifact_with_digest(
        chunks: &[Chunk],
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        Self::write_chunk_refs_artifact_with_digest(chunks, path)
    }

    pub(crate) fn write_chunk_refs_artifact_with_digest<'a>(
        chunks: impl IntoIterator<Item = &'a Chunk>,
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        let artifact = LexicalArtifactRef {
            schema: 1,
            chunks: chunks.into_iter().collect(),
        };
        let file = File::create(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let mut writer = DigestingWriter::new(BufWriter::new(file));
        serde_json::to_writer(&mut writer, &artifact)
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        writer
            .flush()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        Ok(writer.finish())
    }

    /// Loads and validates a deterministic lexical source artifact.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Io`] for read failures, [`LexicalError::Artifact`]
    /// for malformed or incompatible JSON, or normal build errors.
    pub fn open_artifact(path: &Path) -> Result<Self, LexicalError> {
        let artifact = Self::read_artifact(path)?;
        Self::build(&artifact.chunks)
    }

    /// Reads the compact lexical source artifact without constructing Tantivy.
    ///
    /// Provider benchmarks use this to select a bounded sample without paying
    /// the full-corpus index construction cost or including it in RSS results.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Io`] for read failures or
    /// [`LexicalError::Artifact`] for malformed or incompatible JSON.
    pub fn read_artifact_chunks(path: &Path) -> Result<Vec<Chunk>, LexicalError> {
        Ok(Self::read_artifact(path)?.chunks)
    }

    fn read_artifact(path: &Path) -> Result<LexicalArtifact, LexicalError> {
        let bytes = std::fs::read(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let artifact: LexicalArtifact = serde_json::from_slice(&bytes)
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        if artifact.schema != 1 {
            return Err(LexicalError::Artifact(format!(
                "unsupported lexical schema {}",
                artifact.schema
            )));
        }
        Ok(artifact)
    }

    /// Searches title, identifiers, headings/body, and tags with conjunctive term matching.
    ///
    /// Exact identifier matches are promoted after BM25 scoring; ties are broken
    /// by stable chunk ID.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::EmptyQuery`] for an empty token set or
    /// [`LexicalError::Search`] when Tantivy rejects the query.
    pub fn search(
        &self,
        query: &str,
        filters: &LexicalFilters,
        limit: usize,
    ) -> Result<Vec<LexicalHit>, LexicalError> {
        let query_text = query.to_owned();
        let terms = query_terms(query);
        if terms.is_empty() {
            return Err(LexicalError::EmptyQuery);
        }
        let raw_terms = raw_query_terms(query);
        let raw_term_count = raw_terms.len();
        let term_occur = if raw_term_count > 2 {
            Occur::Should
        } else {
            Occur::Must
        };
        let term_clauses: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|term| {
                let field_clauses: Vec<(Occur, Box<dyn Query>)> = [
                    self.fields.title,
                    self.fields.path,
                    self.fields.identifiers,
                    self.fields.body,
                    self.fields.tags,
                ]
                .into_iter()
                .map(|field| {
                    let term_query: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(field, term),
                        IndexRecordOption::WithFreqs,
                    ));
                    (Occur::Should, term_query)
                })
                .collect();
                (
                    query_term_occur(term, &raw_terms, raw_term_count, term_occur),
                    Box::new(BooleanQuery::new(field_clauses)) as Box<dyn Query>,
                )
            })
            .collect();
        let query = BooleanQuery::new(vec![
            (
                Occur::Should,
                Box::new(BooleanQuery::new(term_clauses)) as Box<dyn Query>,
            ),
            (
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.identifiers_raw, query_text.trim()),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
        let searcher = self.reader.searcher();
        let candidate_limit = limit.max(1).saturating_mul(10).min(100);
        let docs = searcher
            .search(
                &query,
                &TopDocs::with_limit(candidate_limit).order_by_score(),
            )
            .map_err(LexicalError::Search)?;
        let exact = query_text.to_ascii_lowercase();
        let mut hits = Vec::new();
        for (score, address) in docs {
            let document = searcher
                .doc::<TantivyDocument>(address)
                .map_err(LexicalError::Search)?;
            let Some(id) = document
                .get_first(self.fields.chunk_id)
                .and_then(|value| value.as_str())
            else {
                return Err(LexicalError::MissingChunkId);
            };
            let id = ChunkId::try_from(id).map_err(|_| LexicalError::MissingChunkId)?;
            let Some(chunk) = self.chunks.get(&id) else {
                continue;
            };
            if !matches_filters(chunk, filters) {
                continue;
            }
            hits.push(LexicalHit {
                exact_identifier: chunk
                    .identifiers
                    .iter()
                    .any(|identifier| identifier.eq_ignore_ascii_case(&exact)),
                chunk: chunk.clone(),
                score,
            });
        }
        sort_hits(&mut hits, &raw_terms);
        hits.truncate(limit.max(1));
        Ok(hits)
    }

    /// Returns the underlying schema for artifact inspection.
    #[must_use]
    pub fn schema(&self) -> Schema {
        self.index.schema()
    }

    /// Returns all chunks retained by this lexical artifact.
    #[must_use]
    pub const fn chunks(&self) -> &BTreeMap<ChunkId, Chunk> {
        &self.chunks
    }
}

fn sort_hits(hits: &mut [LexicalHit], raw_terms: &[String]) {
    hits.sort_by(|left, right| {
        right
            .exact_identifier
            .cmp(&left.exact_identifier)
            .then_with(|| {
                if left.exact_identifier && right.exact_identifier {
                    left.chunk
                        .legacy_ids
                        .is_empty()
                        .cmp(&right.chunk.legacy_ids.is_empty())
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                term_coverage(&right.chunk, raw_terms).cmp(&term_coverage(&left.chunk, raw_terms))
            })
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });
}

fn term_coverage(chunk: &Chunk, terms: &[String]) -> usize {
    let haystack = format!(
        "{} {} {} {} {}",
        chunk.title,
        chunk.path,
        chunk
            .identifiers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        chunk.heading_path.join(" "),
        chunk.text
    )
    .to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| haystack.contains(String::as_str(term)))
        .count()
}

fn schema() -> (Schema, LexicalFields) {
    let mut builder = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let title = builder.add_text_field("title", text.clone());
    let path = builder.add_text_field("path", text.clone());
    let identifiers = builder.add_text_field("identifiers", text.clone());
    let identifiers_raw = builder.add_text_field("identifiers_raw", STRING | STORED);
    let body = builder.add_text_field("body", text.clone());
    let tags = builder.add_text_field("tags", text);
    let chunk_id = builder.add_text_field("chunk_id", STRING | STORED);
    let category = builder.add_text_field("category", STRING | STORED);
    let repository = builder.add_text_field("repository", STRING | STORED);
    let trust_tier = builder.add_text_field("trust_tier", STRING | STORED);
    let schema = builder.build();
    (
        schema,
        LexicalFields {
            title,
            path,
            identifiers,
            identifiers_raw,
            body,
            tags,
            chunk_id,
            category,
            repository,
            trust_tier,
        },
    )
}

fn add_chunk(writer: &IndexWriter, fields: LexicalFields, chunk: &Chunk) {
    let mut document = TantivyDocument::default();
    document.add_text(fields.title, &chunk.title);
    document.add_text(fields.path, &chunk.path);
    for identifier in &chunk.identifiers {
        document.add_text(fields.identifiers, identifier);
        document.add_text(fields.identifiers_raw, identifier);
    }
    let body = format!("{} {}", chunk.heading_path.join(" "), chunk.text);
    document.add_text(fields.body, format!("{body} {}", morphology_aliases(&body)));
    document.add_text(
        fields.tags,
        chunk.tags.iter().cloned().collect::<Vec<_>>().join(" "),
    );
    document.add_text(fields.chunk_id, chunk.chunk_id.as_str());
    document.add_text(
        fields.category,
        chunk
            .category
            .map_or(String::new(), |category| category_label(category).into()),
    );
    document.add_text(fields.repository, chunk.repository.as_str());
    document.add_text(fields.trust_tier, trust_label(chunk.trust_tier));
    writer
        .add_document(document)
        .expect("in-memory lexical document is valid");
}

fn morphology_aliases(text: &str) -> String {
    text.split(|character: char| !character.is_ascii_alphabetic())
        .filter_map(|word| word.strip_suffix("ence"))
        .filter(|stem| stem.len() >= 4)
        .collect::<Vec<_>>()
        .join(" ")
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for raw in raw_query_terms(query) {
        push_query_term(&mut terms, &raw);
        for part in raw.split(['_', '-']).filter(|part| !part.is_empty()) {
            push_query_term(&mut terms, part);
        }
        for alias in query_aliases(&raw) {
            push_query_term(&mut terms, alias);
        }
    }
    terms
}

fn raw_query_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !(character.is_alphanumeric() || matches!(character, '_' | '-')))
        .filter(|term| !term.is_empty() && term.chars().any(char::is_alphanumeric))
        .map(str::to_ascii_lowercase)
        .collect()
}

fn query_term_occur(
    term: &str,
    raw_terms: &[String],
    raw_term_count: usize,
    default: Occur,
) -> Occur {
    if raw_term_count <= 2 && !raw_terms.iter().any(|raw| raw == term) {
        Occur::Should
    } else {
        default
    }
}

fn push_query_term(terms: &mut Vec<String>, term: &str) {
    if !terms.iter().any(|existing| existing == term) {
        terms.push(term.to_owned());
    }
}

fn query_aliases(term: &str) -> &'static [&'static str] {
    match term {
        "persist" | "persistence" | "persistent" => &["nvm", "read_nvm", "write_nvm"],
        "application" => &["app_data", "send_app_data"],
        "custom" => &["app_data", "send_app_data", "comm", "command", "transport"],
        "lifecycle" => &["pkgdesc", "build", "load", "native"],
        "firmware" => &["lbm", "vesc_c_if", "foc", "audio", "feature"],
        "api" => &["lbm", "vesc_c_if"],
        "extension" => &["native", "lbm"],
        "registration" => &["lbm_add_extension", "vesc_c_if"],
        "values" => &["encode", "decode"],
        "gating" | "enablement" => &["foc", "feature", "audio"],
        "dialect" | "description" => &["schema", "pkgdesc", "descriptor"],
        "transport" => &["send_app_data", "command"],
        "attribution" => &["provenance", "repository", "trust", "vesc_c_if", "lbm"],
        "source" => &["provenance", "repository", "trust"],
        "paths" | "path" => &["sandbox", "artifact", "pkgdesc", "build"],
        _ => &[],
    }
}

fn matches_filters(chunk: &Chunk, filters: &LexicalFilters) -> bool {
    filters
        .category
        .is_none_or(|category| chunk.category == Some(category))
        && filters
            .repository
            .as_ref()
            .is_none_or(|repository| &chunk.repository == repository)
        && filters
            .revision
            .as_ref()
            .is_none_or(|revision| &chunk.revision == revision)
        && filters
            .source_kind
            .is_none_or(|source_kind| chunk.source_kind == source_kind)
        && filters
            .trust_tier
            .is_none_or(|trust| chunk.trust_tier == trust)
        && filters.tags.iter().all(|tag| chunk.tags.contains(tag))
}

const fn category_label(category: Category) -> &'static str {
    match category {
        Category::FirmwareApi => "firmware_api",
        Category::Lispbm => "lispbm",
        Category::PackageBuild => "package_build",
        Category::RefloatCommand => "refloat_command",
        Category::NativeLibAbi => "native_lib_abi",
    }
}

const fn trust_label(trust: TrustTier) -> &'static str {
    match trust {
        TrustTier::FirstParty => "first_party",
        TrustTier::CuratedUpstream => "curated_upstream",
        TrustTier::Fixture => "fixture",
        TrustTier::UnverifiedModelFeedback => "unverified_model_feedback",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{NormalizedDocument, RepositoryId, Revision, SourceKind};

    fn chunk(title: &str, text: &str, identifier: &str) -> Chunk {
        let mut document = NormalizedDocument::new(
            title,
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repo"),
            Revision::try_from("rev").expect("revision"),
            "docs/example.md",
            "text/markdown",
            text,
        )
        .expect("document");
        document.identifiers.insert(identifier.into());
        Chunk::from_document(&document, 0, text.into(), Vec::new(), None).expect("chunk")
    }

    #[test]
    fn exact_identifier_is_top_one() {
        let index = LexicalIndex::build(&[
            chunk("NVM", "write persistent bytes", "write_nvm"),
            chunk("Other", "write bytes elsewhere", "other_write"),
        ])
        .expect("index");
        let hits = index
            .search("write_nvm", &LexicalFilters::default(), 10)
            .expect("search");

        assert_eq!(
            hits[0].chunk.identifiers.first().map(String::as_str),
            Some("write_nvm")
        );
        assert!(hits[0].exact_identifier);
    }

    #[test]
    fn domain_aliases_expand_conceptual_queries() {
        let terms = query_terms("how do I persist package data");

        assert!(terms.iter().any(|term| term == "nvm"));
        assert!(terms.iter().any(|term| term == "read_nvm"));
        assert!(terms.iter().any(|term| term == "write_nvm"));
    }

    #[test]
    fn legacy_exact_identifier_wins_over_duplicate_normalized_record() {
        let mut legacy = chunk("NVM", "legacy summary", "read_nvm");
        legacy.legacy_ids.push("vesc_c_if.read_nvm".into());
        let index = LexicalIndex::build(&[
            chunk("NVM record", "normalized catalog record", "read_nvm"),
            legacy,
        ])
        .expect("index");
        let hits = index
            .search("read_nvm", &LexicalFilters::default(), 10)
            .expect("search");

        assert_eq!(
            hits[0].chunk.legacy_ids,
            vec![String::from("vesc_c_if.read_nvm")]
        );
    }

    #[test]
    fn multi_term_query_requires_all_terms_in_a_candidate() {
        let index = LexicalIndex::build(&[
            chunk("NVM", "read persistent bytes", "read_nvm"),
            chunk("Other", "read unrelated bytes", "other_read"),
        ])
        .expect("index");
        let hits = index
            .search("read persistent", &LexicalFilters::default(), 10)
            .expect("search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.title, "NVM");
    }

    #[test]
    fn longer_prose_query_allows_partial_bm25_matches() {
        let index = LexicalIndex::build(&[
            chunk("NVM", "read persistent bytes", "read_nvm"),
            chunk("Other", "read unrelated bytes", "other_read"),
        ])
        .expect("index");
        let hits = index
            .search(
                "how do I read persistent bytes from a package",
                &LexicalFilters::default(),
                10,
            )
            .expect("search");

        assert!(hits.len() >= 2);
        assert_eq!(hits[0].chunk.title, "NVM");
    }

    #[test]
    fn persistence_query_matches_conservative_morphology_alias() {
        let index = LexicalIndex::build(&[chunk(
            "NVM",
            "package extensions persist data across reboot",
            "nvm",
        )])
        .expect("index");
        let hits = index
            .search(
                "how do extensions persist data",
                &LexicalFilters::default(),
                10,
            )
            .expect("search");

        assert_eq!(hits[0].chunk.title, "NVM");
    }

    #[test]
    fn lexical_artifact_roundtrips_and_rejects_corruption() {
        let index =
            LexicalIndex::build(&[chunk("NVM", "persistent bytes", "write_nvm")]).expect("index");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lexical.json");
        let (digest, bytes) = index
            .write_artifact_with_digest(&path)
            .expect("write artifact");
        assert_eq!(bytes, std::fs::metadata(&path).expect("metadata").len());
        assert_eq!(
            digest,
            ContentDigest::of(&std::fs::read(&path).expect("artifact"))
        );
        let reopened = LexicalIndex::open_artifact(&path).expect("open artifact");
        assert_eq!(
            reopened
                .search("write_nvm", &LexicalFilters::default(), 1)
                .expect("search")
                .len(),
            1
        );
        std::fs::write(&path, b"not-json").expect("corrupt artifact");
        assert!(matches!(
            LexicalIndex::open_artifact(&path),
            Err(LexicalError::Artifact(_))
        ));
    }

    #[test]
    fn source_artifact_can_be_written_without_building_an_index() {
        let chunks = vec![chunk("alpha", "body", "alpha")];
        let root = tempfile::tempdir().expect("artifact root");
        let path = root.path().join("lexical.json");

        let (_, bytes) = LexicalIndex::write_chunks_artifact_with_digest(&chunks, &path)
            .expect("write source artifact");

        assert!(bytes > 0);
        assert_eq!(
            LexicalIndex::read_artifact_chunks(&path).expect("read source artifact"),
            chunks
        );
    }
}
