//! Markdown blocks exactly as the agent splits them (`memex-agent`
//! `markdown_blocks::split_blocks`): blank lines separate blocks, a fenced code
//! block is one block even with blank lines inside and ends at its closing fence,
//! and an unclosed fence runs to the end. `block_index` in AI insights and the
//! 对照 pairing both count these blocks, so the reader must split the same way.

/// One block: `markdown[start..end]`, no surrounding blank lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBlock {
    pub start: usize,
    pub end: usize,
    /// Never sent to a model (code, table, image or URL lines).
    pub passthrough: bool,
    /// A fenced code block.
    pub fenced: bool,
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
    (line.starts_with("![") || line.starts_with("[![")) && line.ends_with(')') && line.contains("](")
}

fn is_url_line(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')).unwrap_or(line);
    (line.starts_with("https://") || line.starts_with("http://")) && !line.chars().any(char::is_whitespace)
}

fn is_table_separator(line: &str) -> bool {
    let line = line.trim();
    line.contains('-') && line.contains('|') && line.bytes().all(|byte| matches!(byte, b'|' | b'-' | b':' | b' ' | b'\t'))
}

fn passthrough(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.is_empty() {
        return true;
    }
    let table = lines.iter().all(|line| line.trim_start().starts_with('|')) || (lines.len() >= 2 && is_table_separator(lines[1]));
    table || lines.iter().all(|line| is_image_line(line) || is_url_line(line))
}

/// Splits Markdown into blocks. Blank lines between blocks belong to none.
pub fn split_blocks(markdown: &str) -> Vec<SourceBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let mut fence: Option<(usize, u8, usize)> = None;
    let mut offset = 0;
    let flush = |current: &mut Option<(usize, usize)>, blocks: &mut Vec<SourceBlock>| {
        if let Some((start, end)) = current.take() {
            blocks.push(SourceBlock { start, end, passthrough: passthrough(&markdown[start..end]), fenced: false });
        }
    };
    for raw in markdown.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line_end = start + line.len();
        let line_end = if line.ends_with('\r') { line_end - 1 } else { line_end };
        let content = &markdown[start..line_end];
        if let Some((fence_start, marker, count)) = fence {
            if closes_fence(content, marker, count) {
                blocks.push(SourceBlock { start: fence_start, end: line_end, passthrough: true, fenced: true });
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
        let end = fence_start + markdown[fence_start..].trim_end_matches(['\n', '\r']).len();
        blocks.push(SourceBlock { start: fence_start, end, passthrough: true, fenced: true });
    }
    flush(&mut current, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent's own sample (`markdown_blocks.rs` tests), with its expectations.
    const MIXED: &str = "# A synthetic title\n\nFirst paragraph with two\nlines of prose.\n\n\n```rust\nfn main() {\n\n    println!(\"x\");\n}\n```\nText right after a fence.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![diagram](https://example.invalid/a.png)\n\nhttps://example.invalid/reference\n\n- a list item\n- another item\n\n~~~\nunclosed fence\n";

    #[test]
    fn the_agents_sample_splits_the_same_way() {
        let blocks = split_blocks(MIXED);
        let kinds: Vec<bool> = blocks.iter().map(|b| b.passthrough).collect();
        assert_eq!(kinds, [false, false, true, false, true, true, true, false, true]);
        let text = |i: usize| &MIXED[blocks[i].start..blocks[i].end];
        assert!(text(2).starts_with("```rust") && text(2).ends_with("```") && text(2).contains("\n\n    println!"));
        assert_eq!(text(3), "Text right after a fence.");
        assert_eq!(text(8), "~~~\nunclosed fence");
        assert!(blocks[2].fenced && blocks[8].fenced && !blocks[4].fenced);
        // A heading followed directly by text is one block, as on the agent.
        let md = "## 二、小节\n正文紧跟标题\r\n\r\n下一段";
        let b = split_blocks(md);
        assert_eq!(b.len(), 2);
        assert_eq!(&md[b[0].start..b[0].end], "## 二、小节\n正文紧跟标题");
        assert_eq!(&md[b[1].start..b[1].end], "下一段");
    }
}
