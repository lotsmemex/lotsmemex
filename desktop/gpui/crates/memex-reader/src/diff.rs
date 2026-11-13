//! Line and intraline differences of two complete texts, computed here without
//! any model request (the contract keeps diff rendering frontend-owned).
//!
//! Lines are compared whole, terminator included: lines unique in both texts
//! anchor the comparison (patience), and Myers' O(ND) algorithm in linear space
//! fills the gaps between anchors. Within one changed block, deleted and inserted lines are paired
//! by similarity (a monotonic best match, so a moved or duplicated line is not
//! glued to an unrelated one) and each pair gets a token diff: one CJK character,
//! one Latin word, one whitespace run or one other character per token. A work
//! budget bounds pathological inputs; past it the rest of a box is reported as a
//! whole delete + insert, which is still an exact edit script, never a guess.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Line-level work units before a remaining box is given up as delete + insert.
const LINE_BUDGET: u64 = 40_000_000;
/// Token-level work units per line pair, and for all pairs of one comparison;
/// past either, a pair is marked whole.
const TOKEN_BUDGET: u64 = 2_000_000;
const TOKENS_TOTAL: u64 = 60_000_000;
/// Blocks up to this many delete×insert candidates are paired by similarity.
const PAIR_LIMIT: usize = 4096;
/// Pairs below this similarity are shown as a whole deleted and inserted line.
const MIN_SIMILARITY: f32 = 0.3;
/// Unchanged lines kept next to a change before the rest folds away.
pub const CONTEXT: usize = 3;
/// Fewer hidden lines than this are simply shown.
const MIN_FOLD: usize = 4;

/// An edit script of `a` into `b`: every element of `a` is `Equal` or `Delete`,
/// every element of `b` is `Equal` or `Insert`, both in order.
pub fn myers<T: Eq>(a: &[T], b: &[T], budget: &mut u64) -> Vec<Edit> {
    let max_d = (a.len() + b.len()).div_ceil(2) + 1;
    let mut vf = V::new(max_d);
    let mut vb = V::new(max_d);
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    conquer(a, 0..a.len(), b, 0..b.len(), &mut vf, &mut vb, &mut out, budget);
    out
}

struct V {
    off: isize,
    v: Vec<usize>,
}

impl V {
    fn new(max_d: usize) -> Self {
        V { off: max_d as isize + 1, v: vec![0; 2 * max_d + 3] }
    }
    fn get(&self, k: isize) -> usize {
        self.v[(k + self.off) as usize]
    }
    fn set(&mut self, k: isize, x: usize) {
        self.v[(k + self.off) as usize] = x;
    }
}

#[allow(clippy::too_many_arguments)]
fn conquer<T: Eq>(a: &[T], mut ar: Range<usize>, b: &[T], mut br: Range<usize>, vf: &mut V, vb: &mut V, out: &mut Vec<Edit>, budget: &mut u64) {
    while ar.start < ar.end && br.start < br.end && a[ar.start] == b[br.start] {
        out.push(Edit::Equal(ar.start, br.start));
        ar.start += 1;
        br.start += 1;
    }
    let mut suffix = 0;
    while ar.end - suffix > ar.start && br.end - suffix > br.start && a[ar.end - suffix - 1] == b[br.end - suffix - 1] {
        suffix += 1;
    }
    ar.end -= suffix;
    br.end -= suffix;
    if ar.is_empty() {
        out.extend(br.clone().map(Edit::Insert));
    } else if br.is_empty() {
        out.extend(ar.clone().map(Edit::Delete));
    } else if let Some((x, y)) = middle_snake(a, ar.clone(), b, br.clone(), vf, vb, budget) {
        conquer(a, ar.start..x, b, br.start..y, vf, vb, out, budget);
        conquer(a, x..ar.end, b, y..br.end, vf, vb, out, budget);
    } else {
        out.extend(ar.clone().map(Edit::Delete));
        out.extend(br.clone().map(Edit::Insert));
    }
    out.extend((0..suffix).map(|s| Edit::Equal(ar.end + s, br.end + s)));
}

/// A point on an optimal path roughly halfway through the box, strictly inside
/// it (the box has no common prefix or suffix). `None` once `budget` runs out.
fn middle_snake<T: Eq>(a: &[T], ar: Range<usize>, b: &[T], br: Range<usize>, vf: &mut V, vb: &mut V, budget: &mut u64) -> Option<(usize, usize)> {
    let (n, m) = (ar.len(), br.len());
    let delta = n as isize - m as isize;
    let odd = delta & 1 == 1;
    vf.set(1, 0);
    vb.set(1, 0);
    let d_max = (n + m).div_ceil(2) + 1;
    for d in 0..d_max as isize {
        let cost = 2 * (2 * d as u64 + 1);
        if *budget < cost { return None; }
        *budget -= cost;
        for k in (-d..=d).rev().step_by(2) {
            let mut x = if k == -d || (k != d && vf.get(k - 1) < vf.get(k + 1)) { vf.get(k + 1) } else { vf.get(k - 1) + 1 };
            let y = (x as isize - k) as usize;
            let (x0, y0) = (x, y);
            let mut y = y;
            while x < n && y < m && a[ar.start + x] == b[br.start + y] {
                x += 1;
                y += 1;
            }
            vf.set(k, x);
            if odd && (k - delta).abs() <= d - 1 && vf.get(k) + vb.get(-(k - delta)) >= n {
                return Some((ar.start + x0, br.start + y0));
            }
        }
        for k in (-d..=d).rev().step_by(2) {
            let mut x = if k == -d || (k != d && vb.get(k - 1) < vb.get(k + 1)) { vb.get(k + 1) } else { vb.get(k - 1) + 1 };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[ar.start + n - x - 1] == b[br.start + m - y - 1] {
                x += 1;
                y += 1;
            }
            vb.set(k, x);
            if !odd && (k - delta).abs() <= d && vb.get(k) + vf.get(-(k - delta)) >= n {
                return Some((ar.start + n - x, br.start + m - y));
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Same,
    /// Only in the old text.
    Deleted,
    /// Only in the new text.
    Inserted,
    /// A deleted line and an inserted line side by side. `old_marks` /
    /// `new_marks` are what changed; a dissimilar pair is marked whole.
    Changed,
}

/// One side-by-side row. Line numbers are 0-based indexes into `Diff::old_lines`
/// / `new_lines`; marks are byte ranges of that line's shown content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub old_marks: Vec<Range<usize>>,
    pub new_marks: Vec<Range<usize>>,
    /// The two lines differ only in their line ending (`\r\n`, `\n` or none).
    pub eol_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub old_lines: Vec<Range<usize>>,
    pub new_lines: Vec<Range<usize>>,
    pub rows: Vec<Row>,
    /// Runs of unchanged rows that fold away, as row index ranges.
    pub folds: Vec<Range<usize>>,
    pub deleted: usize,
    pub inserted: usize,
}

impl Diff {
    pub fn identical(&self) -> bool {
        self.deleted == 0 && self.inserted == 0
    }
}

/// Byte ranges of each line, terminator included (the last may have none).
pub fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        out.push(at..at + line.len());
        at += line.len();
    }
    out
}

/// What a line shows: without its `\n` or `\r\n`.
pub fn content(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

pub fn diff(old: &str, new: &str) -> Diff {
    let old_lines = line_ranges(old);
    let new_lines = line_ranges(new);
    // Lines compare as small integers: equal text, equal id.
    let mut table: HashMap<&str, u32> = HashMap::new();
    let mut id = |s| { let n = table.len() as u32; *table.entry(s).or_insert(n) };
    let a: Vec<u32> = old_lines.iter().map(|r| id(&old[r.clone()])).collect();
    let b: Vec<u32> = new_lines.iter().map(|r| id(&new[r.clone()])).collect();
    let edits = line_edits(&a, &b);
    let mut tokens_left = TOKENS_TOTAL;
    let line = |text: &str, ranges: &[Range<usize>], i: usize| -> String { text[ranges[i].clone()].to_string() };
    let mut rows = Vec::with_capacity(edits.len());
    let mut i = 0;
    while i < edits.len() {
        if let Edit::Equal(x, y) = edits[i] {
            rows.push(Row { kind: RowKind::Same, old: Some(x), new: Some(y), old_marks: vec![], new_marks: vec![], eol_only: false });
            i += 1;
            continue;
        }
        let (mut dels, mut ins) = (Vec::new(), Vec::new());
        while i < edits.len() {
            match edits[i] {
                Edit::Delete(x) => dels.push(x),
                Edit::Insert(y) => ins.push(y),
                Edit::Equal(..) => break,
            }
            i += 1;
        }
        let olds: Vec<String> = dels.iter().map(|&x| line(old, &old_lines, x)).collect();
        let news: Vec<String> = ins.iter().map(|&y| line(new, &new_lines, y)).collect();
        block_rows(&dels, &olds, &ins, &news, &mut rows, &mut tokens_left);
    }
    let deleted = rows.iter().filter(|r| r.kind != RowKind::Same && r.old.is_some()).count();
    let inserted = rows.iter().filter(|r| r.kind != RowKind::Same && r.new.is_some()).count();
    let folds = folds(&rows);
    Diff { old_lines, new_lines, rows, folds, deleted, inserted }
}

/// Lines occurring exactly once in each text, in an order both share (the
/// longest such chain): patience anchors. Between them the texts are compared
/// with `myers`, so a repeated line (a blank line between paragraphs) never
/// pulls a unique line out of place.
fn anchors(a: &[u32], b: &[u32]) -> Vec<(usize, usize)> {
    let mut seen: HashMap<u32, (u32, u32, usize, usize)> = HashMap::new();
    for (i, &x) in a.iter().enumerate() { let e = seen.entry(x).or_default(); e.0 += 1; e.2 = i; }
    for (j, &y) in b.iter().enumerate() { let e = seen.entry(y).or_default(); e.1 += 1; e.3 = j; }
    let mut pairs: Vec<(usize, usize)> = seen.values().filter(|e| e.0 == 1 && e.1 == 1).map(|e| (e.2, e.3)).collect();
    pairs.sort_unstable();
    // Longest increasing run of new positions (patience sorting with back links).
    let mut tails: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; pairs.len()];
    for (k, &(_, j)) in pairs.iter().enumerate() {
        let at = tails.partition_point(|&t| pairs[t].1 < j);
        if at > 0 { back[k] = Some(tails[at - 1]); }
        if at == tails.len() { tails.push(k); } else { tails[at] = k; }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut k = tails.last().copied();
    while let Some(i) = k { out.push(pairs[i]); k = back[i]; }
    out.reverse();
    out
}

fn line_edits(a: &[u32], b: &[u32]) -> Vec<Edit> {
    let mut budget = LINE_BUDGET;
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let (mut i, mut j) = (0, 0);
    let ends = anchors(a, b).into_iter().map(Some).chain([None]);
    for anchor in ends {
        let (x, y) = anchor.unwrap_or((a.len(), b.len()));
        out.extend(myers(&a[i..x], &b[j..y], &mut budget).into_iter().map(|e| match e {
            Edit::Equal(p, q) => Edit::Equal(p + i, q + j),
            Edit::Delete(p) => Edit::Delete(p + i),
            Edit::Insert(q) => Edit::Insert(q + j),
        }));
        if anchor.is_some() { out.push(Edit::Equal(x, y)); }
        (i, j) = (x + 1, y + 1);
    }
    out
}

/// Rows of one changed block: matched pairs in order, unmatched lines between
/// them side by side (or alone where one side has none).
fn block_rows(dels: &[usize], olds: &[String], ins: &[usize], news: &[String], rows: &mut Vec<Row>, tokens_left: &mut u64) {
    let pairs = match_lines(olds, news);
    let (mut d, mut n) = (0, 0);
    let gap = |d_to: usize, n_to: usize, d: &mut usize, n: &mut usize, rows: &mut Vec<Row>| {
        while *d < d_to || *n < n_to {
            let (o, w) = ((*d < d_to).then_some(*d), (*n < n_to).then_some(*n));
            rows.push(match (o, w) {
                (Some(o), Some(w)) => {
                    let (eol_only, om, nm) = whole_or_eol(&olds[o], &news[w]);
                    Row { kind: RowKind::Changed, old: Some(dels[o]), new: Some(ins[w]), old_marks: om, new_marks: nm, eol_only }
                }
                (Some(o), None) => Row { kind: RowKind::Deleted, old: Some(dels[o]), new: None, old_marks: whole(&olds[o]), new_marks: vec![], eol_only: false },
                (None, Some(w)) => Row { kind: RowKind::Inserted, old: None, new: Some(ins[w]), old_marks: vec![], new_marks: whole(&news[w]), eol_only: false },
                (None, None) => unreachable!(),
            });
            if o.is_some() { *d += 1; }
            if w.is_some() { *n += 1; }
        }
    };
    for (o, w) in pairs {
        gap(o, w, &mut d, &mut n, rows);
        let (om, nm) = intraline_within(content(&olds[o]), content(&news[w]), tokens_left);
        let eol_only = om.is_empty() && nm.is_empty();
        rows.push(Row { kind: RowKind::Changed, old: Some(dels[o]), new: Some(ins[w]), old_marks: om, new_marks: nm, eol_only });
        d = o + 1;
        n = w + 1;
    }
    gap(dels.len(), ins.len(), &mut d, &mut n, rows);
}

fn whole(line: &str) -> Vec<Range<usize>> {
    let c = content(line);
    if c.is_empty() { vec![] } else { vec![0..c.len()] }
}

/// An unmatched pair: whole lines, unless only their endings differ.
fn whole_or_eol(old: &str, new: &str) -> (bool, Vec<Range<usize>>, Vec<Range<usize>>) {
    if content(old) == content(new) { (true, vec![], vec![]) } else { (false, whole(old), whole(new)) }
}

/// A monotonic matching of deleted to inserted lines maximizing total
/// similarity (pairs below `MIN_SIMILARITY` never match). Large blocks pair in
/// order where similar enough.
fn match_lines(olds: &[String], news: &[String]) -> Vec<(usize, usize)> {
    let (k, m) = (olds.len(), news.len());
    if k == 0 || m == 0 { return vec![]; }
    let grams_o: Vec<Vec<u64>> = olds.iter().map(|l| grams(content(l))).collect();
    let grams_n: Vec<Vec<u64>> = news.iter().map(|l| grams(content(l))).collect();
    let sim = |i: usize, j: usize| {
        if content(&olds[i]) == content(&news[j]) { 1.0 } else { dice(&grams_o[i], &grams_n[j]) }
    };
    if k * m > PAIR_LIMIT {
        return (0..k.min(m)).filter(|&i| sim(i, i) >= MIN_SIMILARITY).map(|i| (i, i)).collect();
    }
    // best[i][j]: the best total over olds[i..] and news[j..].
    let mut best = vec![vec![0f32; m + 1]; k + 1];
    let mut take = vec![vec![false; m + 1]; k + 1];
    for i in (0..k).rev() {
        for j in (0..m).rev() {
            let s = sim(i, j);
            let pair = if s >= MIN_SIMILARITY { s + best[i + 1][j + 1] } else { f32::MIN };
            let skip = best[i + 1][j].max(best[i][j + 1]);
            if pair > skip { best[i][j] = pair; take[i][j] = true; } else { best[i][j] = skip; }
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < k && j < m {
        if take[i][j] { out.push((i, j)); i += 1; j += 1; }
        else if best[i + 1][j] >= best[i][j + 1] { i += 1; } else { j += 1; }
    }
    out
}

/// Sorted character bigrams (single characters for a one-character line).
fn grams(s: &str) -> Vec<u64> {
    let chars: Vec<char> = s.chars().collect();
    let mut g: Vec<u64> = if chars.len() < 2 { chars.iter().map(|&c| c as u64).collect() } else { chars.windows(2).map(|w| ((w[0] as u64) << 32) | w[1] as u64).collect() };
    g.sort_unstable();
    g
}

fn dice(a: &[u64], b: &[u64]) -> f32 {
    if a.is_empty() && b.is_empty() { return 1.0; }
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => { common += 1; i += 1; j += 1; }
        }
    }
    2.0 * common as f32 / (a.len() + b.len()) as f32
}

/// Tokens: one CJK (or other 3+-byte) character, one word of letters/digits,
/// one whitespace run, or one other character.
pub fn tokens(s: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class { Word, Space, Single }
    let class = |c: char| if c.is_whitespace() { Class::Space } else if c.is_alphanumeric() && c.len_utf8() < 3 || c == '_' { Class::Word } else { Class::Single };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<Class> = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        match (out.last_mut(), last) {
            (Some(r), Some(l)) if l == k && k != Class::Single => r.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

/// What changed within a line pair, as merged byte ranges of each side.
/// Words and characters are matched first (so a shared space never outweighs a
/// shared word); whitespace between two matched neighbours is then compared on
/// its own, so a whitespace-only edit still shows.
pub fn intraline(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut unlimited = u64::MAX;
    intraline_within(old, new, &mut unlimited)
}

fn intraline_within(old: &str, new: &str, total: &mut u64) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old == new { return (vec![], vec![]); }
    let whole = |s: &str| if s.is_empty() { vec![] } else { vec![0..s.len()] };
    if *total == 0 { return (whole(old), whole(new)); }
    let significant = |s: &str| -> Vec<Range<usize>> { tokens(s).into_iter().filter(|r| !s[r.clone()].chars().all(char::is_whitespace)).collect() };
    let (ta, tb) = (significant(old), significant(new));
    let a: Vec<&str> = ta.iter().map(|r| &old[r.clone()]).collect();
    let b: Vec<&str> = tb.iter().map(|r| &new[r.clone()]).collect();
    let mut budget = TOKEN_BUDGET.min(*total);
    let edits = myers(&a, &b, &mut budget);
    *total -= TOKEN_BUDGET.min(*total) - budget;
    let (mut om, mut nm) = (Vec::new(), Vec::new());
    // Where the last matched token ended on each side (the line start at first).
    let mut anchor = Some((0, 0));
    let gap = |om: &mut Vec<Range<usize>>, nm: &mut Vec<Range<usize>>, (po, pn): (usize, usize), o: usize, n: usize| {
        if old[po..o] != new[pn..n] {
            if o > po { push(om, po..o); }
            if n > pn { push(nm, pn..n); }
        }
    };
    for e in edits {
        match e {
            Edit::Equal(i, j) => {
                if let Some(at) = anchor { gap(&mut om, &mut nm, at, ta[i].start, tb[j].start); }
                anchor = Some((ta[i].end, tb[j].end));
            }
            Edit::Delete(i) => { push(&mut om, ta[i].clone()); anchor = None; }
            Edit::Insert(j) => { push(&mut nm, tb[j].clone()); anchor = None; }
        }
    }
    if let Some(at) = anchor { gap(&mut om, &mut nm, at, old.len(), new.len()); }
    (bridge(old, om), bridge(new, nm))
}

fn push(marks: &mut Vec<Range<usize>>, r: Range<usize>) {
    match marks.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => marks.push(r),
    }
}

/// Joins two marks separated only by whitespace, so a rewritten phrase reads
/// as one change rather than a row of fragments.
fn bridge(text: &str, marks: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::with_capacity(marks.len());
    for r in marks {
        match out.last_mut() {
            Some(last) if text[last.end..r.start].chars().all(char::is_whitespace) && !text[last.end..r.start].is_empty() => last.end = r.end,
            _ => out.push(r),
        }
    }
    out
}

/// Unchanged runs to fold: all but `CONTEXT` rows next to each change.
fn folds(rows: &[Row]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].kind != RowKind::Same { i += 1; continue; }
        let start = i;
        while i < rows.len() && rows[i].kind == RowKind::Same { i += 1; }
        let lead = if start == 0 { 0 } else { CONTEXT };
        let tail = if i == rows.len() { 0 } else { CONTEXT };
        let (from, to) = (start + lead, i.saturating_sub(tail));
        if to > from && to - from >= MIN_FOLD { out.push(from..to); }
    }
    out
}

/// One line of the shown list: a row, or a folded run (index into `folds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Row(usize),
    Fold(usize),
}

pub fn visible(diff: &Diff, expanded: &HashSet<usize>) -> Vec<Item> {
    let mut out = Vec::with_capacity(diff.rows.len());
    let mut f = 0;
    let mut i = 0;
    while i < diff.rows.len() {
        match diff.folds.get(f) {
            Some(fold) if fold.start == i => {
                if expanded.contains(&f) { out.extend(fold.clone().map(Item::Row)); } else { out.push(Item::Fold(f)); }
                i = fold.end;
                f += 1;
            }
            _ => { out.push(Item::Row(i)); i += 1; }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script rebuilds both sides exactly, in order.
    fn check<T: Eq + Clone + std::fmt::Debug>(a: &[T], b: &[T], edits: &[Edit]) {
        let (mut ra, mut rb) = (vec![], vec![]);
        for e in edits {
            match *e {
                Edit::Equal(x, y) => { assert_eq!(a[x], b[y]); ra.push(x); rb.push(y); }
                Edit::Delete(x) => ra.push(x),
                Edit::Insert(y) => rb.push(y),
            }
        }
        assert_eq!(ra, (0..a.len()).collect::<Vec<_>>());
        assert_eq!(rb, (0..b.len()).collect::<Vec<_>>());
    }

    fn lcs(a: &[u8], b: &[u8]) -> usize {
        let mut t = vec![vec![0; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
            }
        }
        t[0][0]
    }

    #[test]
    fn myers_is_a_valid_shortest_script_on_asymmetric_inputs() {
        // A small deterministic generator: no fixture text, many shapes.
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
        for round in 0..400 {
            let la = (next() % 24) as usize;
            let lb = if round % 5 == 0 { 0 } else { (next() % 31) as usize };
            let alpha = 2 + (next() % 4);
            let a: Vec<u8> = (0..la).map(|_| (next() % alpha) as u8).collect();
            let b: Vec<u8> = (0..lb).map(|_| (next() % alpha) as u8).collect();
            let mut budget = u64::MAX;
            let edits = myers(&a, &b, &mut budget);
            check(&a, &b, &edits);
            let equal = edits.iter().filter(|e| matches!(e, Edit::Equal(..))).count();
            assert_eq!(equal, lcs(&a, &b), "{a:?} {b:?}");
        }
    }

    #[test]
    fn an_exhausted_budget_still_gives_an_exact_script() {
        let a: Vec<u32> = (0..2000).map(|i| i % 7).collect();
        let b: Vec<u32> = (0..1500).map(|i| (i * 3) % 11).collect();
        let mut budget = 10;
        let edits = myers(&a, &b, &mut budget);
        check(&a, &b, &edits);
    }

    fn kinds(d: &Diff) -> Vec<RowKind> {
        d.rows.iter().map(|r| r.kind).collect()
    }

    fn marked<'a>(text: &'a str, d: &Diff, line: usize, marks: &[Range<usize>], old: bool) -> Vec<&'a str> {
        let r = if old { d.old_lines[line].clone() } else { d.new_lines[line].clone() };
        let c = content(&text[r]);
        marks.iter().map(|m| &c[m.clone()]).collect()
    }

    #[test]
    fn deletions_insertions_and_cjk_intraline_changes() {
        let old = "标题\n第一段保留不变。\n今天天气很好，我们去公园散步。\n这一行将被删除。\n结尾。\n";
        let new = "标题\n第一段保留不变。\n今天天气不错，我们去湖边散步吧。\n结尾。\n新增的一行。\n";
        let d = diff(old, new);
        assert_eq!(kinds(&d), [RowKind::Same, RowKind::Same, RowKind::Changed, RowKind::Deleted, RowKind::Same, RowKind::Inserted]);
        let r = &d.rows[2];
        assert_eq!(marked(old, &d, r.old.unwrap(), &r.old_marks, true), ["很好", "公园"]);
        assert_eq!(marked(new, &d, r.new.unwrap(), &r.new_marks, false), ["不错", "湖边", "吧"]);
        assert_eq!((d.deleted, d.inserted), (2, 2));
        assert_eq!(d.rows[3].old, Some(3));
        assert_eq!(d.rows[5].new, Some(4));
    }

    #[test]
    fn blank_lines_never_pull_a_unique_line_out_of_place() {
        let old = "# First light\n\nOne morning.\n\nTwo birds.\n\nThree trees.\n\nUnknown.\n\n";
        let new = "# First light\n\nOne quiet morning.\n\nTwo swallows.\n\nUnknown.\n\nA new closing line.\n\n";
        let d = diff(old, new);
        check_rows(old, new, &d);
        let text = |r: &Row| (r.old.map(|l| content(&old[d.old_lines[l].clone()])), r.new.map(|l| content(&new[d.new_lines[l].clone()])));
        let unknown = d.rows.iter().find(|r| text(r).0 == Some("Unknown.")).unwrap();
        assert_eq!((unknown.kind, text(unknown).1), (RowKind::Same, Some("Unknown.")));
        let three = d.rows.iter().find(|r| text(r).0 == Some("Three trees.")).unwrap();
        assert_eq!((three.kind, three.new), (RowKind::Deleted, None));
        let added = d.rows.iter().find(|r| text(r).1 == Some("A new closing line.")).unwrap();
        assert_eq!((added.kind, added.old), (RowKind::Inserted, None));
        let two = d.rows.iter().find(|r| text(r).0 == Some("Two birds.")).unwrap();
        assert_eq!((two.kind, marked(old, &d, two.old.unwrap(), &two.old_marks, true), marked(new, &d, two.new.unwrap(), &two.new_marks, false)), (RowKind::Changed, vec!["birds"], vec!["swallows"]));
        assert_eq!((d.deleted, d.inserted), (4, 4));
    }

    #[test]
    fn english_words_change_as_words() {
        let (o, n) = intraline("Two birds sat on the old fence.", "Two swallows sat on the fence today.");
        let old = "Two birds sat on the old fence.";
        let new = "Two swallows sat on the fence today.";
        assert_eq!(o.iter().map(|r| old[r.clone()].trim()).collect::<Vec<_>>(), ["birds", "old"]);
        assert_eq!(n.iter().map(|r| new[r.clone()].trim()).collect::<Vec<_>>(), ["swallows", "today"]);
        // A whitespace-only edit is still a visible change.
        assert_eq!(intraline("a b，c", "a  b，c"), (vec![1..2], vec![1..3]));
        assert_eq!(intraline("  ", "\t"), (vec![0..2], vec![0..1]));
    }

    #[test]
    fn moved_and_duplicated_lines_pair_only_with_similar_lines() {
        let old = "春眠不觉晓\n处处闻啼鸟\n夜来风雨声\n";
        // The second line moved to the end, the third duplicated, a new first line.
        let new = "花落知多少\n春眠不觉晓\n夜来风雨声\n夜来风雨声\n处处闻啼鸟\n";
        let d = diff(old, new);
        check_rows(old, new, &d);
        assert_eq!((d.deleted, d.inserted), (1, 3));
        // No changed row pairs beta with an unrelated line's words.
        for r in d.rows.iter().filter(|r| r.kind == RowKind::Changed) {
            let o = content(&old[d.old_lines[r.old.unwrap()].clone()]);
            let n = content(&new[d.new_lines[r.new.unwrap()].clone()]);
            assert!(r.old_marks == [0..o.len()] && r.new_marks == [0..n.len()], "dissimilar pair is marked whole: {o} / {n}");
        }
        // A block of a deleted and two inserted lines pairs the similar one.
        let d = diff("x\nthe quick brown fox\ny\n", "x\nunrelated words entirely\nthe quick brown cat\ny\n");
        assert_eq!(kinds(&d), [RowKind::Same, RowKind::Inserted, RowKind::Changed, RowKind::Same]);
        assert_eq!(d.rows[2].new, Some(2));
    }

    /// Every line of both texts appears exactly once, in order.
    fn check_rows(old: &str, new: &str, d: &Diff) {
        let o: Vec<usize> = d.rows.iter().filter_map(|r| r.old).collect();
        let n: Vec<usize> = d.rows.iter().filter_map(|r| r.new).collect();
        assert_eq!(o, (0..line_ranges(old).len()).collect::<Vec<_>>());
        assert_eq!(n, (0..line_ranges(new).len()).collect::<Vec<_>>());
        for r in d.rows.iter().filter(|r| r.kind == RowKind::Same) {
            assert_eq!(old[d.old_lines[r.old.unwrap()].clone()], new[d.new_lines[r.new.unwrap()].clone()]);
        }
    }

    #[test]
    fn empty_sides() {
        let d = diff("", "一\n二\n");
        assert_eq!(kinds(&d), [RowKind::Inserted, RowKind::Inserted]);
        assert_eq!((d.deleted, d.inserted, d.old_lines.len()), (0, 2, 0));
        let d = diff("a\nb", "");
        assert_eq!(kinds(&d), [RowKind::Deleted, RowKind::Deleted]);
        assert_eq!(d.rows[1].old_marks, [0..1]);
        let d = diff("", "");
        assert!(d.rows.is_empty() && d.identical());
        // An empty line inserted has nothing to mark but still counts.
        let d = diff("a\n", "a\n\n");
        assert_eq!((kinds(&d), d.inserted), (vec![RowKind::Same, RowKind::Inserted], 1));
    }

    #[test]
    fn trailing_newline_and_line_endings_are_changes_shown_as_such() {
        let d = diff("一\n二", "一\n二\n");
        assert_eq!(kinds(&d), [RowKind::Same, RowKind::Changed]);
        assert!(d.rows[1].eol_only && d.rows[1].old_marks.is_empty() && d.rows[1].new_marks.is_empty());
        let d = diff("a\r\nb\n", "a\nb\n");
        assert_eq!(kinds(&d), [RowKind::Changed, RowKind::Same]);
        assert!(d.rows[0].eol_only);
        assert!(!d.identical());
    }

    #[test]
    fn long_unchanged_runs_fold_with_context_and_expand() {
        let old: String = (0..100).map(|i| format!("第{i}行\n")).collect();
        let new = old.replace("第50行\n", "第50行（改）\n");
        let d = diff(&old, &new);
        assert_eq!(d.folds, vec![0..47, 54..100]);
        let shown = visible(&d, &HashSet::new());
        assert_eq!(shown.len(), 1 + 3 + 1 + 3 + 1);
        assert_eq!(shown[0], Item::Fold(0));
        assert_eq!(shown[4], Item::Row(50));
        let all = visible(&d, &HashSet::from([0, 1]));
        assert_eq!(all, (0..100).map(Item::Row).collect::<Vec<_>>());
        // Short runs are not folded.
        let d = diff("a\nb\nc\nd\nx\n", "a\nb\nc\nd\ny\n");
        assert!(d.folds.is_empty());
    }

    #[test]
    fn large_cjk_texts_stay_exact() {
        let old: String = (0..6000).map(|i| format!("合成段落{i}：守灯人沿着旋梯爬上去，亲手修剪灯芯。\n")).collect();
        let mut new = old.replace("合成段落3000：", "合成段落3000（修订）：").replace("合成段落10：守灯人沿着旋梯爬上去，亲手修剪灯芯。\n", "");
        new.push_str("新的结尾。\n");
        let started = std::time::Instant::now();
        let d = diff(&old, &new);
        assert!(started.elapsed().as_secs() < 5);
        check_rows(&old, &new, &d);
        assert_eq!((d.deleted, d.inserted), (2, 2));
        let changed = d.rows.iter().find(|r| r.kind == RowKind::Changed).unwrap();
        assert_eq!(marked(&new, &d, changed.new.unwrap(), &changed.new_marks, false), ["（修订）"]);
    }

    #[test]
    fn tokens_split_cjk_by_character_and_latin_by_word() {
        let s = "MemeX 读书 v0.1, ok";
        let t: Vec<&str> = tokens(s).into_iter().map(|r| &s[r]).collect();
        assert_eq!(t, ["MemeX", " ", "读", "书", " ", "v0", ".", "1", ",", " ", "ok"]);
    }
}
