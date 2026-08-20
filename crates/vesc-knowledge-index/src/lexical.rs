//! Fielded lexical retrieval over normalized chunks.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::collector::{BytesFilterCollector, Count, TopDocs};
use tantivy::merge_policy::NoMergePolicy;
use tantivy::query::{
    AllQuery, BooleanQuery, ConstScoreQuery, Occur, Query, TermQuery, TermSetQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::termdict::TermMerger;
use tantivy::{DocSet, Index, IndexReader, IndexWriter, TERMINATED, TantivyDocument, Term};

use crate::corpus::full_history::{
    CachedGitHistoryProjection, GitHistoryBuildPlan, GitHistoryChunkView,
};
use crate::corpus::git::GitCorpusSource;
use crate::corpus::{
    CORPUS_SCHEMA_V1, Chunk, ChunkId, ContentDigest, DocumentId, ResourceUri, RetrievalMetadata,
    SourceKind, SourceSpan, TrustTier, parse_prefixed_digest,
};
use crate::graph::{GraphArtifact, GraphChunk};
use crate::{Category, RepositoryId, Revision};

pub(crate) const LEXICAL_FORMAT_VERSION: &str = "tantivy-0.26-git-object-locators-v14";
const LEXICAL_DESCRIPTOR_SCHEMA: u16 = 7;
const INDEX_WRITER_MEMORY_BYTES: usize = 128 * 1024 * 1024;
const IN_MEMORY_WRITER_MEMORY_BYTES: usize = 15_000_000;
const MAX_INCREMENTAL_SEGMENTS: usize = 32;
const REACHABILITY_CACHE_SLOTS: usize = 256;
const GRAPH_INPUT_SUFFIX: &str = "graph-input.json";
const HISTORY_INPUT_SUFFIX: &str = "history-input.json";
const EMBEDDING_INPUT_SUFFIX: &str = "embedding-input.json";

/// Typed filters applied after Tantivy candidate retrieval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LexicalFilters {
    pub category: Option<Category>,
    pub repository: Option<RepositoryId>,
    pub paths: Vec<String>,
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

/// A ranked lexical candidate whose Git passage has not been loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalCandidate {
    pub chunk: RetrievalMetadata,
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
    #[error("hydrate {repository}@{revision}:{path} from managed Git repository: {message}")]
    GitHydration {
        repository: String,
        revision: String,
        path: String,
        message: String,
    },
    #[error("filter managed Git repository {repository}@{revision}: {message}")]
    GitFilter {
        repository: String,
        revision: String,
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexicalDescriptor {
    schema: u16,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    git_sources: BTreeMap<RepositoryId, GitSourceDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSourceDescriptor {
    revision: Revision,
    contract: ContentDigest,
    max_file_bytes: u64,
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
        (
            ContentDigest::from_sha256(self.digest.finalize().into()),
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

/// Fielded lexical index backed by memory for ad hoc builds or a persisted artifact for search.
pub struct LexicalIndex {
    index: Index,
    reader: IndexReader,
    fields: LexicalFields,
    chunks: BTreeMap<ChunkId, Chunk>,
    repositories_root: Option<PathBuf>,
    repository_paths: BTreeMap<RepositoryId, PathBuf>,
    git_sources: BTreeMap<RepositoryId, GitSourceDescriptor>,
}

#[derive(Clone, Copy)]
struct LexicalFields {
    title: Field,
    path: Field,
    path_raw: Field,
    identifiers: Field,
    identifiers_raw: Field,
    body: Field,
    tags: Field,
    tags_raw: Field,
    chunk_id: Field,
    document_id: Field,
    chunk_digest: [Field; 4],
    ordinal: Field,
    category: Field,
    repository: Field,
    revision: Field,
    source_kind: Field,
    trust_tier: Field,
    heading_path: Field,
    start_line: Field,
    end_line: Field,
    start_byte: Field,
    end_byte: Field,
    registered_id: Field,
    previous_chunk: Field,
    next_chunk: Field,
    content_digest: Field,
    char_count: Field,
    byte_count: Field,
    git_object_id: Field,
    history_content_key: Field,
    repository_revision: Field,
}

/// Exact lookup over persisted Git-history content identities.
pub struct HistoryContentLookup {
    reader: IndexReader,
    fields: LexicalFields,
}

#[derive(Default)]
pub(crate) struct EmbeddingTextHydrator {
    repositories: BTreeMap<RepositoryId, gix::Repository>,
    cached_object: Option<CachedEmbeddingGitObject>,
    git_blob_loads: usize,
}

struct CachedEmbeddingGitObject {
    repository: RepositoryId,
    object: gix::ObjectId,
    content: String,
}

#[derive(Clone)]
struct RepositoryFilter {
    requested: RepositoryId,
    candidates: Vec<RepositoryId>,
    reachability: Option<Arc<Mutex<GitReachability>>>,
}

struct GitReachability {
    repository: gix::ThreadSafeRepository,
    tip: gix::ObjectId,
    cache: [Option<(gix::ObjectId, bool)>; REACHABILITY_CACHE_SLOTS],
    error: Option<String>,
}

impl RepositoryFilter {
    fn matches_key(&self, key: &[u8]) -> bool {
        let Some((&kind, locator)) = key.split_first() else {
            return false;
        };
        let Some(separator) = locator.iter().position(|byte| *byte == 0) else {
            return false;
        };
        let (repository, revision) = locator.split_at(separator);
        if repository == self.requested.as_str().as_bytes() {
            return true;
        }
        if kind != 1
            || !self
                .candidates
                .iter()
                .any(|candidate| candidate.as_str().as_bytes() == repository)
        {
            return false;
        }
        let Some(reachability) = &self.reachability else {
            return false;
        };
        let Ok(revision) = gix::ObjectId::from_hex(&revision[1..]) else {
            return false;
        };
        reachability
            .lock()
            .expect("Git reachability lock is not poisoned")
            .contains(revision)
    }

    fn matches_locator(&self, locator: &ChunkLocator) -> bool {
        if locator.repository == self.requested {
            return true;
        }
        if !locator.source_kind.is_git()
            || !self
                .candidates
                .iter()
                .any(|candidate| candidate == &locator.repository)
        {
            return false;
        }
        let Some(reachability) = &self.reachability else {
            return false;
        };
        let Ok(revision) = gix::ObjectId::from_hex(locator.revision.as_str().as_bytes()) else {
            return false;
        };
        reachability
            .lock()
            .expect("Git reachability lock is not poisoned")
            .contains(revision)
    }

    fn take_error(&self) -> Option<LexicalError> {
        let mut reachability = self
            .reachability
            .as_ref()?
            .lock()
            .expect("Git reachability lock is not poisoned");
        reachability
            .error
            .take()
            .map(|message| LexicalError::GitFilter {
                repository: self.requested.to_string(),
                revision: reachability.tip.to_string(),
                message,
            })
    }
}

impl GitReachability {
    fn contains(&mut self, revision: gix::ObjectId) -> bool {
        let slot = revision
            .as_bytes()
            .iter()
            .take(8)
            .fold(0_usize, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(usize::from(*byte))
            })
            % REACHABILITY_CACHE_SLOTS;
        if let Some((cached, reachable)) = self.cache[slot]
            && cached == revision
        {
            return reachable;
        }
        let repository = self.repository.to_thread_local();
        let reachable = if revision == self.tip {
            true
        } else if !repository.has_object(revision) {
            false
        } else {
            match repository.merge_base(self.tip, revision) {
                Ok(base) => base.detach() == revision,
                Err(gix::repository::merge_base::Error::NotFound { .. }) => false,
                Err(error) => {
                    self.error.get_or_insert_with(|| error.to_string());
                    return false;
                }
            }
        };
        self.cache[slot] = Some((revision, reachable));
        reachable
    }
}

impl EmbeddingTextHydrator {
    pub(crate) const fn git_blob_loads(&self) -> usize {
        self.git_blob_loads
    }

    fn git_object_content<'content>(
        &'content mut self,
        index: &LexicalIndex,
        locator: &ChunkLocator,
    ) -> Result<&'content str, LexicalError> {
        let repository =
            index.git_repository(&locator.repository, locator, &mut self.repositories)?;
        let object = match locator.source_kind {
            SourceKind::GitBlob => locator
                .git_object_id
                .map_or_else(|| locator.resolve_git_blob(repository), Ok)?,
            SourceKind::GitCommit => locator.git_object_id.ok_or_else(|| {
                locator.git_error("commit locator has no persisted Git object ID")
            })?,
            _ => return Err(locator.git_error("source is not backed by Git")),
        };
        let cached = self.cached_object.as_ref().is_some_and(|cached| {
            cached.repository == locator.repository && cached.object == object
        });
        if !cached {
            let max_file_bytes = index
                .git_sources
                .get(&locator.repository)
                .map(|source| source.max_file_bytes)
                .ok_or_else(|| {
                    locator.git_error("managed Git repository has no persisted per-file limit")
                })?;
            let content = match locator.source_kind {
                SourceKind::GitBlob => {
                    let size = repository
                        .find_header(object)
                        .map_err(|error| locator.git_error(format!("read blob header: {error}")))?
                        .size();
                    if size > max_file_bytes {
                        return Err(locator
                            .git_error("Git blob exceeds the configured per-file byte limit"));
                    }
                    let object = repository
                        .find_object(object)
                        .map_err(|error| locator.git_error(format!("read blob: {error}")))?;
                    if object.data.contains(&0) {
                        return Err(locator.git_error("blob contains binary data"));
                    }
                    crate::corpus::ingest::normalize_text_ref(&object.data).map_err(|error| {
                        locator.git_error(format!("decode blob as UTF-8: {error}"))
                    })?
                }
                SourceKind::GitCommit => {
                    let commit = repository.find_commit(object).map_err(|error| {
                        locator.git_error(format!("read commit message: {error}"))
                    })?;
                    crate::corpus::git::commit_message_content(&commit, max_file_bytes)
                        .ok_or_else(|| locator.git_error("commit message is empty or oversized"))?
                }
                _ => unreachable!("Git source kind checked above"),
            };
            self.cached_object = Some(CachedEmbeddingGitObject {
                repository: locator.repository.clone(),
                object,
                content,
            });
            if locator.source_kind == SourceKind::GitBlob {
                self.git_blob_loads = self.git_blob_loads.saturating_add(1);
            }
        }
        Ok(&self
            .cached_object
            .as_ref()
            .expect("requested Git object was cached above")
            .content)
    }

    fn git_embedding_text(
        &mut self,
        index: &LexicalIndex,
        locator: &ChunkLocator,
    ) -> Result<String, LexicalError> {
        let content = self.git_object_content(index, locator)?;
        let passage = locator.passage(content)?;
        Ok(crate::semantic::embedding_text_from_metadata(
            &locator.title,
            locator.heading_path.iter().map(String::as_str),
            &locator.identifiers,
            &locator.tags,
            passage,
        ))
    }

    fn git_chunk(
        &mut self,
        index: &LexicalIndex,
        locator: ChunkLocator,
    ) -> Result<Chunk, LexicalError> {
        let content = self.git_object_content(index, &locator)?;
        locator.hydrate(content)
    }
}

impl HistoryContentLookup {
    pub(crate) fn matching_chunk_ids(
        &self,
        keys: &BTreeSet<ContentDigest>,
    ) -> Result<BTreeMap<ContentDigest, ChunkId>, LexicalError> {
        let searcher = self.reader.searcher();
        let segment_readers = searcher.segment_readers();
        let history_indexes = segment_readers
            .iter()
            .map(|reader| reader.inverted_index(self.fields.history_content_key))
            .collect::<Result<Vec<_>, _>>()
            .map_err(LexicalError::Search)?;
        let streams = history_indexes
            .iter()
            .map(|index| index.terms().stream())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        let chunk_digests = segment_readers
            .iter()
            .map(|reader| {
                let fast_fields = reader.fast_fields();
                Ok([
                    fast_fields.u64("chunk_digest_0")?,
                    fast_fields.u64("chunk_digest_1")?,
                    fast_fields.u64("chunk_digest_2")?,
                    fast_fields.u64("chunk_digest_3")?,
                ])
            })
            .collect::<Result<Vec<_>, tantivy::TantivyError>>()
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        let mut terms = TermMerger::new(streams);
        let mut matches = BTreeMap::new();
        while terms.advance() {
            let key = std::str::from_utf8(terms.key())
                .ok()
                .and_then(|value| ContentDigest::try_from(value).ok())
                .ok_or_else(|| invalid_field("history_content_key"))?;
            if !keys.contains(&key) {
                continue;
            }
            for (segment_ord, term_info) in terms.current_segment_ords_and_term_infos() {
                let reader = &segment_readers[segment_ord];
                let mut postings = history_indexes[segment_ord]
                    .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)
                    .map_err(|error| LexicalError::Io(error.to_string()))?;
                while postings.doc() != TERMINATED {
                    let doc_id = postings.doc();
                    if !reader.is_deleted(doc_id) {
                        let mut digest = [0_u8; 32];
                        for (part, column) in chunk_digests[segment_ord].iter().enumerate() {
                            let mut values = column.values_for_doc(doc_id);
                            let value = values
                                .next()
                                .ok_or_else(|| invalid_field("chunk digest fast field"))?;
                            if values.next().is_some() {
                                return Err(invalid_field("chunk digest fast field"));
                            }
                            digest[part * 8..(part + 1) * 8].copy_from_slice(&value.to_be_bytes());
                        }
                        let chunk_id = ChunkId::from_sha256(digest);
                        matches
                            .entry(key.clone())
                            .and_modify(|previous| {
                                if chunk_id < *previous {
                                    previous.clone_from(&chunk_id);
                                }
                            })
                            .or_insert(chunk_id);
                    }
                    postings.advance();
                }
            }
        }
        Ok(matches)
    }

    /// Returns whether the previous lexical index already contains this history identity.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Search`] when Tantivy cannot execute the exact lookup.
    pub fn contains(
        &self,
        repository: &RepositoryId,
        path: &str,
        key: &ContentDigest,
    ) -> Result<bool, LexicalError> {
        self.contains_retained(repository, path, key, None, &BTreeSet::new())
    }

    pub(crate) fn contains_retained(
        &self,
        _repository: &RepositoryId,
        _path: &str,
        key: &ContentDigest,
        revision: Option<&gix::ObjectId>,
        removed_document_ids: &BTreeSet<String>,
    ) -> Result<bool, LexicalError> {
        let mut clauses = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(self.fields.history_content_key, &key.to_string()),
                IndexRecordOption::Basic,
            )) as Box<dyn Query>,
        )];
        if let Some(revision) = revision {
            let mut revision_hex = gix::hash::Kind::hex_buf();
            let revision = revision.as_ref().hex_to_buf(&mut revision_hex);
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.revision, revision),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if !removed_document_ids.is_empty() {
            clauses.push((
                Occur::MustNot,
                Box::new(TermSetQuery::new(removed_document_ids.iter().map(
                    |document_id| {
                        Term::from_field_text(self.fields.document_id, document_id.as_str())
                    },
                ))) as Box<dyn Query>,
            ));
        }
        let query = BooleanQuery::new(clauses);
        let searcher = self.reader.searcher();
        let count = searcher
            .search(&query, &Count)
            .map_err(LexicalError::Search)?;
        Ok(count > 0)
    }
}

struct JsonArrayWriter {
    writer: BufWriter<File>,
    first: bool,
}

impl JsonArrayWriter {
    fn create(path: &Path) -> Result<Self, LexicalError> {
        let mut writer = BufWriter::new(
            File::create(path).map_err(|error| LexicalError::Io(error.to_string()))?,
        );
        writer
            .write_all(b"[")
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        Ok(Self {
            writer,
            first: true,
        })
    }

    fn push<T: Serialize>(&mut self, value: &T) -> Result<(), LexicalError> {
        if !self.first {
            self.writer
                .write_all(b",")
                .map_err(|error| LexicalError::Io(error.to_string()))?;
        }
        serde_json::to_writer(&mut self.writer, value)
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        self.first = false;
        Ok(())
    }

    fn append(previous: &Path, destination: &Path) -> Result<Self, LexicalError> {
        let mut source =
            File::open(previous).map_err(|error| LexicalError::Io(error.to_string()))?;
        let length = source
            .metadata()
            .map_err(|error| LexicalError::Io(error.to_string()))?
            .len();
        let start = first_non_whitespace(&mut source, length)?;
        let mut byte = [0_u8; 1];
        source
            .seek(SeekFrom::Start(start))
            .and_then(|_| source.read_exact(&mut byte))
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if byte[0] != b'[' {
            return Err(LexicalError::Artifact(
                "compact sidecar is not a JSON array".into(),
            ));
        }
        let end = last_non_whitespace(&mut source, length)?;
        source
            .seek(SeekFrom::Start(end))
            .and_then(|_| source.read_exact(&mut byte))
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if byte[0] != b']' {
            return Err(LexicalError::Artifact(
                "compact sidecar is not a JSON array".into(),
            ));
        }
        let has_items = has_non_whitespace(&mut source, start + 1, end)?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        let mut writer = BufWriter::new(
            File::create(destination).map_err(|error| LexicalError::Io(error.to_string()))?,
        );
        std::io::copy(&mut source.take(end), &mut writer)
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        Ok(Self {
            writer,
            first: !has_items,
        })
    }

    fn finish(mut self) -> Result<(), LexicalError> {
        self.writer
            .write_all(b"]")
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        self.writer
            .flush()
            .map_err(|error| LexicalError::Io(error.to_string()))
    }
}

fn first_non_whitespace(file: &mut File, length: u64) -> Result<u64, LexicalError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| LexicalError::Io(error.to_string()))?;
    let mut byte = [0_u8; 1];
    for position in 0..length {
        file.read_exact(&mut byte)
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if !byte[0].is_ascii_whitespace() {
            return Ok(position);
        }
    }
    Err(LexicalError::Artifact("compact sidecar is empty".into()))
}

fn last_non_whitespace(file: &mut File, length: u64) -> Result<u64, LexicalError> {
    let mut byte = [0_u8; 1];
    for position in (0..length).rev() {
        file.seek(SeekFrom::Start(position))
            .and_then(|_| file.read_exact(&mut byte))
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if !byte[0].is_ascii_whitespace() {
            return Ok(position);
        }
    }
    Err(LexicalError::Artifact("compact sidecar is empty".into()))
}

fn has_non_whitespace(file: &mut File, start: u64, end: u64) -> Result<bool, LexicalError> {
    if start >= end {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|error| LexicalError::Io(error.to_string()))?;
    let mut remaining = end - start;
    let mut buffer = [0_u8; 8192];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        file.read_exact(&mut buffer[..requested])
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if buffer[..requested]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace())
        {
            return Ok(true);
        }
        remaining -= requested as u64;
    }
    Ok(false)
}

struct GitHistorySidecars {
    graph: Option<JsonArrayWriter>,
    history: Option<JsonArrayWriter>,
    embedding: Option<JsonArrayWriter>,
}

#[derive(Serialize)]
struct BorrowedGraphChunk<'a> {
    chunk_id: &'a ChunkId,
    repository: &'a RepositoryId,
    revision: &'a Revision,
    path: &'a str,
    title: &'a str,
    ordinal: u32,
    source_span: Option<SourceSpan>,
    next_chunk: Option<&'a ChunkId>,
}

impl<'a> BorrowedGraphChunk<'a> {
    fn from_view(view: &'a GitHistoryChunkView<'a>) -> Self {
        Self {
            chunk_id: view.chunk_id(),
            repository: view.repository(),
            revision: view.revision(),
            path: view.path(),
            title: view.title(),
            ordinal: view.ordinal(),
            source_span: view.source_span(),
            next_chunk: view.next_chunk(),
        }
    }
}

#[derive(Serialize)]
struct BorrowedCachedGitHistoryRecord<'a> {
    document_id: &'a DocumentId,
    repository: &'a RepositoryId,
    revision: &'a Revision,
    path: &'a str,
    ordinal: u32,
    has_previous: bool,
    has_next: bool,
    blob: Option<&'a str>,
    source_kind: SourceKind,
    content_key: Option<ContentDigest>,
}

impl<'a> BorrowedCachedGitHistoryRecord<'a> {
    fn from_view(
        view: &'a GitHistoryChunkView<'a>,
        blob: &'a str,
        content_key: ContentDigest,
    ) -> Self {
        Self {
            document_id: view.document_id(),
            repository: view.repository(),
            revision: view.revision(),
            path: view.path(),
            ordinal: view.ordinal(),
            has_previous: view.previous_chunk().is_some(),
            has_next: view.next_chunk().is_some(),
            blob: Some(blob),
            source_kind: view.source_kind(),
            content_key: Some(content_key),
        }
    }
}

#[derive(Serialize)]
struct BorrowedEmbeddingLocatorRecord<'a> {
    chunk_id: &'a ChunkId,
    document_id: &'a DocumentId,
    title: &'a str,
    source_kind: SourceKind,
    repository: &'a RepositoryId,
    revision: &'a Revision,
    path: &'a str,
    heading_path: &'a [&'a str],
    source_span: Option<SourceSpan>,
    identifiers: &'a [compact_str::CompactString],
    tags: &'a BTreeSet<String>,
    git_object_id: Option<&'a str>,
}

impl<'a> BorrowedEmbeddingLocatorRecord<'a> {
    fn from_view(view: &'a GitHistoryChunkView<'a>, blob: &'a str) -> Self {
        Self {
            chunk_id: view.chunk_id(),
            document_id: view.document_id(),
            title: view.title(),
            source_kind: view.source_kind(),
            repository: view.repository(),
            revision: view.revision(),
            path: view.path(),
            heading_path: view.headings(),
            source_span: view.source_span(),
            identifiers: view.identifiers(),
            tags: view.tags(),
            git_object_id: Some(blob),
        }
    }
}

impl GitHistorySidecars {
    fn create(path: &Path) -> Result<Self, LexicalError> {
        Ok(Self {
            graph: Some(JsonArrayWriter::create(&graph_input_path(path))?),
            history: Some(JsonArrayWriter::create(&history_input_path(path))?),
            embedding: Some(JsonArrayWriter::create(&embedding_input_path(path))?),
        })
    }

    fn append(previous: &Path, path: &Path) -> Result<Self, LexicalError> {
        let append = |suffix: fn(&Path) -> PathBuf| {
            let source = suffix(previous);
            source
                .is_file()
                .then(|| JsonArrayWriter::append(&source, &suffix(path)))
                .transpose()
        };
        Ok(Self {
            graph: append(graph_input_path)?,
            history: append(history_input_path)?,
            embedding: append(embedding_input_path)?,
        })
    }

    fn push_embedded(&mut self, chunk: &Chunk) -> Result<(), LexicalError> {
        self.graph
            .as_mut()
            .expect("cold graph sidecar")
            .push(&GraphChunk::from_chunk(chunk))?;
        self.embedding
            .as_mut()
            .expect("cold embedding sidecar")
            .push(&EmbeddingLocatorRecord::from_chunk(chunk, None))
    }

    fn push_history(
        &mut self,
        view: &GitHistoryChunkView<'_>,
        blob: gix::ObjectId,
    ) -> Result<(), LexicalError> {
        self.push_history_borrowed(view, blob)
    }

    fn push_history_borrowed(
        &mut self,
        view: &GitHistoryChunkView<'_>,
        blob: gix::ObjectId,
    ) -> Result<(), LexicalError> {
        let blob = blob.to_string();
        if let Some(graph) = self.graph.as_mut() {
            graph.push(&BorrowedGraphChunk::from_view(view))?;
        }
        if let Some(history) = self.history.as_mut() {
            history.push(&BorrowedCachedGitHistoryRecord::from_view(
                view,
                &blob,
                view.history_content_key(),
            ))?;
        }
        if let Some(embedding) = self.embedding.as_mut() {
            embedding.push(&BorrowedEmbeddingLocatorRecord::from_view(view, &blob))?;
        }
        Ok(())
    }

    fn finish(self) -> Result<(), LexicalError> {
        if let Some(graph) = self.graph {
            graph.finish()?;
        }
        if let Some(history) = self.history {
            history.finish()?;
        }
        if let Some(embedding) = self.embedding {
            embedding.finish()?;
        }
        Ok(())
    }
}

impl LexicalIndex {
    /// Builds an in-memory Tantivy index from chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Writer`] or [`LexicalError::Commit`] when Tantivy
    /// cannot construct or commit the index.
    pub fn build(chunks: &[Chunk]) -> Result<Self, LexicalError> {
        Self::build_owned(chunks.to_vec())
    }

    fn build_owned(chunks: Vec<Chunk>) -> Result<Self, LexicalError> {
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index
            .writer_with_num_threads(1, IN_MEMORY_WRITER_MEMORY_BYTES)
            .map_err(LexicalError::Writer)?;
        for chunk in &chunks {
            add_chunk(&writer, fields, chunk);
        }
        writer.commit().map_err(LexicalError::Commit)?;
        let reader = index.reader().map_err(LexicalError::Writer)?;
        let chunk_map = chunks
            .into_iter()
            .map(|chunk| (chunk.chunk_id.clone(), chunk))
            .collect();
        Ok(Self {
            index,
            reader,
            fields,
            chunks: chunk_map,
            repositories_root: None,
            repository_paths: BTreeMap::new(),
            git_sources: BTreeMap::new(),
        })
    }

    /// Writes deterministic chunk data and a query-ready Tantivy sidecar.
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
        Self::write_search_artifact_with_digest(self.chunks.values(), path)
    }

    pub(crate) fn write_search_artifact_with_digest<'a>(
        chunks: impl IntoIterator<Item = &'a Chunk>,
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        Self::write_persisted_index(chunks, path)?;
        Self::write_descriptor(path, BTreeMap::new())
    }

    pub(crate) fn write_git_search_artifact_with_digest<'a>(
        chunks: impl IntoIterator<Item = &'a Chunk>,
        sources: &[GitCorpusSource],
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        Self::write_persisted_index(chunks, path)?;
        Self::write_descriptor(path, git_source_descriptors(sources))
    }

    pub(crate) fn clone_search_artifact(previous: &Path, path: &Path) -> Result<(), LexicalError> {
        clone_persisted_index(previous, path)?;
        fs::copy(previous, path).map_err(|error| LexicalError::Io(error.to_string()))?;
        for (source, destination) in [
            (graph_input_path(previous), graph_input_path(path)),
            (history_input_path(previous), history_input_path(path)),
            (embedding_input_path(previous), embedding_input_path(path)),
        ] {
            if source.is_file() {
                fs::copy(source, destination)
                    .map_err(|error| LexicalError::Io(error.to_string()))?;
            }
        }
        Ok(())
    }

    pub(crate) fn write_git_history_search_artifact_with_digest(
        plan: &GitHistoryBuildPlan,
        sources: &[GitCorpusSource],
        embedded: &[Chunk],
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        let index_path = persisted_index_path(path);
        if index_path.exists() {
            fs::remove_dir_all(&index_path).map_err(|error| LexicalError::Io(error.to_string()))?;
        }
        fs::create_dir_all(&index_path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let (schema, fields) = schema();
        let index = Index::create_in_dir(index_path, schema).map_err(LexicalError::Writer)?;
        let mut writer = index
            .writer_with_num_threads(1, INDEX_WRITER_MEMORY_BYTES)
            .map_err(LexicalError::Writer)?;
        let mut sidecars = GitHistorySidecars::create(path)?;
        for chunk in embedded {
            add_chunk(&writer, fields, chunk);
            sidecars.push_embedded(chunk)?;
        }
        write_git_history_chunks(plan, sources, &writer, fields, &mut sidecars)?;
        writer.commit().map_err(LexicalError::Commit)?;
        sidecars.finish()?;
        Self::write_descriptor(path, git_source_descriptors(sources))
    }

    /// Clones immutable Tantivy segments and indexes only new chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the prior index cannot be cloned or the
    /// delta cannot be committed.
    pub fn write_incremental_search_artifact_with_digest<'a>(
        previous: &Path,
        chunks: impl IntoIterator<Item = &'a Chunk>,
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        let descriptor = Self::read_descriptor(previous)?;
        clone_persisted_index(previous, path)?;
        let (schema, fields) = schema();
        let index = Index::open_in_dir(persisted_index_path(path)).map_err(LexicalError::Writer)?;
        if index.schema() != schema {
            return Err(LexicalError::Artifact(
                "persisted lexical index schema does not match".into(),
            ));
        }
        let mut writer = index
            .writer_with_num_threads(1, INDEX_WRITER_MEMORY_BYTES)
            .map_err(LexicalError::Writer)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        for chunk in chunks {
            add_chunk(&writer, fields, chunk);
            #[cfg(feature = "coz-profile")]
            crate::profile_progress!("lexical_indexed_chunk");
        }
        writer.commit().map_err(LexicalError::Commit)?;
        let mut segments = index
            .searchable_segment_metas()
            .map_err(LexicalError::Commit)?;
        if segments.len() > MAX_INCREMENTAL_SEGMENTS {
            segments.sort_unstable_by_key(tantivy::SegmentMeta::num_docs);
            let smallest = segments
                .iter()
                .take(2)
                .map(tantivy::SegmentMeta::id)
                .collect::<Vec<_>>();
            writer
                .merge(&smallest)
                .wait()
                .map_err(LexicalError::Commit)?;
            writer
                .wait_merging_threads()
                .map_err(LexicalError::Commit)?;
        }
        Self::write_descriptor(path, descriptor.git_sources)
    }

    pub(crate) fn write_incremental_git_history_search_artifact_with_digest(
        previous: &Path,
        plan: &GitHistoryBuildPlan,
        sources: &[GitCorpusSource],
        path: &Path,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        clone_persisted_index(previous, path)?;
        let (schema, fields) = schema();
        let index = Index::open_in_dir(persisted_index_path(path)).map_err(LexicalError::Writer)?;
        if index.schema() != schema {
            return Err(LexicalError::Artifact(
                "persisted lexical index schema does not match".into(),
            ));
        }
        let mut writer = index
            .writer_with_num_threads(1, INDEX_WRITER_MEMORY_BYTES)
            .map_err(LexicalError::Writer)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        let mut sidecars = GitHistorySidecars::append(previous, path)?;
        for document_id in plan.removed_document_ids() {
            writer.delete_term(Term::from_field_text(fields.document_id, document_id));
        }
        write_git_history_chunks(plan, sources, &writer, fields, &mut sidecars)?;
        writer.commit().map_err(LexicalError::Commit)?;
        let mut segments = index
            .searchable_segment_metas()
            .map_err(LexicalError::Commit)?;
        if segments.len() > MAX_INCREMENTAL_SEGMENTS {
            segments.sort_unstable_by_key(tantivy::SegmentMeta::num_docs);
            let smallest = segments
                .iter()
                .take(2)
                .map(tantivy::SegmentMeta::id)
                .collect::<Vec<_>>();
            writer
                .merge(&smallest)
                .wait()
                .map_err(LexicalError::Commit)?;
            writer
                .wait_merging_threads()
                .map_err(LexicalError::Commit)?;
        }
        sidecars.finish()?;
        Self::write_descriptor(path, git_source_descriptors(sources))
    }

    /// Opens the exact Git-history key lookup without deserializing stored chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the persisted artifact is incompatible.
    pub fn open_history_content_lookup(path: &Path) -> Result<HistoryContentLookup, LexicalError> {
        let index = Self::open_persisted_index(path, BTreeMap::new(), None)?;
        Ok(HistoryContentLookup {
            reader: index.reader,
            fields: index.fields,
        })
    }

    pub(crate) fn graph_from_sidecar<F>(
        path: &Path,
        corpus_digest: ContentDigest,
        project: F,
    ) -> Result<Option<GraphArtifact>, LexicalError>
    where
        F: FnMut(GraphChunk) -> Option<GraphChunk>,
    {
        let path = graph_input_path(path);
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        GraphArtifact::from_graph_chunk_reader(corpus_digest, BufReader::new(file), project)
            .map(Some)
            .map_err(|error| LexicalError::Artifact(error.to_string()))
    }

    pub(crate) fn read_history_projection(
        path: &Path,
    ) -> Result<Option<CachedGitHistoryProjection>, LexicalError> {
        let path = history_input_path(path);
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let projection = CachedGitHistoryProjection::from_json_reader(BufReader::new(file))
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        Ok(projection.ok())
    }

    pub(crate) fn read_embedding_inputs(
        path: &Path,
    ) -> Result<Option<Vec<EmbeddingLocatorRecord>>, LexicalError> {
        let path = embedding_input_path(path);
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        serde_json::from_reader(BufReader::new(file))
            .map(Some)
            .map_err(|error| LexicalError::Artifact(error.to_string()))
    }

    fn write_descriptor(
        path: &Path,
        git_sources: BTreeMap<RepositoryId, GitSourceDescriptor>,
    ) -> Result<(ContentDigest, u64), LexicalError> {
        let file = File::create(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let mut writer = DigestingWriter::new(BufWriter::new(file));
        serde_json::to_writer(
            &mut writer,
            &LexicalDescriptor {
                schema: LEXICAL_DESCRIPTOR_SCHEMA,
                git_sources,
            },
        )
        .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        writer
            .flush()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        Ok(writer.finish())
    }

    pub(crate) fn sidecar_checksum(path: &Path) -> Result<ContentDigest, LexicalError> {
        let root = persisted_index_path(path);
        let mut entries = fs::read_dir(&root)
            .map_err(|error| LexicalError::Io(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        entries.sort_unstable_by_key(std::fs::DirEntry::file_name);

        let mut digest = Sha256::new();
        digest.update(b"vesc-mcp lexical sidecar v2\0");
        let mut buffer = [0_u8; 8 * 1024];
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                LexicalError::Artifact("persisted lexical filename is not UTF-8".into())
            })?;
            if Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"))
            {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|error| LexicalError::Io(error.to_string()))?;
            if !metadata.is_file() {
                return Err(LexicalError::Artifact(
                    "persisted lexical index contains a non-file entry".into(),
                ));
            }
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update(metadata.len().to_le_bytes());
            let mut file = BufReader::new(
                File::open(entry.path()).map_err(|error| LexicalError::Io(error.to_string()))?,
            );
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| LexicalError::Io(error.to_string()))?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
        }
        for path in [
            graph_input_path(path),
            history_input_path(path),
            embedding_input_path(path),
        ] {
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    LexicalError::Artifact("persisted lexical sidecar filename is not UTF-8".into())
                })?;
            let metadata = path
                .metadata()
                .map_err(|error| LexicalError::Io(error.to_string()))?;
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update(metadata.len().to_le_bytes());
            let mut file = BufReader::new(
                File::open(path).map_err(|error| LexicalError::Io(error.to_string()))?,
            );
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| LexicalError::Io(error.to_string()))?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
        }
        Ok(ContentDigest::from_sha256(digest.finalize().into()))
    }

    /// Loads chunk data from the persisted Tantivy index.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::Io`] for read failures, [`LexicalError::Artifact`]
    /// for a malformed descriptor or Tantivy index.
    pub fn open_artifact(path: &Path) -> Result<Self, LexicalError> {
        Self::open_search_artifact(path)
    }

    /// Opens the query-ready Tantivy sidecar and its compact descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the sidecar or its stored chunks are invalid.
    pub fn open_search_artifact(path: &Path) -> Result<Self, LexicalError> {
        let descriptor = Self::read_descriptor(path)?;
        if descriptor.schema != LEXICAL_DESCRIPTOR_SCHEMA {
            return Err(LexicalError::Artifact(format!(
                "unsupported lexical schema {}",
                descriptor.schema
            )));
        }
        let mut index =
            Self::open_persisted_index(path, BTreeMap::new(), managed_repositories_root(path))?;
        index.git_sources = descriptor.git_sources;
        Ok(index)
    }

    /// Opens a query-ready sidecar whose Git passages resolve below
    /// `repositories_root/<repository>.git`.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the sidecar is incompatible.
    pub fn open_git_search_artifact(
        path: &Path,
        repositories_root: &Path,
    ) -> Result<Self, LexicalError> {
        let mut index = Self::open_search_artifact(path)?;
        index.repositories_root = Some(repositories_root.to_owned());
        Ok(index)
    }

    /// Validate that a persisted Git sidecar was built for the expected source contracts.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the descriptor is incompatible.
    pub fn validate_git_source_contracts(
        path: &Path,
        sources: &[GitCorpusSource],
    ) -> Result<(), LexicalError> {
        let descriptor = Self::read_descriptor(path)?;
        if descriptor.schema != LEXICAL_DESCRIPTOR_SCHEMA
            || descriptor.git_sources != git_source_descriptors(sources)
        {
            return Err(LexicalError::Artifact(
                "persisted Git source contracts do not match configured sources".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn open_git_search_artifact_with_sources(
        path: &Path,
        sources: &[GitCorpusSource],
    ) -> Result<Self, LexicalError> {
        let mut index = Self::open_search_artifact(path)?;
        let expected_sources = git_source_descriptors(sources);
        if index.git_sources != expected_sources {
            return Err(LexicalError::Artifact(
                "persisted Git source contracts do not match configured sources".into(),
            ));
        }
        index.repositories_root = None;
        index.repository_paths = sources
            .iter()
            .map(|source| (source.repository_id.clone(), source.repository_path.clone()))
            .collect();
        Ok(index)
    }

    pub(crate) fn embedding_chunk_ids(&self) -> Result<Vec<ChunkId>, LexicalError> {
        let document_count = self.embedding_document_count()?;
        let mut chunks = Vec::with_capacity(document_count);
        self.for_each_embedding_chunk_id(|chunk_id| chunks.push(chunk_id.clone()))?;
        Ok(chunks)
    }

    pub(crate) fn embedding_chunk_id_set(&self) -> Result<(HashSet<ChunkId>, usize), LexicalError> {
        let document_count = self.embedding_document_count()?;
        let mut chunks = HashSet::with_capacity(document_count);
        let count = self.for_each_embedding_chunk_id(|chunk_id| {
            chunks.insert(chunk_id.clone());
        })?;
        Ok((chunks, count))
    }

    fn embedding_document_count(&self) -> Result<usize, LexicalError> {
        usize::try_from(self.reader.searcher().num_docs())
            .map_err(|_| LexicalError::Artifact("lexical document count is too large".into()))
    }

    fn for_each_embedding_chunk_id(
        &self,
        mut visit: impl FnMut(&ChunkId),
    ) -> Result<usize, LexicalError> {
        let searcher = self.reader.searcher();
        let document_count = self.embedding_document_count()?;
        let segment_readers = searcher.segment_readers();
        let document_indexes = segment_readers
            .iter()
            .map(|reader| reader.inverted_index(self.fields.document_id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(LexicalError::Search)?;
        let streams = document_indexes
            .iter()
            .map(|index| index.terms().stream())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        let chunk_digests = segment_readers
            .iter()
            .map(|reader| {
                let fast_fields = reader.fast_fields();
                Ok([
                    fast_fields.u64("chunk_digest_0")?,
                    fast_fields.u64("chunk_digest_1")?,
                    fast_fields.u64("chunk_digest_2")?,
                    fast_fields.u64("chunk_digest_3")?,
                ])
            })
            .collect::<Result<Vec<_>, tantivy::TantivyError>>()
            .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        let mut documents = TermMerger::new(streams);
        let mut document_chunks = Vec::new();
        let mut count = 0_usize;

        while documents.advance() {
            document_chunks.clear();
            for (segment_ord, term_info) in documents.current_segment_ords_and_term_infos() {
                let reader = &segment_readers[segment_ord];
                let mut postings = document_indexes[segment_ord]
                    .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)
                    .map_err(|error| LexicalError::Io(error.to_string()))?;
                while postings.doc() != TERMINATED {
                    let doc_id = postings.doc();
                    if !reader.is_deleted(doc_id) {
                        if document_chunks.is_empty()
                            && std::str::from_utf8(documents.key())
                                .ok()
                                .and_then(|value| parse_prefixed_digest(value, "doc-"))
                                .is_none()
                        {
                            return Err(invalid_field("document_id"));
                        }
                        let mut digest = [0_u8; 32];
                        for (part, column) in chunk_digests[segment_ord].iter().enumerate() {
                            let mut values = column.values_for_doc(doc_id);
                            let value = values
                                .next()
                                .ok_or_else(|| invalid_field("chunk digest fast field"))?;
                            if values.next().is_some() {
                                return Err(invalid_field("chunk digest fast field"));
                            }
                            digest[part * 8..(part + 1) * 8].copy_from_slice(&value.to_be_bytes());
                        }
                        document_chunks.push(ChunkId::from_sha256(digest));
                    }
                    postings.advance();
                }
            }
            document_chunks.sort_unstable();
            for chunk_id in &document_chunks {
                count += 1;
                visit(chunk_id);
            }
        }
        if count != document_count {
            return Err(LexicalError::Artifact(
                "lexical index document inventory is incomplete".into(),
            ));
        }
        Ok(count)
    }

    /// Streams the sorted unique document and chunk IDs from a persisted index.
    ///
    /// The digest is identical to [`crate::CorpusManifest::new`] without
    /// materializing either complete ID inventory.
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the persisted index is missing, corrupt,
    /// or contains duplicate chunk documents.
    pub fn corpus_inventory(path: &Path) -> Result<(usize, usize, ContentDigest), LexicalError> {
        let index = Self::open_persisted_index(path, BTreeMap::new(), None)?;
        let searcher = index.reader.searcher();
        let mut writer = DigestingWriter::new(std::io::sink());
        let document_count = write_unique_terms(&searcher, index.fields.document_id, &mut writer)?;
        let chunk_count = write_unique_terms(&searcher, index.fields.chunk_id, &mut writer)?;
        let live_documents = usize::try_from(searcher.num_docs())
            .map_err(|_| LexicalError::Artifact("lexical document count is too large".into()))?;
        if live_documents != chunk_count {
            return Err(LexicalError::Artifact(
                "lexical index contains duplicate or missing chunk documents".into(),
            ));
        }
        let (digest, _) = writer.finish();
        Ok((document_count, chunk_count, digest))
    }

    fn open_persisted_index(
        path: &Path,
        chunks: BTreeMap<ChunkId, Chunk>,
        repositories_root: Option<PathBuf>,
    ) -> Result<Self, LexicalError> {
        let (schema, fields) = schema();
        let index = Index::open_in_dir(persisted_index_path(path)).map_err(LexicalError::Writer)?;
        if index.schema() != schema {
            return Err(LexicalError::Artifact(
                "persisted lexical index schema does not match".into(),
            ));
        }
        let reader = index.reader().map_err(LexicalError::Writer)?;
        Ok(Self {
            index,
            reader,
            fields,
            chunks,
            repositories_root,
            repository_paths: BTreeMap::new(),
            git_sources: BTreeMap::new(),
        })
    }

    fn write_persisted_index<'a>(
        chunks: impl IntoIterator<Item = &'a Chunk>,
        path: &Path,
    ) -> Result<(), LexicalError> {
        let index_path = persisted_index_path(path);
        if index_path.exists() {
            fs::remove_dir_all(&index_path).map_err(|error| LexicalError::Io(error.to_string()))?;
        }
        fs::create_dir_all(&index_path).map_err(|error| LexicalError::Io(error.to_string()))?;
        let (schema, fields) = schema();
        let index = Index::create_in_dir(index_path, schema).map_err(LexicalError::Writer)?;
        let mut writer = index
            .writer_with_num_threads(1, INDEX_WRITER_MEMORY_BYTES)
            .map_err(LexicalError::Writer)?;
        for chunk in chunks {
            add_chunk(&writer, fields, chunk);
            #[cfg(feature = "coz-profile")]
            crate::profile_progress!("lexical_indexed_chunk");
        }
        writer.commit().map_err(LexicalError::Commit)?;
        Ok(())
    }

    /// Reads all chunks from the persisted Tantivy index.
    ///
    /// Provider benchmarks use this to select a bounded sample without paying
    /// the full-corpus index construction cost or including it in RSS results.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when the persisted index is missing or invalid.
    pub fn read_artifact_chunks(path: &Path) -> Result<Vec<Chunk>, LexicalError> {
        Self::open_search_artifact(path)?.read_persisted_chunks()
    }

    /// Reads all chunks by resolving Git locators against managed repositories.
    ///
    /// This is intended for offline validation and benchmarks. Query paths
    /// hydrate only requested top-k chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when a locator or Git object is unavailable.
    pub fn read_git_artifact_chunks(
        path: &Path,
        repositories_root: &Path,
    ) -> Result<Vec<Chunk>, LexicalError> {
        Self::open_git_search_artifact(path, repositories_root)?.read_persisted_chunks()
    }

    fn read_persisted_chunks(&self) -> Result<Vec<Chunk>, LexicalError> {
        let searcher = self.reader.searcher();
        let limit = usize::try_from(searcher.num_docs())
            .map_err(|_| LexicalError::Artifact("lexical document count is too large".into()))?;
        let documents = searcher
            .search(&AllQuery, &TopDocs::with_limit(limit).order_by_score())
            .map_err(LexicalError::Search)?;
        let mut hydrator = EmbeddingTextHydrator::default();
        documents
            .into_iter()
            .map(|(_, address)| {
                let document = searcher
                    .doc::<TantivyDocument>(address)
                    .map_err(LexicalError::Search)?;
                self.hydrate_document(&document, &mut hydrator)
            })
            .collect()
    }

    fn read_descriptor(path: &Path) -> Result<LexicalDescriptor, LexicalError> {
        let file = File::open(path).map_err(|error| LexicalError::Io(error.to_string()))?;
        serde_json::from_reader(BufReader::new(file))
            .map_err(|error| LexicalError::Artifact(error.to_string()))
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
        let candidates = self.search_candidates(query, filters, limit)?;
        let ids = candidates
            .iter()
            .map(|candidate| candidate.chunk.chunk_id.clone())
            .collect();
        let mut chunks = self.chunks_by_id(&ids)?;
        candidates
            .into_iter()
            .map(|candidate| {
                let chunk = chunks
                    .remove(&candidate.chunk.chunk_id)
                    .ok_or(LexicalError::MissingChunkId)?;
                Ok(LexicalHit {
                    chunk,
                    score: candidate.score,
                    exact_identifier: candidate.exact_identifier,
                })
            })
            .collect()
    }

    /// Ranks lexical candidates without loading their passage text from Git.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError::EmptyQuery`] for an empty token set or
    /// [`LexicalError::Search`] when Tantivy rejects the query.
    #[allow(clippy::too_many_lines)] // Keep one readable query/ranking pipeline.
    pub fn search_candidates(
        &self,
        query: &str,
        filters: &LexicalFilters,
        limit: usize,
    ) -> Result<Vec<LexicalCandidate>, LexicalError> {
        let query_text = query.to_owned();
        let terms = query_terms(query);
        if terms.is_empty() {
            return Err(LexicalError::EmptyQuery);
        }
        let raw_terms = raw_query_terms(query);
        let symbol_terms = symbol_query_terms(query);
        let raw_term_count = raw_terms.len();
        let term_occur = if raw_term_count > 2 {
            Occur::Should
        } else {
            Occur::Must
        };
        let term_clauses: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|term| {
                (
                    query_term_occur(term, &raw_terms, raw_term_count, term_occur),
                    Box::new(indexed_text_term_query(self.fields, term)) as Box<dyn Query>,
                )
            })
            .collect();
        let text_query = BooleanQuery::new(vec![
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
        let repository_filter = filters
            .repository
            .as_ref()
            .map(|repository| self.repository_filter(repository))
            .transpose()?;
        let mut query_clauses = vec![(Occur::Must, Box::new(text_query) as Box<dyn Query>)];
        if let Some(category) = filters.category {
            query_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.category, category_label(category)),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if !filters.paths.is_empty() {
            query_clauses.push((
                Occur::Must,
                Box::new(BooleanQuery::new(
                    filters
                        .paths
                        .iter()
                        .map(|path| {
                            (
                                Occur::Should,
                                Box::new(TermQuery::new(
                                    Term::from_field_text(self.fields.path_raw, path),
                                    IndexRecordOption::Basic,
                                )) as Box<dyn Query>,
                            )
                        })
                        .collect(),
                )),
            ));
        }
        if let Some(revision) = &filters.revision {
            query_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.revision, revision.as_str()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(source_kind) = filters.source_kind {
            query_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.source_kind, source_kind_label(source_kind)),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(trust_tier) = filters.trust_tier {
            query_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.trust_tier, trust_label(trust_tier)),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        for tag in &filters.tags {
            query_clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.tags_raw, tag),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(filter) = &repository_filter {
            let repositories = TermSetQuery::new(filter.candidates.iter().map(|repository| {
                Term::from_field_text(self.fields.repository, repository.as_str())
            }));
            query_clauses.push((Occur::Must, Box::new(repositories)));
        }
        let query = BooleanQuery::new(query_clauses);
        let searcher = self.reader.searcher();
        let candidate_limit = limit.max(1).saturating_mul(10).min(100);
        let mut docs = collect_top_docs(
            &searcher,
            &query,
            repository_filter.as_ref(),
            candidate_limit,
        )?;
        if let Some(error) = repository_filter
            .as_ref()
            .and_then(RepositoryFilter::take_error)
        {
            return Err(error);
        }
        if filters.source_kind.is_none() {
            let non_commit_query = BooleanQuery::new(vec![
                (Occur::Must, Box::new(query.clone()) as Box<dyn Query>),
                (
                    Occur::MustNot,
                    Box::new(TermQuery::new(
                        Term::from_field_text(
                            self.fields.source_kind,
                            source_kind_label(SourceKind::GitCommit),
                        ),
                        IndexRecordOption::Basic,
                    )),
                ),
            ]);
            let non_commit_docs = collect_top_docs(
                &searcher,
                &non_commit_query,
                repository_filter.as_ref(),
                candidate_limit,
            )?;
            if let Some(error) = repository_filter
                .as_ref()
                .and_then(RepositoryFilter::take_error)
            {
                return Err(error);
            }
            for (score, address) in non_commit_docs {
                if !docs
                    .iter()
                    .any(|(_, existing_address)| *existing_address == address)
                {
                    docs.push((score, address));
                }
            }
        }
        if raw_term_count > 2 {
            let full_coverage = BooleanQuery::new(
                raw_terms
                    .iter()
                    .map(|term| {
                        (
                            Occur::Must,
                            Box::new(indexed_text_term_query(self.fields, term)) as Box<dyn Query>,
                        )
                    })
                    .collect(),
            );
            let full_coverage_query = BooleanQuery::new(vec![
                (Occur::Must, Box::new(query) as Box<dyn Query>),
                (
                    Occur::Must,
                    Box::new(ConstScoreQuery::new(Box::new(full_coverage), 0.0)),
                ),
            ]);
            let coverage_docs = collect_top_docs(
                &searcher,
                &full_coverage_query,
                repository_filter.as_ref(),
                candidate_limit,
            )?;
            if let Some(error) = repository_filter
                .as_ref()
                .and_then(RepositoryFilter::take_error)
            {
                return Err(error);
            }
            for (score, address) in coverage_docs {
                if !docs
                    .iter()
                    .any(|(_, existing_address)| *existing_address == address)
                {
                    docs.push((score, address));
                }
            }
        }
        let mut candidates = Vec::new();
        for (score, address) in docs {
            let document = searcher
                .doc::<TantivyDocument>(address)
                .map_err(LexicalError::Search)?;
            let (chunk, exact_identifier, source_kind) = if self.chunks.is_empty() {
                let locator = ChunkLocator::from_document(self.fields, &document)?;
                if !Self::locator_matches_filters(&locator, filters, repository_filter.as_ref()) {
                    continue;
                }
                let exact_identifier = has_exact_identifier(&locator.identifiers, &symbol_terms);
                (
                    locator.retrieval_metadata(),
                    exact_identifier,
                    locator.source_kind,
                )
            } else {
                let Some(id) = document
                    .get_first(self.fields.chunk_id)
                    .and_then(|value| value.as_str())
                else {
                    return Err(LexicalError::MissingChunkId);
                };
                let id = ChunkId::try_from(id).map_err(|_| LexicalError::MissingChunkId)?;
                let Some(chunk) = self.chunks.get(&id) else {
                    return Err(LexicalError::MissingChunkId);
                };
                if !matches_filters(chunk, filters) {
                    continue;
                }
                let exact_identifier = has_exact_identifier(&chunk.identifiers, &symbol_terms);
                (
                    chunk.retrieval_metadata(),
                    exact_identifier,
                    chunk.source_kind,
                )
            };
            let term_coverage = indexed_term_coverage(&searcher, address, self.fields, &raw_terms)?;
            candidates.push((
                LexicalCandidate {
                    chunk,
                    score,
                    exact_identifier,
                },
                term_coverage,
                source_kind == SourceKind::GitCommit,
            ));
        }
        sort_candidates(&mut candidates);
        candidates.truncate(limit.max(1));
        Ok(candidates
            .into_iter()
            .map(|(candidate, _term_coverage, _is_commit)| candidate)
            .collect())
    }

    /// Reads compact metadata for requested candidates without loading Git passages.
    ///
    /// Unknown or filtered IDs are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when Tantivy cannot read a matching locator.
    pub fn metadata_by_id(
        &self,
        ids: &BTreeSet<ChunkId>,
        filters: &LexicalFilters,
    ) -> Result<BTreeMap<ChunkId, RetrievalMetadata>, LexicalError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        if !self.chunks.is_empty() {
            return Ok(ids
                .iter()
                .filter_map(|id| {
                    self.chunks
                        .get(id)
                        .filter(|chunk| matches_filters(chunk, filters))
                        .map(|chunk| (id.clone(), chunk.retrieval_metadata()))
                })
                .collect());
        }

        let query = TermSetQuery::new(ids.iter().map(|id| {
            let encoded = id.encoded();
            Term::from_field_text(self.fields.chunk_id, encoded.as_str())
        }));
        let searcher = self.reader.searcher();
        let documents = searcher
            .search(&query, &TopDocs::with_limit(ids.len()).order_by_score())
            .map_err(LexicalError::Search)?;
        let mut metadata = BTreeMap::new();
        let repository_filter = filters
            .repository
            .as_ref()
            .map(|repository| self.repository_filter(repository))
            .transpose()?;
        for (_, address) in documents {
            let document = searcher
                .doc::<TantivyDocument>(address)
                .map_err(LexicalError::Search)?;
            let locator = ChunkLocator::from_document(self.fields, &document)?;
            if Self::locator_matches_filters(&locator, filters, repository_filter.as_ref()) {
                let candidate = locator.retrieval_metadata();
                metadata.insert(candidate.chunk_id.clone(), candidate);
            }
        }
        if let Some(error) = repository_filter
            .as_ref()
            .and_then(RepositoryFilter::take_error)
        {
            return Err(error);
        }
        Ok(metadata)
    }

    /// Reads only the requested chunks from the persisted Tantivy index.
    ///
    /// Unknown IDs are ignored, matching a map lookup against an in-memory
    /// index without deserializing unrelated stored chunks.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when Tantivy cannot read a matching document
    /// or its stored chunk is invalid.
    pub fn chunks_by_id(
        &self,
        ids: &BTreeSet<ChunkId>,
    ) -> Result<BTreeMap<ChunkId, Chunk>, LexicalError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        if !self.chunks.is_empty() {
            return Ok(ids
                .iter()
                .filter_map(|id| {
                    self.chunks
                        .get(id)
                        .cloned()
                        .map(|chunk| (id.clone(), chunk))
                })
                .collect());
        }

        let query = TermSetQuery::new(ids.iter().map(|id| {
            let encoded = id.encoded();
            Term::from_field_text(self.fields.chunk_id, encoded.as_str())
        }));
        let searcher = self.reader.searcher();
        let documents = searcher
            .search(&query, &TopDocs::with_limit(ids.len()).order_by_score())
            .map_err(LexicalError::Search)?;
        let mut locators = documents
            .into_iter()
            .map(|(_, address)| {
                let document = searcher
                    .doc::<TantivyDocument>(address)
                    .map_err(LexicalError::Search)?;
                ChunkLocator::from_document(self.fields, &document)
            })
            .collect::<Result<Vec<_>, _>>()?;
        locators.sort_unstable_by(ChunkLocator::compare_hydration_order);
        let mut hydrator = EmbeddingTextHydrator::default();
        locators
            .into_iter()
            .map(|locator| {
                let chunk = self.hydrate_locator(locator, &mut hydrator)?;
                Ok((chunk.chunk_id.clone(), chunk))
            })
            .collect()
    }

    pub(crate) fn embedding_texts_by_id(
        &self,
        ids: &[ChunkId],
        hydrator: &mut EmbeddingTextHydrator,
    ) -> Result<Vec<String>, LexicalError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let query = TermSetQuery::new(ids.iter().map(|id| {
            let encoded = id.encoded();
            Term::from_field_text(self.fields.chunk_id, encoded.as_str())
        }));
        let searcher = self.reader.searcher();
        let documents = searcher
            .search(&query, &TopDocs::with_limit(ids.len()).order_by_score())
            .map_err(LexicalError::Search)?;
        let mut locators = Vec::with_capacity(documents.len());
        for (_, address) in documents {
            let document = searcher
                .doc::<TantivyDocument>(address)
                .map_err(LexicalError::Search)?;
            locators.push(ChunkLocator::from_document(self.fields, &document)?);
        }
        locators.sort_unstable_by(|left, right| {
            left.document_id
                .cmp(&right.document_id)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        let mut texts = BTreeMap::new();
        for locator in locators {
            let id = locator.chunk_id.clone();
            let text = match locator.source_kind {
                SourceKind::EmbeddedCatalog => {
                    let chunk = embedded_chunk(&id).ok_or_else(|| {
                        LexicalError::Artifact(format!("embedded chunk {id} is unavailable"))
                    })?;
                    crate::semantic::embedding_text(&chunk)
                }
                SourceKind::GitBlob | SourceKind::GitCommit => {
                    hydrator.git_embedding_text(self, &locator)?
                }
                source_kind => {
                    return Err(LexicalError::Artifact(format!(
                        "persisted {source_kind:?} chunk {id} has no canonical Git source"
                    )));
                }
            };
            texts.insert(id, text);
        }
        ids.iter()
            .map(|id| {
                texts.remove(id).ok_or_else(|| {
                    LexicalError::Artifact(format!(
                        "lexical artifact is missing requested chunk {id}"
                    ))
                })
            })
            .collect()
    }

    pub(crate) fn embedding_texts_by_id_from_inputs(
        &self,
        ids: &[ChunkId],
        inputs: &[EmbeddingLocatorRecord],
        hydrator: &mut EmbeddingTextHydrator,
    ) -> Result<Vec<String>, LexicalError> {
        let mut records = BTreeMap::new();
        for input in inputs {
            records.insert(input.chunk_id.clone(), input);
        }
        self.embedding_texts_by_id_from_record_map(ids, &records, hydrator)
    }

    pub(crate) fn embedding_texts_by_id_from_record_map(
        &self,
        ids: &[ChunkId],
        records: &BTreeMap<ChunkId, &EmbeddingLocatorRecord>,
        hydrator: &mut EmbeddingTextHydrator,
    ) -> Result<Vec<String>, LexicalError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        ids.iter()
            .map(|id| {
                let input = records.get(id).ok_or_else(|| {
                    LexicalError::Artifact(format!(
                        "lexical embedding sidecar is missing requested chunk {id}"
                    ))
                })?;
                let locator = (*input).clone().into_locator()?;
                match locator.source_kind {
                    SourceKind::EmbeddedCatalog => embedded_chunk(id)
                        .map(|chunk| crate::semantic::embedding_text(&chunk))
                        .ok_or_else(|| {
                            LexicalError::Artifact(format!("embedded chunk {id} is unavailable"))
                        }),
                    SourceKind::GitBlob | SourceKind::GitCommit => {
                        hydrator.git_embedding_text(self, &locator)
                    }
                    source_kind => Err(LexicalError::Artifact(format!(
                        "persisted {source_kind:?} chunk {id} has no canonical Git source"
                    ))),
                }
            })
            .collect()
    }

    /// Reads only the chunks belonging to one document from the persisted index.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalError`] when Tantivy cannot read a matching document
    /// or its stored chunk is invalid.
    pub fn chunks_by_document_id(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<Chunk>, LexicalError> {
        if !self.chunks.is_empty() {
            let mut chunks = self
                .chunks
                .values()
                .filter(|chunk| &chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>();
            chunks.sort_by_key(|chunk| chunk.ordinal);
            return Ok(chunks);
        }

        let query = TermQuery::new(
            Term::from_field_text(self.fields.document_id, document_id.as_str()),
            IndexRecordOption::Basic,
        );
        let searcher = self.reader.searcher();
        let count = searcher
            .search(&query, &Count)
            .map_err(LexicalError::Search)?;
        let documents = searcher
            .search(&query, &TopDocs::with_limit(count).order_by_score())
            .map_err(LexicalError::Search)?;
        let mut hydrator = EmbeddingTextHydrator::default();
        let mut chunks = documents
            .into_iter()
            .map(|(_, address)| {
                let document = searcher
                    .doc::<TantivyDocument>(address)
                    .map_err(LexicalError::Search)?;
                self.hydrate_document(&document, &mut hydrator)
            })
            .collect::<Result<Vec<_>, _>>()?;
        chunks.sort_by_key(|chunk| chunk.ordinal);
        Ok(chunks)
    }

    fn hydrate_document(
        &self,
        document: &TantivyDocument,
        hydrator: &mut EmbeddingTextHydrator,
    ) -> Result<Chunk, LexicalError> {
        let locator = ChunkLocator::from_document(self.fields, document)?;
        self.hydrate_locator(locator, hydrator)
    }

    fn hydrate_locator(
        &self,
        locator: ChunkLocator,
        hydrator: &mut EmbeddingTextHydrator,
    ) -> Result<Chunk, LexicalError> {
        match locator.source_kind {
            SourceKind::EmbeddedCatalog => embedded_chunk(&locator.chunk_id).ok_or_else(|| {
                LexicalError::Artifact(format!(
                    "embedded chunk {} is unavailable",
                    locator.chunk_id
                ))
            }),
            SourceKind::GitBlob | SourceKind::GitCommit => hydrator.git_chunk(self, locator),
            source_kind => Err(LexicalError::Artifact(format!(
                "persisted {source_kind:?} chunk {} has no canonical Git source",
                locator.chunk_id
            ))),
        }
    }

    fn repository_filter(
        &self,
        requested: &RepositoryId,
    ) -> Result<RepositoryFilter, LexicalError> {
        let candidates = self.git_sources.get(requested).map_or_else(
            || vec![requested.clone()],
            |requested_source| {
                self.git_sources
                    .iter()
                    .filter(|(_, source)| source.contract == requested_source.contract)
                    .map(|(repository, _)| repository.clone())
                    .collect()
            },
        );
        let reachability = candidates
            .iter()
            .any(|repository| repository != requested)
            .then(|| {
                let source = self.git_sources.get(requested).ok_or_else(|| {
                    LexicalError::Artifact(
                        "repository alias has no persisted source contract".into(),
                    )
                })?;
                let path = self
                    .repository_paths
                    .get(requested)
                    .cloned()
                    .or_else(|| {
                        self.repositories_root
                            .as_ref()
                            .map(|root| root.join(format!("{}.git", requested.as_str())))
                    })
                    .ok_or_else(|| LexicalError::GitFilter {
                        repository: requested.to_string(),
                        revision: source.revision.to_string(),
                        message: "managed Git repository root is not configured".into(),
                    })?;
                let repository = gix::open(&path).map_err(|error| LexicalError::GitFilter {
                    repository: requested.to_string(),
                    revision: source.revision.to_string(),
                    message: format!("open {}: {error}", path.display()),
                })?;
                let tip = gix::ObjectId::from_hex(source.revision.as_str().as_bytes()).map_err(
                    |error| LexicalError::GitFilter {
                        repository: requested.to_string(),
                        revision: source.revision.to_string(),
                        message: format!("parse selected tip: {error}"),
                    },
                )?;
                Ok::<_, LexicalError>(Arc::new(Mutex::new(GitReachability {
                    repository: repository.into_sync(),
                    tip,
                    cache: [None; REACHABILITY_CACHE_SLOTS],
                    error: None,
                })))
            })
            .transpose()?;
        Ok(RepositoryFilter {
            requested: requested.clone(),
            candidates,
            reachability,
        })
    }

    fn git_repository<'repositories>(
        &self,
        repository_id: &RepositoryId,
        locator: &ChunkLocator,
        repositories: &'repositories mut BTreeMap<RepositoryId, gix::Repository>,
    ) -> Result<&'repositories gix::Repository, LexicalError> {
        if !repositories.contains_key(repository_id) {
            let path = self
                .repository_paths
                .get(repository_id)
                .cloned()
                .or_else(|| {
                    self.repositories_root
                        .as_ref()
                        .map(|root| root.join(format!("{}.git", repository_id.as_str())))
                })
                .ok_or_else(|| {
                    locator.git_error("managed Git repository root is not configured")
                })?;
            let repository = gix::open(&path)
                .map_err(|error| locator.git_error(format!("open {}: {error}", path.display())))?;
            repositories.insert(repository_id.clone(), repository);
        }
        Ok(repositories
            .get(repository_id)
            .expect("repository inserted above"))
    }

    fn locator_matches_filters(
        locator: &ChunkLocator,
        filters: &LexicalFilters,
        repository_filter: Option<&RepositoryFilter>,
    ) -> bool {
        if !locator.matches_non_repository_filters(filters) {
            return false;
        }
        repository_filter.is_none_or(|filter| filter.matches_locator(locator))
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

    /// Consumes the index and returns its stored chunks in stable ID order.
    #[must_use]
    pub fn into_chunks(self) -> Vec<Chunk> {
        self.chunks.into_values().collect()
    }
}

struct ChunkLocator {
    chunk_id: ChunkId,
    document_id: DocumentId,
    ordinal: u32,
    title: String,
    source_kind: SourceKind,
    repository: RepositoryId,
    revision: Revision,
    path: String,
    heading_path: Vec<String>,
    source_span: Option<SourceSpan>,
    char_count: u32,
    byte_count: u64,
    category: Option<Category>,
    tags: BTreeSet<String>,
    identifiers: Vec<compact_str::CompactString>,
    registered_id: Option<String>,
    trust_tier: TrustTier,
    previous_chunk: Option<ChunkId>,
    next_chunk: Option<ChunkId>,
    content_digest: ContentDigest,
    git_object_id: Option<gix::ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EmbeddingLocatorRecord {
    chunk_id: ChunkId,
    document_id: DocumentId,
    title: String,
    source_kind: SourceKind,
    repository: RepositoryId,
    revision: Revision,
    path: String,
    heading_path: Vec<String>,
    source_span: Option<SourceSpan>,
    identifiers: Vec<compact_str::CompactString>,
    tags: BTreeSet<String>,
    git_object_id: Option<String>,
}

impl EmbeddingLocatorRecord {
    pub(crate) const fn chunk_id(&self) -> &ChunkId {
        &self.chunk_id
    }

    fn from_chunk(chunk: &Chunk, git_object_id: Option<gix::ObjectId>) -> Self {
        Self {
            chunk_id: chunk.chunk_id.clone(),
            document_id: chunk.document_id.clone(),
            title: chunk.title.clone(),
            source_kind: chunk.source_kind,
            repository: chunk.repository.clone(),
            revision: chunk.revision.clone(),
            path: chunk.path.clone(),
            heading_path: chunk.heading_path.clone(),
            source_span: chunk.source_span,
            identifiers: chunk.identifiers.clone(),
            tags: chunk.tags.clone(),
            git_object_id: git_object_id.map(|object| object.to_string()),
        }
    }

    fn into_locator(self) -> Result<ChunkLocator, LexicalError> {
        let git_object_id = self
            .git_object_id
            .as_deref()
            .map(|object| {
                gix::ObjectId::from_hex(object.as_bytes()).map_err(|error| {
                    LexicalError::Artifact(format!("invalid Git object id: {error}"))
                })
            })
            .transpose()?;
        Ok(ChunkLocator {
            chunk_id: self.chunk_id,
            document_id: self.document_id,
            ordinal: 0,
            title: self.title,
            source_kind: self.source_kind,
            repository: self.repository,
            revision: self.revision,
            path: self.path,
            heading_path: self.heading_path,
            source_span: self.source_span,
            char_count: 0,
            byte_count: 0,
            category: None,
            tags: self.tags,
            identifiers: self.identifiers,
            registered_id: None,
            trust_tier: TrustTier::FirstParty,
            previous_chunk: None,
            next_chunk: None,
            content_digest: ContentDigest::of(b"embedding-locator"),
            git_object_id,
        })
    }
}

impl ChunkLocator {
    fn compare_hydration_order(left: &Self, right: &Self) -> std::cmp::Ordering {
        left.repository
            .cmp(&right.repository)
            .then_with(|| left.git_object_id.cmp(&right.git_object_id))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    }

    fn resolve_git_blob(
        &self,
        repository: &gix::Repository,
    ) -> Result<gix::ObjectId, LexicalError> {
        let revision = gix::ObjectId::from_hex(self.revision.as_str().as_bytes())
            .map_err(|error| self.git_error(format!("parse revision: {error}")))?;
        let commit = repository
            .find_commit(revision)
            .map_err(|error| self.git_error(format!("read commit: {error}")))?;
        let tree = commit
            .tree()
            .map_err(|error| self.git_error(format!("read commit tree: {error}")))?;
        let entry = tree
            .lookup_entry_by_path(&self.path)
            .map_err(|error| self.git_error(format!("resolve path: {error}")))?
            .ok_or_else(|| self.git_error("path is absent from the pinned commit"))?;
        if !entry.mode().is_blob() {
            return Err(self.git_error("path in the pinned commit is not a blob"));
        }
        Ok(entry.object_id())
    }

    fn from_document(
        fields: LexicalFields,
        document: &TantivyDocument,
    ) -> Result<Self, LexicalError> {
        let source_kind =
            parse_source_kind(required_text(document, fields.source_kind, "source_kind")?)?;
        let category = match required_text(document, fields.category, "category")? {
            "" => None,
            value => Some(parse_category(value)?),
        };
        let start_line = optional_u64(document, fields.start_line)
            .map(|value| u32::try_from(value).map_err(|_| invalid_field("start_line")))
            .transpose()?;
        let end_line = optional_u64(document, fields.end_line)
            .map(|value| u32::try_from(value).map_err(|_| invalid_field("end_line")))
            .transpose()?;
        let source_span = match (start_line, end_line) {
            (None, None) => None,
            (Some(start_line), Some(end_line)) => Some(
                SourceSpan::new(
                    start_line,
                    end_line,
                    optional_u64(document, fields.start_byte),
                    optional_u64(document, fields.end_byte),
                )
                .map_err(|_| invalid_field("source_span"))?,
            ),
            _ => return Err(invalid_field("source_span")),
        };
        Ok(Self {
            chunk_id: ChunkId::try_from(required_text(document, fields.chunk_id, "chunk_id")?)
                .map_err(|_| invalid_field("chunk_id"))?,
            document_id: DocumentId::try_from(required_text(
                document,
                fields.document_id,
                "document_id",
            )?)
            .map_err(|_| invalid_field("document_id"))?,
            ordinal: u32::try_from(required_u64(document, fields.ordinal, "ordinal")?)
                .map_err(|_| invalid_field("ordinal"))?,
            title: required_text(document, fields.title, "title")?.to_owned(),
            source_kind,
            repository: RepositoryId::try_from(required_text(
                document,
                fields.repository,
                "repository",
            )?)
            .map_err(|_| invalid_field("repository"))?,
            revision: Revision::try_from(required_text(document, fields.revision, "revision")?)
                .map_err(|_| invalid_field("revision"))?,
            path: required_text(document, fields.path, "path")?.to_owned(),
            heading_path: stored_texts(document, fields.heading_path),
            source_span,
            char_count: u32::try_from(required_u64(document, fields.char_count, "char_count")?)
                .map_err(|_| invalid_field("char_count"))?,
            byte_count: required_u64(document, fields.byte_count, "byte_count")?,
            category,
            tags: stored_texts(document, fields.tags_raw)
                .into_iter()
                .collect(),
            identifiers: stored_texts(document, fields.identifiers_raw)
                .into_iter()
                .map(compact_str::CompactString::from)
                .collect(),
            registered_id: optional_text(document, fields.registered_id).map(str::to_owned),
            trust_tier: parse_trust_tier(required_text(
                document,
                fields.trust_tier,
                "trust_tier",
            )?)?,
            previous_chunk: optional_text(document, fields.previous_chunk)
                .map(ChunkId::try_from)
                .transpose()
                .map_err(|_| invalid_field("previous_chunk"))?,
            next_chunk: optional_text(document, fields.next_chunk)
                .map(ChunkId::try_from)
                .transpose()
                .map_err(|_| invalid_field("next_chunk"))?,
            content_digest: ContentDigest::try_from(required_text(
                document,
                fields.content_digest,
                "content_digest",
            )?)
            .map_err(|_| invalid_field("content_digest"))?,
            git_object_id: document
                .get_first(fields.git_object_id)
                .and_then(|value| value.as_bytes())
                .map(gix::ObjectId::try_from)
                .transpose()
                .map_err(|_| invalid_field("git_object_id"))?,
        })
    }

    fn hydrate(self, content: &str) -> Result<Chunk, LexicalError> {
        let text = self.passage(content)?.to_owned();
        let resource_uri =
            ResourceUri::try_from(format!("vesc://knowledge/chunk/{}", self.chunk_id))
                .expect("stored chunk id always forms a valid resource URI");
        let chunk = Chunk {
            schema: CORPUS_SCHEMA_V1,
            chunk_id: self.chunk_id,
            document_id: self.document_id,
            ordinal: self.ordinal,
            title: self.title,
            source_kind: self.source_kind,
            repository: self.repository,
            revision: self.revision,
            path: self.path,
            heading_path: self.heading_path,
            text,
            source_span: self.source_span,
            char_count: self.char_count,
            byte_count: self.byte_count,
            category: self.category,
            tags: self.tags,
            identifiers: self.identifiers,
            registered_id: self.registered_id,
            trust_tier: self.trust_tier,
            resource_uri: Some(resource_uri),
            previous_chunk: self.previous_chunk,
            next_chunk: self.next_chunk,
            content_digest: self.content_digest,
        };
        chunk.validate().map_err(|error| {
            LexicalError::Artifact(format!("hydrated chunk is invalid: {error}"))
        })?;
        Ok(chunk)
    }

    fn passage<'content>(&self, content: &'content str) -> Result<&'content str, LexicalError> {
        let span = self
            .source_span
            .ok_or_else(|| self.git_error("chunk locator has no source span"))?;
        let start = span
            .start_byte
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| self.git_error("chunk locator has no valid start byte"))?;
        let end = span
            .end_byte
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| self.git_error("chunk locator has no valid end byte"))?;
        content
            .get(start..end)
            .ok_or_else(|| self.git_error("chunk byte range is outside the normalized blob"))
    }

    fn retrieval_metadata(&self) -> RetrievalMetadata {
        RetrievalMetadata {
            chunk_id: self.chunk_id.clone(),
            document_id: self.document_id.clone(),
            registered_id: self.registered_id.clone(),
            content_digest: self.content_digest.clone(),
        }
    }

    fn matches_non_repository_filters(&self, filters: &LexicalFilters) -> bool {
        filters
            .category
            .is_none_or(|category| self.category == Some(category))
            && (filters.paths.is_empty() || filters.paths.contains(&self.path))
            && filters
                .revision
                .as_ref()
                .is_none_or(|revision| &self.revision == revision)
            && filters
                .source_kind
                .is_none_or(|source_kind| self.source_kind == source_kind)
            && filters
                .trust_tier
                .is_none_or(|trust| self.trust_tier == trust)
            && filters.tags.iter().all(|tag| self.tags.contains(tag))
    }

    fn git_error(&self, message: impl Into<String>) -> LexicalError {
        LexicalError::GitHydration {
            repository: self.repository.to_string(),
            revision: self.revision.to_string(),
            path: self.path.clone(),
            message: message.into(),
        }
    }
}

fn embedded_chunk(id: &ChunkId) -> Option<Chunk> {
    crate::embedded_entries().iter().find_map(|entry| {
        crate::corpus::NormalizedDocument::from_catalog_entry(entry)
            .and_then(|document| document.catalog_chunk())
            .ok()
            .filter(|chunk| &chunk.chunk_id == id)
    })
}

fn required_text<'a>(
    document: &'a TantivyDocument,
    field: Field,
    name: &'static str,
) -> Result<&'a str, LexicalError> {
    optional_text(document, field).ok_or_else(|| invalid_field(name))
}

fn optional_text(document: &TantivyDocument, field: Field) -> Option<&str> {
    document.get_first(field).and_then(|value| value.as_str())
}

fn stored_texts(document: &TantivyDocument, field: Field) -> Vec<String> {
    document
        .get_all(field)
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn required_u64(
    document: &TantivyDocument,
    field: Field,
    name: &'static str,
) -> Result<u64, LexicalError> {
    optional_u64(document, field).ok_or_else(|| invalid_field(name))
}

fn optional_u64(document: &TantivyDocument, field: Field) -> Option<u64> {
    document.get_first(field).and_then(|value| value.as_u64())
}

fn invalid_field(name: &'static str) -> LexicalError {
    LexicalError::Artifact(format!("persisted lexical locator has invalid {name}"))
}

fn parse_category(value: &str) -> Result<Category, LexicalError> {
    match value {
        "firmware_api" => Ok(Category::FirmwareApi),
        "lispbm" => Ok(Category::Lispbm),
        "package_build" => Ok(Category::PackageBuild),
        "refloat_command" => Ok(Category::RefloatCommand),
        "native_lib_abi" => Ok(Category::NativeLibAbi),
        _ => Err(invalid_field("category")),
    }
}

fn parse_source_kind(value: &str) -> Result<SourceKind, LexicalError> {
    match value {
        "embedded_catalog" => Ok(SourceKind::EmbeddedCatalog),
        "markdown" => Ok(SourceKind::Markdown),
        "catalog_yaml" => Ok(SourceKind::CatalogYaml),
        "catalog_json" => Ok(SourceKind::CatalogJson),
        "fixture" => Ok(SourceKind::Fixture),
        "vendor_file" => Ok(SourceKind::VendorFile),
        "git_blob" => Ok(SourceKind::GitBlob),
        "git_commit" => Ok(SourceKind::GitCommit),
        "model_feedback" => Ok(SourceKind::ModelFeedback),
        _ => Err(invalid_field("source_kind")),
    }
}

fn parse_trust_tier(value: &str) -> Result<TrustTier, LexicalError> {
    match value {
        "first_party" => Ok(TrustTier::FirstParty),
        "curated_upstream" => Ok(TrustTier::CuratedUpstream),
        "fixture" => Ok(TrustTier::Fixture),
        "unverified_model_feedback" => Ok(TrustTier::UnverifiedModelFeedback),
        _ => Err(invalid_field("trust_tier")),
    }
}

fn indexed_text_term_query(fields: LexicalFields, text: &str) -> BooleanQuery {
    BooleanQuery::new(
        [
            fields.title,
            fields.path,
            fields.identifiers,
            fields.body,
            fields.tags,
        ]
        .into_iter()
        .map(|field| {
            (
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(field, text),
                    IndexRecordOption::WithFreqs,
                )) as Box<dyn Query>,
            )
        })
        .collect(),
    )
}

fn collect_top_docs(
    searcher: &tantivy::Searcher,
    query: &dyn Query,
    repository_filter: Option<&RepositoryFilter>,
    limit: usize,
) -> Result<Vec<(f32, tantivy::DocAddress)>, LexicalError> {
    repository_filter
        .map_or_else(
            || searcher.search(query, &TopDocs::with_limit(limit).order_by_score()),
            |filter| {
                let predicate = filter.clone();
                let collector = BytesFilterCollector::new(
                    "repository_revision".into(),
                    move |key: &[u8]| predicate.matches_key(key),
                    TopDocs::with_limit(limit).order_by_score(),
                );
                searcher.search(query, &collector)
            },
        )
        .map_err(LexicalError::Search)
}

fn sort_candidates(candidates: &mut [(LexicalCandidate, usize, bool)]) {
    candidates.sort_by(
        |(left, left_coverage, left_is_commit), (right, right_coverage, right_is_commit)| {
            right
                .exact_identifier
                .cmp(&left.exact_identifier)
                .then_with(|| {
                    if left.exact_identifier && right.exact_identifier {
                        right
                            .chunk
                            .registered_id
                            .is_some()
                            .cmp(&left.chunk.registered_id.is_some())
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| right_coverage.cmp(left_coverage))
                .then_with(|| left_is_commit.cmp(right_is_commit))
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
        },
    );
}

fn indexed_term_coverage(
    searcher: &tantivy::Searcher,
    address: tantivy::DocAddress,
    fields: LexicalFields,
    terms: &[String],
) -> Result<usize, LexicalError> {
    let segment = searcher.segment_reader(address.segment_ord);
    let fields = [
        fields.title,
        fields.path,
        fields.identifiers,
        fields.body,
        fields.tags,
    ];
    let mut coverage = 0;
    for text in terms {
        let mut matched = false;
        for field in fields {
            let inverted = segment
                .inverted_index(field)
                .map_err(LexicalError::Search)?;
            let term = Term::from_field_text(field, text);
            let Some(mut postings) = inverted
                .read_postings(&term, IndexRecordOption::Basic)
                .map_err(|error| LexicalError::Search(error.into()))?
            else {
                continue;
            };
            let current = postings.doc();
            if current == address.doc_id
                || (current < address.doc_id && postings.seek(address.doc_id) == address.doc_id)
            {
                matched = true;
                break;
            }
        }
        coverage += usize::from(matched);
    }
    Ok(coverage)
}

fn schema() -> (Schema, LexicalFields) {
    let mut builder = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let title = builder.add_text_field("title", text.clone().set_stored());
    let path = builder.add_text_field("path", text.clone().set_stored());
    let path_raw = builder.add_text_field("path_raw", STRING);
    let identifiers = builder.add_text_field("identifiers", text.clone());
    let identifiers_raw = builder.add_text_field("identifiers_raw", STRING | STORED);
    let body = builder.add_text_field("body", text.clone());
    let tags = builder.add_text_field("tags", text);
    let tags_raw = builder.add_text_field("tags_raw", STRING | STORED);
    let chunk_id = builder.add_text_field("chunk_id", STRING | STORED);
    let document_id = builder.add_text_field("document_id", STRING | STORED);
    let chunk_digest = [
        builder.add_u64_field("chunk_digest_0", tantivy::schema::FAST),
        builder.add_u64_field("chunk_digest_1", tantivy::schema::FAST),
        builder.add_u64_field("chunk_digest_2", tantivy::schema::FAST),
        builder.add_u64_field("chunk_digest_3", tantivy::schema::FAST),
    ];
    let ordinal = builder.add_u64_field("ordinal", STORED);
    let category = builder.add_text_field("category", STRING | STORED);
    let repository = builder.add_text_field("repository", STRING | STORED);
    let revision = builder.add_text_field("revision", STRING | STORED);
    let source_kind = builder.add_text_field("source_kind", STRING | STORED);
    let trust_tier = builder.add_text_field("trust_tier", STRING | STORED);
    let heading_path = builder.add_text_field("heading_path", STORED);
    let start_line = builder.add_u64_field("start_line", STORED);
    let end_line = builder.add_u64_field("end_line", STORED);
    let start_byte = builder.add_u64_field("start_byte", STORED);
    let end_byte = builder.add_u64_field("end_byte", STORED);
    let registered_id = builder.add_text_field("registered_id", STORED);
    let previous_chunk = builder.add_text_field("previous_chunk", STORED);
    let next_chunk = builder.add_text_field("next_chunk", STORED);
    let content_digest = builder.add_text_field("content_digest", STORED);
    let char_count = builder.add_u64_field("char_count", STORED);
    let byte_count = builder.add_u64_field("byte_count", STORED);
    let git_object_id = builder.add_bytes_field("git_object_id", STORED);
    let history_content_key = builder.add_text_field("history_content_key", STRING);
    let repository_revision = builder.add_bytes_field("repository_revision", tantivy::schema::FAST);
    let schema = builder.build();
    (
        schema,
        LexicalFields {
            title,
            path,
            path_raw,
            identifiers,
            identifiers_raw,
            body,
            tags,
            tags_raw,
            chunk_id,
            document_id,
            chunk_digest,
            ordinal,
            category,
            repository,
            revision,
            source_kind,
            trust_tier,
            heading_path,
            start_line,
            end_line,
            start_byte,
            end_byte,
            registered_id,
            previous_chunk,
            next_chunk,
            content_digest,
            char_count,
            byte_count,
            git_object_id,
            history_content_key,
            repository_revision,
        },
    )
}

fn add_chunk(writer: &IndexWriter, fields: LexicalFields, chunk: &Chunk) {
    writer
        .add_document(tantivy_document(fields, chunk, None))
        .expect("in-memory lexical document is valid");
}

trait TantivyChunkFields {
    fn chunk_id(&self) -> &ChunkId;
    fn document_id(&self) -> &str;
    fn ordinal(&self) -> u32;
    fn title(&self) -> &str;
    fn source_kind(&self) -> SourceKind;
    fn repository(&self) -> &str;
    fn revision(&self) -> &str;
    fn path(&self) -> &str;
    fn headings(&self) -> impl Iterator<Item = &str> + Clone;
    fn text(&self) -> &str;
    fn source_span(&self) -> Option<SourceSpan>;
    fn category(&self) -> Option<Category>;
    fn tags(&self) -> impl Iterator<Item = &str> + Clone;
    fn identifiers(&self) -> impl Iterator<Item = &str> + Clone;
    fn registered_id(&self) -> Option<&str>;
    fn trust_tier(&self) -> TrustTier;
    fn previous_chunk(&self) -> Option<&ChunkId>;
    fn next_chunk(&self) -> Option<&ChunkId>;
    fn content_digest(&self) -> &ContentDigest;
    fn char_count(&self) -> u32;
    fn byte_count(&self) -> u64;
    fn history_content_key(&self) -> Option<ContentDigest>;
}

impl TantivyChunkFields for Chunk {
    fn chunk_id(&self) -> &ChunkId {
        &self.chunk_id
    }

    fn document_id(&self) -> &str {
        self.document_id.as_str()
    }

    fn ordinal(&self) -> u32 {
        self.ordinal
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    fn repository(&self) -> &str {
        self.repository.as_str()
    }

    fn revision(&self) -> &str {
        self.revision.as_str()
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn headings(&self) -> impl Iterator<Item = &str> + Clone {
        self.heading_path.iter().map(String::as_str)
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn source_span(&self) -> Option<SourceSpan> {
        self.source_span
    }

    fn category(&self) -> Option<Category> {
        self.category
    }

    fn tags(&self) -> impl Iterator<Item = &str> + Clone {
        self.tags.iter().map(String::as_str)
    }

    fn identifiers(&self) -> impl Iterator<Item = &str> + Clone {
        self.identifiers
            .iter()
            .map(compact_str::CompactString::as_str)
    }

    fn registered_id(&self) -> Option<&str> {
        self.registered_id.as_deref()
    }

    fn trust_tier(&self) -> TrustTier {
        self.trust_tier
    }

    fn previous_chunk(&self) -> Option<&ChunkId> {
        self.previous_chunk.as_ref()
    }

    fn next_chunk(&self) -> Option<&ChunkId> {
        self.next_chunk.as_ref()
    }

    fn content_digest(&self) -> &ContentDigest {
        &self.content_digest
    }

    fn char_count(&self) -> u32 {
        self.char_count
    }

    fn byte_count(&self) -> u64 {
        self.byte_count
    }

    fn history_content_key(&self) -> Option<ContentDigest> {
        crate::corpus::history_content_key_for_chunk(self)
    }
}

impl TantivyChunkFields for GitHistoryChunkView<'_> {
    fn chunk_id(&self) -> &ChunkId {
        self.chunk_id()
    }

    fn document_id(&self) -> &str {
        self.document_id().as_str()
    }

    fn ordinal(&self) -> u32 {
        self.ordinal()
    }

    fn title(&self) -> &str {
        self.title()
    }

    fn source_kind(&self) -> SourceKind {
        self.source_kind()
    }

    fn repository(&self) -> &str {
        self.repository().as_str()
    }

    fn revision(&self) -> &str {
        self.revision().as_str()
    }

    fn path(&self) -> &str {
        self.path()
    }

    fn headings(&self) -> impl Iterator<Item = &str> + Clone {
        self.headings().iter().copied()
    }

    fn text(&self) -> &str {
        self.text()
    }

    fn source_span(&self) -> Option<SourceSpan> {
        self.source_span()
    }

    fn category(&self) -> Option<Category> {
        self.category()
    }

    fn tags(&self) -> impl Iterator<Item = &str> + Clone {
        self.tags().iter().map(String::as_str)
    }

    fn identifiers(&self) -> impl Iterator<Item = &str> + Clone {
        self.identifiers()
            .iter()
            .map(compact_str::CompactString::as_str)
    }

    fn registered_id(&self) -> Option<&str> {
        self.registered_id()
    }

    fn trust_tier(&self) -> TrustTier {
        self.trust_tier()
    }

    fn previous_chunk(&self) -> Option<&ChunkId> {
        self.previous_chunk()
    }

    fn next_chunk(&self) -> Option<&ChunkId> {
        self.next_chunk()
    }

    fn content_digest(&self) -> &ContentDigest {
        self.content_digest()
    }

    fn char_count(&self) -> u32 {
        self.char_count()
    }

    fn byte_count(&self) -> u64 {
        self.byte_count()
    }

    fn history_content_key(&self) -> Option<ContentDigest> {
        Some(GitHistoryChunkView::history_content_key(self))
    }
}

fn add_git_history_chunk(
    writer: &IndexWriter,
    fields: LexicalFields,
    chunk: &GitHistoryChunkView<'_>,
    object: gix::ObjectId,
) {
    writer
        .add_document(tantivy_document(fields, chunk, Some(object)))
        .expect("in-memory lexical document is valid");
}

fn write_git_history_chunks(
    plan: &GitHistoryBuildPlan,
    sources: &[GitCorpusSource],
    writer: &IndexWriter,
    fields: LexicalFields,
    sidecars: &mut GitHistorySidecars,
) -> Result<(), LexicalError> {
    let mut sidecar_error = None;
    plan.try_for_each_history_view(sources, |view, blob| {
        if sidecar_error.is_some() {
            return;
        }
        add_git_history_chunk(writer, fields, &view, blob);
        if let Err(error) = sidecars.push_history(&view, blob) {
            sidecar_error = Some(error);
        }
        #[cfg(feature = "coz-profile")]
        if sidecar_error.is_none() {
            crate::profile_progress!("lexical_indexed_chunk");
        }
    })
    .map_err(|error| LexicalError::Artifact(error.to_string()))?;
    sidecar_error.map_or(Ok(()), Err)
}

#[allow(clippy::too_many_lines)] // Direct field writes avoid per-chunk builder abstractions.
fn tantivy_document<T: TantivyChunkFields>(
    fields: LexicalFields,
    chunk: &T,
    git_object_id: Option<gix::ObjectId>,
) -> TantivyDocument {
    let history_content_key = chunk.history_content_key();
    let content_digest = chunk.content_digest().encoded();
    let history_content_key_encoded = history_content_key.as_ref().map(ContentDigest::encoded);
    let field_value_count = 20_usize
        .saturating_add(chunk.identifiers().clone().count().saturating_mul(2))
        .saturating_add(chunk.tags().clone().count())
        .saturating_add(chunk.headings().clone().count())
        .saturating_add(usize::from(chunk.source_span().is_some()).saturating_mul(2))
        .saturating_add(usize::from(
            chunk
                .source_span()
                .is_some_and(|span| span.start_byte.is_some()),
        ))
        .saturating_add(usize::from(
            chunk
                .source_span()
                .is_some_and(|span| span.end_byte.is_some()),
        ))
        .saturating_add(usize::from(chunk.registered_id().is_some()))
        .saturating_add(usize::from(chunk.previous_chunk().is_some()))
        .saturating_add(usize::from(chunk.next_chunk().is_some()))
        .saturating_add(usize::from(git_object_id.is_some()))
        .saturating_add(usize::from(chunk.source_kind().is_git()))
        .saturating_add(1);
    let heading_bytes = chunk.headings().map(str::len).sum::<usize>();
    let mut body = String::with_capacity(
        heading_bytes
            .saturating_add(chunk.headings().count())
            .saturating_add(chunk.text().len()),
    );
    append_separated(&mut body, chunk.headings(), " ");
    if !body.is_empty() {
        body.push(' ');
    }
    body.push_str(chunk.text());
    append_morphology_aliases(
        &mut body,
        chunk.headings().chain(std::iter::once(chunk.text())),
    );
    let mut tags = String::new();
    append_separated(&mut tags, chunk.tags(), " ");
    let category = chunk.category().map_or("", category_label);
    let trust_tier = trust_label(chunk.trust_tier());
    let identifier_bytes = chunk
        .identifiers()
        .clone()
        .map(str::len)
        .sum::<usize>()
        .saturating_mul(2);
    let node_data_capacity = field_value_count
        .saturating_mul(5)
        .saturating_add(chunk.title().len())
        .saturating_add(chunk.path().len().saturating_mul(2))
        .saturating_add(identifier_bytes)
        .saturating_add(body.len())
        .saturating_add(tags.len())
        .saturating_add(ChunkId::ENCODED_LEN)
        .saturating_add(chunk.document_id().len())
        .saturating_add(category.len())
        .saturating_add(chunk.repository().len())
        .saturating_add(chunk.revision().len())
        .saturating_add(source_kind_label(chunk.source_kind()).len())
        .saturating_add(trust_tier.len())
        .saturating_add(heading_bytes)
        .saturating_add(chunk.tags().clone().map(str::len).sum::<usize>())
        .saturating_add(chunk.registered_id().map_or(0, str::len))
        .saturating_add(chunk.previous_chunk().map_or(0, |_| ChunkId::ENCODED_LEN))
        .saturating_add(chunk.next_chunk().map_or(0, |_| ChunkId::ENCODED_LEN))
        .saturating_add(content_digest.as_str().len())
        .saturating_add(git_object_id.as_ref().map_or(0, |id| id.as_bytes().len()))
        .saturating_add(
            history_content_key_encoded
                .as_ref()
                .map_or(0, |key| key.as_str().len()),
        )
        .saturating_add(chunk.repository().len())
        .saturating_add(chunk.revision().len())
        .saturating_add(2);
    let mut document = TantivyDocument::with_capacities(node_data_capacity, field_value_count);
    document.add_text(fields.title, chunk.title());
    document.add_text(fields.path, chunk.path());
    document.add_text(fields.path_raw, chunk.path());
    for identifier in chunk.identifiers() {
        document.add_text(fields.identifiers, identifier);
    }
    for identifier in chunk.identifiers() {
        document.add_text(fields.identifiers_raw, identifier);
    }
    document.add_text(fields.body, body);
    document.add_text(fields.tags, tags);
    for tag in chunk.tags() {
        document.add_text(fields.tags_raw, tag);
    }
    let chunk_id = chunk.chunk_id().encoded();
    document.add_text(fields.chunk_id, chunk_id.as_str());
    document.add_text(fields.document_id, chunk.document_id());
    for (field, bytes) in fields
        .chunk_digest
        .into_iter()
        .zip(chunk.chunk_id().as_bytes().chunks_exact(8))
    {
        document.add_u64(
            field,
            u64::from_be_bytes(bytes.try_into().expect("chunk digest part is eight bytes")),
        );
    }
    document.add_u64(fields.ordinal, u64::from(chunk.ordinal()));
    document.add_text(fields.category, category);
    document.add_text(fields.repository, chunk.repository());
    document.add_text(fields.revision, chunk.revision());
    document.add_text(fields.source_kind, source_kind_label(chunk.source_kind()));
    document.add_text(fields.trust_tier, trust_tier);
    for heading in chunk.headings() {
        document.add_text(fields.heading_path, heading);
    }
    if let Some(span) = chunk.source_span() {
        document.add_u64(fields.start_line, u64::from(span.start_line));
        document.add_u64(fields.end_line, u64::from(span.end_line));
        if let Some(start_byte) = span.start_byte {
            document.add_u64(fields.start_byte, start_byte);
        }
        if let Some(end_byte) = span.end_byte {
            document.add_u64(fields.end_byte, end_byte);
        }
    }
    if let Some(registered_id) = chunk.registered_id() {
        document.add_text(fields.registered_id, registered_id);
    }
    if let Some(previous_chunk) = chunk.previous_chunk() {
        let previous_chunk = previous_chunk.encoded();
        document.add_text(fields.previous_chunk, previous_chunk.as_str());
    }
    if let Some(next_chunk) = chunk.next_chunk() {
        let next_chunk = next_chunk.encoded();
        document.add_text(fields.next_chunk, next_chunk.as_str());
    }
    document.add_text(fields.content_digest, content_digest.as_str());
    document.add_u64(fields.char_count, u64::from(chunk.char_count()));
    document.add_u64(fields.byte_count, chunk.byte_count());
    if let Some(git_object_id) = git_object_id {
        document.add_bytes(fields.git_object_id, git_object_id.as_bytes());
    }
    if let Some(history_content_key) = history_content_key_encoded {
        document.add_text(fields.history_content_key, history_content_key.as_str());
    }
    document.add_bytes(
        fields.repository_revision,
        &repository_revision_key_values(chunk.source_kind(), chunk.repository(), chunk.revision()),
    );
    document
}

fn append_separated<'a>(
    output: &mut String,
    values: impl Iterator<Item = &'a str>,
    separator: &str,
) {
    for value in values {
        if !output.is_empty() {
            output.push_str(separator);
        }
        output.push_str(value);
    }
}

fn git_source_descriptors(
    sources: &[GitCorpusSource],
) -> BTreeMap<RepositoryId, GitSourceDescriptor> {
    sources
        .iter()
        .map(|source| {
            (
                source.repository_id.clone(),
                GitSourceDescriptor {
                    revision: source.revision.clone(),
                    contract: git_corpus_contract(source),
                    max_file_bytes: source.policy.limits.max_file_bytes(),
                },
            )
        })
        .collect()
}

fn git_corpus_contract(source: &GitCorpusSource) -> ContentDigest {
    #[derive(Serialize)]
    struct Contract<'a> {
        trust_tier: TrustTier,
        license: &'a crate::LicenseStatus,
        include_prefixes: &'a [String],
        exclude_prefixes: &'a [String],
        include_patterns: &'a [String],
        exclude_patterns: &'a [String],
        extensions: &'a BTreeSet<String>,
        filenames: &'a BTreeSet<String>,
        max_file_bytes: u64,
        max_files: usize,
        max_total_bytes: u64,
    }

    let contract = Contract {
        trust_tier: source.trust_tier,
        license: &source.license,
        include_prefixes: &source.policy.include_prefixes,
        exclude_prefixes: &source.policy.exclude_prefixes,
        include_patterns: &source.policy.include_patterns,
        exclude_patterns: &source.policy.exclude_patterns,
        extensions: &source.policy.extensions,
        filenames: &source.policy.filenames,
        max_file_bytes: source.policy.limits.max_file_bytes(),
        max_files: source.policy.limits.max_files(),
        max_total_bytes: source.policy.limits.max_total_bytes(),
    };
    ContentDigest::of(
        &serde_json::to_vec(&contract).expect("Git corpus contract serialization is infallible"),
    )
}

fn repository_revision_key_values(
    source_kind: SourceKind,
    repository: &str,
    revision: &str,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(
        repository
            .len()
            .saturating_add(revision.len())
            .saturating_add(2),
    );
    key.push(u8::from(source_kind.is_git()));
    key.extend_from_slice(repository.as_bytes());
    key.push(0);
    key.extend_from_slice(revision.as_bytes());
    key
}

fn persisted_index_path(path: &Path) -> PathBuf {
    path.with_extension("tantivy")
}

fn graph_input_path(path: &Path) -> PathBuf {
    path.with_extension(GRAPH_INPUT_SUFFIX)
}

fn history_input_path(path: &Path) -> PathBuf {
    path.with_extension(HISTORY_INPUT_SUFFIX)
}

fn embedding_input_path(path: &Path) -> PathBuf {
    path.with_extension(EMBEDDING_INPUT_SUFFIX)
}

fn managed_repositories_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        (ancestor.file_name()?.to_str()? == "artifacts").then(|| {
            ancestor
                .parent()
                .expect("artifacts directory has a parent")
                .join("repositories")
        })
    })
}

fn clone_persisted_index(previous: &Path, path: &Path) -> Result<(), LexicalError> {
    let source = persisted_index_path(previous);
    let destination = persisted_index_path(path);
    if previous == path || source == destination {
        return Err(LexicalError::Artifact(
            "incremental lexical destination must differ from its predecessor".into(),
        ));
    }
    if !source.is_dir() {
        return Err(LexicalError::Artifact(
            "persisted lexical predecessor is missing".into(),
        ));
    }
    fs::create_dir(&destination).map_err(|error| {
        LexicalError::Io(format!(
            "failed to create new lexical destination {}: {error}",
            destination.display()
        ))
    })?;
    for entry in fs::read_dir(source).map_err(|error| LexicalError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| LexicalError::Io(error.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        if !file_type.is_file() {
            return Err(LexicalError::Artifact(
                "persisted lexical index contains a non-file entry".into(),
            ));
        }
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".lock") {
            continue;
        }
        let target = destination.join(&name);
        if name == "meta.json" || name == ".managed.json" {
            fs::copy(entry.path(), target).map_err(|error| LexicalError::Io(error.to_string()))?;
        } else if fs::hard_link(entry.path(), &target).is_err() {
            if target.exists() {
                return Err(LexicalError::Artifact(format!(
                    "incremental lexical target already exists: {}",
                    target.display()
                )));
            }
            fs::copy(entry.path(), target).map_err(|error| LexicalError::Io(error.to_string()))?;
        }
    }
    Ok(())
}

fn write_unique_terms(
    searcher: &tantivy::Searcher,
    field: Field,
    writer: &mut impl Write,
) -> Result<usize, LexicalError> {
    let indexes = searcher
        .segment_readers()
        .iter()
        .map(|reader| reader.inverted_index(field))
        .collect::<Result<Vec<_>, _>>()
        .map_err(LexicalError::Search)?;
    let streams = indexes
        .iter()
        .map(|reader| reader.terms().stream())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| LexicalError::Io(error.to_string()))?;
    let mut terms = TermMerger::new(streams);
    let mut count = 0_usize;
    while terms.advance() {
        let mut live = false;
        for (segment_ord, term_info) in terms.current_segment_ords_and_term_infos() {
            let reader = &searcher.segment_readers()[segment_ord];
            let mut postings = indexes[segment_ord]
                .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)
                .map_err(|error| LexicalError::Io(error.to_string()))?;
            while postings.doc() != TERMINATED {
                if !reader.is_deleted(postings.doc()) {
                    live = true;
                    break;
                }
                postings.advance();
            }
            if live {
                break;
            }
        }
        if !live {
            continue;
        }
        writer
            .write_all(terms.key())
            .and_then(|()| writer.write_all(&[0]))
            .map_err(|error| LexicalError::Io(error.to_string()))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| LexicalError::Artifact("lexical term count is too large".into()))?;
    }
    Ok(count)
}

fn append_morphology_aliases<'a>(output: &mut String, texts: impl IntoIterator<Item = &'a str>) {
    for text in texts {
        for stem in text
            .split(|character: char| !character.is_ascii_alphabetic())
            .filter_map(|word| word.strip_suffix("ence"))
            .filter(|stem| stem.len() >= 4)
        {
            output.push(' ');
            output.push_str(stem);
        }
    }
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

fn has_exact_identifier(identifiers: &[compact_str::CompactString], raw_terms: &[String]) -> bool {
    identifiers.iter().any(|identifier| {
        raw_terms
            .iter()
            .any(|term| identifier.eq_ignore_ascii_case(term))
    })
}

fn raw_query_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !(character.is_alphanumeric() || matches!(character, '_' | '-')))
        .filter(|term| !term.is_empty() && term.chars().any(char::is_alphanumeric))
        .map(str::to_ascii_lowercase)
        .collect()
}

fn symbol_query_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !(character.is_alphanumeric() || matches!(character, '_' | '-')))
        .filter(|term| {
            term.chars().any(char::is_alphanumeric)
                && (term.contains('_')
                    || term.bytes().any(|byte| byte.is_ascii_digit())
                    || term.chars().any(char::is_uppercase))
        })
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
        && (filters.paths.is_empty() || filters.paths.contains(&chunk.path))
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

const fn source_kind_label(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::EmbeddedCatalog => "embedded_catalog",
        SourceKind::Markdown => "markdown",
        SourceKind::CatalogYaml => "catalog_yaml",
        SourceKind::CatalogJson => "catalog_json",
        SourceKind::Fixture => "fixture",
        SourceKind::VendorFile => "vendor_file",
        SourceKind::GitBlob => "git_blob",
        SourceKind::GitCommit => "git_commit",
        SourceKind::ModelFeedback => "model_feedback",
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
    use crate::corpus::full_history::CachedGitHistoryRecord;
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

    fn git_chunk(title: &str, text: &str, identifier: &str) -> Chunk {
        let mut chunk = chunk(title, text, identifier);
        chunk.source_kind = SourceKind::GitBlob;
        chunk
    }

    fn git_chunk_at_path(path: &str, text: &str) -> Chunk {
        let document = NormalizedDocument::new(
            path,
            SourceKind::GitBlob,
            RepositoryId::try_from("repo").expect("repo"),
            Revision::try_from("rev").expect("revision"),
            path,
            "text/plain",
            text,
        )
        .expect("document");
        Chunk::from_document(&document, 0, text.into(), Vec::new(), None).expect("chunk")
    }

    fn catalog_chunks(count: usize) -> Vec<Chunk> {
        crate::embedded_entries()
            .iter()
            .take(count)
            .map(|entry| {
                NormalizedDocument::from_catalog_entry(entry)
                    .and_then(|document| document.catalog_chunk())
                    .expect("embedded catalog chunk")
            })
            .collect()
    }

    #[test]
    fn compact_sidecar_writer_streams_a_valid_json_array() {
        let root = tempfile::tempdir().expect("sidecar root");
        let path = root.path().join("graph-input.json");
        let mut writer = JsonArrayWriter::create(&path).expect("create sidecar");
        writer
            .push(&serde_json::json!({"chunk": 1}))
            .expect("first record");
        writer
            .push(&serde_json::json!({"chunk": 2}))
            .expect("second record");
        writer.finish().expect("finish sidecar");

        assert_eq!(
            fs::read(path).expect("read sidecar"),
            br#"[{"chunk":1},{"chunk":2}]"#
        );
    }

    #[test]
    fn history_sidecars_accept_one_borrowed_projection() {
        fn compile_borrowed_writer(
            sidecars: &mut GitHistorySidecars,
            view: &GitHistoryChunkView<'_>,
            blob: gix::ObjectId,
        ) -> Result<(), LexicalError> {
            sidecars.push_history_borrowed(view, blob)
        }

        let _ = compile_borrowed_writer;
    }

    #[test]
    fn read_history_projection_loads_the_compact_sidecar() {
        let root = tempfile::tempdir().expect("sidecar root");
        let path = root.path().join("lexical.json");
        let record = CachedGitHistoryRecord {
            document_id: "document".into(),
            repository: "repo".into(),
            revision: "0123456789012345678901234567890123456789".into(),
            path: "file.c".into(),
            ordinal: 0,
            has_previous: false,
            has_next: false,
            blob: None,
            source_kind: SourceKind::GitBlob,
            content_key: Some(ContentDigest::of(b"content")),
        };
        fs::write(
            history_input_path(&path),
            serde_json::to_vec(&[record]).expect("serialize history sidecar"),
        )
        .expect("write history sidecar");

        assert!(
            LexicalIndex::read_history_projection(&path)
                .expect("read history projection")
                .is_some()
        );
    }

    #[test]
    fn read_history_projection_treats_invalid_blob_ids_as_a_cold_rebuild() {
        let root = tempfile::tempdir().expect("sidecar root");
        let path = root.path().join("lexical.json");
        let record = CachedGitHistoryRecord {
            document_id: "document".into(),
            repository: "repo".into(),
            revision: "0123456789012345678901234567890123456789".into(),
            path: "file.c".into(),
            ordinal: 0,
            has_previous: false,
            has_next: false,
            blob: Some("not-a-git-object".into()),
            source_kind: SourceKind::GitBlob,
            content_key: Some(ContentDigest::of(b"content")),
        };
        fs::write(
            history_input_path(&path),
            serde_json::to_vec(&[record]).expect("serialize history sidecar"),
        )
        .expect("write history sidecar");

        assert!(
            LexicalIndex::read_history_projection(&path)
                .expect("read history projection")
                .is_none()
        );
    }

    #[test]
    fn compact_sidecar_appender_preserves_existing_records() {
        let root = tempfile::tempdir().expect("sidecar root");
        let previous = root.path().join("previous.json");
        let destination = root.path().join("destination.json");
        fs::write(&previous, br#"[{"chunk":1}]"#).expect("write previous sidecar");
        let mut writer = JsonArrayWriter::append(&previous, &destination).expect("append sidecar");
        writer
            .push(&serde_json::json!({"chunk": 2}))
            .expect("append record");
        writer.finish().expect("finish sidecar");

        assert_eq!(
            fs::read(destination).expect("read sidecar"),
            br#"[{"chunk":1},{"chunk":2}]"#
        );
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
            hits[0]
                .chunk
                .identifiers
                .first()
                .map(compact_str::CompactString::as_str),
            Some("write_nvm")
        );
        assert!(hits[0].exact_identifier);
    }

    #[test]
    fn any_exact_identifier_in_a_symbol_query_is_promoted() {
        let source = git_chunk(
            "motor.rs",
            "pub trait MotorControlBindings { fn update_pid_position_offset(&self, position: PidPosition); }",
            "MotorControlBindings",
        );
        let prose = git_chunk(
            "history",
            "trait MotorControlBindings impl MotorControlBindings PidPosition trait MotorControlBindings impl MotorControlBindings PidPosition",
            "history_note",
        );
        let index = LexicalIndex::build(&[prose, source]).expect("index");
        let hits = index
            .search(
                "update_pid_position_offset|trait MotorControlBindings|impl.*MotorControlBindings|PidPosition",
                &LexicalFilters::default(),
                2,
            )
            .expect("search");

        assert_eq!(hits[0].chunk.title, "motor.rs");
        assert!(hits[0].exact_identifier);
    }

    #[test]
    fn prose_terms_do_not_become_exact_symbol_matches() {
        let index = LexicalIndex::build(&[chunk(
            "history",
            "persistent package lifecycle",
            "persistent",
        )])
        .expect("index");
        let hits = index
            .search(
                "persistent package lifecycle",
                &LexicalFilters::default(),
                1,
            )
            .expect("search");

        assert!(!hits[0].exact_identifier);
    }

    #[test]
    fn typed_filters_are_applied_before_the_candidate_limit() {
        let mut chunks = (0..128)
            .map(|index| {
                let mut chunk = chunk(
                    &format!("Noise {index}"),
                    &format!("needle needle needle noise {index}"),
                    "needle",
                );
                chunk.category = Some(Category::FirmwareApi);
                chunk
            })
            .collect::<Vec<_>>();
        let mut target = chunk("Target", "needle", "target");
        target.category = Some(Category::PackageBuild);
        target.trust_tier = TrustTier::FirstParty;
        target.tags.insert("selected".into());
        chunks.push(target);
        let index = LexicalIndex::build(&chunks).expect("index");
        let filters = LexicalFilters {
            category: Some(Category::PackageBuild),
            repository: Some(RepositoryId::try_from("repo").expect("repository")),
            paths: vec!["docs/example.md".into()],
            revision: Some(Revision::try_from("rev").expect("revision")),
            source_kind: Some(SourceKind::Markdown),
            trust_tier: Some(TrustTier::FirstParty),
            tags: vec!["selected".into()],
        };

        let hits = index.search("needle", &filters, 1).expect("search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.title, "Target");
    }

    #[test]
    fn full_term_coverage_is_collected_before_partial_match_truncation() {
        let pairs = [("alpha", "beta"), ("beta", "gamma"), ("alpha", "gamma")];
        let mut chunks = (0..128)
            .map(|index| {
                let (left, right) = pairs[index % pairs.len()];
                let mut chunk = chunk(
                    &format!("{left} {right}"),
                    &format!("{left} {right} {left} {right} noise_{index}"),
                    left,
                );
                chunk.tags.extend([left.to_owned(), right.to_owned()]);
                chunk
            })
            .collect::<Vec<_>>();
        chunks.push(chunk("Target", "alpha beta gamma", "target"));
        let index = LexicalIndex::build(&chunks).expect("index");

        let hits = index
            .search("alpha beta gamma", &LexicalFilters::default(), 1)
            .expect("search");

        assert_eq!(hits[0].chunk.title, "Target");
    }

    #[test]
    fn staged_tantivy_document_has_exact_ordered_field_capacity() {
        let mut chunk = chunk("NVM", "write persistent bytes", "write_nvm");
        chunk.identifiers.push("read_nvm".into());
        chunk.identifiers.push("erase_nvm".into());
        let (_schema, fields) = schema();

        let document = tantivy_document(fields, &chunk, None);
        let field_ids = document
            .field_values()
            .map(|(field, _)| field.field_id())
            .collect::<Vec<_>>();

        assert_eq!(field_ids.len(), 21 + 2 * chunk.identifiers.len());
        assert!(field_ids.is_sorted());
        assert_eq!(document.get_all(fields.body).count(), 1);
    }

    #[test]
    fn domain_aliases_expand_conceptual_queries() {
        let terms = query_terms("how do I persist package data");

        assert!(terms.iter().any(|term| term == "nvm"));
        assert!(terms.iter().any(|term| term == "read_nvm"));
        assert!(terms.iter().any(|term| term == "write_nvm"));
    }

    #[test]
    fn registered_exact_identifier_wins_over_anonymous_record() {
        let mut registered = chunk("NVM", "registered summary", "read_nvm");
        registered.registered_id = Some("vesc_c_if.read_nvm".into());
        let index = LexicalIndex::build(&[
            chunk("NVM record", "normalized catalog record", "read_nvm"),
            registered,
        ])
        .expect("index");
        let hits = index
            .search("read_nvm", &LexicalFilters::default(), 10)
            .expect("search");

        assert_eq!(
            hits[0].chunk.registered_id,
            Some(String::from("vesc_c_if.read_nvm"))
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
        let chunks = catalog_chunks(1);
        let query = chunks[0].identifiers[0].to_string();
        let index = LexicalIndex::build(&chunks).expect("index");
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
                .search(&query, &LexicalFilters::default(), 1)
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
    fn embedding_inventory_preserves_exact_order_without_string_fast_fields() {
        let chunks = catalog_chunks(2);
        let index = LexicalIndex::build(&chunks).expect("index");
        let schema = index.schema();

        for name in ["document_id", "chunk_id"] {
            let field = schema.get_field(name).expect("ID field");
            assert!(
                !schema.get_field_entry(field).is_fast(),
                "{name} must not duplicate its encoded string in a fast-field dictionary"
            );
        }
        let mut expected = chunks
            .iter()
            .map(|chunk| (chunk.document_id.clone(), chunk.chunk_id.clone()))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            index.embedding_chunk_ids().expect("embedding inventory"),
            expected
                .into_iter()
                .map(|(_, chunk_id)| chunk_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn embedding_inventory_set_matches_ordered_inventory() {
        let index = LexicalIndex::build(&catalog_chunks(2)).expect("index");
        let ordered = index.embedding_chunk_ids().expect("ordered inventory");
        let (set, count) = index.embedding_chunk_id_set().expect("set inventory");

        assert_eq!(count, ordered.len());
        assert_eq!(set.len(), ordered.len());
        assert!(ordered.iter().all(|chunk_id| set.contains(chunk_id)));
    }

    #[test]
    fn embedding_inventory_has_fixed_chunk_digest_columns() {
        let index = LexicalIndex::build(&catalog_chunks(1)).expect("index");
        let schema = index.schema();

        for name in [
            "chunk_digest_0",
            "chunk_digest_1",
            "chunk_digest_2",
            "chunk_digest_3",
        ] {
            let field = schema.get_field(name).expect("chunk digest field");
            assert!(schema.get_field_entry(field).is_fast());
        }
    }

    #[test]
    fn embedding_inventory_rejects_a_missing_chunk_digest() {
        let chunk = catalog_chunks(1).remove(0);
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index
            .writer_with_num_threads(1, IN_MEMORY_WRITER_MEMORY_BYTES)
            .expect("writer");
        let mut document = TantivyDocument::default();
        document.add_text(fields.document_id, chunk.document_id.as_str());
        document.add_text(fields.chunk_id, chunk.chunk_id.encoded().as_str());
        writer.add_document(document).expect("malformed document");
        writer.commit().expect("commit");
        let reader = index.reader().expect("reader");
        let malformed = LexicalIndex {
            index,
            reader,
            fields,
            chunks: BTreeMap::new(),
            repositories_root: None,
            repository_paths: BTreeMap::new(),
            git_sources: BTreeMap::new(),
        };

        assert!(matches!(
            malformed.embedding_chunk_ids(),
            Err(LexicalError::Artifact(message))
                if message.contains("chunk digest fast field")
        ));
    }

    #[test]
    fn embedding_inventory_sorts_chunks_within_a_document_across_segments() {
        let document = NormalizedDocument::new(
            "Shared document",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/shared.md",
            "text/markdown",
            "shared content",
        )
        .expect("document");
        let first = Chunk::from_document(&document, 0, "first".into(), Vec::new(), None)
            .expect("first chunk");
        let second = Chunk::from_document(&document, 1, "second".into(), Vec::new(), None)
            .expect("second chunk");
        let mut expected = vec![first.chunk_id.clone(), second.chunk_id.clone()];
        expected.sort_unstable();

        let temp = tempfile::tempdir().expect("tempdir");
        let previous = temp.path().join("previous.json");
        let next = temp.path().join("next.json");
        LexicalIndex::write_search_artifact_with_digest([&first], &previous)
            .expect("write previous");
        LexicalIndex::write_incremental_search_artifact_with_digest(&previous, [&second], &next)
            .expect("write incremental");

        let index = LexicalIndex::open_search_artifact(&next).expect("open incremental");
        assert_eq!(
            index.embedding_chunk_ids().expect("embedding inventory"),
            expected
        );
    }

    #[test]
    fn incremental_artifact_reuses_old_index_and_adds_only_delta_chunks() {
        let previous_chunk = git_chunk("Old", "old history", "old_identifier");
        let delta_chunk = git_chunk("New", "new history", "new_identifier");
        let temp = tempfile::tempdir().expect("tempdir");
        let previous = temp.path().join("previous.json");
        let next = temp.path().join("next.json");

        LexicalIndex::write_search_artifact_with_digest([&previous_chunk], &previous)
            .expect("write previous");
        let lookup = LexicalIndex::open_history_content_lookup(&previous).expect("history lookup");
        assert!(
            lookup
                .contains(
                    &previous_chunk.repository,
                    &previous_chunk.path,
                    &crate::corpus::history_content_key_for_chunk(&previous_chunk)
                        .expect("history key")
                )
                .expect("contains")
        );
        assert!(
            !lookup
                .contains(
                    &delta_chunk.repository,
                    &delta_chunk.path,
                    &crate::corpus::history_content_key_for_chunk(&delta_chunk)
                        .expect("history key")
                )
                .expect("contains")
        );

        LexicalIndex::write_incremental_search_artifact_with_digest(
            &previous,
            [&delta_chunk],
            &next,
        )
        .expect("write incremental");
        let lookup =
            LexicalIndex::open_history_content_lookup(&next).expect("incremental history lookup");
        for chunk in [&previous_chunk, &delta_chunk] {
            assert!(
                lookup
                    .contains(
                        &chunk.repository,
                        &chunk.path,
                        &crate::corpus::history_content_key_for_chunk(chunk).expect("history key"),
                    )
                    .expect("contains")
            );
        }
        let (documents, chunks, digest) =
            LexicalIndex::corpus_inventory(&next).expect("stream inventory");
        let expected = crate::CorpusManifest::new(
            crate::CorpusVersion::try_from("test-v1").expect("corpus version"),
            vec![
                previous_chunk.document_id.clone(),
                delta_chunk.document_id.clone(),
            ],
            vec![previous_chunk.chunk_id, delta_chunk.chunk_id],
        );
        assert_eq!(documents, expected.document_count());
        assert_eq!(chunks, expected.chunk_count());
        assert_eq!(digest, expected.content_digest);
    }

    #[test]
    fn history_content_lookup_uses_exact_stored_keys() {
        let underscored = git_chunk_at_path("src/full_history.rs", "underscored");
        let hyphenated = git_chunk_at_path("docs/foo-bar.md", "hyphenated");
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("lexical.json");
        LexicalIndex::write_search_artifact_with_digest([&underscored, &hyphenated], &artifact)
            .expect("write artifact");
        let lookup = LexicalIndex::open_history_content_lookup(&artifact).expect("history lookup");

        for chunk in [&underscored, &hyphenated] {
            let key =
                crate::corpus::history_content_key_for_chunk(chunk).expect("history content key");
            assert!(
                lookup
                    .contains(&chunk.repository, &chunk.path, &key)
                    .expect("contains")
            );
        }

        let underscored_key = crate::corpus::history_content_key_for_chunk(&underscored)
            .expect("underscored history content key");
        assert_eq!(
            lookup
                .matching_chunk_ids(&BTreeSet::from([underscored_key.clone()]))
                .expect("matching chunk IDs"),
            BTreeMap::from([(underscored_key, underscored.chunk_id)])
        );
    }

    #[test]
    fn history_contains_does_not_require_a_stored_document_id() {
        let chunk = git_chunk("History", "history", "history_identifier");
        let key =
            crate::corpus::history_content_key_for_chunk(&chunk).expect("history content key");
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index
            .writer_with_num_threads(1, IN_MEMORY_WRITER_MEMORY_BYTES)
            .expect("writer");
        let mut document = TantivyDocument::default();
        document.add_text(fields.history_content_key, key.encoded().as_str());
        writer.add_document(document).expect("malformed document");
        writer.commit().expect("commit");
        let reader = index.reader().expect("reader");
        let lookup = HistoryContentLookup { reader, fields };

        assert!(
            lookup
                .contains_retained(&chunk.repository, &chunk.path, &key, None, &BTreeSet::new(),)
                .expect("contains")
        );
    }

    #[test]
    fn incremental_artifact_rejects_existing_or_same_destination() {
        let previous_chunk = git_chunk("Old", "old history", "old_identifier");
        let delta_chunk = git_chunk("New", "new history", "new_identifier");
        let temp = tempfile::tempdir().expect("tempdir");
        let previous = temp.path().join("previous.json");
        let occupied = temp.path().join("occupied.json");

        LexicalIndex::write_search_artifact_with_digest([&previous_chunk], &previous)
            .expect("write previous");
        LexicalIndex::write_search_artifact_with_digest([&delta_chunk], &occupied)
            .expect("write occupied");

        assert!(
            LexicalIndex::write_incremental_search_artifact_with_digest(
                &previous,
                [&delta_chunk],
                &previous,
            )
            .is_err()
        );
        assert!(
            LexicalIndex::write_incremental_search_artifact_with_digest(
                &previous,
                [&delta_chunk],
                &occupied,
            )
            .is_err()
        );

        let lookup = LexicalIndex::open_history_content_lookup(&previous)
            .expect("predecessor remains valid");
        let old_key =
            crate::corpus::history_content_key_for_chunk(&previous_chunk).expect("old history key");
        let new_key =
            crate::corpus::history_content_key_for_chunk(&delta_chunk).expect("new history key");
        assert!(
            lookup
                .contains(&previous_chunk.repository, &previous_chunk.path, &old_key)
                .expect("old key")
        );
        assert!(
            !lookup
                .contains(&delta_chunk.repository, &delta_chunk.path, &new_key)
                .expect("new key")
        );
    }

    #[test]
    fn repeated_incremental_artifacts_bound_segment_count_without_mutating_predecessors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("generation-0.json");
        let original = git_chunk("Original", "original history", "original_identifier");
        LexicalIndex::write_search_artifact_with_digest([&original], &first)
            .expect("write original");
        let original_key =
            crate::corpus::history_content_key_for_chunk(&original).expect("original history key");
        let mut previous = first.clone();

        for generation in 1..=(MAX_INCREMENTAL_SEGMENTS + 2) {
            let next = temp.path().join(format!("generation-{generation}.json"));
            let delta = git_chunk(
                &format!("Generation {generation}"),
                &format!("history {generation}"),
                &format!("identifier_{generation}"),
            );
            LexicalIndex::write_incremental_search_artifact_with_digest(&previous, [&delta], &next)
                .expect("write incremental generation");
            previous = next;
        }

        let latest =
            Index::open_in_dir(persisted_index_path(&previous)).expect("open latest sidecar");
        assert!(
            latest
                .searchable_segment_ids()
                .expect("latest segments")
                .len()
                <= MAX_INCREMENTAL_SEGMENTS
        );
        let lookup =
            LexicalIndex::open_history_content_lookup(&first).expect("original remains readable");
        assert!(
            lookup
                .contains(&original.repository, &original.path, &original_key)
                .expect("original lookup")
        );
    }

    #[test]
    fn written_artifact_keeps_only_compact_locators_in_the_persisted_index() {
        let chunks = catalog_chunks(1);
        let query = chunks[0].identifiers[0].to_string();
        let index = LexicalIndex::build(&chunks).expect("index");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lexical.json");
        index.write_artifact(&path).expect("write artifact");

        let descriptor: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read artifact"))
                .expect("parse artifact");
        assert_eq!(
            descriptor,
            serde_json::json!({ "schema": LEXICAL_DESCRIPTOR_SCHEMA })
        );
        assert_eq!(
            LexicalIndex::read_artifact_chunks(&path).expect("stored chunks"),
            chunks
        );

        let reopened = LexicalIndex::open_artifact(&path).expect("open artifact");
        assert!(reopened.chunks().is_empty());
        assert!(reopened.schema().get_field("chunk_json").is_err());
        assert_eq!(
            reopened
                .search(&query, &LexicalFilters::default(), 1)
                .expect("search")
                .len(),
            1
        );
    }

    #[test]
    fn search_artifact_requires_the_descriptor() {
        let chunks = catalog_chunks(1);
        let index = LexicalIndex::build(&chunks).expect("index");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lexical.json");
        index.write_artifact(&path).expect("write artifact");
        std::fs::remove_file(&path).expect("remove descriptor");

        assert!(matches!(
            LexicalIndex::open_search_artifact(&path),
            Err(LexicalError::Io(_))
        ));
    }

    #[test]
    fn cloned_search_artifact_keeps_compact_sidecars() {
        let root = tempfile::tempdir().expect("artifact root");
        let previous = root.path().join("previous.json");
        let destination = root.path().join("destination.json");
        let index_path = persisted_index_path(&previous);
        std::fs::create_dir(&index_path).expect("index directory");
        std::fs::write(index_path.join("meta.json"), b"index").expect("index metadata");
        std::fs::write(&previous, b"descriptor").expect("descriptor");
        std::fs::write(graph_input_path(&previous), b"graph").expect("graph sidecar");
        std::fs::write(history_input_path(&previous), b"history").expect("history sidecar");

        LexicalIndex::clone_search_artifact(&previous, &destination).expect("clone artifact");

        assert_eq!(
            std::fs::read(&destination).expect("destination descriptor"),
            b"descriptor"
        );
        assert_eq!(
            std::fs::read(graph_input_path(&destination)).expect("destination graph sidecar"),
            b"graph"
        );
        assert_eq!(
            std::fs::read(history_input_path(&destination)).expect("destination history sidecar"),
            b"history"
        );
    }

    #[test]
    fn embedding_locator_sidecar_round_trips_only_embedding_metadata() {
        let chunk = catalog_chunks(1).pop().expect("catalog chunk");
        let record = EmbeddingLocatorRecord::from_chunk(&chunk, None);
        let root = tempfile::tempdir().expect("artifact root");
        let path = root.path().join("lexical.json");

        let mut writer =
            JsonArrayWriter::create(&embedding_input_path(&path)).expect("write embedding sidecar");
        writer.push(&record).expect("embedding record");
        writer.finish().expect("finish embedding sidecar");
        let records = LexicalIndex::read_embedding_inputs(&path)
            .expect("read embedding sidecar")
            .expect("embedding sidecar");

        assert_eq!(records, vec![record]);
        let locator = records[0].clone().into_locator().expect("locator");
        assert_eq!(locator.chunk_id, chunk.chunk_id);
        assert_eq!(locator.title, chunk.title);
        assert_eq!(locator.heading_path, chunk.heading_path);
        assert_eq!(locator.identifiers, chunk.identifiers);
        assert_eq!(locator.tags, chunk.tags);
        assert_eq!(locator.source_span, chunk.source_span);
    }

    #[test]
    fn persisted_index_reads_only_requested_chunks() {
        let chunks = catalog_chunks(3);
        let index = LexicalIndex::build(&chunks).expect("index");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lexical.json");
        index.write_artifact(&path).expect("write artifact");
        let reopened = LexicalIndex::open_search_artifact(&path).expect("open search artifact");
        let requested = BTreeSet::from([
            chunks[0].chunk_id.clone(),
            ChunkId::from_sha256([u8::MAX; 32]),
        ]);

        assert_eq!(
            reopened
                .chunks_by_id(&requested)
                .expect("requested chunks")
                .into_values()
                .collect::<Vec<_>>(),
            vec![chunks[0].clone()]
        );
    }

    #[test]
    fn persisted_index_reads_only_requested_document() {
        let chunks = catalog_chunks(2);
        let index = LexicalIndex::build(&chunks).expect("index");
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lexical.json");
        index.write_artifact(&path).expect("write artifact");
        let reopened = LexicalIndex::open_search_artifact(&path).expect("open search artifact");

        assert_eq!(
            reopened
                .chunks_by_document_id(&chunks[0].document_id)
                .expect("document chunks"),
            vec![chunks[0].clone()]
        );
    }

    #[test]
    fn persisted_chunks_reject_invalid_descriptor_contents() {
        let chunks = catalog_chunks(1);
        let root = tempfile::tempdir().expect("artifact root");
        let path = root.path().join("lexical.json");
        LexicalIndex::build(&chunks)
            .expect("build index")
            .write_artifact(&path)
            .expect("write artifact");
        std::fs::write(&path, b"obsolete descriptor contents").expect("replace descriptor");
        assert!(matches!(
            LexicalIndex::read_artifact_chunks(&path),
            Err(LexicalError::Artifact(_))
        ));
    }

    #[test]
    fn sidecar_checksum_binds_compact_history_graph_and_embedding_inputs() {
        let root = tempfile::tempdir().expect("artifact root");
        let path = root.path().join("lexical.json");
        let index_path = persisted_index_path(&path);
        std::fs::create_dir(&index_path).expect("index directory");
        std::fs::write(index_path.join("meta.json"), b"index").expect("index metadata");

        let baseline = LexicalIndex::sidecar_checksum(&path).expect("baseline checksum");
        std::fs::write(graph_input_path(&path), b"graph").expect("graph input");
        let graph = LexicalIndex::sidecar_checksum(&path).expect("graph checksum");
        assert_ne!(graph, baseline);

        std::fs::write(history_input_path(&path), b"history").expect("history input");
        let history = LexicalIndex::sidecar_checksum(&path).expect("history checksum");
        assert_ne!(history, graph);

        std::fs::write(embedding_input_path(&path), b"embedding").expect("embedding input");
        let embedding = LexicalIndex::sidecar_checksum(&path).expect("embedding checksum");
        assert_ne!(embedding, history);
    }
}
