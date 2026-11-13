//! Reading typeface for article text, AI 纪要 and titles. Sans-serif (PingFang SC,
//! whose Latin matches the system font) is the screen default; the serif (Songti SC)
//! is the print-like alternative; 寒蝉拙楷 (bundled, SIL OFL) is for 古籍. One
//! process-wide choice, like the interface language; an item may override it.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingFont {
    #[default]
    Sans,
    Serif,
    /// 寒蝉拙楷 (bundled with the app).
    Kai,
}

/// The family name the bundled 寒蝉拙楷 registered under (set at startup from the
/// text system's own list; the name in the font file until then).
static KAI_FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub const KAI_FAMILY_IN_FILE: &str = "寒蝉拙楷体 R";

pub fn set_kai_family(name: String) {
    let _ = KAI_FAMILY.set(name);
}

impl ReadingFont {
    pub fn family(self) -> &'static str {
        match self {
            ReadingFont::Sans => "PingFang SC",
            ReadingFont::Serif => "Songti SC",
            ReadingFont::Kai => KAI_FAMILY.get().map(String::as_str).unwrap_or(KAI_FAMILY_IN_FILE),
        }
    }

    pub fn label(self) -> &'static str {
        crate::tx(match self {
            ReadingFont::Sans => "无衬线（屏幕）",
            ReadingFont::Serif => "衬线（印刷）",
            ReadingFont::Kai => "寒蝉拙楷",
        })
    }

    /// The next choice: 无衬线 → 衬线 → 寒蝉拙楷 → 无衬线.
    pub fn other(self) -> Self {
        match self {
            ReadingFont::Sans => ReadingFont::Serif,
            ReadingFont::Serif => ReadingFont::Kai,
            ReadingFont::Kai => ReadingFont::Sans,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_reading_font(font: ReadingFont) {
    CURRENT.store(match font { ReadingFont::Sans => 0, ReadingFont::Serif => 1, ReadingFont::Kai => 2 }, Ordering::SeqCst);
}

pub fn reading_font() -> ReadingFont {
    match CURRENT.load(Ordering::SeqCst) { 1 => ReadingFont::Serif, 2 => ReadingFont::Kai, _ => ReadingFont::Sans }
}

/// One item's 古籍 display choices, kept locally per item: 简繁, 竖排 and an
/// explicit typeface. Display only; the item's text is never changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemStyle {
    pub script: crate::opencc::Script,
    pub vertical: bool,
    /// Chosen for this item; `None` follows the app's choice (寒蝉拙楷 in 竖排).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<ReadingFont>,
}

impl ItemStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The typeface this item is drawn with.
    pub fn font(&self, app: ReadingFont) -> ReadingFont {
        self.font.unwrap_or(if self.vertical { ReadingFont::Kai } else { app })
    }
}

/// The family to draw reading text with right now.
pub fn reading_family() -> &'static str {
    reading_font().family()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl ReadingSize {
    pub fn pixels(self) -> f32 {
        match self { Self::Small => 16., Self::Medium => 18., Self::Large => 20., Self::ExtraLarge => 22. }
    }

    pub fn next(self) -> Self {
        match self { Self::Small => Self::Medium, Self::Medium => Self::Large, Self::Large => Self::ExtraLarge, Self::ExtraLarge => Self::Small }
    }
}

/// Typeface of notes (注释): Kaiti by default so notes stand apart from the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteFont {
    #[default]
    Kai,
    Song,
    /// Same as the body text.
    Body,
}

impl NoteFont {
    pub const ALL: [NoteFont; 3] = [NoteFont::Kai, NoteFont::Song, NoteFont::Body];

    pub fn family(self) -> &'static str {
        match self {
            NoteFont::Kai => "Kaiti SC",
            NoteFont::Song => "Songti SC",
            NoteFont::Body => reading_family(),
        }
    }

    pub fn label(self) -> &'static str {
        crate::tx(match self {
            NoteFont::Kai => "楷体",
            NoteFont::Song => "宋体",
            NoteFont::Body => "与正文相同",
        })
    }
}

static NOTE: AtomicU8 = AtomicU8::new(0);

pub fn set_note_font(font: NoteFont) {
    NOTE.store(match font { NoteFont::Kai => 0, NoteFont::Song => 1, NoteFont::Body => 2 }, Ordering::SeqCst);
}

pub fn note_font() -> NoteFont {
    match NOTE.load(Ordering::SeqCst) { 1 => NoteFont::Song, 2 => NoteFont::Body, _ => NoteFont::Kai }
}

/// How speakers' names are shown (the 「谁在说」 bubble and the speaker label at
/// the start of a translated paragraph). Display only; no text is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerNames {
    /// The name as the original writes it (「Thorsten」).
    #[default]
    Original,
    /// The Chinese reference: the name as the translation writes it (「托尔斯滕」).
    Translated,
    /// 「Thorsten（托尔斯滕）」.
    Both,
}

impl SpeakerNames {
    pub const ALL: [SpeakerNames; 3] = [SpeakerNames::Original, SpeakerNames::Translated, SpeakerNames::Both];

    pub fn label(self) -> &'static str {
        crate::tx(match self {
            SpeakerNames::Original => "原文名（默认）",
            SpeakerNames::Translated => "中文参考名",
            SpeakerNames::Both => "原文名（中文名）",
        })
    }
}

static SPEAKERS: AtomicU8 = AtomicU8::new(0);

pub fn set_speaker_names(names: SpeakerNames) {
    SPEAKERS.store(match names { SpeakerNames::Original => 0, SpeakerNames::Translated => 1, SpeakerNames::Both => 2 }, Ordering::SeqCst);
}

pub fn speaker_names() -> SpeakerNames {
    match SPEAKERS.load(Ordering::SeqCst) { 1 => SpeakerNames::Translated, 2 => SpeakerNames::Both, _ => SpeakerNames::Original }
}

/// A note block: starts with a circled number (① … ⑳) or a Markdown footnote
/// definition (`[^x]:`). Display only; never used for pairing.
pub fn is_note_text(text: &str) -> bool {
    let t = text.trim_start();
    t.chars().next().is_some_and(|c| ('\u{2460}'..='\u{2473}').contains(&c))
        || (t.starts_with("[^") && t.find("]:").is_some_and(|i| i > 2 && !t[2..i].contains(char::is_whitespace)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_circled_numbers_or_footnote_definitions() {
        assert!(is_note_text("① 这句话模仿爱伦·坡"));
        assert!(is_note_text("  ⑳ last"));
        assert!(is_note_text("[^1]: 合成注释。"));
        assert!(!is_note_text("洛丽塔，我生命之光①"));
        assert!(!is_note_text("[^bad id]: x"));
        assert!(!is_note_text("[link](x)"));
        assert_eq!(NoteFont::default(), NoteFont::Kai);
        assert_eq!(serde_json::from_str::<NoteFont>("\"song\"").unwrap(), NoteFont::Song);
    }

    #[test]
    fn default_is_sans_and_unknown_pref_values_are_rejected_not_guessed() {
        assert_eq!(ReadingFont::default(), ReadingFont::Sans);
        assert_eq!(serde_json::from_str::<ReadingFont>("\"serif\"").unwrap(), ReadingFont::Serif);
        assert!(serde_json::from_str::<ReadingFont>("\"comic\"").is_err());
        assert_eq!(ReadingFont::Sans.family(), "PingFang SC");
        assert_eq!(ReadingFont::Serif.other(), ReadingFont::Kai);
        assert_eq!(ReadingFont::Kai.other(), ReadingFont::Sans);
        assert_eq!(serde_json::from_str::<ReadingFont>("\"kai\"").unwrap(), ReadingFont::Kai);
    }

    #[test]
    fn vertical_defaults_to_kai_unless_chosen() {
        let mut s = ItemStyle::default();
        assert!(s.is_default());
        assert_eq!(s.font(ReadingFont::Serif), ReadingFont::Serif);
        s.vertical = true;
        assert_eq!(s.font(ReadingFont::Serif), ReadingFont::Kai);
        s.font = Some(ReadingFont::Sans);
        assert_eq!(s.font(ReadingFont::Serif), ReadingFont::Sans);
        // Stored compactly; older files without the fields read as defaults.
        assert_eq!(serde_json::to_string(&ItemStyle { script: crate::opencc::Script::Traditional, vertical: true, font: None }).unwrap(), r#"{"script":"traditional","vertical":true}"#);
        assert_eq!(serde_json::from_str::<ItemStyle>("{}").unwrap(), ItemStyle::default());
    }
}
