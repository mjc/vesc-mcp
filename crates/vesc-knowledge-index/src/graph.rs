//! Deterministic, bounded graph artifacts for immutable knowledge snapshots.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::{
    Chunk, ChunkId, ContentDigest, RepositoryId, Revision, SchemaVersion, SourceSpan,
};

pub const GRAPH_ARTIFACT_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 0 };
const MAGIC: &[u8; 8] = b"VESCGRPH";
const MAX_NODES: usize = 10_000_000;
const MAX_EDGES: usize = 50_000_000;
const MAX_STRING_BYTES: usize = 1 << 20;
type NodeLocator = (usize, (u32, u32, u32, u32), Option<ChunkId>);

struct GraphChunkBuilder {
    corpus_digest: ContentDigest,
    nodes: Vec<GraphNode>,
    node_by_chunk: HashMap<ChunkId, NodeLocator>,
}

impl GraphChunkBuilder {
    #[allow(clippy::missing_const_for_fn)]
    fn new(corpus_digest: ContentDigest) -> Self {
        Self {
            corpus_digest,
            nodes: Vec::new(),
            node_by_chunk: HashMap::new(),
        }
    }

    fn push(&mut self, chunk: GraphChunk) -> Result<(), GraphArtifactError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(GraphArtifactError::Contract(
                "graph node count exceeds the configured limit".into(),
            ));
        }
        let GraphChunk {
            chunk_id,
            repository,
            revision,
            path,
            title,
            ordinal,
            source_span,
            next_chunk,
        } = chunk;
        let span = graph_span(source_span)?;
        let symbol = format!("{title}#{ordinal}");
        let node = GraphNode::new(
            repository.as_str(),
            revision.as_str(),
            &path,
            span,
            &symbol,
            "chunk",
        );
        if self
            .node_by_chunk
            .insert(chunk_id, (self.nodes.len(), span, next_chunk))
            .is_some()
        {
            return Err(GraphArtifactError::Contract(
                "graph snapshot contains duplicate chunk identities".into(),
            ));
        }
        self.nodes.push(node);
        Ok(())
    }

    fn finish(self) -> Result<GraphArtifact, GraphArtifactError> {
        let mut edges = Vec::new();
        for (source_index, source_span, next) in self.node_by_chunk.values() {
            let Some(next) = next else {
                continue;
            };
            let Some(target_index) = self.node_by_chunk.get(next).map(|locator| locator.0) else {
                return Err(GraphArtifactError::Contract(
                    "chunk adjacency points outside the graph snapshot".into(),
                ));
            };
            let source = &self.nodes[*source_index].id;
            let target = &self.nodes[target_index].id;
            edges.push(GraphEdge {
                source: source.clone(),
                target: target.clone(),
                evidence: GraphEvidence {
                    node: source.clone(),
                    start_line: source_span.0,
                    end_line: source_span.1,
                    start_byte: source_span.2,
                    end_byte: source_span.3,
                },
                relation: "adjacent-next".into(),
                extractor: "chunk-adjacency-v1".into(),
                confidence: u8::MAX,
                verified: true,
            });
        }
        GraphArtifact::new(self.corpus_digest, self.nodes, edges)
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphArtifactError {
    #[error("graph artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("graph artifact contract failed: {0}")]
    Contract(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: ContentDigest,
    pub repository: String,
    pub revision: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub symbol: String,
    pub kind: String,
}

impl GraphNode {
    /// Derive a stable node identity from immutable source provenance.
    #[must_use]
    pub fn new(
        repository: &str,
        revision: &str,
        path: &str,
        span: (u32, u32, u32, u32),
        symbol: &str,
        kind: &str,
    ) -> Self {
        let mut identity = Vec::from(b"vesc-graph-node-v1\0".as_slice());
        for value in [repository, revision, path, symbol, kind] {
            let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
            identity.extend_from_slice(&length.to_le_bytes());
            identity.extend_from_slice(value.as_bytes());
        }
        for value in [span.0, span.1, span.2, span.3] {
            identity.extend_from_slice(&value.to_le_bytes());
        }
        Self {
            id: ContentDigest::of(&identity),
            repository: repository.to_owned(),
            revision: revision.to_owned(),
            path: path.to_owned(),
            start_line: span.0,
            end_line: span.1,
            start_byte: span.2,
            end_byte: span.3,
            symbol: symbol.to_owned(),
            kind: kind.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub source: ContentDigest,
    pub target: ContentDigest,
    pub evidence: GraphEvidence,
    pub relation: String,
    pub extractor: String,
    pub confidence: u8,
    pub verified: bool,
}

/// Exact source location supporting an edge claim. The path and revision are
/// inherited from the referenced graph node, avoiding duplicated strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEvidence {
    pub node: ContentDigest,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphArtifact {
    pub corpus_digest: ContentDigest,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Forward CSR offsets into `edges`.
    pub forward_offsets: Vec<u32>,
    /// Reverse adjacency stores indexes into `edges`, avoiding duplicate edge records.
    pub reverse_offsets: Vec<u32>,
    pub reverse_edge_indices: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphChunk {
    pub(crate) chunk_id: ChunkId,
    pub(crate) repository: RepositoryId,
    pub(crate) revision: Revision,
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) ordinal: u32,
    pub(crate) source_span: Option<SourceSpan>,
    pub(crate) next_chunk: Option<ChunkId>,
}

impl GraphChunk {
    pub(crate) fn from_chunk(chunk: &Chunk) -> Self {
        Self {
            chunk_id: chunk.chunk_id.clone(),
            repository: chunk.repository.clone(),
            revision: chunk.revision.clone(),
            path: chunk.path.clone(),
            title: chunk.title.clone(),
            ordinal: chunk.ordinal,
            source_span: chunk.source_span,
            next_chunk: chunk.next_chunk.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphArtifactSummary {
    pub bytes: u64,
    pub node_count: u64,
    pub edge_count: u64,
}

impl GraphArtifact {
    /// Build the always-safe structural graph already present in chunk metadata.
    ///
    /// This intentionally emits only verified same-snapshot adjacency edges;
    /// domain and language extractors can add stronger relationships later.
    ///
    /// # Errors
    ///
    /// Returns an error when a source span exceeds the graph wire bounds or
    /// chunk adjacency points outside the supplied snapshot.
    pub fn from_chunks(
        corpus_digest: ContentDigest,
        chunks: &[Chunk],
    ) -> Result<Self, GraphArtifactError> {
        Self::from_graph_chunks(corpus_digest, chunks.iter().map(GraphChunk::from_chunk))
    }

    pub(crate) fn from_graph_chunk_reader<R, F>(
        corpus_digest: ContentDigest,
        reader: R,
        project: F,
    ) -> Result<Self, GraphArtifactError>
    where
        R: Read,
        F: FnMut(GraphChunk) -> Option<GraphChunk>,
    {
        struct GraphChunkVisitor<F> {
            corpus_digest: ContentDigest,
            project: F,
        }

        impl<'de, F> Visitor<'de> for GraphChunkVisitor<F>
        where
            F: FnMut(GraphChunk) -> Option<GraphChunk>,
        {
            type Value = GraphArtifact;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON array of projected graph chunks")
            }

            fn visit_seq<A>(mut self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut builder = GraphChunkBuilder::new(self.corpus_digest);
                while let Some(chunk) = sequence.next_element::<GraphChunk>()? {
                    if let Some(chunk) = (self.project)(chunk) {
                        builder.push(chunk).map_err(de::Error::custom)?;
                    }
                }
                builder.finish().map_err(de::Error::custom)
            }
        }

        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        deserializer
            .deserialize_seq(GraphChunkVisitor {
                corpus_digest,
                project,
            })
            .map_err(|error| GraphArtifactError::Contract(error.to_string()))
    }

    pub(crate) fn from_graph_chunks(
        corpus_digest: ContentDigest,
        chunks: impl IntoIterator<Item = GraphChunk>,
    ) -> Result<Self, GraphArtifactError> {
        let mut builder = GraphChunkBuilder::new(corpus_digest);
        for chunk in chunks {
            builder.push(chunk)?;
        }
        builder.finish()
    }

    /// Construct a canonical artifact. Input ordering does not affect bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when provenance, endpoint, or size invariants fail.
    pub fn new(
        corpus_digest: ContentDigest,
        mut nodes: Vec<GraphNode>,
        mut edges: Vec<GraphEdge>,
    ) -> Result<Self, GraphArtifactError> {
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        edges.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| left.evidence.node.cmp(&right.evidence.node))
                .then_with(|| left.evidence.start_line.cmp(&right.evidence.start_line))
                .then_with(|| left.evidence.end_line.cmp(&right.evidence.end_line))
                .then_with(|| left.evidence.start_byte.cmp(&right.evidence.start_byte))
                .then_with(|| left.evidence.end_byte.cmp(&right.evidence.end_byte))
                .then_with(|| left.relation.cmp(&right.relation))
                .then_with(|| left.extractor.cmp(&right.extractor))
                .then_with(|| left.confidence.cmp(&right.confidence))
                .then_with(|| left.verified.cmp(&right.verified))
        });
        let mut artifact = Self {
            corpus_digest,
            nodes,
            edges,
            forward_offsets: Vec::new(),
            reverse_offsets: Vec::new(),
            reverse_edge_indices: Vec::new(),
        };
        artifact.forward_offsets = artifact.build_forward_index()?;
        (artifact.reverse_offsets, artifact.reverse_edge_indices) =
            artifact.build_reverse_index()?;
        artifact.validate()?;
        Ok(artifact)
    }

    /// Validate ordering, provenance, endpoints, and configured size bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when any graph invariant is violated.
    pub fn validate(&self) -> Result<(), GraphArtifactError> {
        if self.nodes.len() > MAX_NODES || self.edges.len() > MAX_EDGES {
            return Err(GraphArtifactError::Contract(
                "graph size exceeds configured limits".into(),
            ));
        }
        if self.nodes.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            return Err(GraphArtifactError::Contract(
                "graph nodes are not strictly sorted".into(),
            ));
        }
        let node_indexes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                validate_node(node)?;
                let expected_id = GraphNode::new(
                    &node.repository,
                    &node.revision,
                    &node.path,
                    (
                        node.start_line,
                        node.end_line,
                        node.start_byte,
                        node.end_byte,
                    ),
                    &node.symbol,
                    &node.kind,
                )
                .id;
                if node.id != expected_id {
                    return Err(GraphArtifactError::Contract(
                        "graph node identity does not match provenance".into(),
                    ));
                }
                Ok((node.id.clone(), index))
            })
            .collect::<Result<BTreeMap<_, _>, GraphArtifactError>>()?;
        self.validate_edges(&node_indexes)?;
        let forward_offsets = self.build_forward_index()?;
        if self.forward_offsets != forward_offsets {
            return Err(GraphArtifactError::Contract(
                "graph forward adjacency is not canonical".into(),
            ));
        }
        let (reverse_offsets, reverse_edge_indices) = self.build_reverse_index()?;
        if self.reverse_offsets != reverse_offsets
            || self.reverse_edge_indices != reverse_edge_indices
        {
            return Err(GraphArtifactError::Contract(
                "graph reverse adjacency is not canonical".into(),
            ));
        }
        Ok(())
    }

    /// Return the outgoing edges for a node without reconstructing adjacency.
    #[must_use]
    pub fn outgoing(&self, node: &ContentDigest) -> &[GraphEdge] {
        let Some(index) = self
            .nodes
            .binary_search_by_key(node, |entry| entry.id.clone())
            .ok()
        else {
            return &[];
        };
        let start = self.forward_offsets[index] as usize;
        let end = self.forward_offsets[index + 1] as usize;
        &self.edges[start..end]
    }

    /// Return incoming edges using the serialized reverse index.
    #[must_use]
    pub fn incoming(&self, node: &ContentDigest) -> Vec<&GraphEdge> {
        let Some(index) = self
            .nodes
            .binary_search_by_key(node, |entry| entry.id.clone())
            .ok()
        else {
            return Vec::new();
        };
        let start = self.reverse_offsets[index] as usize;
        let end = self.reverse_offsets[index + 1] as usize;
        self.reverse_edge_indices[start..end]
            .iter()
            .map(|&edge_index| &self.edges[edge_index as usize])
            .collect()
    }

    fn validate_edges(
        &self,
        node_indexes: &BTreeMap<ContentDigest, usize>,
    ) -> Result<(), GraphArtifactError> {
        let mut previous = None;
        for edge in &self.edges {
            let source_index = node_indexes.get(&edge.source).ok_or_else(|| {
                GraphArtifactError::Contract("graph edge source is not a node".into())
            })?;
            let target_index = node_indexes.get(&edge.target).ok_or_else(|| {
                GraphArtifactError::Contract("graph edge target is not a node".into())
            })?;
            let evidence_index = node_indexes.get(&edge.evidence.node).ok_or_else(|| {
                GraphArtifactError::Contract("graph edge evidence is not a node".into())
            })?;
            let source_node = &self.nodes[*source_index];
            let target_node = &self.nodes[*target_index];
            let evidence_node = &self.nodes[*evidence_index];
            if source_node.repository != target_node.repository
                || source_node.revision != target_node.revision
                || source_node.repository != evidence_node.repository
                || source_node.revision != evidence_node.revision
            {
                return Err(GraphArtifactError::Contract(
                    "graph edge crosses repository or revision scope".into(),
                ));
            }
            if edge.evidence.start_line > edge.evidence.end_line
                || edge.evidence.start_byte > edge.evidence.end_byte
            {
                return Err(GraphArtifactError::Contract(
                    "graph edge evidence span is invalid".into(),
                ));
            }
            if edge.relation.trim().is_empty() || edge.extractor.trim().is_empty() {
                return Err(GraphArtifactError::Contract(
                    "graph edge metadata is empty".into(),
                ));
            }
            let key = (
                *source_index,
                *target_index,
                &edge.evidence.node,
                edge.evidence.start_line,
                edge.evidence.end_line,
                edge.evidence.start_byte,
                edge.evidence.end_byte,
                &edge.relation,
                &edge.extractor,
                edge.confidence,
                edge.verified,
            );
            if previous.as_ref().is_some_and(|prior| prior >= &key) {
                return Err(GraphArtifactError::Contract(
                    "graph edges are not canonical".into(),
                ));
            }
            previous = Some(key);
        }
        Ok(())
    }

    /// Return the checksum of the complete encoded artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when the graph cannot be encoded.
    pub fn encoded_digest(&self) -> Result<ContentDigest, GraphArtifactError> {
        Ok(ContentDigest::of(&self.encode()?))
    }

    /// Encode the graph as a checksummed, bounded CSR artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when graph invariants fail.
    pub fn encode(&self) -> Result<Vec<u8>, GraphArtifactError> {
        self.validate()?;
        let payload = self.encode_payload()?;
        let digest = ContentDigest::of(&payload);
        let mut bytes = payload;
        bytes.extend_from_slice(digest.as_bytes());
        Ok(bytes)
    }

    /// Decode and validate a complete graph artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are truncated, corrupt, incompatible,
    /// or violate graph invariants.
    pub fn decode(bytes: &[u8]) -> Result<Self, GraphArtifactError> {
        if bytes.len() < 8 + 2 + 2 + 4 + 4 + 32 + 4 + 4 + 32 {
            return Err(GraphArtifactError::Contract(
                "graph artifact is truncated".into(),
            ));
        }
        let payload_len = bytes.len() - 32;
        let mut expected_bytes = [0_u8; 32];
        expected_bytes.copy_from_slice(&bytes[payload_len..]);
        let expected = ContentDigest::from_sha256(expected_bytes);
        if ContentDigest::of(&bytes[..payload_len]) != expected {
            return Err(GraphArtifactError::Contract(
                "graph artifact checksum mismatch".into(),
            ));
        }
        let mut reader = Reader::new(&bytes[..payload_len]);
        if reader.take(8)? != MAGIC {
            return Err(GraphArtifactError::Contract(
                "graph artifact magic mismatch".into(),
            ));
        }
        let schema = SchemaVersion {
            major: reader.u16()?,
            minor: reader.u16()?,
        };
        schema
            .ensure_major(GRAPH_ARTIFACT_SCHEMA_V1, "graph artifact")
            .map_err(|error| GraphArtifactError::Contract(error.to_string()))?;
        let node_count = bounded_count(reader.u32()?, MAX_NODES, "nodes")?;
        let edge_count = bounded_count(reader.u32()?, MAX_EDGES, "edges")?;
        let corpus_digest = ContentDigest::from_sha256(reader.array()?);
        let mut nodes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            nodes.push(GraphNode {
                id: ContentDigest::from_sha256(reader.array()?),
                repository: reader.string()?,
                revision: reader.string()?,
                path: reader.string()?,
                start_line: reader.u32()?,
                end_line: reader.u32()?,
                start_byte: reader.u32()?,
                end_byte: reader.u32()?,
                symbol: reader.string()?,
                kind: reader.string()?,
            });
        }
        let mut forward_offsets = Vec::with_capacity(node_count + 1);
        for _ in 0..=node_count {
            forward_offsets.push(
                u32::try_from(bounded_count(reader.u32()?, edge_count, "edge offset")?)
                    .map_err(|_| GraphArtifactError::Contract("edge offset overflows".into()))?,
            );
        }
        let edge_count_u32 = u32::try_from(edge_count)
            .map_err(|_| GraphArtifactError::Contract("graph edge count overflows".into()))?;
        if forward_offsets.first() != Some(&0)
            || forward_offsets.last() != Some(&edge_count_u32)
            || forward_offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(GraphArtifactError::Contract(
                "graph adjacency offsets are invalid".into(),
            ));
        }
        let mut edges = Vec::with_capacity(edge_count);
        for node_index in 0..node_count {
            for _ in forward_offsets[node_index]..forward_offsets[node_index + 1] {
                edges.push(read_edge(&mut reader, nodes[node_index].id.clone())?);
            }
        }
        let (reverse_offsets, reverse_edge_indices) =
            decode_reverse_index(&mut reader, node_count, edge_count)?;
        if !reader.is_empty() {
            return Err(GraphArtifactError::Contract(
                "graph artifact has trailing payload".into(),
            ));
        }
        let artifact = Self {
            corpus_digest,
            nodes,
            edges,
            forward_offsets,
            reverse_offsets,
            reverse_edge_indices,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    /// Write an encoded graph and return its checksum.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding or writing fails.
    pub fn write(&self, path: &Path) -> Result<ContentDigest, GraphArtifactError> {
        self.validate()?;
        let file = fs::File::create(path)?;
        let mut writer = GraphDigestWriter::new(BufWriter::new(file));
        self.write_payload_to(&mut writer)?;
        let payload_digest: [u8; 32] = writer.payload_digest.clone().finalize().into();
        writer.write_unhashed(&payload_digest)?;
        writer.finish()
    }

    /// Open and validate a graph artifact from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when reading or validation fails.
    pub fn open(path: &Path) -> Result<Self, GraphArtifactError> {
        Self::decode(&fs::read(path)?)
    }

    /// Validate a graph file against its manifest checksum and corpus digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is missing, corrupt, or bound to another
    /// corpus.
    pub fn validate_path(
        path: &Path,
        expected_checksum: &ContentDigest,
        expected_corpus: &ContentDigest,
    ) -> Result<GraphArtifactSummary, GraphArtifactError> {
        let bytes = fs::read(path)?;
        if ContentDigest::of(&bytes) != *expected_checksum {
            return Err(GraphArtifactError::Contract(
                "graph artifact checksum mismatch".into(),
            ));
        }
        let artifact = Self::decode(&bytes)?;
        if artifact.corpus_digest != *expected_corpus {
            return Err(GraphArtifactError::Contract(
                "graph artifact corpus mismatch".into(),
            ));
        }
        Ok(GraphArtifactSummary {
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            node_count: u64::try_from(artifact.nodes.len()).unwrap_or(u64::MAX),
            edge_count: u64::try_from(artifact.edges.len()).unwrap_or(u64::MAX),
        })
    }

    fn encode_payload(&self) -> Result<Vec<u8>, GraphArtifactError> {
        let mut payload = Vec::new();
        self.write_payload_to(&mut payload)?;
        Ok(payload)
    }

    fn write_payload_to<W: Write>(&self, writer: &mut W) -> Result<(), GraphArtifactError> {
        writer.write_all(MAGIC)?;
        writer.write_all(&GRAPH_ARTIFACT_SCHEMA_V1.major.to_le_bytes())?;
        writer.write_all(&GRAPH_ARTIFACT_SCHEMA_V1.minor.to_le_bytes())?;
        writer.write_all(
            &u32::try_from(self.nodes.len())
                .map_err(|_| {
                    GraphArtifactError::Contract("graph node count overflows wire".into())
                })?
                .to_le_bytes(),
        )?;
        writer.write_all(
            &u32::try_from(self.edges.len())
                .map_err(|_| {
                    GraphArtifactError::Contract("graph edge count overflows wire".into())
                })?
                .to_le_bytes(),
        )?;
        writer.write_all(self.corpus_digest.as_bytes())?;
        for node in &self.nodes {
            writer.write_all(node.id.as_bytes())?;
            for value in [&node.repository, &node.revision, &node.path] {
                put_string(writer, value)?;
            }
            for value in [
                node.start_line,
                node.end_line,
                node.start_byte,
                node.end_byte,
            ] {
                writer.write_all(&value.to_le_bytes())?;
            }
            put_string(writer, &node.symbol)?;
            put_string(writer, &node.kind)?;
        }
        for offset in &self.forward_offsets {
            writer.write_all(&offset.to_le_bytes())?;
        }
        for edge in &self.edges {
            writer.write_all(edge.target.as_bytes())?;
            writer.write_all(edge.evidence.node.as_bytes())?;
            writer.write_all(&edge.evidence.start_line.to_le_bytes())?;
            writer.write_all(&edge.evidence.end_line.to_le_bytes())?;
            writer.write_all(&edge.evidence.start_byte.to_le_bytes())?;
            writer.write_all(&edge.evidence.end_byte.to_le_bytes())?;
            put_string(writer, &edge.relation)?;
            put_string(writer, &edge.extractor)?;
            writer.write_all(&[edge.confidence, u8::from(edge.verified)])?;
        }
        for offset in &self.reverse_offsets {
            writer.write_all(&offset.to_le_bytes())?;
        }
        for edge_index in &self.reverse_edge_indices {
            writer.write_all(&edge_index.to_le_bytes())?;
        }
        Ok(())
    }

    fn build_forward_index(&self) -> Result<Vec<u32>, GraphArtifactError> {
        let node_indexes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let mut counts = vec![0_usize; self.nodes.len()];
        let mut source = None;
        for edge in &self.edges {
            let index = *node_indexes.get(&edge.source).ok_or_else(|| {
                GraphArtifactError::Contract("graph edge source is not a node".into())
            })?;
            if source.is_some_and(|previous| index < previous) {
                return Err(GraphArtifactError::Contract(
                    "graph edges are not grouped by source".into(),
                ));
            }
            source = Some(index);
            counts[index] = counts[index].saturating_add(1);
        }
        let mut offsets = vec![0_u32];
        for count in counts {
            let next = usize::try_from(offsets.last().copied().unwrap_or(0))
                .expect("u32 graph offset fits usize")
                .saturating_add(count);
            offsets.push(
                u32::try_from(next).map_err(|_| {
                    GraphArtifactError::Contract("graph edge offset overflows".into())
                })?,
            );
        }
        Ok(offsets)
    }

    fn build_reverse_index(&self) -> Result<(Vec<u32>, Vec<u32>), GraphArtifactError> {
        let node_indexes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let mut incoming = vec![Vec::<usize>::new(); self.nodes.len()];
        for (edge_index, edge) in self.edges.iter().enumerate() {
            let target = *node_indexes.get(&edge.target).ok_or_else(|| {
                GraphArtifactError::Contract("graph edge target is not a node".into())
            })?;
            incoming[target].push(edge_index);
        }
        for indexes in &mut incoming {
            indexes.sort_by(|left, right| {
                self.edges[*left]
                    .source
                    .cmp(&self.edges[*right].source)
                    .then_with(|| self.edges[*left].target.cmp(&self.edges[*right].target))
                    .then_with(|| {
                        self.edges[*left]
                            .evidence
                            .node
                            .cmp(&self.edges[*right].evidence.node)
                    })
                    .then_with(|| {
                        self.edges[*left]
                            .evidence
                            .start_line
                            .cmp(&self.edges[*right].evidence.start_line)
                    })
                    .then_with(|| {
                        self.edges[*left]
                            .evidence
                            .end_line
                            .cmp(&self.edges[*right].evidence.end_line)
                    })
                    .then_with(|| {
                        self.edges[*left]
                            .evidence
                            .start_byte
                            .cmp(&self.edges[*right].evidence.start_byte)
                    })
                    .then_with(|| {
                        self.edges[*left]
                            .evidence
                            .end_byte
                            .cmp(&self.edges[*right].evidence.end_byte)
                    })
                    .then_with(|| self.edges[*left].relation.cmp(&self.edges[*right].relation))
                    .then_with(|| {
                        self.edges[*left]
                            .extractor
                            .cmp(&self.edges[*right].extractor)
                    })
                    .then_with(|| {
                        self.edges[*left]
                            .confidence
                            .cmp(&self.edges[*right].confidence)
                    })
                    .then_with(|| self.edges[*left].verified.cmp(&self.edges[*right].verified))
            });
        }
        let mut offsets = Vec::with_capacity(self.nodes.len() + 1);
        offsets.push(0);
        let mut indexes = Vec::with_capacity(self.edges.len());
        for incoming in incoming {
            indexes.extend(
                incoming
                    .into_iter()
                    .map(|index| u32::try_from(index).expect("graph edge count is bounded by u32")),
            );
            offsets.push(u32::try_from(indexes.len()).map_err(|_| {
                GraphArtifactError::Contract("graph reverse index exceeds wire limits".into())
            })?);
        }
        Ok((offsets, indexes))
    }
}

fn graph_span(source_span: Option<SourceSpan>) -> Result<(u32, u32, u32, u32), GraphArtifactError> {
    let Some(span) = source_span else {
        return Ok((0, 0, 0, 0));
    };
    let start_byte = span
        .start_byte
        .map(u32::try_from)
        .transpose()
        .map_err(|_| {
            GraphArtifactError::Contract(
                "chunk source span byte offset exceeds graph limits".into(),
            )
        })?
        .unwrap_or(0);
    let end_byte = span
        .end_byte
        .map(u32::try_from)
        .transpose()
        .map_err(|_| {
            GraphArtifactError::Contract(
                "chunk source span byte offset exceeds graph limits".into(),
            )
        })?
        .unwrap_or(0);
    Ok((span.start_line, span.end_line, start_byte, end_byte))
}

fn decode_reverse_index(
    reader: &mut Reader<'_>,
    node_count: usize,
    edge_count: usize,
) -> Result<(Vec<u32>, Vec<u32>), GraphArtifactError> {
    let mut reverse_offsets = Vec::with_capacity(node_count + 1);
    for _ in 0..=node_count {
        reverse_offsets.push(
            u32::try_from(bounded_count(
                reader.u32()?,
                edge_count,
                "reverse edge offset",
            )?)
            .map_err(|_| GraphArtifactError::Contract("reverse edge offset overflows".into()))?,
        );
    }
    let edge_count_u32 = u32::try_from(edge_count)
        .map_err(|_| GraphArtifactError::Contract("graph edge count overflows".into()))?;
    if reverse_offsets.first() != Some(&0)
        || reverse_offsets.last() != Some(&edge_count_u32)
        || reverse_offsets.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Err(GraphArtifactError::Contract(
            "graph reverse adjacency offsets are invalid".into(),
        ));
    }
    let mut reverse_edge_indices = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        reverse_edge_indices.push(
            u32::try_from(bounded_count(
                reader.u32()?,
                edge_count,
                "reverse edge index",
            )?)
            .map_err(|_| GraphArtifactError::Contract("reverse edge index overflows".into()))?,
        );
    }
    Ok((reverse_offsets, reverse_edge_indices))
}

fn read_edge(
    reader: &mut Reader<'_>,
    source: ContentDigest,
) -> Result<GraphEdge, GraphArtifactError> {
    let target = ContentDigest::from_sha256(reader.array()?);
    let evidence = GraphEvidence {
        node: ContentDigest::from_sha256(reader.array()?),
        start_line: reader.u32()?,
        end_line: reader.u32()?,
        start_byte: reader.u32()?,
        end_byte: reader.u32()?,
    };
    let relation = reader.string()?;
    let extractor = reader.string()?;
    let confidence = reader.u8()?;
    let verified = match reader.u8()? {
        0 => false,
        1 => true,
        _ => {
            return Err(GraphArtifactError::Contract(
                "graph edge verification flag is invalid".into(),
            ));
        }
    };
    Ok(GraphEdge {
        source,
        target,
        evidence,
        relation,
        extractor,
        confidence,
        verified,
    })
}

fn validate_node(node: &GraphNode) -> Result<(), GraphArtifactError> {
    if node.repository.trim().is_empty()
        || node.revision.trim().is_empty()
        || node.path.trim().is_empty()
        || node.symbol.trim().is_empty()
        || node.kind.trim().is_empty()
        || std::path::Path::new(&node.path).is_absolute()
        || std::path::Path::new(&node.path)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || node.start_line > node.end_line
        || node.start_byte > node.end_byte
    {
        return Err(GraphArtifactError::Contract(
            "graph node provenance is invalid".into(),
        ));
    }
    for value in [
        &node.repository,
        &node.revision,
        &node.path,
        &node.symbol,
        &node.kind,
    ] {
        if value.len() > MAX_STRING_BYTES {
            return Err(GraphArtifactError::Contract(
                "graph node field is too large".into(),
            ));
        }
    }
    Ok(())
}

fn put_string<W: Write>(writer: &mut W, value: &str) -> Result<(), GraphArtifactError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(GraphArtifactError::Contract(
            "graph string is too large".into(),
        ));
    }
    writer.write_all(
        &u32::try_from(value.len())
            .map_err(|_| GraphArtifactError::Contract("graph string length overflows wire".into()))?
            .to_le_bytes(),
    )?;
    writer.write_all(value.as_bytes())?;
    Ok(())
}

struct GraphDigestWriter<W> {
    writer: W,
    payload_digest: Sha256,
    artifact_digest: Sha256,
}

impl<W: Write> GraphDigestWriter<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            payload_digest: Sha256::new(),
            artifact_digest: Sha256::new(),
        }
    }

    fn write_unhashed(&mut self, bytes: &[u8]) -> Result<(), GraphArtifactError> {
        self.writer.write_all(bytes)?;
        self.artifact_digest.update(bytes);
        Ok(())
    }

    fn finish(mut self) -> Result<ContentDigest, GraphArtifactError> {
        self.writer.flush()?;
        Ok(ContentDigest::from_sha256(
            self.artifact_digest.finalize().into(),
        ))
    }
}

impl<W: Write> Write for GraphDigestWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.payload_digest.update(&bytes[..written]);
        self.artifact_digest.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn bounded_count(value: u32, max: usize, name: &str) -> Result<usize, GraphArtifactError> {
    let value = usize::try_from(value)
        .map_err(|_| GraphArtifactError::Contract(format!("graph {name} count overflows")))?;
    (value <= max)
        .then_some(value)
        .ok_or_else(|| GraphArtifactError::Contract(format!("graph {name} count exceeds limit")))
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], GraphArtifactError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| GraphArtifactError::Contract("graph offset overflow".into()))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| GraphArtifactError::Contract("graph artifact is truncated".into()))?;
        self.offset = end;
        Ok(value)
    }

    fn array(&mut self) -> Result<[u8; 32], GraphArtifactError> {
        let mut value = [0_u8; 32];
        value.copy_from_slice(self.take(32)?);
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, GraphArtifactError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, GraphArtifactError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("u16 length"),
        ))
    }
    fn u32(&mut self) -> Result<u32, GraphArtifactError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("u32 length"),
        ))
    }

    fn string(&mut self) -> Result<String, GraphArtifactError> {
        let length = bounded_count(self.u32()?, MAX_STRING_BYTES, "string")?;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| GraphArtifactError::Contract("graph string is not UTF-8".into()))
    }

    const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{NormalizedDocument, RepositoryId, Revision, SourceKind};

    fn fixture() -> GraphArtifact {
        let declaration = GraphNode::new(
            "repo",
            "rev",
            "src/lib.rs",
            (1, 2, 0, 20),
            "Thing",
            "declaration",
        );
        let implementation = GraphNode::new(
            "repo",
            "rev",
            "src/lib.rs",
            (4, 8, 21, 80),
            "Thing",
            "definition",
        );
        GraphArtifact::new(
            ContentDigest::of(b"corpus"),
            vec![implementation.clone(), declaration.clone()],
            vec![GraphEdge {
                source: declaration.id.clone(),
                target: implementation.id,
                evidence: GraphEvidence {
                    node: declaration.id,
                    start_line: 1,
                    end_line: 2,
                    start_byte: 0,
                    end_byte: 20,
                },
                relation: "declaration-definition".into(),
                extractor: "fixture-v1".into(),
                confidence: 100,
                verified: true,
            }],
        )
        .expect("fixture graph")
    }

    #[test]
    fn identical_graph_inputs_have_identical_bytes() {
        let first = fixture().encode().expect("encode");
        let second = fixture().encode().expect("encode");
        assert_eq!(first, second);
    }

    #[test]
    fn streaming_payload_matches_the_encoded_payload() {
        let graph = fixture();
        let mut streamed = Vec::new();
        graph
            .write_payload_to(&mut streamed)
            .expect("stream graph payload");
        let mut encoded = graph.encode().expect("encode");
        encoded.truncate(encoded.len() - 32);
        assert_eq!(streamed, encoded);
    }

    #[test]
    fn round_trip_preserves_csr_edges_and_provenance() {
        let graph = fixture();
        let decoded = GraphArtifact::decode(&graph.encode().expect("encode")).expect("decode");
        assert_eq!(decoded, graph);
        let source = &graph.edges[0].source;
        let target = &graph.edges[0].target;
        assert_eq!(graph.outgoing(source).len(), 1);
        assert!(graph.outgoing(target).is_empty());
        assert_eq!(graph.incoming(target).len(), 1);
        assert!(graph.incoming(source).is_empty());
    }

    #[test]
    fn checksum_and_endpoint_validation_fail_closed() {
        let graph = fixture();
        let mut bytes = graph.encode().expect("encode");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(GraphArtifact::decode(&bytes).is_err());
        let mut invalid = fixture();
        invalid.edges[0].target = ContentDigest::of(b"missing");
        assert!(invalid.validate().is_err());

        let foreign = GraphNode::new(
            "repo",
            "other-revision",
            "src/other.rs",
            (1, 1, 0, 4),
            "foreign",
            "definition",
        );
        let mut cross_revision = fixture();
        cross_revision.nodes.push(foreign.clone());
        cross_revision.edges[0].target = foreign.id;
        assert!(
            GraphArtifact::new(
                cross_revision.corpus_digest,
                cross_revision.nodes,
                cross_revision.edges,
            )
            .is_err()
        );
    }

    #[test]
    fn from_chunks_emits_verified_adjacency_with_source_evidence() {
        let repository = RepositoryId::try_from("fixture-repo").expect("repository");
        let revision = Revision::try_from("fixture-revision").expect("revision");
        let first_document = NormalizedDocument::new(
            "first",
            SourceKind::EmbeddedCatalog,
            repository.clone(),
            revision.clone(),
            "src/first.rs",
            "text/plain",
            "first",
        )
        .expect("first document");
        let second_document = NormalizedDocument::new(
            "second",
            SourceKind::EmbeddedCatalog,
            repository,
            revision,
            "src/second.rs",
            "text/plain",
            "second",
        )
        .expect("second document");
        let mut first = Chunk::from_document(&first_document, 0, "first".into(), Vec::new(), None)
            .expect("first chunk");
        let second = Chunk::from_document(&second_document, 0, "second".into(), Vec::new(), None)
            .expect("second chunk");
        first.next_chunk = Some(second.chunk_id.clone());

        let graph = GraphArtifact::from_chunks(ContentDigest::of(b"corpus"), &[first, second])
            .expect("graph");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].relation, "adjacent-next");
        assert_eq!(graph.edges[0].extractor, "chunk-adjacency-v1");
        assert!(graph.edges[0].verified);
        assert_eq!(graph.edges[0].evidence.node, graph.edges[0].source);

        let mut dangling = graph_fixture_chunk(&first_document);
        dangling.next_chunk = Some(
            ChunkId::try_from(
                "chunk-0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("chunk id"),
        );
        assert!(GraphArtifact::from_chunks(ContentDigest::of(b"corpus"), &[dangling]).is_err());
    }

    #[test]
    fn projected_graph_metadata_matches_full_chunks() {
        let repository = RepositoryId::try_from("fixture-repo").expect("repository");
        let revision = Revision::try_from("fixture-revision").expect("revision");
        let first_document = NormalizedDocument::new(
            "first",
            SourceKind::EmbeddedCatalog,
            repository.clone(),
            revision.clone(),
            "src/first.rs",
            "text/plain",
            "first",
        )
        .expect("first document");
        let second_document = NormalizedDocument::new(
            "second",
            SourceKind::EmbeddedCatalog,
            repository,
            revision,
            "src/second.rs",
            "text/plain",
            "second",
        )
        .expect("second document");
        let mut first = Chunk::from_document(&first_document, 0, "first".into(), Vec::new(), None)
            .expect("first chunk");
        let second = Chunk::from_document(&second_document, 0, "second".into(), Vec::new(), None)
            .expect("second chunk");
        first.next_chunk = Some(second.chunk_id.clone());

        let full = GraphArtifact::from_chunks(
            ContentDigest::of(b"corpus"),
            &[first.clone(), second.clone()],
        )
        .expect("full graph");
        let projected = GraphArtifact::from_graph_chunks(
            ContentDigest::of(b"corpus"),
            [
                GraphChunk::from_chunk(&first),
                GraphChunk::from_chunk(&second),
            ],
        )
        .expect("projected graph");

        assert_eq!(projected, full);
    }

    #[test]
    fn projected_graph_reader_streams_json_array() {
        let repository = RepositoryId::try_from("fixture-repo").expect("repository");
        let revision = Revision::try_from("fixture-revision").expect("revision");
        let document = NormalizedDocument::new(
            "streamed",
            SourceKind::EmbeddedCatalog,
            repository,
            revision,
            "src/streamed.rs",
            "text/plain",
            "streamed",
        )
        .expect("document");
        let chunk =
            Chunk::from_document(&document, 0, "streamed".into(), Vec::new(), None).expect("chunk");
        let projected = GraphChunk::from_chunk(&chunk);
        let bytes = serde_json::to_vec(std::slice::from_ref(&projected)).expect("encode");

        let streamed = GraphArtifact::from_graph_chunk_reader(
            ContentDigest::of(b"corpus"),
            std::io::Cursor::new(bytes),
            Some,
        )
        .expect("streamed graph");
        let expected = GraphArtifact::from_graph_chunks(ContentDigest::of(b"corpus"), [projected])
            .expect("expected graph");

        assert_eq!(streamed, expected);
    }

    #[test]
    fn projected_graph_metadata_round_trips_for_staging_sidecar() {
        let repository = RepositoryId::try_from("fixture-repo").expect("repository");
        let revision = Revision::try_from("fixture-revision").expect("revision");
        let document = NormalizedDocument::new(
            "sidecar",
            SourceKind::EmbeddedCatalog,
            repository,
            revision,
            "src/sidecar.rs",
            "text/plain",
            "sidecar",
        )
        .expect("document");
        let chunk =
            Chunk::from_document(&document, 0, "sidecar".into(), Vec::new(), None).expect("chunk");
        let projected = GraphChunk::from_chunk(&chunk);
        let encoded = serde_json::to_vec(&projected).expect("encode graph projection");
        let decoded: GraphChunk =
            serde_json::from_slice(&encoded).expect("decode graph projection");

        assert_eq!(decoded, projected);
    }

    fn graph_fixture_chunk(document: &NormalizedDocument) -> Chunk {
        Chunk::from_document(document, 0, document.content.clone(), Vec::new(), None)
            .expect("fixture chunk")
    }
}
