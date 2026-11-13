//! Inline Markdown for the reader body: `**strong**` / `__strong__` stay bold,
//! `*emphasis*` / `_emphasis_` are shown as ordinary upright text (italics hurt
//! reading here), `code` loses its backticks and is shaded, `[text](url)` shows
//! its text as a link (http/https only; other targets stay plain text), images
//! become a labelled placeholder (a link when the target is http/https). Lines
//! of a block that are list items, quotes, rules or table rows are recorded so
//! the reader can lay them out. Every raw byte maps to a position in the shown
//! text, so AI highlights quoted from the raw block land on the right characters.

use crate::model::image_placeholder;
use memex_client::insights::{HighlightItem, HighlightStyle, Side};
use std::ops::Range;

/// How one line of a block is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Text,
    /// A list item; `marker` is `1.` / `2)` for ordered items, `None` for bullets.
    Item { depth: u8, marker: Option<String> },
    Quote { depth: u8 },
    /// `---` / `***` / `___` alone on a line (shows nothing).
    Rule,
    /// A table row `| a | b |`; `cells` are ranges of the shown text.
    Row { cells: Vec<Range<usize>>, header: bool },
    /// A table's `|---|---|` line (shows nothing).
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line's shown text (without its marker or newline).
    pub range: Range<usize>,
    pub kind: LineKind,
}

/// Inline marks besides bold, kept with a reader block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks2 {
    pub code: Vec<Range<usize>>,
    /// Shown range and its http/https target.
    pub links: Vec<(Range<usize>, String)>,
    pub images: Vec<(Range<usize>, String)>,
    /// One entry per line when the block has list / quote / rule / table lines;
    /// empty for ordinary text.
    pub lines: Vec<Line>,
    /// `<font color=red>…</font>` runs (the tags themselves are not shown).
    pub red: Vec<Range<usize>>,
}

/// Shown text of one raw block, with its bold ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub bold: Vec<Range<usize>>,
    pub code: Vec<Range<usize>>,
    pub links: Vec<(Range<usize>, String)>,
    pub images: Vec<(Range<usize>, String)>,
    pub lines: Vec<Line>,
    pub red: Vec<Range<usize>>,
    /// `map[i]`: position in `text` of the raw boundary before byte `i`
    /// (`map.len() == raw.len() + 1`).
    map: Vec<usize>,
}

impl Inline {
    /// Invert parser-owned byte boundaries, never locate a duplicate quote by search.
    /// Removed delimiters use inward bias. Synthetic image-label interiors have no mapping.
    pub fn source_range(&self, raw: &str, shown: Range<usize>) -> Option<Range<usize>> {
        if shown.start >= shown.end || self.text.get(shown.clone()).is_none() { return None; }
        if self.images.iter().any(|(range, _)| range.start < shown.end && range.end > shown.start) { return None; }
        let boundaries = || raw.char_indices().map(|(i, _)| i).chain(std::iter::once(raw.len()));
        let start = boundaries().filter(|&i| self.map.get(i) == Some(&shown.start)).last()?;
        let end = boundaries().find(|&i| self.map.get(i) == Some(&shown.end))?;
        (start < end).then_some(start..end)
    }
}

/// Finds `![alt](target)` starting at `i`; returns (alt, end byte).
pub(crate) fn image_at(s: &str, i: usize) -> Option<(&str, usize)> {
    let rest = s.get(i..)?.strip_prefix("![")?;
    let close = rest.find("](")?;
    let alt = &rest[..close];
    if alt.contains('\n') {
        return None;
    }
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    Some((alt, i + 2 + close + 2 + end + 1))
}

fn alnum_before(raw: &str, i: usize) -> bool {
    raw[..i].chars().next_back().is_some_and(char::is_alphanumeric)
}

fn alnum_at(raw: &str, i: usize) -> bool {
    raw.get(i..).and_then(|s| s.chars().next()).is_some_and(char::is_alphanumeric)
}

fn space_at(raw: &str, i: usize) -> bool {
    raw.get(i..).and_then(|s| s.chars().next()).is_none_or(char::is_whitespace)
}

/// Closing delimiter `delim` for an opening one ending at `open_end`, before `to`.
fn closing(raw: &str, delim: &str, open_end: usize, to: usize) -> Option<usize> {
    if space_at(raw, open_end) {
        return None;
    }
    let single = delim.len() == 1;
    let c = delim.as_bytes()[0];
    let mut j = open_end + 1;
    while j + delim.len() <= to {
        let b = raw.as_bytes();
        if raw.is_char_boundary(j) && raw[j..].starts_with(delim) {
            let doubled = single && (b.get(j + 1) == Some(&c) || b[j - 1] == c);
            let before_space = raw[..j].chars().next_back().is_some_and(char::is_whitespace);
            let underscore_ok = c != b'_' || !alnum_at(raw, j + delim.len());
            if !doubled && !before_space && underscore_ok {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

/// Ranges collected while parsing.
#[derive(Default)]
struct Acc {
    bold: Vec<Range<usize>>,
    code: Vec<Range<usize>>,
    links: Vec<(Range<usize>, String)>,
    images: Vec<(Range<usize>, String)>,
    red: Vec<Range<usize>>,
}

/// `<font …>` opening at `i` (before `to`): its end, whether its color is red,
/// and where the matching `</font>` starts. Anything else is left as text.
fn font_at(raw: &str, i: usize, to: usize) -> Option<(usize, bool, usize)> {
    let rest = raw.get(i..to)?;
    if !rest.get(..5)?.eq_ignore_ascii_case("<font") || !rest[5..].starts_with([' ', '>']) { return None; }
    let open = rest.find('>')?;
    let attrs = rest[5..open].to_ascii_lowercase();
    if attrs.contains('<') || attrs.contains('\n') { return None; }
    let close = rest[open + 1..].to_ascii_lowercase().find("</font>")?;
    let color = attrs.split_once("color").map(|(_, c)| c.trim_start_matches([' ', '=', '"', '\'']).split(['"', '\'', ' ', ';']).next().unwrap_or("").to_string()).unwrap_or_default();
    let red = matches!(color.as_str(), "red" | "#f00" | "#ff0000" | "#e00" | "#c00" | "#cc0000" | "crimson" | "darkred");
    Some((i + open + 1, red, i + open + 1 + close))
}

fn parse_into(raw: &str, from: usize, to: usize, out: &mut String, map: &mut [usize], acc: &mut Acc) {
    let b = raw.as_bytes();
    let mut i = from;
    while i < to {
        map[i] = out.len();
        match b[i] {
            b'<' => {
                if let Some((inner, red, close)) = font_at(raw, i, to) {
                    map[i..inner].fill(out.len());
                    let start = out.len();
                    parse_into(raw, inner, close, out, map, acc);
                    if red && out.len() > start {
                        acc.red.push(start..out.len());
                    }
                    map[close..close + 7].fill(out.len());
                    i = close + 7;
                    continue;
                }
            }
            b'!' => {
                if let Some(link) = memex_client::markdown::link_at(raw, i).filter(|l| l.image && l.end <= to) {
                    let start = out.len();
                    let alt = &raw[link.text.clone()];
                    let safe = memex_client::markdown::safe_url(&link.url).map(str::to_string);
                    out.push('〔');
                    out.push_str(&if safe.is_some() { memex_client::markdown::image_label(alt) } else { image_placeholder(alt) });
                    out.push('〕');
                    map[i..link.end].fill(start);
                    if memex_client::images::local_image_target(&link.url).is_some() {
                        acc.images.push((start..out.len(), link.url.clone()));
                    }
                    if let Some(url) = safe {
                        acc.links.push((start..out.len(), url));
                    }
                    i = link.end;
                    continue;
                }
            }
            b'[' => {
                if let Some(link) = memex_client::markdown::link_at(raw, i).filter(|l| !l.image && l.end <= to) {
                    map[i] = out.len();
                    let start = out.len();
                    parse_into(raw, link.text.start, link.text.end, out, map, acc);
                    map[link.text.end..link.end].fill(out.len());
                    if let Some(url) = memex_client::markdown::safe_url(&link.url).filter(|_| out.len() > start) {
                        acc.links.push((start..out.len(), url.to_string()));
                    }
                    i = link.end;
                    continue;
                }
            }
            b'\\' if i + 1 < to && b"*_`\\[]()!<>#|-+.~".contains(&b[i + 1]) => {
                out.push(b[i + 1] as char);
                map[i + 1] = out.len() - 1;
                i += 2;
                continue;
            }
            b'`' => {
                // A code span: as many backticks close it as opened it; contents literal.
                let ticks = raw[i..to].bytes().take_while(|c| *c == b'`').count();
                if let Some(len) = raw[i + ticks..to].find(&raw[i..i + ticks]) {
                    let (inner, end) = (i + ticks..i + ticks + len, i + ticks + len + ticks);
                    map[i..inner.start].fill(out.len());
                    let start = out.len();
                    for k in inner.clone() {
                        map[k] = start + (k - inner.start);
                    }
                    out.push_str(&raw[inner.clone()]);
                    map[inner.end..end].fill(out.len());
                    if out.len() > start {
                        acc.code.push(start..out.len());
                    }
                    i = end;
                    continue;
                }
                // Unmatched backticks are text.
                out.push_str(&raw[i..i + ticks]);
                for k in 0..ticks {
                    map[i + k] = out.len() - ticks + k;
                }
                i += ticks;
                continue;
            }
            c @ (b'*' | b'_') => {
                let double = b.get(i + 1) == Some(&c) && i + 1 < to;
                let delim = if double { &raw[i..i + 2] } else { &raw[i..i + 1] };
                let boundary_ok = c != b'_' || !alnum_before(raw, i);
                if let Some(close) = boundary_ok.then(|| closing(raw, delim, i + delim.len(), to)).flatten() {
                    let n = delim.len();
                    map[i..i + n].fill(out.len());
                    let start = out.len();
                    parse_into(raw, i + n, close, out, map, acc);
                    if double && out.len() > start {
                        acc.bold.push(start..out.len());
                    }
                    map[close..close + n].fill(out.len());
                    i = close + n;
                    continue;
                }
                // A lone or unmatched delimiter is just text; a pair `**` stays together.
                if double {
                    out.push_str(delim);
                    map[i + 1] = out.len() - 1;
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        let ch = raw[i..].chars().next().unwrap_or(' ');
        let start = out.len();
        out.push(ch);
        map[i..i + ch.len_utf8()].fill(start);
        i += ch.len_utf8();
    }
}

/// The layout of one raw line and the bytes of its marker (not shown).
/// `next` is the following line (a table header is followed by `|---|`).
fn classify(line: &str, next: Option<&str>) -> (LineKind, usize) {
    let indent_bytes = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent: usize = line[..indent_bytes].chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
    let body = &line[indent_bytes..];
    let trimmed = body.trim_end();
    let is_rule = |t: &str| {
        let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
        t.len() >= 3 && ["-", "*", "_"].iter().any(|c| t.chars().all(|x| x.to_string() == *c))
    };
    let is_separator = |t: &str| {
        let t = t.trim();
        t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    };
    if is_separator(trimmed) {
        return (LineKind::Skip, line.len());
    }
    if is_rule(trimmed) {
        return (LineKind::Rule, line.len());
    }
    if trimmed.starts_with('|') && trimmed.len() > 1 && trimmed.ends_with('|') {
        return (LineKind::Row { cells: vec![], header: next.is_some_and(is_separator) }, indent_bytes);
    }
    let depth = (indent / 2).min(6) as u8;
    for m in ["- ", "* ", "+ "] {
        if body.starts_with(m) {
            return (LineKind::Item { depth, marker: None }, indent_bytes + 2);
        }
    }
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) && (body[digits..].starts_with(". ") || body[digits..].starts_with(") ")) {
        return (LineKind::Item { depth, marker: Some(body[..digits + 1].to_string()) }, indent_bytes + digits + 2);
    }
    if body.starts_with('>') {
        let mut k = 0;
        let mut q = 0u8;
        let b = body.as_bytes();
        while k < b.len() && (b[k] == b'>' || (b[k] == b' ' && q > 0 && b.get(k + 1) == Some(&b'>'))) {
            q += (b[k] == b'>') as u8;
            k += 1;
        }
        if b.get(k) == Some(&b' ') {
            k += 1;
        }
        return (LineKind::Quote { depth: q }, indent_bytes + k);
    }
    (LineKind::Text, indent_bytes)
}

impl Inline {
    /// Parses `raw`; the first `skip` bytes (a heading's `## `) are not shown.
    /// A heading line is always text, whatever it starts with.
    pub fn parse(raw: &str, skip: usize) -> Self {
        let mut out = String::with_capacity(raw.len());
        let mut map = vec![0; raw.len() + 1];
        let mut acc = Acc::default();
        let skip = skip.min(raw.len());
        let raw_lines: Vec<(usize, &str)> = {
            let mut at = skip;
            raw[skip..].split_inclusive('\n').map(|l| { let s = at; at += l.len(); (s, l) }).collect()
        };
        fn content(l: &str) -> &str {
            let c = l.strip_suffix('\n').unwrap_or(l);
            c.strip_suffix('\r').unwrap_or(c)
        }
        let kinds: Vec<(LineKind, usize)> = raw_lines
            .iter()
            .enumerate()
            .map(|(n, (_, l))| if n == 0 && skip > 0 { (LineKind::Text, 0) } else { classify(content(l), raw_lines.get(n + 1).map(|(_, l)| content(l))) })
            .collect();
        let mut lines = vec![];
        if kinds.iter().all(|(k, _)| *k == LineKind::Text) {
            parse_into(raw, skip, raw.len(), &mut out, &mut map, &mut acc);
        } else {
            for ((start, l), (kind, prefix)) in raw_lines.iter().zip(kinds) {
                let end = start + content(l).len();
                let prefix = prefix.min(end - start);
                map[*start..start + prefix].fill(out.len());
                let shown = out.len();
                let kind = match kind {
                    LineKind::Row { header, .. } => {
                        // Cells between unescaped pipes; each pipe maps to the gap between cells.
                        let mut cells = vec![];
                        let row = &raw[start + prefix..end];
                        let mut bounds = vec![];
                        let rb = row.as_bytes();
                        for (k, c) in rb.iter().enumerate() {
                            if *c == b'|' && (k == 0 || rb[k - 1] != b'\\') {
                                bounds.push(start + prefix + k);
                            }
                        }
                        let last = *bounds.last().unwrap_or(&end);
                        for w in bounds.windows(2) {
                            let (a, z) = (w[0], w[1]);
                            map[a] = out.len();
                            if !cells.is_empty() {
                                out.push(' ');
                            }
                            let lead = raw[a + 1..z].len() - raw[a + 1..z].trim_start().len();
                            let trail = raw[a + 1..z].len() - raw[a + 1..z].trim_end().len();
                            map[a + 1..a + 1 + lead].fill(out.len());
                            let cell = out.len();
                            parse_into(raw, a + 1 + lead, z - trail, &mut out, &mut map, &mut acc);
                            map[z - trail..z].fill(out.len());
                            cells.push(cell..out.len());
                        }
                        map[last..end].fill(out.len());
                        if bounds.first().is_some_and(|f| *f > start + prefix) {
                            map[start + prefix..bounds[0]].fill(shown);
                        }
                        LineKind::Row { cells, header }
                    }
                    other => {
                        parse_into(raw, start + prefix, end, &mut out, &mut map, &mut acc);
                        other
                    }
                };
                lines.push(Line { range: shown..out.len(), kind });
                if end < start + l.len() {
                    // `\r\n` / `\n`: one shown newline.
                    map[end..start + l.len()].fill(out.len());
                    out.push('\n');
                }
            }
        }
        map[raw.len()] = out.len();
        acc.bold.sort_by_key(|r| r.start);
        acc.code.sort_by_key(|r| r.start);
        acc.links.sort_by_key(|(r, _)| r.start);
        acc.red.sort_by_key(|r| r.start);
        Self { text: out, bold: acc.bold, code: acc.code, links: acc.links, images: acc.images, lines, red: acc.red, map }
    }

    /// The marks a reader block keeps besides bold.
    pub fn marks(&self) -> Marks2 {
        Marks2 { code: self.code.clone(), links: self.links.clone(), images: self.images.clone(), lines: self.lines.clone(), red: self.red.clone() }
    }

    /// The shown range of raw bytes `range`, if anything of it is shown.
    pub fn shown(&self, range: Range<usize>) -> Option<Range<usize>> {
        let (s, e) = (*self.map.get(range.start)?, *self.map.get(range.end)?);
        (s < e).then_some(s..e)
    }
}

/// One AI highlight in shown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: HighlightStyle,
    pub label: Option<String>,
}

/// Places `items` (all for block `block` of `side`) on its shown text.
/// A v2 item is drawn only from its span on this side: the span must name this
/// block, lie on character boundaries and hold exactly its `text` (UTF-8 byte
/// offsets into the raw block). A v1 item (no spans) is its quote's first
/// occurrence in the original. Spans overlapping one already placed, and any
/// that do not match, are skipped, never guessed. Returns the spans in order
/// and how many could not be placed (an item with no span on this side is not
/// counted: it simply belongs to the other side).
pub fn highlight_spans(raw: &str, inline: &Inline, items: &[&HighlightItem], side: Side, block: usize) -> (Vec<Span>, usize) {
    let mut spans: Vec<Span> = vec![];
    let mut skipped = 0;
    let free = |spans: &[Span], r: &Range<usize>| spans.iter().all(|s| r.end <= s.range.start || s.range.end <= r.start);
    for item in items {
        let placed = if item.has_spans() {
            let Some(span) = item.span(side) else { continue };
            (span.block == block && span.start < span.end && raw.get(span.start..span.end) == Some(span.text.as_str()))
                .then(|| inline.shown(span.start..span.end))
                .flatten()
                .filter(|r| free(&spans, r))
        } else if side == Side::Original && !item.quote.is_empty() {
            raw.match_indices(item.quote.as_str()).find_map(|(at, q)| inline.shown(at..at + q.len()).filter(|r| free(&spans, r)))
        } else if side == Side::Original {
            None
        } else {
            continue;
        };
        match placed {
            Some(range) => spans.push(Span { range, style: item.style.clone(), label: item.label.clone() }),
            None => skipped += 1,
        }
    }
    spans.sort_by_key(|s| s.range.start);
    (spans, skipped)
}

/// Markdown with emphasis made upright for a Markdown renderer (the AI 纪要
/// card): per line, list, quote and heading markers stay, `*x*` / `_x_` become
/// plain text, strong stays `**strong**`, code and links are written back.
/// Table and rule lines are kept as written. Run `safe_markdown` afterwards.
pub fn upright_markdown(md: &str) -> String {
    let mut fenced = false;
    md.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
                fenced = !fenced;
                return line.to_string();
            }
            if fenced {
                return line.to_string();
            }
            let body = line.trim_start();
            let hashes = body.chars().take_while(|c| *c == '#').count();
            let cut = if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
                line.len() - body.len() + hashes + 1
            } else {
                match classify(line, None) {
                    (LineKind::Row { .. } | LineKind::Rule | LineKind::Skip, _) => return line.to_string(),
                    (_, prefix) => prefix,
                }
            };
            let inline = Inline::parse(&line[cut..], 0);
            format!("{}{}", &line[..cut], inline.to_markdown())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Inline {
    /// The shown text as Markdown again: bold, code and links kept, emphasis
    /// gone, Markdown characters in plain text escaped.
    fn to_markdown(&self) -> String {
        // (position, closing?, length, opener, closer); properly nested by construction.
        let mut events: Vec<(usize, bool, usize, String)> = vec![];
        for r in &self.bold {
            events.push((r.start, false, r.len(), "**".into()));
            events.push((r.end, true, r.len(), "**".into()));
        }
        for (r, url) in &self.links {
            events.push((r.start, false, r.len(), "[".into()));
            events.push((r.end, true, r.len(), format!("](<{url}>)")));
        }
        // Closings before openings at one position; inner closes first, outer opens first.
        events.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| if a.1 { a.2.cmp(&b.2) } else { b.2.cmp(&a.2) }));
        let mut out = String::with_capacity(self.text.len() + 8);
        let mut at = 0;
        let push_text = |out: &mut String, from: usize, to: usize| {
            let mut k = from;
            for code in self.code.iter().filter(|c| c.start < to && c.end > from) {
                escape_into(out, &self.text[k..code.start.max(k)]);
                let ticks = if self.text[code.clone()].contains('`') { "``" } else { "`" };
                out.push_str(ticks);
                out.push_str(&self.text[code.start.max(k)..code.end.min(to)]);
                out.push_str(ticks);
                k = code.end.min(to);
            }
            escape_into(out, &self.text[k..to]);
        };
        for (pos, _, _, mark) in events {
            push_text(&mut out, at, pos);
            out.push_str(&mark);
            at = pos;
        }
        push_text(&mut out, at, self.text.len());
        out
    }
}

fn escape_into(out: &mut String, s: &str) {
    let chars: Vec<char> = s.chars().collect();
    for (k, &c) in chars.iter().enumerate() {
        // `*` / `_` between spaces (`2 * 3`) cannot open or close emphasis.
        let spaced = || {
            k > 0 && chars[k - 1].is_whitespace() && chars.get(k + 1).is_some_and(|n| n.is_whitespace())
        };
        if matches!(c, '`' | '[' | ']') || (matches!(c, '*' | '_') && !spaced()) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// AI highlights placed on one side's blocks (`markdown` and `blocks` of that
/// side), by block index, and how many could not be placed (skipped, never
/// guessed; zero for a correct agent).
pub fn block_spans(markdown: &str, blocks: &[crate::model::Block], items: &[HighlightItem], side: Side) -> (std::collections::HashMap<usize, Vec<Span>>, usize) {
    let mut by_block: std::collections::BTreeMap<usize, Vec<&HighlightItem>> = Default::default();
    for item in items {
        // A v2 item without a span on this side is not drawn here.
        if item.has_spans() && item.span(side).is_none() || !item.has_spans() && side == Side::Translation {
            continue;
        }
        by_block.entry(item.block_index).or_default().push(item);
    }
    let mut out = std::collections::HashMap::new();
    let mut skipped = 0;
    for (index, items) in by_block {
        let Some(b) = blocks.get(index).filter(|b| matches!(b.kind, crate::model::BlockKind::Paragraph | crate::model::BlockKind::Heading(_))) else {
            skipped += items.len();
            continue;
        };
        let Some(raw) = markdown.get(b.start..b.end) else {
            skipped += items.len();
            continue;
        };
        let inline = Inline::parse(raw, b.skip);
        debug_assert_eq!(inline.text, b.text, "the reader shows the block it highlights");
        let (spans, missed) = highlight_spans(raw, &inline, &items, side, index);
        skipped += missed;
        out.insert(index, spans);
    }
    (out, skipped)
}

/// Which sides carry 重点标注 marks in what is shown: the original wherever it
/// is shown, the translation wherever it is shown and the agent marked it.
pub fn marked_sides(shown: crate::bilingual::Mode, translation_marked: bool) -> (bool, bool) {
    use crate::bilingual::Mode;
    (shown != Mode::Translation, shown != Mode::Original && translation_marked)
}

/// One 重点标注 mark along the article, for the scrollbar ticks and the
/// 重点导航 list (the Mac counterpart of the phone's scrubber dial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlongMark {
    /// Layout row (the list row is this plus the lead rows).
    pub row: usize,
    pub block: usize,
    pub side: Side,
    /// The marked term as shown.
    pub text: String,
    pub style: HighlightStyle,
    pub label: Option<String>,
}

/// The marks of the side(s) being read, in reading order: each row's original
/// half, then its translated half. `spans_o` / `spans_t` are what the rows
/// draw, so a tick never points at an unmarked side.
pub fn marks_along(
    rows: &[crate::bilingual::Row],
    original: &[crate::model::Block],
    translation: &[crate::model::Block],
    spans_o: &std::collections::HashMap<usize, Vec<Span>>,
    spans_t: &std::collections::HashMap<usize, Vec<Span>>,
) -> Vec<AlongMark> {
    use crate::bilingual::Row;
    let mut out = vec![];
    let mut push = |row: usize, block: usize, side: Side, blocks: &[crate::model::Block], spans: &std::collections::HashMap<usize, Vec<Span>>| {
        let Some(b) = blocks.get(block) else { return };
        for s in spans.get(&block).into_iter().flatten() {
            let Some(text) = b.text.get(s.range.clone()) else { continue };
            out.push(AlongMark { row, block, side, text: text.to_string(), style: s.style.clone(), label: s.label.clone() });
        }
    };
    for (row, r) in rows.iter().enumerate() {
        match *r {
            Row::Original(i) | Row::Shared(i) => push(row, i, Side::Original, original, spans_o),
            Row::Translation(i) => push(row, i, Side::Translation, translation, spans_t),
            Row::Pair(i) => {
                push(row, i, Side::Original, original, spans_o);
                push(row, i, Side::Translation, translation, spans_t);
            }
            // Book chapters carry no AI highlights.
            Row::Aligned(_) => {}
        }
    }
    out
}

/// Style flags of one stretch of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub code: bool,
    pub link: bool,
    pub highlight: bool,
    pub positive: bool,
    pub negative: bool,
    pub underline: bool,
}

/// Non-overlapping, ordered stretches with their combined marks (unmarked text
/// omitted), from the block's own bold ranges and the AI spans.
pub fn merge(len: usize, bold: &[Range<usize>], code: &[Range<usize>], links: &[Range<usize>], spans: &[Span]) -> Vec<(Range<usize>, Marks)> {
    let mut cuts: Vec<usize> = vec![0, len];
    for r in bold.iter().chain(code).chain(links).chain(spans.iter().map(|s| &s.range)) {
        cuts.push(r.start.min(len));
        cuts.push(r.end.min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(Range<usize>, Marks)> = vec![];
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let inside = |r: &Range<usize>| r.start <= a && b <= r.end;
        let mut m = Marks { bold: bold.iter().any(inside), code: code.iter().any(inside), link: links.iter().any(inside), ..Default::default() };
        for s in spans.iter().filter(|s| inside(&s.range)) {
            match s.style {
                HighlightStyle::Bold => m.bold = true,
                HighlightStyle::Highlight => m.highlight = true,
                HighlightStyle::Positive => m.positive = true,
                HighlightStyle::Negative => m.negative = true,
                HighlightStyle::Other(ref kind) if kind == "manual_underline" => m.underline = true,
                // Five-style manual marks (the reader's own overlay, never Markdown).
                HighlightStyle::Other(ref kind) if kind == "manual_bold" => m.bold = true,
                HighlightStyle::Other(ref kind) if kind == "manual_red" => m.negative = true,
                HighlightStyle::Other(ref kind) if kind == "manual_blue" => m.positive = true,
                HighlightStyle::Other(ref kind) if kind == "manual_yellow" => m.highlight = true,
                HighlightStyle::Other(_) => {}
            }
        }
        if m == Marks::default() {
            continue;
        }
        match out.last_mut() {
            Some((r, last)) if r.end == a && *last == m => r.end = b,
            _ => out.push((a..b, m)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(quote: &str, style: &str) -> HighlightItem {
        HighlightItem::v1(0, quote, style, None)
    }

    fn o(items: &[&HighlightItem], raw: &str, inline: &Inline) -> (Vec<Span>, usize) {
        highlight_spans(raw, inline, items, Side::Original, 0)
    }

    #[test]
    fn font_color_tags_are_hidden_and_red_runs_kept() {
        let raw = "道可道，<font color=red>**非常**道</font>。<FONT color=\"#333\">名</FONT> a<b <font>x";
        let i = Inline::parse(raw, 0);
        assert_eq!(i.text, "道可道，非常道。名 a<b <font>x");
        assert_eq!(i.red.len(), 1);
        assert_eq!(&i.text[i.red[0].clone()], "非常道");
        assert_eq!(&i.text[i.bold[0].clone()], "非常");
        // Raw offsets of the red words still map onto them (highlights, notes).
        let at = raw.find("非常").unwrap();
        assert_eq!(i.shown(at..at + "非常".len()).map(|r| &i.text[r]), Some("非常"));
        // Removed tags and delimiters bias inward, as for bold.
        assert_eq!(i.source_range(raw, i.red[0].clone()).map(|r| &raw[r]), Some("非常**道"));
    }

    #[test]
    fn emphasis_is_upright_strong_stays_bold_and_code_is_untouched() {
        let i = Inline::parse("A *soft* and **strong** _word_ in snake_case_name, `a*b*c` and 2 * 3 * 4 \\*lit\\*.", 0);
        assert_eq!(i.text, "A soft and strong word in snake_case_name, a*b*c and 2 * 3 * 4 *lit*.");
        // Code keeps its contents literal and loses its backticks.
        assert_eq!(i.code.len(), 1);
        assert_eq!(&i.text[i.code[0].clone()], "a*b*c");
        assert_eq!(i.bold.len(), 1);
        assert_eq!(&i.text[i.bold[0].clone()], "strong");
        // Nested: emphasis inside strong is upright and still bold.
        let i = Inline::parse("**粗体里有*斜体*字**", 0);
        assert_eq!(i.text, "粗体里有斜体字");
        assert_eq!(i.bold, [0..i.text.len()]);
        // Unmatched markers stay as written.
        assert_eq!(Inline::parse("**未闭合 和 *孤立", 0).text, "**未闭合 和 *孤立");
        // A heading prefix is skipped and maps to the start.
        let h = Inline::parse("## 标题 *一*", 3);
        assert_eq!(h.text, "标题 一");
    }

    #[test]
    fn spans_are_exact_non_overlapping_and_respect_multibyte_boundaries() {
        let raw = "开头一句。中间有**加粗**的关键信息，结尾一句";
        let inline = Inline::parse(raw, 0);
        let items = [item("开头一句", "bold"), item("加粗**的关键信息", "highlight"), item("结尾一句", "negative"), item("关键", "positive"), item("不存在的引文", "bold")];
        let refs: Vec<&HighlightItem> = items.iter().collect();
        let (spans, skipped) = o(&refs, raw, &inline);
        // "关键" overlaps the highlight and has no other occurrence; the missing quote is skipped too.
        assert_eq!(skipped, 2);
        let shown: Vec<&str> = spans.iter().map(|s| &inline.text[s.range.clone()]).collect();
        assert_eq!(shown, ["开头一句", "加粗的关键信息", "结尾一句"]);
        assert_eq!(spans[0].range.start, 0, "quote at block start");
        assert_eq!(spans[2].range.end, inline.text.len(), "quote at block end");
        for s in &spans {
            assert!(inline.text.is_char_boundary(s.range.start) && inline.text.is_char_boundary(s.range.end));
        }
        // A repeated quote takes the next free occurrence.
        let raw = "重复，重复";
        let inline = Inline::parse(raw, 0);
        let (a, b) = (item("重复", "bold"), item("重复", "negative"));
        let (spans, skipped) = o(&[&a, &b], raw, &inline);
        assert_eq!((spans.len(), skipped), (2, 0));
        assert!(spans[0].range.end <= spans[1].range.start);
    }

    #[test]
    fn minutes_markdown_keeps_lists_and_bold_but_no_emphasis() {
        assert_eq!(upright_markdown("## 要点\n- 一个*强调*与**加粗**\n* _下划线强调_\n普通 2 * 3"), "## 要点\n- 一个强调与**加粗**\n* 下划线强调\n普通 2 * 3");
    }

    #[test]
    fn a_link_renders_but_the_same_text_inside_code_stays_literal() {
        let i = Inline::parse("看 [x](https://example.invalid/y) 与 `[x](y)`", 0);
        assert_eq!(i.text, "看 x 与 [x](y)");
        assert_eq!(i.links.len(), 1);
        assert_eq!(&i.text[i.links[0].0.clone()], "x");
        assert_eq!(i.links[0].1, "https://example.invalid/y");
        assert_eq!(&i.text[i.code[0].clone()], "[x](y)");
        // The YouTube case the user saw printed literally.
        let i = Inline::parse("[YouTube: https://www.youtube.com/watch?v=abc](https://www.youtube.com/watch?v=abc)", 0);
        assert_eq!(i.text, "YouTube: https://www.youtube.com/watch?v=abc");
        assert_eq!(i.links, [(0..i.text.len(), "https://www.youtube.com/watch?v=abc".to_string())]);
    }

    #[test]
    fn a_javascript_link_is_plain_text_without_a_target() {
        for raw in ["点 [这里](javascript:alert(1)) 吧", "点 [这里](JavaScript:alert) 吧", "点 [这里](file:///etc/passwd) 吧"] {
            let i = Inline::parse(raw, 0);
            assert!(i.text.starts_with("点 这里"), "{raw} -> {}", i.text);
            assert!(i.links.is_empty(), "{raw}");
        }
    }

    #[test]
    fn nested_and_ordered_lists_quotes_rules_and_tables_are_laid_out() {
        let i = Inline::parse("- 一级\n  - 二级 **粗**\n    1. 三级有序\n2) 顶层有序", 0);
        let kinds: Vec<LineKind> = i.lines.iter().map(|l| l.kind.clone()).collect();
        assert_eq!(kinds, [
            LineKind::Item { depth: 0, marker: None },
            LineKind::Item { depth: 1, marker: None },
            LineKind::Item { depth: 2, marker: Some("1.".into()) },
            LineKind::Item { depth: 0, marker: Some("2)".into()) },
        ]);
        assert_eq!(i.text, "一级\n二级 粗\n三级有序\n顶层有序");
        assert_eq!(&i.text[i.bold[0].clone()], "粗");
        let q = Inline::parse("> 引用\n> > 嵌套", 0);
        assert_eq!(q.lines.iter().map(|l| l.kind.clone()).collect::<Vec<_>>(), [LineKind::Quote { depth: 1 }, LineKind::Quote { depth: 2 }]);
        assert_eq!(q.text, "引用\n嵌套");
        let t = Inline::parse("| 名 | 值 |\n|---|---|\n| [a](https://e.invalid) | `b` |\n***", 0);
        let LineKind::Row { cells, header: true } = &t.lines[0].kind else { panic!("{:?}", t.lines[0]) };
        assert_eq!(cells.iter().map(|c| &t.text[c.clone()]).collect::<Vec<_>>(), ["名", "值"]);
        assert_eq!(t.lines[1].kind, LineKind::Skip);
        let LineKind::Row { cells, header: false } = &t.lines[2].kind else { panic!() };
        assert_eq!(cells.iter().map(|c| &t.text[c.clone()]).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(t.links.len(), 1);
        assert_eq!(t.lines[3].kind, LineKind::Rule);
    }

    #[test]
    fn a_highlight_quoted_across_a_link_lands_on_the_shown_text() {
        let raw = "据 [OpenAI 报告](https://e.invalid/r) 显示，增长 40%。";
        let inline = Inline::parse(raw, 0);
        let it = item("[OpenAI 报告](https://e.invalid/r) 显示", "highlight");
        let (spans, skipped) = o(&[&it], raw, &inline);
        assert_eq!(skipped, 0);
        assert_eq!(&inline.text[spans[0].range.clone()], "OpenAI 报告 显示");
        let it = item("增长 40%", "positive");
        let (spans, _) = o(&[&it], raw, &inline);
        assert_eq!(&inline.text[spans[0].range.clone()], "增长 40%");
    }

    fn v2(block: usize, side: Side, raw: &str, term: &str, style: &str) -> HighlightItem {
        let start = raw.find(term).unwrap();
        let span = memex_client::insights::TermSpan { text: term.into(), block, start, end: start + term.len() };
        let (original, translation) = if side == Side::Original { (Some(span), None) } else { (None, Some(span)) };
        HighlightItem { block_index: block, quote: term.into(), style: style.to_string().into(), label: None, original, translation }
    }

    #[test]
    fn v2_spans_use_utf8_byte_offsets_after_cjk_and_emoji() {
        // CJK (3 bytes), an emoji (4 bytes) and bold markup before the term.
        let raw = "中文🌹 **前缀** then the key term, 末尾。";
        let inline = Inline::parse(raw, 0);
        let it = v2(3, Side::Translation, raw, "key term", "positive");
        assert_eq!(it.translation.as_ref().unwrap().start, "中文🌹 **前缀** then the ".len());
        let (spans, skipped) = highlight_spans(raw, &inline, &[&it], Side::Translation, 3);
        assert_eq!(skipped, 0);
        assert_eq!(&inline.text[spans[0].range.clone()], "key term");
        // A CJK term right after the emoji.
        let raw = "🌹表情的显示，其后。";
        let inline = Inline::parse(raw, 0);
        let it = v2(0, Side::Original, raw, "表情的显示", "highlight");
        let (spans, _) = highlight_spans(raw, &inline, &[&it], Side::Original, 0);
        assert_eq!(spans[0].range, 4..4 + "表情的显示".len());
        // Character offsets (not bytes) would point into the middle of a character: skipped.
        let mut chars = it.clone();
        let sp = chars.original.as_mut().unwrap();
        (sp.start, sp.end) = (1, 6);
        assert_eq!(highlight_spans(raw, &inline, &[&chars], Side::Original, 0), (vec![], 1));
        // The other side's span is not drawn here and not counted as missing.
        assert_eq!(highlight_spans(raw, &inline, &[&it], Side::Translation, 0), (vec![], 0));
        // Overlapping spans: the later one is skipped.
        let a = v2(0, Side::Original, raw, "表情的显示", "bold");
        let b = v2(0, Side::Original, raw, "的显示", "negative");
        let (spans, skipped) = highlight_spans(raw, &inline, &[&a, &b], Side::Original, 0);
        assert_eq!((spans.len(), skipped), (1, 1));
    }

    #[test]
    fn selection_maps_the_second_utf8_occurrence_through_markdown() {
        let raw = "## 中文🌹 **重复** / [重复](https://e.invalid) / `尾巴`";
        let inline = Inline::parse(raw, 3);
        let start = inline.text.rfind("重复").unwrap();
        let selected = inline.source_range(raw, start..start + "重复".len()).unwrap();
        assert_eq!(selected, raw.rfind("重复").unwrap()..raw.rfind("重复").unwrap() + "重复".len());
        let tail = inline.text.find("尾巴").unwrap();
        let tail_raw = raw.find("尾巴").unwrap();
        assert_eq!(inline.source_range(raw, tail..tail + "尾巴".len()), Some(tail_raw..tail_raw + "尾巴".len()));
        assert!(inline.source_range(raw, start + 1..start + 3).is_none());
        let image = format!("![图](attachments/{}.png)", "a".repeat(64));
        let inline = Inline::parse(&image, 0);
        assert!(inline.source_range(&image, 0..inline.text.len()).is_none());
    }

    #[test]
    fn merged_marks_never_overlap() {
        let bold = [0..6];
        let spans = [Span { range: 3..9, style: HighlightStyle::Highlight, label: None }, Span { range: 9..12, style: HighlightStyle::Positive, label: None }];
        let m = merge(15, &bold, &[], &[], &spans);
        assert_eq!(m.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>(), [0..3, 3..6, 6..9, 9..12]);
        assert!(m[1].1.bold && m[1].1.highlight && !m[2].1.bold && m[3].1.positive);
        let underline = [Span { range: 3..6, style: HighlightStyle::Other("manual_underline".into()), label: None }];
        let plain = merge(12, &[], &[], &[], &underline);
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].0, 3..6);
        assert!(plain[0].1.underline);
        let bold = merge(12, &[0..12], &[], &[], &underline);
        assert_eq!(bold.iter().map(|(r, m)| (r.clone(), m.underline)).collect::<Vec<_>>(), [(0..3, false), (3..6, true), (6..12, false)]);
    }

    #[test]
    fn local_inline_image_preserves_following_utf8_coordinates() {
        let target = format!("attachments/{}.png", "a".repeat(64));
        let raw = format!("前文 ![示意图]({target}) 后文 **证据🙂**");
        let inline = Inline::parse(&raw, 0);
        assert_eq!(inline.images.len(), 1);
        assert_eq!(inline.images[0].1, target);
        let start = raw.find("证据🙂").unwrap();
        let shown = inline.shown(start..start + "证据🙂".len()).unwrap();
        assert_eq!(&inline.text[shown.clone()], "证据🙂");
        assert!(shown.start >= inline.images[0].0.end);
        assert!(inline.bold.contains(&shown));
        assert!(Inline::parse("`![code](attachments/no.png)`", 0).images.is_empty());
        let linked = Inline::parse(&format!("[![演示封面]({target})](https://example.test/article#demo)"), 0);
        assert_eq!(linked.images.len(), 1);
        assert!(linked.links.iter().any(|(range, url)| range == &linked.images[0].0 && url == "https://example.test/article#demo"));
    }
}
