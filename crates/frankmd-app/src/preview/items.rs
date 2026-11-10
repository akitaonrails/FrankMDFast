//! Flattening the render IR into virtual-list rows.
//!
//! The preview is a GPUI `list()` with one row per [`Item`]. Most rows are a
//! whole top-level block, but huge blocks (a 10 000-line code fence, a list
//! with thousands of items, a giant table) are split into chunks so a frame
//! never lays out more than a screenful-ish of content. Every row keeps the
//! true source span of what it shows, so sync scroll and the "visible source
//! line" report work on real editor lines.

use frankmd_markdown::RenderDoc;
use frankmd_markdown::ir::{Block, Inline, ListItem, SourceSpan};

/// Code lines per chunk of a long fenced block.
pub(crate) const CODE_CHUNK_LINES: usize = 120;
/// List items per chunk of a long top-level list.
pub(crate) const LIST_CHUNK_ITEMS: usize = 40;
/// Body rows per chunk of a long table.
pub(crate) const TABLE_CHUNK_ROWS: usize = 60;

/// What part of a top-level block a row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Part {
    /// The whole block.
    Whole,
    /// Code lines `range` (row 0 also draws the header with label and copy button).
    CodeLines(std::ops::Range<usize>),
    /// List items `range`.
    ListItems(std::ops::Range<usize>),
    /// Table body rows `range` (the first chunk also draws the header row).
    TableRows(std::ops::Range<usize>),
    /// The "Front matter" disclosure row (`block` is unused).
    FrontMatter,
}

/// One virtual-list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    /// Index into `RenderDoc::blocks`.
    pub block: usize,
    /// Which part of it.
    pub part: Part,
    /// Source span of exactly what this row shows (1-based, inclusive).
    pub span: SourceSpan,
    /// Ordinal of the first image this row draws (document order), so each
    /// image gets a stable focus slot and element id.
    pub first_image: usize,
    /// Ordinal of the code block this row belongs to, if any.
    pub code_ordinal: Option<usize>,
}

/// Rows of a document plus document-wide tables the rows refer to.
#[derive(Debug, Clone, Default)]
pub(crate) struct Layout {
    /// Rows in document order.
    pub items: Vec<Item>,
    /// Every image in document order: `(src, alt)` (`src` `None` = rejected URL).
    pub images: Vec<(Option<String>, String)>,
    /// Heading anchors → row index (for `#section` links).
    pub anchors: Vec<(String, usize)>,
}

/// Build the rows for `doc`.
pub(crate) fn layout(doc: &RenderDoc) -> Layout {
    let mut out = Layout::default();
    let mut code_ordinal = 0;
    if let Some(fm) = &doc.front_matter {
        let lines = fm.trim_end_matches('\n').matches('\n').count() as u32 + 1;
        out.items.push(Item {
            block: 0,
            part: Part::FrontMatter,
            span: SourceSpan {
                start_line: 1,
                end_line: lines.max(1),
            },
            first_image: 0,
            code_ordinal: None,
        });
    }
    for (block_ix, block) in doc.blocks.iter().enumerate() {
        let first_image = out.images.len();
        match block {
            Block::Code { lines, span, .. } if lines.len() > CODE_CHUNK_LINES => {
                for start in (0..lines.len()).step_by(CODE_CHUNK_LINES) {
                    let end = (start + CODE_CHUNK_LINES).min(lines.len());
                    out.items.push(Item {
                        block: block_ix,
                        part: Part::CodeLines(start..end),
                        span: code_chunk_span(*span, lines.len(), start..end),
                        first_image,
                        code_ordinal: Some(code_ordinal),
                    });
                }
                code_ordinal += 1;
                continue;
            }
            Block::List { items, .. } if items.len() > LIST_CHUNK_ITEMS => {
                for start in (0..items.len()).step_by(LIST_CHUNK_ITEMS) {
                    let end = (start + LIST_CHUNK_ITEMS).min(items.len());
                    let first_image = out.images.len();
                    for item in &items[start..end] {
                        collect_item_images(item, &mut out.images);
                    }
                    out.items.push(Item {
                        block: block_ix,
                        part: Part::ListItems(start..end),
                        span: SourceSpan {
                            start_line: items[start].span.start_line,
                            end_line: items[end - 1].span.end_line,
                        },
                        first_image,
                        code_ordinal: None,
                    });
                }
                continue;
            }
            Block::Table {
                rows, span, header, ..
            } if rows.len() > TABLE_CHUNK_ROWS => {
                for start in (0..rows.len()).step_by(TABLE_CHUNK_ROWS) {
                    let end = (start + TABLE_CHUNK_ROWS).min(rows.len());
                    let first_image = out.images.len();
                    if start == 0 {
                        header
                            .iter()
                            .for_each(|cell| collect_inline_images(cell, &mut out.images));
                    }
                    for row in &rows[start..end] {
                        row.iter()
                            .for_each(|cell| collect_inline_images(cell, &mut out.images));
                    }
                    out.items.push(Item {
                        block: block_ix,
                        part: Part::TableRows(start..end),
                        span: table_chunk_span(*span, rows.len(), start..end),
                        first_image,
                        code_ordinal: None,
                    });
                }
                continue;
            }
            Block::Heading { anchor, .. } if !anchor.is_empty() => {
                out.anchors.push((anchor.clone(), out.items.len()));
            }
            _ => {}
        }
        collect_block_images(block, &mut out.images);
        let is_code = matches!(block, Block::Code { .. });
        out.items.push(Item {
            block: block_ix,
            part: Part::Whole,
            span: block.span(),
            first_image,
            code_ordinal: is_code.then_some(code_ordinal),
        });
        if is_code {
            code_ordinal += 1;
        }
    }
    out
}

/// Fence line + code lines: line `i` of the code is source line
/// `start_line + 1 + i` when the block is fenced. Indented blocks have no
/// fence line; the span then has exactly `n` lines.
fn code_chunk_span(span: SourceSpan, n: usize, range: std::ops::Range<usize>) -> SourceSpan {
    let block_lines = (span.end_line - span.start_line + 1) as usize;
    let first_code_line = if block_lines > n {
        span.start_line + 1
    } else {
        span.start_line
    };
    let start = if range.start == 0 {
        span.start_line
    } else {
        first_code_line + range.start as u32
    };
    let end = if range.end == n {
        span.end_line
    } else {
        first_code_line + range.end as u32 - 1
    };
    SourceSpan {
        start_line: start,
        end_line: end.max(start),
    }
}

/// Table rows are one source line each after the header and delimiter lines.
fn table_chunk_span(span: SourceSpan, n: usize, range: std::ops::Range<usize>) -> SourceSpan {
    let first_body_line = span.start_line + 2;
    let start = if range.start == 0 {
        span.start_line
    } else {
        first_body_line + range.start as u32
    };
    let end = if range.end == n {
        span.end_line
    } else {
        first_body_line + range.end as u32 - 1
    };
    SourceSpan {
        start_line: start.min(span.end_line),
        end_line: end.clamp(start.min(span.end_line), span.end_line),
    }
}

/// Collect images of a block in render order.
pub(crate) fn collect_block_images(block: &Block, out: &mut Vec<(Option<String>, String)>) {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph { inlines, .. } => {
            collect_inline_images(inlines, out)
        }
        Block::Quote { children, .. } => children.iter().for_each(|b| collect_block_images(b, out)),
        Block::List { items, .. } => items.iter().for_each(|i| collect_item_images(i, out)),
        Block::Table { header, rows, .. } => {
            header.iter().for_each(|c| collect_inline_images(c, out));
            rows.iter()
                .flatten()
                .for_each(|c| collect_inline_images(c, out));
        }
        Block::Code { .. }
        | Block::Math { .. }
        | Block::Rule { .. }
        | Block::Embed { .. }
        | Block::Html { .. } => {}
    }
}

fn collect_item_images(item: &ListItem, out: &mut Vec<(Option<String>, String)>) {
    item.children
        .iter()
        .for_each(|b| collect_block_images(b, out));
}

/// Collect images of an inline run in render order.
pub(crate) fn collect_inline_images(inlines: &[Inline], out: &mut Vec<(Option<String>, String)>) {
    for inline in inlines {
        match inline {
            Inline::Image { src, alt, .. } => {
                out.push((src.as_ref().map(|s| s.0.clone()), alt.clone()))
            }
            Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) => {
                collect_inline_images(c, out)
            }
            Inline::Link { children, .. } => collect_inline_images(children, out),
            _ => {}
        }
    }
}

/// Index of the row to show for source `line`: the last row starting at or
/// before it (rows are in source order), or row 0.
pub(crate) fn item_for_line(items: &[Item], line: u32) -> usize {
    items
        .partition_point(|item| item.span.start_line <= line)
        .saturating_sub(1)
}

/// Fractional position of `line`, `within` (`[0, 1)`) of the way into it, inside a
/// row's span, in `[0, 1)`.
pub(crate) fn fraction_in_item(item: &Item, line: u32, within: f32) -> f32 {
    let len = (item.span.end_line - item.span.start_line + 1) as f32;
    let into = line.saturating_sub(item.span.start_line) as f32 + within.clamp(0.0, 1.0);
    (into / len).clamp(0.0, 0.999)
}

/// Source line at `fraction` of the way through a row, and how far into that
/// line (`[0, 1)`) the position falls.
pub(crate) fn line_in_item(item: &Item, fraction: f32) -> (u32, f32) {
    let len = item.span.end_line - item.span.start_line + 1;
    let pos = fraction.clamp(0.0, 1.0) * len as f32;
    let into = (pos.floor() as u32).min(len - 1);
    (
        item.span.start_line + into,
        (pos - into as f32).clamp(0.0, 0.999),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankmd_markdown::{RenderOptions, render};

    #[test]
    fn rows_follow_blocks_with_true_lines() {
        let doc = render(
            "---\ntitle: x\n---\n\n# Head\n\npara\n\n- [ ] a\n- [x] b\n",
            &RenderOptions::default(),
        );
        let l = layout(&doc);
        let spans: Vec<_> = l
            .items
            .iter()
            .map(|i| (i.span.start_line, i.span.end_line))
            .collect();
        assert_eq!(spans[1..], [(5, 5), (7, 7), (9, 10)]);
        assert_eq!(l.items[0].part, Part::FrontMatter);
        assert_eq!(item_for_line(&l.items, 1), 0);
        assert_eq!(item_for_line(&l.items, 8), 2);
        assert_eq!(item_for_line(&l.items, 10), 3);
        assert_eq!(l.anchors.len(), 1);
    }

    #[test]
    fn long_code_is_chunked_with_contiguous_spans() {
        let body: String = (0..300).map(|i| format!("let x{i} = {i};\n")).collect();
        let src = format!("intro\n\n```rust\n{body}```\n");
        let doc = render(&src, &RenderOptions::default());
        let l = layout(&doc);
        assert_eq!(l.items.len(), 4); // intro + 3 chunks
        let chunks = &l.items[1..];
        assert_eq!(chunks[0].span.start_line, 3);
        assert_eq!(chunks[2].span.end_line, 304);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].span.end_line + 1, pair[1].span.start_line);
        }
        assert!(chunks.iter().all(|c| c.code_ordinal == Some(0)));
    }

    #[test]
    fn line_fraction_round_trip() {
        let item = Item {
            block: 0,
            part: Part::Whole,
            span: SourceSpan {
                start_line: 10,
                end_line: 19,
            },
            first_image: 0,
            code_ordinal: None,
        };
        assert_eq!(
            line_in_item(&item, fraction_in_item(&item, 15, 0.0)),
            (15, 0.0)
        );
        let (line, within) = line_in_item(&item, fraction_in_item(&item, 15, 0.5));
        assert_eq!(line, 15);
        assert!((within - 0.5).abs() < 1e-3, "{within}");
        assert_eq!(line_in_item(&item, 1.0).0, 19);
    }

    #[test]
    fn images_are_numbered_in_document_order() {
        let doc = render(
            "![a](a.png)\n\ntext ![b](b.png) and [![c](c.png)](x.md)\n",
            &RenderOptions::default(),
        );
        let l = layout(&doc);
        let alts: Vec<_> = l.images.iter().map(|(_, alt)| alt.as_str()).collect();
        assert_eq!(alts, ["a", "b", "c"]);
        assert_eq!(l.items[1].first_image, 1);
    }
}
