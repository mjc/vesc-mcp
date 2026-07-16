//! Deterministic Markdown passage chunking.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

use super::{Chunk, NormalizedDocument, SourceSpan};
use crate::corpus::CorpusError;

/// Starting chunk limits for the v1 corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkingConfig {
    pub target_chars: usize,
    pub hard_max_chars: usize,
    pub minimum_chars: usize,
    pub overlap_chars: usize,
    pub max_chunks_per_document: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            target_chars: 1_200,
            hard_max_chars: 2_400,
            minimum_chars: 120,
            overlap_chars: 0,
            max_chunks_per_document: 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChunkingError {
    #[error("chunk target and hard maximum must be positive and ordered")]
    InvalidConfig,
    #[error("document produced more than {max} chunks")]
    TooManyChunks { max: usize },
    #[error("fenced code block exceeds hard maximum of {max} characters")]
    OversizedCodeBlock { max: usize },
    #[error("structured record exceeds hard maximum of {max} characters")]
    OversizedStructuredRecord { max: usize },
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
/// Returns [`ChunkingError`] when the configuration is invalid, a code block is
/// too large, the chunk count is bounded out, or a generated chunk violates a
/// corpus contract.
pub fn chunk_markdown(
    document: &NormalizedDocument,
    config: ChunkingConfig,
) -> Result<Vec<Chunk>, ChunkingError> {
    validate_config(config)?;
    let blocks = markdown_blocks(&document.content);
    let mut chunks = Vec::new();
    for block in blocks {
        let text = &document.content[block.start..block.end];
        if text.trim().is_empty() {
            continue;
        }
        let pieces = split_block(text, block.code, config)?;
        let mut offset = block.start;
        for piece in pieces {
            let piece_start = offset;
            let piece_end = piece_start + piece.len();
            offset = piece_end;
            let span = Some(source_span(&document.content, piece_start, piece_end));
            let chunk = Chunk::from_document(
                document,
                u32::try_from(chunks.len()).map_err(|_| ChunkingError::TooManyChunks {
                    max: config.max_chunks_per_document,
                })?,
                piece,
                block.heading_path.clone(),
                span,
            )?;
            chunks.push(chunk);
            if chunks.len() > config.max_chunks_per_document {
                return Err(ChunkingError::TooManyChunks {
                    max: config.max_chunks_per_document,
                });
            }
        }
    }
    for index in 0..chunks.len().saturating_sub(1) {
        let (left, right) = chunks.split_at_mut(index + 1);
        left[index].next_chunk = Some(right[0].chunk_id.clone());
        right[0].previous_chunk = Some(left[index].chunk_id.clone());
    }
    Ok(chunks)
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
    if document.media_type != "text/markdown" {
        validate_config(config)?;
        let heading_path = document
            .path
            .split_once('#')
            .map(|(_, anchor)| vec![anchor.to_owned()])
            .unwrap_or_default();
        if document.content.chars().count() <= config.hard_max_chars {
            return Ok(vec![Chunk::from_document(
                document,
                0,
                document.content.clone(),
                heading_path,
                document.source_span,
            )?]);
        }
        let pieces = split_block(&document.content, false, config)?;
        let mut chunks = Vec::with_capacity(pieces.len());
        let mut search_start = 0;
        for (ordinal, piece) in pieces.into_iter().enumerate() {
            let Some(relative_start) = document.content[search_start..].find(&piece) else {
                return Err(ChunkingError::Contract(CorpusError::InvalidValue {
                    kind: "structured chunk boundary",
                    value: document.path.clone(),
                }));
            };
            let start = search_start + relative_start;
            let end = start + piece.len();
            search_start = end;
            if piece.chars().count() > config.hard_max_chars {
                return Err(ChunkingError::OversizedStructuredRecord {
                    max: config.hard_max_chars,
                });
            }
            chunks.push(Chunk::from_document(
                document,
                u32::try_from(ordinal).map_err(|_| ChunkingError::TooManyChunks {
                    max: config.max_chunks_per_document,
                })?,
                piece,
                heading_path.clone(),
                Some(source_span(&document.content, start, end)),
            )?);
            if chunks.len() > config.max_chunks_per_document {
                return Err(ChunkingError::TooManyChunks {
                    max: config.max_chunks_per_document,
                });
            }
        }
        for index in 0..chunks.len().saturating_sub(1) {
            let (left, right) = chunks.split_at_mut(index + 1);
            left[index].next_chunk = Some(right[0].chunk_id.clone());
            right[0].previous_chunk = Some(left[index].chunk_id.clone());
        }
        return Ok(chunks);
    }
    chunk_markdown(document, config)
}

#[derive(Debug)]
struct Block {
    start: usize,
    end: usize,
    code: bool,
    heading_path: Vec<String>,
}

fn markdown_blocks(source: &str) -> Vec<Block> {
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
    let mut headings: Vec<(u8, String)> = Vec::new();
    let mut in_code = false;
    let mut heading_only = false;
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.trim();
        let line_is_code = code_ranges
            .iter()
            .any(|range| line_start < range.end && line_end > range.start);
        if !line_is_code && trimmed.starts_with('#') {
            if let Some(current) = start.take() {
                push_block(&mut blocks, current, line_start, in_code, &headings);
            }
            let level =
                u8::try_from(trimmed.bytes().take_while(|byte| *byte == b'#').count()).unwrap_or(6);
            let title = trimmed[usize::from(level)..].trim().to_owned();
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
            heading_path: Vec::new(),
        });
    }
    blocks
}

fn push_block(
    blocks: &mut Vec<Block>,
    start: usize,
    end: usize,
    code: bool,
    headings: &[(u8, String)],
) {
    if start < end {
        blocks.push(Block {
            start,
            end,
            code,
            heading_path: headings.iter().map(|(_, title)| title.clone()).collect(),
        });
    }
}

fn split_block(
    text: &str,
    code: bool,
    config: ChunkingConfig,
) -> Result<Vec<String>, ChunkingError> {
    let char_count = text.chars().count();
    if code {
        if char_count > config.hard_max_chars {
            return Err(ChunkingError::OversizedCodeBlock {
                max: config.hard_max_chars,
            });
        }
        return Ok(vec![text.to_owned()]);
    }
    if char_count <= config.target_chars {
        return Ok(vec![text.to_owned()]);
    }

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut pieces = Vec::new();
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
        let piece = text[byte_start..byte_end].to_owned();
        if !piece.trim().is_empty() {
            pieces.push(piece);
        }
        start_char = end_char;
    }
    Ok(pieces)
}

fn source_span(source: &str, start: usize, end: usize) -> SourceSpan {
    let start_line = source[..start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let end_line = source[..end].bytes().filter(|byte| *byte == b'\n').count() + 1;
    SourceSpan::new(
        u32::try_from(start_line).unwrap_or(u32::MAX),
        u32::try_from(end_line).unwrap_or(u32::MAX),
        Some(start as u64),
        Some(end as u64),
    )
    .expect("computed source span is ordered")
}

const fn validate_config(config: ChunkingConfig) -> Result<(), ChunkingError> {
    if config.target_chars == 0
        || config.hard_max_chars < config.target_chars
        || config.minimum_chars > config.target_chars
        || config.overlap_chars > config.target_chars
        || config.max_chunks_per_document == 0
    {
        Err(ChunkingError::InvalidConfig)
    } else {
        Ok(())
    }
}
