//! 竖排: a block's shown text laid out as 古籍 columns. Characters run top to
//! bottom, columns right to left; every paragraph and every heading starts a new
//! column, a heading is followed by one empty column. CJK punctuation takes its
//! vertical presentation form; Latin letters and digits stay upright, one per
//! cell. Closing punctuation never starts a column: it hangs below the last cell
//! of the previous one. Display only — the text and its offsets are untouched.

use memex_client::opencc::{self, Script};
use std::ops::Range;

/// What a column is drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    Text,
    Heading,
    /// Spacing after a heading (nothing drawn but the column rule).
    Gap,
}

/// Style of a stretch of one column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mark {
    pub bold: bool,
    pub red: bool,
}

/// One column: its glyphs joined by `\n` (one line per cell) and its marked runs
/// (byte ranges of `text`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub text: String,
    pub runs: Vec<(Range<usize>, Mark)>,
    pub kind: ColKind,
}

impl Column {
    pub fn cells(&self) -> usize {
        if self.text.is_empty() { 0 } else { self.text.split('\n').count() }
    }
}

/// One block to lay out: its shown text with bold and red ranges (bytes of `text`),
/// and the bytes that are its heading line (0 for an ordinary paragraph).
pub struct Source<'a> {
    pub text: &'a str,
    pub bold: &'a [Range<usize>],
    pub red: &'a [Range<usize>],
    pub heading_len: usize,
}

/// Closing punctuation that may not start a column (hangs at the previous one's foot).
fn closing(c: char) -> bool {
    matches!(c, '，' | '。' | '、' | '：' | '；' | '！' | '？' | '」' | '』' | '）' | '》' | '〉' | '】' | '〕' | '…' | ',' | '.' | ';' | ':' | '!' | '?' | ')' | '”' | '’')
}

struct Builder {
    per: usize,
    out: Vec<Column>,
    cur: Option<(Vec<(char, Mark)>, ColKind)>,
}

impl Builder {
    fn flush(&mut self) {
        if let Some((cells, kind)) = self.cur.take() {
            if cells.is_empty() { return; }
            let mut text = String::new();
            let mut runs: Vec<(Range<usize>, Mark)> = vec![];
            for (n, (c, m)) in cells.into_iter().enumerate() {
                if n > 0 { text.push('\n'); }
                let start = text.len();
                text.push(c);
                if m == Mark::default() { continue; }
                match runs.last_mut() {
                    // A run continues over the newline between two marked cells.
                    Some((r, last)) if *last == m && r.end + 1 == start => r.end = text.len(),
                    _ => runs.push((start..text.len(), m)),
                }
            }
            self.out.push(Column { text, runs, kind });
        }
    }

    fn push(&mut self, c: char, m: Mark, kind: ColKind) {
        let full = self.cur.as_ref().is_some_and(|(cells, _)| cells.len() >= self.per);
        if full && !closing(c) { self.flush(); }
        let (cells, _) = self.cur.get_or_insert_with(|| (vec![], kind));
        // Spaces never open a column; a run of them is one cell.
        if c.is_whitespace() && (cells.is_empty() || cells.last().is_some_and(|(p, _)| *p == '\u{3000}')) { return; }
        let glyph = if c.is_whitespace() { '\u{3000}' } else { opencc::vertical_form(c) };
        cells.push((glyph, m));
    }

    fn line(&mut self, text: &str, offset: usize, bold: &[Range<usize>], red: &[Range<usize>], kind: ColKind) {
        let inside = |rs: &[Range<usize>], i: usize| rs.iter().any(|r| r.start <= i && i < r.end);
        for (i, c) in text.char_indices() {
            let at = offset + i;
            match c {
                '\r' => {}
                '\n' => self.flush(),
                _ => self.push(c, Mark { bold: kind == ColKind::Heading || inside(bold, at), red: inside(red, at) }, kind),
            }
        }
        // A column ending in a space would show an empty cell at its foot.
        if let Some((cells, _)) = self.cur.as_mut() {
            while cells.last().is_some_and(|(c, _)| *c == '\u{3000}') { cells.pop(); }
        }
        self.flush();
    }
}

/// The columns of `blocks` in reading order (the first is the rightmost), at most
/// `per_column` cells each (plus one hanging closing mark), in `script`.
pub fn columns(blocks: &[Source], per_column: usize, script: Script) -> Vec<Column> {
    let mut b = Builder { per: per_column.max(1), out: vec![], cur: None };
    for s in blocks {
        let mut ranges: Vec<Range<usize>> = s.bold.to_vec();
        ranges.extend(s.red.iter().cloned());
        let cut = s.heading_len.min(s.text.len());
        ranges.push(cut..cut);
        let (text, mapped) = opencc::convert_with_ranges(s.text, script, &ranges);
        let (bold, rest) = mapped.split_at(s.bold.len());
        let (red, cut) = rest.split_at(s.red.len());
        let cut = cut[0].start;
        if cut > 0 {
            b.line(text[..cut].trim_end(), 0, bold, red, ColKind::Heading);
            b.out.push(Column { text: String::new(), runs: vec![], kind: ColKind::Gap });
        }
        let body = &text[cut..];
        let lead = body.len() - body.trim_start().len();
        if lead < body.len() {
            b.line(&body[lead..], cut + lead, bold, red, ColKind::Text);
        }
    }
    b.out
}

/// Columns on screen for a strip of `width` scrolled `scroll` pixels leftwards
/// from the right edge (`pad` inside each edge, `col` per column).
pub fn visible(count: usize, width: f32, scroll: f32, pad: f32, col: f32) -> Range<usize> {
    if count == 0 || col <= 0. { return 0..0; }
    let from = ((scroll - pad) / col).floor().max(0.) as usize;
    let to = (((width - pad + scroll) / col).ceil().max(0.) as usize + 1).min(count);
    from.min(to)..to
}

/// How far the strip can scroll: everything left of the viewport.
pub fn max_scroll(count: usize, width: f32, pad: f32, col: f32) -> f32 {
    (count as f32 * col + 2. * pad - width).max(0.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(text: &str) -> Source<'_> {
        Source { text, bold: &[], red: &[], heading_len: 0 }
    }

    fn glyphs(c: &Column) -> String {
        c.text.split('\n').collect()
    }

    #[test]
    fn columns_run_top_to_bottom_with_vertical_punctuation() {
        let cols = columns(&[src("道可道，非常道。名可名，非常名。")], 6, Script::Original);
        let texts: Vec<String> = cols.iter().map(glyphs).collect();
        // 6 cells, a closing mark hangs below the sixth instead of opening a column.
        assert_eq!(texts, vec!["道可道︐非常", "道︒名可名︐", "非常名︒"]);
        assert_eq!(cols[0].cells(), 6);
        assert!(cols.iter().all(|c| c.kind == ColKind::Text));
        // Every cell is its own line.
        assert_eq!(cols[2].text, "非\n常\n名\n︒");
    }

    #[test]
    fn hanging_punctuation_and_spaces() {
        let cols = columns(&[src("一二三。」四  五 ")], 3, Script::Original);
        assert_eq!(cols.iter().map(glyphs).collect::<Vec<_>>(), vec!["一二三︒﹂", "四\u{3000}五"]);
        assert!(columns(&[src("   ")], 3, Script::Original).is_empty());
    }

    #[test]
    fn headings_take_their_own_columns_then_a_gap_and_paragraphs_restart() {
        let h = "第一章 道可道";
        let text = format!("{h}\n道可道。");
        let cols = columns(&[Source { text: &text, bold: &[], red: &[], heading_len: h.len() }, src("無名天地之始；\n有名萬物之母。")], 20, Script::Original);
        let kinds: Vec<ColKind> = cols.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![ColKind::Heading, ColKind::Gap, ColKind::Text, ColKind::Text, ColKind::Text]);
        assert_eq!(glyphs(&cols[0]), "第一章\u{3000}道可道");
        // The heading is bold all through.
        assert_eq!(cols[0].runs, vec![(0..cols[0].text.len(), Mark { bold: true, red: false })]);
        assert_eq!(glyphs(&cols[3]), "無名天地之始︔");
        assert_eq!(glyphs(&cols[4]), "有名萬物之母︒");
    }

    #[test]
    fn bold_and_red_survive_conversion_and_column_breaks() {
        let text = "常无欲以观其妙";
        let bold = [text.find('无').unwrap()..text.find('以').unwrap()];
        let red = [text.find('观').unwrap()..text.len()];
        let cols = columns(&[Source { text, bold: &bold, red: &red, heading_len: 0 }], 4, Script::Traditional);
        let all: String = cols.iter().map(glyphs).collect();
        assert_eq!(all, opencc::convert(text, Script::Traditional));
        let marked = |c: &Column, m: Mark| -> String { c.runs.iter().filter(|(_, x)| *x == m).map(|(r, _)| c.text[r.clone()].replace('\n', "")).collect() };
        let bold_m = Mark { bold: true, red: false };
        let red_m = Mark { bold: false, red: true };
        assert_eq!(marked(&cols[0], bold_m), opencc::convert("无欲", Script::Traditional));
        // The red run is split across the column break and stays red on both sides.
        assert_eq!(format!("{}{}", marked(&cols[0], red_m), marked(&cols[1], red_m)), opencc::convert("观其妙", Script::Traditional));
    }

    #[test]
    fn virtual_window_from_the_right_edge() {
        // 100 columns of 40px, a 400px strip with 20px padding.
        assert_eq!(visible(100, 400., 0., 20., 40.), 0..11);
        assert_eq!(visible(100, 400., 400., 20., 40.), 9..21);
        assert_eq!(visible(3, 400., 0., 20., 40.), 0..3);
        assert_eq!(visible(0, 400., 0., 20., 40.), 0..0);
        assert_eq!(max_scroll(100, 400., 20., 40.), 3640.);
        assert_eq!(max_scroll(3, 400., 20., 40.), 0.);
    }
}
