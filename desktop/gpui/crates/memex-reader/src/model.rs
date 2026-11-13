//! Reader state: open tabs, original/translation view, Markdown blocks with byte
//! offsets, and citation resolution.
//!
//! Citations address the revision's Markdown in UTF-8 bytes
//! (`revision_markdown_utf8_v1`). The reader never maps them onto translated text:
//! without a backend mapping it switches to the original and says so. Before a jump
//! the exact slice comes from `context_read`, and the reader highlights the block
//! whose displayed text contains it; a stale revision or an agent without
//! `context_read` is reported instead of guessing.

use crate::inline::{image_at, Inline};
use memex_client::reading_stats::{ReadingStats, StatsPrefs};
use memex_client::{tx, txf, Citation, ClientError, ContextSlice, ItemDetail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    /// A block that is only an image; `text` is its alt text. Images are not loaded:
    /// the agent has no owner-only image read yet, and the reader never follows paths
    /// or URLs from the Markdown itself.
    Image,
    /// A fenced code block; `text` is the raw block, fences included.
    Code,
}

/// Placeholder shown for an image the reader cannot load.
pub fn image_placeholder(alt: &str) -> String {
    let alt = alt.trim();
    if alt.is_empty() { tx("图片 · 待后端供图").into() } else { txf("图片：{0} · 待后端供图", &[&alt]) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
    pub kind: BlockKind,
    /// Shown text: inline Markdown resolved (bold kept, emphasis upright, images
    /// as placeholders). A heading block may continue with lines of text after
    /// its heading line, as the agent counts it as one block.
    pub text: String,
    /// Bold ranges of `text`.
    pub bold: Vec<std::ops::Range<usize>>,
    /// Bytes of `text` that are the heading line (the rest is ordinary text).
    pub heading_len: usize,
    /// Raw bytes before `text` starts (a heading's `## `).
    pub skip: usize,
    /// Code spans, links and list / quote / table lines of `text`.
    pub marks: crate::inline::Marks2,
    /// An image block's http/https target (shown as a link, never loaded).
    pub href: Option<String>,
}

/// Splits Markdown into the agent's blocks (`memex_client::blocks`), keeping
/// byte offsets; AI insights and 对照 pairing count the same blocks.
pub fn blocks(markdown: &str) -> Vec<Block> {
    memex_client::blocks::split_blocks(markdown)
        .into_iter()
        .map(|sb| {
            let raw = &markdown[sb.start..sb.end];
            let (start, end) = (sb.start, sb.end);
            if sb.fenced {
                return Block { start, end, kind: BlockKind::Code, text: raw.to_string(), bold: vec![], heading_len: 0, skip: 0, marks: Default::default(), href: None };
            }
            // A block the agent kept in the original: its marker line is not shown.
            let pre = raw
                .strip_prefix(memex_client::UNTRANSLATED_BLOCK_MARKER)
                .and_then(|rest| rest.strip_prefix('\n'))
                .map_or(0, |rest| raw.len() - rest.len());
            let body = &raw[pre..];
            let hashes = body.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
                let skip = pre + hashes + 1 + (body[hashes + 1..].len() - body[hashes + 1..].trim_start().len());
                let inline = Inline::parse(raw, skip);
                let first = pre + body.find('\n').unwrap_or(body.len());
                let heading_len = inline.shown(skip..first).map(|r| r.end).unwrap_or(0);
                // Trailing spaces of the heading line are not shown.
                let marks = inline.marks();
                return Block { start, end, kind: BlockKind::Heading(hashes as u8), text: inline.text, bold: inline.bold, heading_len, skip, marks, href: None };
            }
            if let Some((alt, image_end)) = image_at(raw, pre) {
                if image_end == raw.len() {
                    let href = memex_client::markdown::link_at(raw, pre).and_then(|l| {
                        (memex_client::images::local_image_target(&l.url).is_some() || memex_client::markdown::safe_url(&l.url).is_some()).then_some(l.url)
                    });
                    return Block { start, end, kind: BlockKind::Image, text: alt.trim().to_string(), bold: vec![], heading_len: 0, skip: pre, marks: Default::default(), href };
                }
            }
            let inline = Inline::parse(raw, pre);
            let marks = inline.marks();
            Block { start, end, kind: BlockKind::Paragraph, text: inline.text, bold: inline.bold, heading_len: 0, skip: pre, marks, href: None }
        })
        .collect()
}

impl Block {
    /// The heading line and the text that follows it in the same block.
    pub fn heading_parts(&self) -> (&str, &str) {
        let cut = self.heading_len.min(self.text.len());
        (self.text[..cut].trim_end(), self.text[cut..].trim_start_matches('\n'))
    }
}

pub use crate::bilingual::{Layout, Mode, Row};

/// Code shown without its fence lines.
pub fn code_text(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.lines().collect();
    if lines.first().is_some_and(|l| l.starts_with("```") || l.starts_with("~~~")) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim() == "```" || l.trim() == "~~~") {
        lines.pop();
    }
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub item_id: String,
    pub title: String,
    /// A preview tab (not pinned) is replaced by the next single-click open.
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpOutcome {
    /// Scroll to and highlight this block of the original text.
    Highlight { block: usize, notice: Option<String> },
    Refused(String),
}

#[derive(Debug, Default, Clone)]
pub struct ReaderModel {
    tabs: Vec<Tab>,
    active: Option<usize>,
    detail: Option<ItemDetail>,
    original: Vec<Block>,
    translation: Vec<Block>,
    mode: Mode,
    highlight: Option<usize>,
    notice: Option<String>,
    stats_prefs: StatsPrefs,
    stats: Option<ReadingStats>,
    /// Rows of the chapter's paragraph index (book 对照, 段落 layout).
    aligned: Vec<AlignedRow>,
    /// Book 对照 shows the two chapters side by side instead of paragraph pairs.
    parallel: bool,
    /// A paired book (`reader_v1`): 原文 and 译文 are its two editions, one chapter
    /// at a time, instead of the canonical text and a machine translation.
    book: Option<Book>,
}

/// One paired book and the chapter on screen.
#[derive(Debug, Clone)]
pub struct Book {
    pub doc: memex_client::book::ReaderDocument,
    pub entries: Vec<memex_client::book::Entry>,
    /// Index into `entries`.
    pub at: usize,
    /// The chapter's text on each side, when that side has the chapter.
    pub original: Option<String>,
    pub translation: Option<String>,
    /// The chapter's paragraph index, as read and checked against both texts.
    pub alignment: Paired,
}

/// What is known about the paragraph correspondence of the chapter on screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Paired {
    /// No counterpart chapter, or not read yet.
    #[default]
    NotApplicable,
    /// The agent has no `reader_paragraph_alignment_v1`.
    Unsupported,
    /// The index, valid for both texts (`version` may be `missing`).
    Index(memex_client::book::Alignment),
    /// Read but inconsistent with the texts, or unreadable: never used.
    Invalid(String),
    /// The agent stayed busy (`library_loading`): not read yet, not invalid.
    Busy,
}

/// One row of a paragraph index in block indexes of each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignedRow {
    pub o: std::ops::Range<usize>,
    pub t: std::ops::Range<usize>,
    pub kind: memex_client::book::PairKind,
    pub relation: memex_client::book::Relation,
}

/// Block index ranges per row: a block belongs to the row whose byte range holds its start.
pub fn aligned_rows(alignment: &memex_client::book::Alignment, original: &[Block], translation: &[Block]) -> Vec<AlignedRow> {
    let take = |range: Option<[usize; 2]>, blocks: &[Block]| match range {
        Some([a, b]) => {
            let first = blocks.iter().position(|x| x.start >= a).unwrap_or(blocks.len());
            let last = blocks.iter().position(|x| x.start >= b).unwrap_or(blocks.len());
            first..last.max(first)
        }
        None => 0..0,
    };
    alignment.pairs.iter().map(|p| AlignedRow { o: take(p.o, original), t: take(p.t, translation), kind: p.kind, relation: p.relation }).collect()
}

impl Book {
    pub fn new(doc: memex_client::book::ReaderDocument) -> Option<Self> {
        let entries = doc.entries();
        (doc.is_book() && !entries.is_empty()).then_some(Self { doc, entries, at: 0, original: None, translation: None, alignment: Paired::NotApplicable })
    }

    /// The original chapter key of entry `at` when it has a counterpart (what an index is keyed by).
    pub fn paired_key(&self, at: usize) -> Option<String> {
        match self.keys(at) {
            (Some(o), Some(_)) => Some(o),
            _ => None,
        }
    }

    /// The chapter key on each side for entry `at`.
    pub fn keys(&self, at: usize) -> (Option<String>, Option<String>) {
        let Some(e) = self.entries.get(at) else { return (None, None) };
        let key = |side: &str, i: Option<usize>| i.and_then(|i| self.doc.edition(side)?.chapters.get(i)).map(|c| c.key.clone());
        (key("original", e.original), key("translation", e.translation))
    }

    /// 「译名 ｜ Original」 for entry `at`.
    pub fn title(&self, at: usize) -> String {
        let Some(e) = self.entries.get(at) else { return String::new() };
        let title = |side: &str, i: Option<usize>| i.and_then(|i| self.doc.edition(side)?.chapters.get(i)).map(|c| c.title.trim().to_string());
        match (title("translation", e.translation), title("original", e.original)) {
            (Some(t), Some(o)) if t != o => format!("{t} ｜ {o}"),
            (Some(t), _) => t,
            (None, Some(o)) => o,
            (None, None) => String::new(),
        }
    }

    pub fn level(&self, at: usize) -> u8 {
        let Some(e) = self.entries.get(at) else { return 1 };
        let level = |side: &str, i: Option<usize>| i.and_then(|i| self.doc.edition(side)?.chapters.get(i)).map(|c| c.level);
        level("translation", e.translation).or(level("original", e.original)).unwrap_or(1)
    }

    pub fn language(&self, side: &str) -> String {
        self.doc.edition(side).map(|e| e.language.clone()).unwrap_or_default()
    }
}

impl ReaderModel {
    pub fn stats_prefs(&self) -> StatsPrefs {
        self.stats_prefs
    }
    pub fn stats(&self) -> Option<ReadingStats> {
        self.detail.as_ref().and(self.stats)
    }
    pub fn set_stats_prefs(&mut self, prefs: StatsPrefs) {
        self.stats_prefs = prefs.normalized();
        if let (Some(detail), Some(stats)) = (&self.detail, self.stats) {
            self.stats = Some(ReadingStats::new(&detail.canonical.markdown, stats.total_bytes, self.stats_prefs));
        }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn active(&self) -> Option<usize> {
        self.active
    }
    pub fn active_item(&self) -> Option<&str> {
        self.active.and_then(|i| self.tabs.get(i)).map(|t| t.item_id.as_str())
    }
    pub fn detail(&self) -> Option<&ItemDetail> {
        self.detail.as_ref()
    }
    /// The chosen mode (remembered in prefs); see `layout` for what is shown.
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn book(&self) -> Option<&Book> {
        self.book.as_ref()
    }
    /// Shows `book` instead of the canonical text; its chapter text arrives with `set_chapter`.
    pub fn set_book(&mut self, book: Book) {
        self.book = Some(book);
        self.original.clear();
        self.translation.clear();
        self.highlight = None;
    }
    /// The text of entry `at` on each side (`None`: that side has no such chapter).
    pub fn set_chapter(&mut self, at: usize, original: Option<String>, translation: Option<String>, alignment: Paired) {
        let Some(book) = self.book.as_mut() else { return };
        book.at = at;
        self.original = original.as_deref().map(blocks).unwrap_or_default();
        self.translation = translation.as_deref().map(blocks).unwrap_or_default();
        book.original = original;
        book.translation = translation;
        self.aligned = match &alignment {
            Paired::Index(a) if !a.is_missing() => aligned_rows(a, &self.original, &self.translation),
            _ => Vec::new(),
        };
        book.alignment = alignment;
        self.highlight = None;
    }
    pub fn aligned(&self) -> &[AlignedRow] {
        &self.aligned
    }
    pub fn parallel(&self) -> bool {
        self.parallel
    }
    pub fn set_parallel(&mut self, parallel: bool) {
        self.parallel = parallel;
    }
    /// Book 对照 as side-by-side columns: chosen, or no usable paragraph index
    /// while both sides have the chapter.
    pub fn book_columns(&self) -> bool {
        self.book.is_some() && self.mode == Mode::Bilingual && !self.original.is_empty() && !self.translation.is_empty()
            && (self.parallel || self.aligned.iter().all(|r| r.relation != memex_client::book::Relation::Matched))
    }
    /// The Markdown that block offsets on one side refer to.
    pub fn side_markdown(&self, translation: bool) -> &str {
        match (&self.book, &self.detail) {
            (Some(b), _) => if translation { b.translation.as_deref() } else { b.original.as_deref() }.unwrap_or(""),
            (None, Some(d)) => if translation { d.analysis.markdown.as_deref().unwrap_or("") } else { &d.canonical.markdown },
            (None, None) => "",
        }
    }
    pub fn has_translation(&self) -> bool {
        !self.translation.is_empty()
    }
    pub fn original(&self) -> &[Block] {
        &self.original
    }
    pub fn translation(&self) -> &[Block] {
        &self.translation
    }
    pub fn layout(&self) -> Layout {
        if self.book.is_some() {
            // Book chapters correspond by chapter order only: 对照 shows the two
            // chapters side by side (the view's two columns), never paired by index.
            let originals = (0..self.original.len()).map(Row::Original).collect();
            return match self.mode {
                // 段落对照: the index's rows in order (columns use their own lists).
                Mode::Bilingual if !self.book_columns() && !self.aligned.is_empty() => Layout { rows: (0..self.aligned.len()).map(Row::Aligned).collect(), shown: Mode::Bilingual, mismatch: false, missing_translation: false },
                // A chapter only one edition has.
                Mode::Bilingual if self.original.is_empty() => Layout { rows: (0..self.translation.len()).map(Row::Translation).collect(), shown: Mode::Bilingual, mismatch: false, missing_translation: false },
                Mode::Original | Mode::Bilingual => Layout { rows: originals, shown: self.mode, mismatch: false, missing_translation: false },
                Mode::Translation => Layout { rows: (0..self.translation.len()).map(Row::Translation).collect(), shown: Mode::Translation, mismatch: false, missing_translation: false },
            };
        }
        crate::bilingual::layout(self.mode, &self.original, &self.translation)
    }
    pub fn highlight(&self) -> Option<usize> {
        self.highlight
    }
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }
    /// Marks a block of the original (定位原文); `None` clears it.
    pub fn set_highlight(&mut self, block: Option<usize>) {
        self.highlight = block;
    }

    /// Shows `item_id`: an open tab is activated (and pinned when `pin`); otherwise
    /// a pinned open gets its own tab, and a preview open replaces the preview tab
    /// (pinned tabs are never replaced).
    pub fn open(&mut self, item_id: &str, title: &str, pin: bool) {
        if let Some(existing) = self.tabs.iter().position(|t| t.item_id == item_id) {
            self.active = Some(existing);
            if pin { self.tabs[existing].pinned = true; }
        } else {
            let tab = Tab { item_id: item_id.into(), title: title.into(), pinned: pin };
            match self.tabs.iter().position(|t| !t.pinned).filter(|_| !pin) {
                Some(i) => {
                    self.tabs[i] = tab;
                    self.active = Some(i);
                }
                None => {
                    self.tabs.push(tab);
                    self.active = Some(self.tabs.len() - 1);
                }
            }
        }
        self.detail = None;
        self.book = None;
        self.original.clear();
        self.translation.clear();
        self.highlight = None;
        self.notice = None;
    }

    /// Keeps a tab: the next single-click open no longer replaces it.
    pub fn pin(&mut self, index: usize) {
        if let Some(t) = self.tabs.get_mut(index) { t.pinned = true; }
    }

    pub fn activate(&mut self, index: usize) -> Option<String> {
        let tab = self.tabs.get(index)?.clone();
        self.open(&tab.item_id, &tab.title, false);
        Some(tab.item_id)
    }

    /// Closes a tab; returns the item that becomes active, if any.
    pub fn close(&mut self, index: usize) -> Option<String> {
        if index >= self.tabs.len() {
            return None;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = None;
            self.detail = None;
            self.book = None;
            self.original.clear();
            self.translation.clear();
            return None;
        }
        let next = index.min(self.tabs.len() - 1);
        self.activate(next)
    }

    /// Rebuilds blocks from the loaded detail so inline image placeholders follow
    /// an interface language switch. Offsets and block count do not change.
    pub fn relocalize(&mut self) {
        if let Some(book) = &self.book {
            self.original = book.original.as_deref().map(blocks).unwrap_or_default();
            self.translation = book.translation.as_deref().map(blocks).unwrap_or_default();
        } else if let Some(detail) = &self.detail {
            self.original = blocks(&detail.canonical.markdown);
            self.translation = detail.analysis.markdown.as_deref().map(blocks).unwrap_or_default();
        }
    }

    /// `item_read` returns the complete canonical body or errors (never truncates).
    /// A future chunked caller must use `load_partial` instead.
    pub fn load(&mut self, detail: ItemDetail) -> bool {
        let total = detail.canonical.markdown.len();
        self.load_partial(detail, Some(total))
    }

    /// Accepts loaded original text only for the active item. Unknown total stays
    /// unknown; no byte-ratio extrapolation of character counts or reading time.
    pub fn load_partial(&mut self, detail: ItemDetail, total_bytes: Option<usize>) -> bool {
        if self.active_item() != Some(detail.canonical.item_id.as_str()) {
            return false;
        }
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            let translated = detail.analysis.chinese_title(&detail.canonical.title);
            tab.title = memex_client::display_title(&detail.canonical.title, translated.as_deref());
        }
        self.original = blocks(&detail.canonical.markdown);
        self.translation = detail.analysis.markdown.as_deref().map(blocks).unwrap_or_default();
        self.stats = Some(ReadingStats::new(&detail.canonical.markdown, total_bytes, self.stats_prefs));
        self.detail = Some(detail);
        true
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.highlight = None;
    }

    /// Applies the result of `context_read` for `citation`.
    pub fn resolve(&mut self, citation: &Citation, slice: Result<ContextSlice, ClientError>) -> JumpOutcome {
        let outcome = self.resolve_inner(citation, slice);
        match &outcome {
            JumpOutcome::Highlight { block, notice } => {
                self.highlight = Some(*block);
                self.notice = notice.clone();
            }
            JumpOutcome::Refused(message) => {
                self.highlight = None;
                self.notice = Some(message.clone());
            }
        }
        outcome
    }

    fn resolve_inner(&mut self, citation: &Citation, slice: Result<ContextSlice, ClientError>) -> JumpOutcome {
        let Some(detail) = &self.detail else { return JumpOutcome::Refused(tx("条目尚未载入").into()) };
        // Citations address the canonical (translation) text, not a book chapter on screen.
        if self.book.is_some() {
            return JumpOutcome::Refused(tx("书籍按章阅读时暂不支持引用定位").into());
        }
        if detail.canonical.item_id != citation.item_id {
            return JumpOutcome::Refused(tx("引用属于另一篇条目").into());
        }
        if detail.canonical.revision_id != citation.revision_id {
            return JumpOutcome::Refused(tx("引用基于旧版本，条目已修改，无法定位").into());
        }
        let slice = match slice {
            Ok(slice) => slice,
            Err(ClientError::Unsupported) => return JumpOutcome::Refused(tx("当前代理版本不支持引用定位").into()),
            Err(e) if e.is_stale_revision() => return JumpOutcome::Refused(tx("引用基于旧版本，条目已修改，无法定位").into()),
            Err(e) => return JumpOutcome::Refused(txf("引用定位失败：{0}", &[&e.message()])),
        };
        let needle = slice.content_markdown.trim();
        let probe: String = needle.chars().take(48).collect();
        let found = self
            .original
            .iter()
            .position(|b| !probe.is_empty() && (b.text.contains(&probe) || probe.contains(b.text.as_str())));
        let Some(block) = found else { return JumpOutcome::Refused(tx("原文中找不到该引用片段").into()) };
        // 对照 already shows the original (rows follow block indexes); only 译文 has
        // no mapping and switches.
        let shown = self.layout().shown;
        let notice = (shown == Mode::Translation).then(|| tx("译文无对应映射，已跳到原文").to_string());
        if shown == Mode::Translation {
            self.mode = Mode::Original;
        }
        JumpOutcome::Highlight { block, notice }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_kept_in_the_original_hides_its_marker_line() {
        let md = format!("译文段落。\n\n{}\nKept English paragraph with **bold**.\n\n{}\n## Kept heading", memex_client::UNTRANSLATED_BLOCK_MARKER, memex_client::UNTRANSLATED_BLOCK_MARKER);
        let b = blocks(&md);
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].text, "Kept English paragraph with bold.");
        assert!(!b[1].text.contains("memex"));
        assert_eq!(b[2].kind, BlockKind::Heading(2));
        assert_eq!(b[2].heading_parts().0, "Kept heading");
    }
    use memex_client::{fixture::FixtureBackend, Client, ContextRequest, Sensitivity};

    #[test]
    fn reading_stats_count_original_across_modes_and_never_extrapolate_partial_text() {
        use memex_client::reading_stats::{CountMethod, TextCount};
        let client = Client::new(FixtureBackend::new(2));
        let mut detail = client.item_detail("item_fixture_000000").unwrap();
        detail.canonical.markdown = "**Hello** 中文，".into();
        detail.analysis.markdown = Some("这是一篇更长的译文，不应和原文重复计算。".into());
        let mut r = ReaderModel::default();
        r.open(&detail.canonical.item_id, &detail.canonical.title, false);
        assert!(r.load(detail.clone()));
        for mode in [Mode::Original, Mode::Bilingual, Mode::Translation] {
            r.set_mode(mode);
            assert_eq!(r.stats().unwrap().count, TextCount { cjk: 3, words: 1 });
            assert!(r.stats().unwrap().complete());
        }
        assert!(r.load_partial(detail.clone(), Some(2000)));
        assert_eq!(r.stats().unwrap().count, TextCount { cjk: 3, words: 1 });
        assert!(!r.stats().unwrap().complete());
        r.set_stats_prefs(StatsPrefs { method: CountMethod::Chars, markup: true, ..StatsPrefs::default() });
        assert_eq!(r.stats().unwrap().count, TextCount { cjk: 12, words: 0 });
        assert_eq!(r.stats().unwrap().total_bytes, Some(2000));
        assert!(r.load_partial(detail.clone(), None));
        assert!(!r.stats().unwrap().complete());
        r.open("item_fixture_000001", "next", false);
        assert!(r.stats().is_none());
        assert!(!r.load(detail));
        assert!(r.stats().is_none());
        assert_eq!(r.stats_prefs().method, CountMethod::Chars);
    }

    #[test]
    fn images_become_placeholders_and_keep_offsets() {
        let md = "# 剪藏\n\n![玫瑰 🌹](file:///Users/x/a.png)\n\n前文![](https://example.invalid/b.jpg)后文 ![图二](c.png) 结尾\n\n不是图片 ![缺括号](x\n";
        let b = blocks(md);
        assert_eq!(b[1].kind, BlockKind::Image);
        assert_eq!(b[1].text, "玫瑰 🌹");
        assert_eq!(&md[b[1].start..b[1].end], "![玫瑰 🌹](file:///Users/x/a.png)");
        assert_eq!(b[2].kind, BlockKind::Paragraph);
        // An http image is a labelled link; a relative one stays a placeholder.
        assert_eq!(b[2].text, "前文〔图片〕后文 〔图片：图二 · 待后端供图〕 结尾");
        assert!(!b[2].text.contains("https://") && !b[2].text.contains("c.png"));
        let link = &b[2].marks.links;
        assert_eq!(link.len(), 1);
        assert_eq!(&b[2].text[link[0].0.clone()], "〔图片〕");
        assert_eq!(link[0].1, "https://example.invalid/b.jpg");
        assert_eq!(b[3].text, "不是图片 ![缺括号](x");
        assert_eq!(image_placeholder(" "), "图片 · 待后端供图");
    }

    #[test]
    fn blocks_keep_utf8_offsets_for_chinese_and_emoji() {
        let md = "# 标题 🌹\n\n第一段，中文。\n第二行 emoji 🌹。\n\n## 二、小节\n正文紧跟*标题*\n";
        let b = blocks(md);
        // As the agent splits: a heading followed directly by text is one block.
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].kind, BlockKind::Heading(1));
        assert_eq!(b[0].text, "标题 🌹");
        assert_eq!(&md[b[1].start..b[1].end], "第一段，中文。\n第二行 emoji 🌹。");
        assert_eq!(b[2].kind, BlockKind::Heading(2));
        assert_eq!(&md[b[2].start..b[2].end], "## 二、小节\n正文紧跟*标题*");
        assert_eq!(b[2].heading_parts(), ("二、小节", "正文紧跟标题"));
    }

    /// The agent's own sample (`memex-agent` `markdown_blocks.rs` tests, `MIXED`).
    const AGENT_MIXED: &str = "# A synthetic title\n\nFirst paragraph with two\nlines of prose.\n\n\n```rust\nfn main() {\n\n    println!(\"x\");\n}\n```\nText right after a fence.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![diagram](https://example.invalid/a.png)\n\nhttps://example.invalid/reference\n\n- a list item\n- another item\n\n~~~\nunclosed fence\n";

    #[test]
    fn the_reader_splits_the_agents_sample_into_the_agents_blocks() {
        let reader = blocks(AGENT_MIXED);
        let agent = memex_client::blocks::split_blocks(AGENT_MIXED);
        assert_eq!(reader.len(), 9);
        assert_eq!(reader.iter().map(|b| (b.start, b.end)).collect::<Vec<_>>(), agent.iter().map(|b| (b.start, b.end)).collect::<Vec<_>>());
        let kinds: Vec<BlockKind> = reader.iter().map(|b| b.kind).collect();
        assert_eq!(kinds[0], BlockKind::Heading(1));
        assert_eq!((kinds[2], kinds[8], kinds[5]), (BlockKind::Code, BlockKind::Code, BlockKind::Image));
        assert_eq!(reader[3].text, "Text right after a fence.");
    }

    #[test]
    fn reader_text_has_no_italics_but_keeps_bold() {
        let b = blocks("Plain *soft* and _gentle_ words with **firm** ones.\n\n## A *heading*\nthen __strong__ text");
        assert_eq!(b[0].text, "Plain soft and gentle words with firm ones.");
        assert_eq!(b[0].bold.iter().map(|r| &b[0].text[r.clone()]).collect::<Vec<_>>(), ["firm"]);
        assert_eq!(b[1].heading_parts(), ("A heading", "then strong text"));
        assert!(b.iter().all(|b| !b.text.contains('*') && !b.text.contains('_')));
    }

    #[test]
    fn showcase_highlights_mark_only_their_terms_on_both_sides() {
        use memex_client::insights::{Side, SIDES_BOTH};
        let client = Client::new(FixtureBackend::new(30));
        let detail = client.item_detail(memex_client::fixture::SHOWCASE_ITEM).unwrap();
        let section = detail.analysis.ai_insights.as_ref().and_then(|a| a.highlights.clone()).unwrap();
        assert_eq!(section.sides.as_deref(), Some(SIDES_BOTH));
        let items = section.done().unwrap().to_vec();
        assert!(items.iter().all(|i| i.original.is_some() && i.translation.is_some()));
        let translated_md = detail.analysis.markdown.clone().unwrap();
        for (side, md) in [(Side::Original, detail.canonical.markdown.as_str()), (Side::Translation, translated_md.as_str())] {
            let bs = blocks(md);
            let (spans, skipped) = crate::inline::block_spans(md, &bs, &items, side);
            assert_eq!(skipped, 0, "every fixture span is placed on {side:?}");
            assert_eq!(spans.values().map(Vec::len).sum::<usize>(), items.len());
            // Only the term is marked: the shown text of each span is the term itself.
            for (i, s) in &spans {
                for span in s {
                    let shown = &bs[*i].text[span.range.clone()];
                    assert!(items.iter().any(|it| it.span(side).is_some_and(|t| t.text == shown)), "{side:?} {shown}");
                }
            }
        }
        // A span whose bytes do not hold its text is skipped, never guessed.
        let original = blocks(&detail.canonical.markdown);
        let mut wrong = items[0].clone();
        wrong.original.as_mut().unwrap().start += 1;
        assert_eq!(crate::inline::block_spans(&detail.canonical.markdown, &original, &[wrong], Side::Original).1, 1);
        let mut elsewhere = items[0].clone();
        elsewhere.original.as_mut().unwrap().block += 1;
        assert_eq!(crate::inline::block_spans(&detail.canonical.markdown, &original, &[elsewhere], Side::Original).1, 1);
    }

    #[test]
    fn ticks_and_the_highlight_list_follow_the_side_being_read() {
        use crate::inline::{block_spans, marked_sides, marks_along};
        use memex_client::insights::Side;
        let client = Client::new(FixtureBackend::new(30));
        let along = |item: &str, mode: Mode, marked: bool| {
            let detail = client.item_detail(item).unwrap();
            let items = detail.analysis.ai_insights.as_ref().and_then(|a| a.highlights.as_ref()).and_then(|h| h.done()).unwrap().to_vec();
            let md_t = detail.analysis.markdown.clone().unwrap_or_default();
            let (o, t) = (blocks(&detail.canonical.markdown), blocks(&md_t));
            let layout = crate::bilingual::layout(mode, &o, &t);
            let (so, st) = marked_sides(layout.shown, marked);
            let spans_o = if so { block_spans(&detail.canonical.markdown, &o, &items, Side::Original).0 } else { Default::default() };
            let spans_t = if st { block_spans(&md_t, &t, &items, Side::Translation).0 } else { Default::default() };
            (marks_along(&layout.rows, &o, &t, &spans_o, &spans_t), items.len())
        };
        let showcase = memex_client::fixture::SHOWCASE_ITEM;
        let (marks, n) = along(showcase, Mode::Original, true);
        assert!(marks.len() == n && marks.iter().all(|m| m.side == Side::Original));
        let (marks, n) = along(showcase, Mode::Translation, true);
        assert!(marks.len() == n && marks.iter().all(|m| m.side == Side::Translation));
        assert!(marks.iter().any(|m| m.text == "检索增强生成"));
        let (marks, n) = along(showcase, Mode::Bilingual, true);
        assert_eq!(marks.len(), 2 * n);
        assert!(marks.windows(2).all(|w| w[0].row <= w[1].row), "reading order");
        assert_eq!((marks[0].side, marks[1].side), (Side::Original, Side::Original), "a row's original half first");
        // `original_only`: 译文 has no ticks, 对照 only the original's.
        let item = memex_client::fixture::ORIGINAL_ONLY_HIGHLIGHTS_ITEM;
        assert!(along(item, Mode::Translation, false).0.is_empty());
        let (marks, n) = along(item, Mode::Bilingual, false);
        assert!(marks.len() == n && marks.iter().all(|m| m.side == Side::Original));
    }

    #[test]
    fn v1_highlights_without_spans_still_render_on_the_original_only() {
        use memex_client::insights::Side;
        let fixture = FixtureBackend::new(30);
        fixture.set_controls(memex_client::fixture::FixtureControls { highlights_v1: true, ..Default::default() });
        let client = Client::new(fixture);
        let detail = client.item_detail(memex_client::fixture::SHOWCASE_ITEM).unwrap();
        let section = detail.analysis.ai_insights.as_ref().and_then(|a| a.highlights.clone()).unwrap();
        assert_eq!(section.sides, None);
        let items = section.done().unwrap().to_vec();
        assert!(items.iter().all(|i| !i.has_spans()));
        let original = blocks(&detail.canonical.markdown);
        let (spans, skipped) = crate::inline::block_spans(&detail.canonical.markdown, &original, &items, Side::Original);
        assert_eq!((spans.values().map(Vec::len).sum::<usize>(), skipped), (items.len(), 0));
        let md = detail.analysis.markdown.unwrap();
        let (spans, skipped) = crate::inline::block_spans(&md, &blocks(&md), &items, Side::Translation);
        assert!(spans.is_empty() && skipped == 0, "a v1 quote is never searched in the translation");
    }

    fn citation_for(client: &Client, item: &str, needle: &str) -> (Citation, ItemDetail) {
        let detail = client.item_detail(item).unwrap();
        let start = detail.canonical.markdown.find(needle).unwrap();
        (Citation { label: "§".into(), item_id: item.into(), revision_id: detail.canonical.revision_id.clone(), start_byte: start, end_byte: start + needle.len() }, detail)
    }

    fn read(client: &Client, c: &Citation) -> Result<ContextSlice, ClientError> {
        client.context_read(ContextRequest { item_id: c.item_id.clone(), expected_revision_id: c.revision_id.clone(), start_byte: c.start_byte, end_byte: c.end_byte, max_sensitivity: Sensitivity::Restricted })
    }

    #[test]
    fn citation_in_translation_view_jumps_to_original_with_notice() {
        let client = Client::new(FixtureBackend::new(10));
        let (citation, detail) = citation_for(&client, "item_fixture_000000", "这是第 9 段合成示例正文");
        let mut reader = ReaderModel::default();
        reader.open("item_fixture_000000", "t", false);
        assert!(reader.load(detail));
        reader.set_mode(Mode::Translation);
        let outcome = reader.resolve(&citation, read(&client, &citation));
        let JumpOutcome::Highlight { block, notice } = outcome else { panic!("expected highlight") };
        assert!(reader.original()[block].text.contains("这是第 9 段"));
        assert_eq!(notice.as_deref(), Some("译文无对应映射，已跳到原文"));
        assert_eq!(reader.mode(), Mode::Original);
    }

    #[test]
    fn stale_and_unsupported_citations_are_refused() {
        let fixture = FixtureBackend::new(10);
        let client = Client::new(fixture.clone());
        let (citation, detail) = citation_for(&client, "item_fixture_000001", "这是第 2 段");
        let mut reader = ReaderModel::default();
        reader.open("item_fixture_000001", "t", false);
        reader.load(detail);
        fixture.set_controls(memex_client::fixture::FixtureControls { legacy_agent: true, ..Default::default() });
        assert_eq!(reader.resolve(&citation, read(&client, &citation)), JumpOutcome::Refused("当前代理版本不支持引用定位".into()));
        fixture.set_controls(Default::default());
        fixture.simulate_external_edit("item_fixture_000001");
        assert!(matches!(reader.resolve(&citation, read(&client, &citation)), JumpOutcome::Refused(m) if m.contains("旧版本")));
    }

    #[test]
    fn tabs_replace_open_new_and_ignore_late_detail() {
        let client = Client::new(FixtureBackend::new(10));
        let mut reader = ReaderModel::default();
        reader.open("item_fixture_000001", "一", false);
        reader.open("item_fixture_000002", "二", false);
        assert_eq!(reader.tabs().len(), 1);
        reader.open("item_fixture_000003", "三", true);
        assert_eq!(reader.tabs().len(), 2);
        // Detail for the replaced item arrives late and is ignored.
        assert!(!reader.load(client.item_detail("item_fixture_000001").unwrap()));
        assert_eq!(reader.close(1).as_deref(), Some("item_fixture_000002"));
        assert_eq!(reader.active_item(), Some("item_fixture_000002"));
    }

    #[test]
    fn single_click_replaces_only_the_preview_tab_and_double_click_pins() {
        let mut reader = ReaderModel::default();
        reader.open("a", "A", true);
        reader.open("b", "B", false);
        reader.open("c", "C", false);
        let tabs = |r: &ReaderModel| r.tabs().iter().map(|t| (t.item_id.clone(), t.pinned)).collect::<Vec<_>>();
        assert_eq!(tabs(&reader), [("a".to_string(), true), ("c".to_string(), false)]);
        // Pinning the preview: the next single click gets a new preview tab.
        reader.pin(1);
        reader.open("d", "D", false);
        assert_eq!(tabs(&reader).len(), 3);
        assert_eq!(reader.active_item(), Some("d"));
        // Opening an already open item activates it; a pinned open of the preview pins it.
        reader.open("a", "A", false);
        assert_eq!(reader.active(), Some(0));
        reader.open("d", "D", true);
        assert!(reader.tabs()[2].pinned);
        reader.open("e", "E", false);
        assert_eq!(tabs(&reader).last().cloned(), Some(("e".to_string(), false)));
    }
}
