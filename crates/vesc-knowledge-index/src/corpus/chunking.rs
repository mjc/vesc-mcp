//! Deterministic Markdown passage chunking.

use std::ops::Range;

use compact_str::CompactString;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

use super::{Chunk, ChunkId, ChunkIdentity, ContentDigest, NormalizedDocument, SourceSpan};
use crate::corpus::CorpusError;

/// Starting chunk limits for the v1 corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkingConfig {
    pub target_chars: usize,
    pub hard_max_chars: usize,
    pub minimum_chars: usize,
    pub overlap_chars: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            target_chars: 1_200,
            hard_max_chars: 2_400,
            minimum_chars: 120,
            overlap_chars: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChunkingError {
    #[error("chunk target and hard maximum must be positive and ordered")]
    InvalidConfig,
    #[error(transparent)]
    Contract(#[from] CorpusError),
}

/// Chunks a Markdown document into bounded, provenance-preserving passages.
///
/// Headings stay with the following content, fenced code blocks remain whole,
/// and ordinary oversized blocks split only at UTF-8 boundaries.
///
/// # Errors
///
/// Returns [`ChunkingError`] when the configuration is invalid or a generated
/// chunk violates a corpus contract.
pub fn chunk_markdown(
    document: &NormalizedDocument,
    config: ChunkingConfig,
) -> Result<Vec<Chunk>, ChunkingError> {
    materialize_all(&chunk_markdown_drafts(document, config)?)
}

/// Chunks a normalized document according to its media type.
///
/// Markdown uses heading/code-block semantics. Code, plain text, and structured
/// records use deterministic bounded text splitting so source indentation and
/// preprocessor directives are not misread as Markdown.
///
/// # Errors
///
/// Returns [`ChunkingError`] when the configuration is invalid or a structured
/// record exceeds its hard size limit.
pub fn chunk_document(
    document: &NormalizedDocument,
    config: ChunkingConfig,
) -> Result<Vec<Chunk>, ChunkingError> {
    materialize_all(&chunk_document_drafts(document, config)?)
}

const MAX_HEADING_DEPTH: usize = 6;

#[derive(Debug, Clone)]
enum HeadingRefs<'a> {
    Inline {
        titles: [&'a str; MAX_HEADING_DEPTH],
        len: usize,
    },
    Overflow(Vec<&'a str>),
}

impl<'a> HeadingRefs<'a> {
    const fn empty() -> Self {
        Self::Inline {
            titles: [""; MAX_HEADING_DEPTH],
            len: 0,
        }
    }

    fn as_slice(&self) -> &[&'a str] {
        match self {
            Self::Inline { titles, len } => &titles[..*len],
            Self::Overflow(titles) => titles,
        }
    }

    fn from_stack(headings: &[(u8, &'a str)]) -> Self {
        if headings.len() > MAX_HEADING_DEPTH {
            return Self::Overflow(headings.iter().map(|(_, title)| *title).collect());
        }
        let mut titles = [""; MAX_HEADING_DEPTH];
        for (output, (_, title)) in titles.iter_mut().zip(headings) {
            *output = title;
        }
        Self::Inline {
            titles,
            len: headings.len(),
        }
    }
}

#[derive(Debug)]
struct Block<'a> {
    start: usize,
    end: usize,
    code: bool,
    headings: HeadingRefs<'a>,
}

#[derive(Debug)]
pub(crate) struct ChunkDraft<'a> {
    document: &'a NormalizedDocument,
    text: Range<usize>,
    headings: HeadingRefs<'a>,
    source_span: Option<SourceSpan>,
    ordinal: u32,
    content_digest: ContentDigest,
    chunk_identity: [u8; 32],
}

impl ChunkDraft<'_> {
    pub(crate) fn text(&self) -> &str {
        &self.document.content[self.text.clone()]
    }

    pub(crate) fn headings(&self) -> &[&str] {
        self.headings.as_slice()
    }

    pub(crate) const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

#[derive(Debug)]
pub(crate) struct ChunkDrafts<'a> {
    chunks: Vec<ChunkDraft<'a>>,
}

impl ChunkDrafts<'_> {
    pub(crate) const fn len(&self) -> usize {
        self.chunks.len()
    }

    pub(crate) fn get(&self, index: usize) -> &ChunkDraft<'_> {
        &self.chunks[index]
    }

    pub(crate) fn materialize(
        &self,
        index: usize,
        identifiers: Option<Vec<CompactString>>,
    ) -> Result<Chunk, CorpusError> {
        let draft = &self.chunks[index];
        let mut chunk = Chunk::from_document_identity(
            draft.document,
            draft.ordinal,
            draft.text().to_owned(),
            draft
                .headings()
                .iter()
                .map(|heading| (*heading).to_owned())
                .collect(),
            draft.source_span,
            ChunkIdentity::from_sha256(draft.content_digest.clone(), draft.chunk_identity),
        )?;
        if let Some(identifiers) = identifiers {
            chunk.identifiers = identifiers;
        }
        chunk.previous_chunk = index
            .checked_sub(1)
            .map(|previous| ChunkId::from_sha256(self.chunks[previous].chunk_identity));
        chunk.next_chunk = self
            .chunks
            .get(index + 1)
            .map(|next| ChunkId::from_sha256(next.chunk_identity));
        Ok(chunk)
    }
}

pub(crate) fn chunk_document_drafts(
    document: &NormalizedDocument,
    config: ChunkingConfig,
) -> Result<ChunkDrafts<'_>, ChunkingError> {
    if document.media_type == "text/markdown" {
        return chunk_markdown_drafts(document, config);
    }
    validate_config(config)?;
    let headings = document
        .path
        .split_once('#')
        .map_or_else(HeadingRefs::empty, |(_, anchor)| HeadingRefs::Inline {
            titles: [anchor, "", "", "", "", ""],
            len: 1,
        });
    if document.content.chars().count() <= config.hard_max_chars {
        return finish_drafts(
            document,
            vec![(0..document.content.len(), headings, document.source_span)],
        );
    }
    let mut descriptors = Vec::new();
    let mut source_spans = SourceSpanCursor::new(&document.content);
    visit_block_ranges(&document.content, false, config, |text| {
        let span = if text.start == 0 && text.end == document.content.len() {
            document.source_span
        } else {
            Some(source_spans.span(text.start, text.end))
        };
        descriptors.push((text, headings.clone(), span));
    });
    finish_drafts(document, descriptors)
}

fn chunk_markdown_drafts(
    document: &NormalizedDocument,
    config: ChunkingConfig,
) -> Result<ChunkDrafts<'_>, ChunkingError> {
    validate_config(config)?;
    let mut descriptors = Vec::new();
    let mut source_spans = SourceSpanCursor::new(&document.content);
    for block in markdown_blocks(&document.content) {
        let text = &document.content[block.start..block.end];
        if text.trim().is_empty() {
            continue;
        }
        visit_block_ranges(text, block.code, config, |piece| {
            let text = block.start + piece.start..block.start + piece.end;
            let span = Some(source_spans.span(text.start, text.end));
            descriptors.push((text, block.headings.clone(), span));
        });
    }
    finish_drafts(document, descriptors)
}

fn finish_drafts<'a>(
    document: &'a NormalizedDocument,
    descriptors: Vec<(Range<usize>, HeadingRefs<'a>, Option<SourceSpan>)>,
) -> Result<ChunkDrafts<'a>, ChunkingError> {
    let mut chunks = Vec::with_capacity(descriptors.len());
    for (ordinal, (text, headings, source_span)) in descriptors.into_iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| CorpusError::InvalidValue {
            kind: "chunk ordinal",
            value: ordinal.to_string(),
        })?;
        let content_digest = ContentDigest::of(document.content[text.clone()].as_bytes());
        let chunk_identity = ChunkId::heading_identity_digest(
            &document.document_id,
            ordinal,
            headings.as_slice(),
            &content_digest,
        );
        chunks.push(ChunkDraft {
            document,
            text,
            headings,
            source_span,
            ordinal,
            content_digest,
            chunk_identity,
        });
    }
    Ok(ChunkDrafts { chunks })
}

fn materialize_all(drafts: &ChunkDrafts<'_>) -> Result<Vec<Chunk>, ChunkingError> {
    (0..drafts.len())
        .map(|index| {
            drafts
                .materialize(index, None)?
                .with_derived_resource_uri()
                .map_err(ChunkingError::from)
        })
        .collect()
}

fn markdown_blocks(source: &str) -> Vec<Block<'_>> {
    let mut code_ranges = Vec::new();
    let mut code_start = None;
    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_start.take() {
                    code_ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }

    let mut blocks = Vec::new();
    let mut start = None;
    let mut headings: Vec<(u8, &str)> = Vec::new();
    let mut in_code = false;
    let mut heading_only = false;
    let mut line_start = 0;
    let mut code_range = 0;
    for line in source.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.trim();
        while code_ranges
            .get(code_range)
            .is_some_and(|range| range.end <= line_start)
        {
            code_range += 1;
        }
        let line_is_code = code_ranges
            .get(code_range)
            .is_some_and(|range| line_start < range.end && line_end > range.start);
        if !line_is_code && trimmed.starts_with('#') {
            if let Some(current) = start.take() {
                push_block(&mut blocks, current, line_start, in_code, &headings);
            }
            let level =
                u8::try_from(trimmed.bytes().take_while(|byte| *byte == b'#').count()).unwrap_or(6);
            let title = trimmed[usize::from(level)..].trim();
            while headings.last().is_some_and(|(old, _)| *old >= level) {
                headings.pop();
            }
            headings.push((level, title));
            start = Some(line_start);
            heading_only = true;
        } else if trimmed.is_empty() && !line_is_code {
            if heading_only {
                heading_only = false;
            } else if let Some(current) = start.take() {
                push_block(&mut blocks, current, line_start, in_code, &headings);
            }
        } else if start.is_none() {
            start = Some(line_start);
        } else if !trimmed.is_empty() {
            heading_only = false;
        }
        in_code = line_is_code;
        line_start = line_end;
    }
    if let Some(current) = start {
        push_block(&mut blocks, current, source.len(), in_code, &headings);
    }
    if blocks.is_empty() && !source.trim().is_empty() {
        blocks.push(Block {
            start: 0,
            end: source.len(),
            code: false,
            headings: HeadingRefs::empty(),
        });
    }
    blocks
}

fn push_block<'a>(
    blocks: &mut Vec<Block<'a>>,
    start: usize,
    end: usize,
    code: bool,
    headings: &[(u8, &'a str)],
) {
    if start < end {
        blocks.push(Block {
            start,
            end,
            code,
            headings: HeadingRefs::from_stack(headings),
        });
    }
}

fn visit_block_ranges(
    text: &str,
    code: bool,
    config: ChunkingConfig,
    mut visit: impl FnMut(Range<usize>),
) {
    let char_count = text.chars().count();
    if code || char_count <= config.target_chars {
        visit(0..text.len());
        return;
    }

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut start_char = 0;
    while start_char < chars.len() {
        let remaining = chars.len() - start_char;
        let take = remaining.min(config.target_chars);
        let mut end_char = start_char + take;
        if end_char < chars.len() {
            let candidate = chars[start_char..end_char]
                .iter()
                .rposition(|(_, character)| character.is_whitespace())
                .map_or(end_char, |index| start_char + index + 1);
            if candidate.saturating_sub(start_char) >= config.minimum_chars {
                end_char = candidate;
            }
        }
        let byte_start = chars[start_char].0;
        let byte_end = if end_char == chars.len() {
            text.len()
        } else {
            chars[end_char].0
        };
        if !text[byte_start..byte_end].trim().is_empty() {
            visit(byte_start..byte_end);
        }
        start_char = end_char;
    }
}

struct SourceSpanCursor<'a> {
    source: &'a [u8],
    offset: usize,
    line: u32,
}

impl<'a> SourceSpanCursor<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            offset: 0,
            line: 1,
        }
    }

    fn span(&mut self, start: usize, end: usize) -> SourceSpan {
        assert!(
            start >= self.offset && end >= start,
            "source spans must be ordered and non-overlapping"
        );
        let start_line = self.advance(start);
        let end_line = self.advance(end);
        SourceSpan::new(start_line, end_line, Some(start as u64), Some(end as u64))
            .expect("computed source span is ordered")
    }

    fn advance(&mut self, end: usize) -> u32 {
        let newlines = memchr::memchr_iter(b'\n', &self.source[self.offset..end]).count();
        self.line = self
            .line
            .saturating_add(u32::try_from(newlines).unwrap_or(u32::MAX));
        self.offset = end;
        self.line
    }
}

const fn validate_config(config: ChunkingConfig) -> Result<(), ChunkingError> {
    if config.target_chars == 0
        || config.hard_max_chars < config.target_chars
        || config.minimum_chars > config.target_chars
        || config.overlap_chars > config.target_chars
    {
        Err(ChunkingError::InvalidConfig)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{RepositoryId, Revision, SourceKind};

    #[test]
    fn draft_identity_matches_materialized_chunk_contract() {
        let document = NormalizedDocument::new(
            "Title",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("revision").expect("revision"),
            "docs/example.md",
            "text/markdown",
            "# Heading\n\npassage",
        )
        .expect("document");
        let drafts =
            chunk_markdown_drafts(&document, ChunkingConfig::default()).expect("chunk drafts");
        let materialized = drafts.materialize(0, None).expect("materialized chunk");
        let direct = Chunk::from_document(
            &document,
            0,
            materialized.text.clone(),
            materialized.heading_path.clone(),
            materialized.source_span,
        )
        .expect("direct chunk");

        assert_eq!(materialized.chunk_id, direct.chunk_id);
        assert_eq!(materialized.content_digest, direct.content_digest);
    }

    #[test]
    fn only_public_chunks_materialize_their_derived_resource_uri() {
        let document = NormalizedDocument::new(
            "Title",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("revision").expect("revision"),
            "docs/example.md",
            "text/markdown",
            "# Heading\n\npassage",
        )
        .expect("document");
        let drafts =
            chunk_markdown_drafts(&document, ChunkingConfig::default()).expect("chunk drafts");
        let history_chunk = drafts.materialize(0, None).expect("history chunk");
        let public_chunk = chunk_markdown(&document, ChunkingConfig::default())
            .expect("public chunks")
            .remove(0);

        assert!(history_chunk.resource_uri.is_none());
        assert!(public_chunk.resource_uri.is_some());
    }

    #[test]
    fn short_block_visits_one_inline_range() {
        let mut ranges = Vec::new();
        visit_block_ranges("short", false, ChunkingConfig::default(), |range| {
            ranges.push(range);
        });

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..5);
    }

    #[test]
    fn source_span_cursor_matches_prefix_count_semantics() {
        let source = "α first\n\nsecond\nγ third\n\nlast";
        let ranges = ["α first\n", "second\n", "γ third\n", "last"].map(|text| {
            let start = source.find(text).expect("range text");
            start..start + text.len()
        });
        let mut cursor = SourceSpanCursor::new(source);

        for range in ranges {
            let expected = SourceSpan::new(
                u32::try_from(
                    source[..range.start]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1,
                )
                .unwrap_or(u32::MAX),
                u32::try_from(
                    source[..range.end]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1,
                )
                .unwrap_or(u32::MAX),
                Some(range.start as u64),
                Some(range.end as u64),
            )
            .expect("expected span");

            assert_eq!(cursor.span(range.start, range.end), expected);
        }
        assert_eq!(cursor.offset, source.len());
    }
}
