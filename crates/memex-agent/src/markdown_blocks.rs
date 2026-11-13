//! Paragraph-aligned Markdown blocks for the bilingual reader.
//!
//! A document is split on blank lines into blocks. Fenced code blocks, tables, image lines and
//! pure-URL lines are passthrough blocks: they are never sent to a model and are reassembled
//! byte-identical. Output always has the same block count and order as the source, separated
//! by exactly one blank line.

/// Most source bytes one translation request carries. A document whose translatable text fits
/// is translated in ONE request; a longer one in as few consecutive segments of at most this
/// size as block boundaries allow (a 70 KiB transcript → 6 segments). A 24 KiB segment's
/// streamed Chinese output took about 3 minutes per request (observed), with no visible
/// progress in between; 12 KiB halves that and doubles the progress steps.
pub(crate) const SEGMENT_BYTES: usize = 12 * 1024;
/// The earlier segment size: a document whose segments were stored under it keeps it, so a
/// resumed job never re-sends finished work.
pub(crate) const LEGACY_SEGMENT_BYTES: usize = 24 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block<'a> {
    pub text: &'a str,
    pub passthrough: bool,
}

fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = *rest.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let count = rest.bytes().take_while(|byte| *byte == marker).count();
    (count >= 3).then_some((marker, count))
}

fn closes_fence(line: &str, marker: u8, count: usize) -> bool {
    fence_marker(line).is_some_and(|(found, length)| {
        let indent = line.len() - line.trim_start_matches(' ').len();
        found == marker && length >= count && line[indent + length..].trim().is_empty()
    })
}

fn is_image_line(line: &str) -> bool {
    let line = line.trim();
    (line.starts_with("![") || line.starts_with("[!["))
        && line.ends_with(')')
        && line.contains("](")
}

fn is_url_line(line: &str) -> bool {
    let line = line.trim();
    let line = line
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(line);
    (line.starts_with("https://") || line.starts_with("http://"))
        && !line.chars().any(char::is_whitespace)
}

fn is_table_separator(line: &str) -> bool {
    let line = line.trim();
    line.contains('-')
        && line.contains('|')
        && line
            .bytes()
            .all(|byte| matches!(byte, b'|' | b'-' | b':' | b' ' | b'\t'))
}

fn passthrough(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return true;
    }
    let table = lines.iter().all(|line| line.trim_start().starts_with('|'))
        || (lines.len() >= 2 && is_table_separator(lines[1]));
    table
        || lines
            .iter()
            .all(|line| is_image_line(line) || is_url_line(line))
}

/// Splits Markdown into blocks. Text between blocks (blank lines) is not part of any block.
pub(crate) fn split_blocks<'a>(markdown: &'a str) -> Vec<Block<'a>> {
    let mut blocks = Vec::new();
    // (start byte, end byte) of the current paragraph-like block.
    let mut current: Option<(usize, usize)> = None;
    let mut fence: Option<(usize, u8, usize)> = None;
    let mut offset = 0;
    let flush = |current: &mut Option<(usize, usize)>, blocks: &mut Vec<Block<'a>>| {
        if let Some((start, end)) = current.take() {
            let text = &markdown[start..end];
            blocks.push(Block {
                text,
                passthrough: passthrough(text),
            });
        }
    };
    for raw in markdown.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line_end = start + line.len();
        let line_end = if line.ends_with('\r') {
            line_end - 1
        } else {
            line_end
        };
        let content = &markdown[start..line_end];
        if let Some((fence_start, marker, count)) = fence {
            if closes_fence(content, marker, count) {
                blocks.push(Block {
                    text: &markdown[fence_start..line_end],
                    passthrough: true,
                });
                fence = None;
            }
            continue;
        }
        if let Some((marker, count)) = fence_marker(content) {
            flush(&mut current, &mut blocks);
            fence = Some((start, marker, count));
            continue;
        }
        if content.trim().is_empty() {
            flush(&mut current, &mut blocks);
            continue;
        }
        match &mut current {
            Some((_, end)) => *end = line_end,
            None => current = Some((start, line_end)),
        }
    }
    if let Some((fence_start, _, _)) = fence {
        // An unclosed fence runs to the end of the document, as in CommonMark.
        let text = markdown[fence_start..].trim_end_matches(['\n', '\r']);
        blocks.push(Block {
            text,
            passthrough: true,
        });
    }
    flush(&mut current, &mut blocks);
    blocks
}

/// Groups translatable block indices into consecutive segments of at most `limit` bytes
/// (a single larger block forms its own segment). Passthrough blocks are never included.
pub(crate) fn segments(blocks: &[Block<'_>], limit: usize) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut size = 0;
    for (index, block) in blocks.iter().enumerate() {
        if block.passthrough {
            continue;
        }
        let fits = batches
            .last()
            .is_some_and(|batch| !batch.is_empty() && size + block.text.len() <= limit);
        if fits {
            size += block.text.len();
            batches.last_mut().expect("batch").push(index);
        } else {
            size = block.text.len();
            batches.push(vec![index]);
        }
    }
    batches
}

/// One translated element as a single block: no blank lines inside, no outer whitespace.
pub(crate) fn normalize_translated_block(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.trim().is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Reassembles with identical block count and order; `translated[i]` replaces block `i` when
/// present, passthrough blocks are copied byte-identical.
pub(crate) fn reassemble(blocks: &[Block<'_>], translated: &[Option<String>]) -> String {
    blocks
        .iter()
        .zip(translated)
        .map(|(block, translated)| match translated {
            Some(text) if !block.passthrough => text.as_str(),
            _ => block.text,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "# A synthetic title\n\nFirst paragraph with two\nlines of prose.\n\n\n```rust\nfn main() {\n\n    println!(\"x\");\n}\n```\nText right after a fence.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![diagram](https://example.invalid/a.png)\n\nhttps://example.invalid/reference\n\n- a list item\n- another item\n\n~~~\nunclosed fence\n";

    #[test]
    fn mixed_document_keeps_block_count_order_and_passthrough_bytes() {
        let blocks = split_blocks(MIXED);
        let kinds: Vec<bool> = blocks.iter().map(|block| block.passthrough).collect();
        assert_eq!(
            kinds,
            [false, false, true, false, true, true, true, false, true]
        );
        assert!(blocks[2].text.starts_with("```rust") && blocks[2].text.ends_with("```"));
        assert!(blocks[2].text.contains("\n\n    println!"));
        let translated: Vec<Option<String>> = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                (!block.passthrough)
                    .then(|| normalize_translated_block(&format!("译文 {index}\n\n第二行")))
                    .flatten()
            })
            .collect();
        let output = reassemble(&blocks, &translated);
        let output_blocks = split_blocks(&output);
        assert_eq!(output_blocks.len(), blocks.len());
        for (source, result) in blocks.iter().zip(&output_blocks) {
            assert_eq!(source.passthrough, result.passthrough);
            if source.passthrough {
                assert_eq!(source.text.as_bytes(), result.text.as_bytes());
            } else {
                assert!(result.text.starts_with("译文 "));
            }
        }
    }

    #[test]
    fn segments_are_few_bounded_consecutive_and_skip_passthrough() {
        let paragraph = "word ".repeat(1200); // 6,000 bytes
        let document = [paragraph.as_str(); 5].join("\n\n") + "\n\n```\ncode\n```\n\n" + &paragraph;
        let blocks = split_blocks(&document);
        let segments = segments(&blocks, SEGMENT_BYTES);
        assert_eq!(segments, [vec![0, 1], vec![2, 3], vec![4, 6]]);
        for segment in &segments {
            let size: usize = segment.iter().map(|index| blocks[*index].text.len()).sum();
            assert!(size <= SEGMENT_BYTES);
        }
        // The legacy 24 KiB bound groups the same blocks in fewer segments.
        assert_eq!(super::segments(&blocks, LEGACY_SEGMENT_BYTES), [vec![0, 1, 2, 3], vec![4, 6]]);
        // A short document is one segment.
        let short = split_blocks("One.\n\n```\ncode\n```\n\nTwo.");
        assert_eq!(super::segments(&short, SEGMENT_BYTES), [vec![0, 2]]);
    }
}
