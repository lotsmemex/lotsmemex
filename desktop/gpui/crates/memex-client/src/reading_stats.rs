//! Phone `countText` semantics, independent of UI, storage and the backend.
//! Counts always describe the supplied original text, never extrapolate a total
//! from its byte length. A partial body's estimate is explicitly for loaded text.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountMethod {
    Chars,
    #[default]
    #[serde(other)]
    Mixed,
}

pub const ZH_SPEEDS: &[u32] = &[250, 300, 350, 400, 500, 600, 800];
pub const EN_SPEEDS: &[u32] = &[150, 200, 230, 250, 300, 400];
/// The example sentence the statistics settings count; it names the product by its short name.
pub fn sample() -> String {
    format!("{} 是一个**本地优先**的资料库，支持 retrieval-augmented reading。", crate::ASSISTANT_NAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsPrefs {
    pub markup: bool,
    pub method: CountMethod,
    pub zh: u32,
    pub en: u32,
}

impl Default for StatsPrefs {
    fn default() -> Self {
        Self { markup: false, method: CountMethod::Mixed, zh: 400, en: 230 }
    }
}

impl StatsPrefs {
    pub fn normalized(mut self) -> Self {
        if !ZH_SPEEDS.contains(&self.zh) { self.zh = 400; }
        if !EN_SPEEDS.contains(&self.en) { self.en = 230; }
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextCount {
    /// CJK including the phone's punctuation ranges; all nonspace codepoints in chars mode.
    pub cjk: usize,
    pub words: usize,
}

// ECMAScript whitespace differs from Rust/Unicode White_Space (notably BOM and NEL).
const SPACE: &str = r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (r"(?mR)^\s*(```|~~~)[^\r\n\u{2028}\u{2029}]*$", ""),
            (r"!\[([^\]]*)\]\([^)]*\)", "$1"),
            (r"\[([^\]]*)\]\([^)]*\)", "$1"),
            (r"<[^>\n]+>", " "),
            (r"(?mR)^\s{0,3}(#{1,6}|>+|[-*+]|\d+[.)])\s+", ""),
            (r"(?mR)^\s*\|?[\s:|-]+\|[\s:|-]*$", ""),
            (r"[*_~`|\\]", " "),
        ].into_iter().map(|(pattern, replacement)| {
            (Regex::new(&pattern.replace(r"\s", SPACE).replace(r"\d", "[0-9]")).unwrap(), replacement)
        }).collect()
    })
}

pub fn count_text(text: &str, prefs: StatsPrefs) -> TextCount {
    let mut t = text.to_string();
    if !prefs.markup {
        for (pattern, replacement) in patterns() {
            t = pattern.replace_all(&t, *replacement).into_owned();
        }
    }
    static NONSPACE: OnceLock<Regex> = OnceLock::new();
    if prefs.method == CountMethod::Chars {
        let whitespace = NONSPACE.get_or_init(|| Regex::new(SPACE).unwrap());
        return TextCount { cjk: whitespace.replace_all(&t, "").chars().count(), words: 0 };
    }
    static CJK: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let cjk = CJK.get_or_init(|| Regex::new(r"[\u{2e80}-\u{2fdf}\u{3000}-\u{303f}\u{3040}-\u{30ff}\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}\u{f900}-\u{faff}\u{fe30}-\u{fe4f}\u{ff00}-\u{ffef}\u{2014}\u{201c}\u{201d}\u{2026}]").unwrap());
    let words = WORD.get_or_init(|| Regex::new(r"[\p{L}\p{N}]+(?:['’.-][\p{L}\p{N}]+)*").unwrap());
    TextCount { cjk: cjk.find_iter(&t).count(), words: words.find_iter(&cjk.replace_all(&t, " ")).count() }
}

impl TextCount {
    pub fn minutes(self, prefs: StatsPrefs) -> usize {
        let p = prefs.normalized();
        (self.cjk as f64 / p.zh as f64 + self.words as f64 / p.en as f64).round().max(1.) as usize
    }

    pub fn label(self, method: CountMethod) -> String {
        let n = |v: usize| {
            let s = v.to_string();
            s.chars().enumerate().map(|(i, c)| if i > 0 && (s.len() - i) % 3 == 0 { format!(",{c}") } else { c.to_string() }).collect::<String>()
        };
        if method == CountMethod::Chars {
            return format!("{} {}", n(self.cjk), label("字符", "characters"));
        }
        let mut parts = vec![];
        if self.cjk > 0 { parts.push(format!("{} {}", n(self.cjk), label("字", "characters"))); }
        if self.words > 0 { parts.push(format!("{} {}", n(self.words), label("词", "words"))); }
        if parts.is_empty() { parts.push(format!("0 {}", label("字", "characters"))); }
        parts.join(" + ")
    }
}

/// Localized strings for this feature, without changing the shared catalogue.
pub fn label(zh: &'static str, en: &'static str) -> &'static str {
    if crate::i18n::language() == crate::Lang::En { en } else { zh }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingStats {
    pub count: TextCount,
    pub loaded_bytes: usize,
    pub total_bytes: Option<usize>,
}

impl ReadingStats {
    /// `None` means the total is unknown, not that the loaded text is complete.
    pub fn new(original: &str, total_bytes: Option<usize>, prefs: StatsPrefs) -> Self {
        Self { count: count_text(original, prefs), loaded_bytes: original.len(), total_bytes: total_bytes.filter(|n| *n >= original.len()) }
    }
    pub fn complete(self) -> bool {
        self.total_bytes == Some(self.loaded_bytes)
    }
    pub fn loaded_fraction(self) -> Option<f32> {
        self.total_bytes.map(|n| if n == 0 { 1. } else { (self.loaded_bytes as f32 / n as f32).clamp(0., 1.) })
    }
    pub fn count_label(self, p: StatsPrefs) -> String {
        format!("{}{}", if self.complete() { "" } else { label("已载入：", "Loaded: ") }, self.count.label(p.method))
    }
    pub fn time_label(self, p: StatsPrefs) -> String {
        let prefix = if self.complete() { label("约", "About") } else { label("已载入部分约", "Loaded text: about") };
        format!("{prefix} {} {}", self.count.minutes(p), label("分钟", "min"))
    }
    pub fn loaded_label(self) -> String {
        if self.complete() { return label("已全部载入", "Fully loaded").into(); }
        match self.loaded_fraction() {
            Some(f) => format!("{} {}%", label("已载入", "Loaded"), ((f * 100.).floor() as u32).min(99)),
            None => label("已载入部分 · 全文长度未知", "Partly loaded · total unknown").into(),
        }
    }
}

/// Scroll position within the available body, not a claim of how much was read.
pub fn reading_percent(offset: f32, max: f32) -> u8 {
    if !offset.is_finite() || !max.is_finite() { return 0; }
    if max <= 0. { return 100; }
    ((offset / max).clamp(0., 1.) * 100.).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phone_english_cjk_and_punctuation_rules() {
        let p = StatsPrefs::default();
        assert_eq!(count_text("It’s retrieval-augmented reading, v1.2! 中文，。“好”—…", p), TextCount { cjk: 9, words: 4 });
        assert_eq!(count_text("café αβ １２ あ 😀 𠀀", p), TextCount { cjk: 3, words: 3 });
        assert_eq!(count_text(&sample(), p), TextCount { cjk: 15, words: 3 });
    }
    #[test]
    fn phone_markdown_excludes_syntax_not_content() {
        let p = StatsPrefs::default();
        let md = "# **Hello**\n\n[world](https://example.test/path) ![图](image.png)\n\n| A | B |\n| --- | :--: |\n\n```rust\nlet x = 1;\n```\n> _中文_ <b>hi</b>";
        assert_eq!(count_text(md, p), TextCount { cjk: 3, words: 8 });
        let chars = StatsPrefs { method: CountMethod::Chars, ..p };
        assert_eq!(count_text("**A** [中](url) 😀", chars), TextCount { cjk: 3, words: 0 });
        assert_eq!(count_text("**A** [中](url) 😀", StatsPrefs { markup: true, ..chars }), TextCount { cjk: 14, words: 0 });
        assert_eq!(count_text("a\u{feff}\u{85}😀", chars).cjk, 3);
        assert_eq!(count_text("# Hi\r\n```rust\r\nlet x = 1;\r\n```\r\n中文", p), TextCount { cjk: 2, words: 4 });
        assert_eq!(count_text("[one](https://example.test)", p).words, 1);
        assert_eq!(count_text("[one](https://example.test)", StatsPrefs { markup: true, ..p }).words, 3);
    }
    #[test]
    fn loaded_counts_never_become_invented_whole_totals() {
        let p = StatsPrefs::default();
        let whole = ReadingStats::new("中文 Hello", Some(12), p);
        assert!(whole.complete());
        let partial = ReadingStats::new("中文 Hello", Some(1200), p);
        assert!(!partial.complete());
        assert_eq!(partial.count, whole.count);
        assert_eq!(partial.count.minutes(p), 1);
        assert_eq!(partial.loaded_fraction(), Some(0.01));
        let unknown = ReadingStats::new("中文 Hello", None, p);
        assert!(!unknown.complete());
        assert_eq!(unknown.loaded_fraction(), None);
        assert!(ReadingStats::new("", Some(0), p).complete());
        assert_eq!(ReadingStats::new("more than declared", Some(1), p).total_bytes, None);
    }
    #[test]
    fn speeds_and_preferences_round_trip_and_old_files_default() {
        assert_eq!(serde_json::from_str::<StatsPrefs>("{}").unwrap(), StatsPrefs::default());
        let p = StatsPrefs { markup: true, method: CountMethod::Chars, zh: 800, en: 150 };
        assert_eq!(serde_json::from_str::<StatsPrefs>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
        assert_eq!(TextCount { cjk: 800, words: 150 }.minutes(p), 2);
        assert_eq!(StatsPrefs { zh: 0, en: u32::MAX, ..p }.normalized(), StatsPrefs { zh: 400, en: 230, ..p });
        assert_eq!(serde_json::from_str::<StatsPrefs>(r#"{"method":"future"}"#).unwrap().method, CountMethod::Mixed);
    }
    #[test]
    fn reading_position_handles_short_text_and_overscroll() {
        assert_eq!(reading_percent(0., 0.), 100);
        assert_eq!(reading_percent(-10., 100.), 0);
        assert_eq!(reading_percent(50., 100.), 50);
        assert_eq!(reading_percent(110., 100.), 100);
    }
}
