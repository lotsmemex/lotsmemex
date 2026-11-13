//! Long-read navigation, pure: the 重点 switch, the three knobs' stops and
//! 上次读到, the dial cards' context, bookmark snapping, speakers, timestamps
//! and the mark shortcuts. Positions are list rows, never pixels saved anywhere.

use memex_client::reader_state::MarkStyle;
use memex_client::SpeakerNames;

/// 重点 · 全部 / AI / 手工 / 关.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Ai,
    Mine,
    Off,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Ai, Filter::Mine, Filter::Off];
    pub fn shows_ai(self) -> bool {
        matches!(self, Filter::All | Filter::Ai)
    }
    pub fn shows_mine(self) -> bool {
        matches!(self, Filter::All | Filter::Mine)
    }
    /// The button's suffix: 重点 · 全部.
    pub fn short(self) -> &'static str {
        match self {
            Filter::All => "全部",
            Filter::Ai => "AI",
            Filter::Mine => "手工",
            Filter::Off => "关",
        }
    }
    /// The menu option.
    pub fn option(self) -> &'static str {
        match self {
            Filter::All => "都显示",
            Filter::Ai => "只看 AI",
            Filter::Mine => "只看手工",
            Filter::Off => "都不显示",
        }
    }
    /// Count shown beside the option (`None`: 都不显示 has none).
    pub fn count(self, ai: usize, mine: usize) -> Option<usize> {
        match self {
            Filter::All => Some(ai + mine),
            Filter::Ai => Some(ai),
            Filter::Mine => Some(mine),
            Filter::Off => None,
        }
    }
    /// After a new manual mark: a switch that hides it goes back to 全部.
    pub fn after_mark(self) -> Filter {
        if self.shows_mine() { self } else { Filter::All }
    }
}

/// The four places ▲ / ▼ move between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Minutes,
    Start,
    End,
    Chat,
}

impl Stop {
    pub const ALL: [Stop; 4] = [Stop::Minutes, Stop::Start, Stop::End, Stop::Chat];
    pub fn label(self) -> &'static str {
        match self {
            Stop::Minutes => "纪要",
            Stop::Start => "正文开头",
            Stop::End => "正文结尾",
            Stop::Chat => "AI 对话记录",
        }
    }
}

/// ▲ (`up`) / ▼ from position `cur`, with the stops' positions in the same
/// units (ascending). On a stop it moves to the neighbour; from mid-reading it
/// moves to the nearest stop that way and the place left becomes 上次读到 (`true`).
pub fn step(cur: f32, stops: [f32; 4], up: bool) -> (Stop, bool) {
    let near = |v: f32| (v - cur).abs() < 0.5;
    match stops.iter().rposition(|v| near(*v)) {
        Some(at) => {
            let to = if up { at.saturating_sub(1) } else { (at + 1).min(3) };
            (Stop::ALL[to], false)
        }
        None => {
            let to = if up { stops.iter().rposition(|v| *v < cur).unwrap_or(0) } else { stops.iter().position(|v| *v > cur).unwrap_or(3) };
            (Stop::ALL[to], true)
        }
    }
}

/// Percent of the way through `rows` at row `row`.
pub fn percent(row: usize, rows: usize) -> u8 {
    if rows <= 1 { return 0; }
    ((row.min(rows - 1) as f32 / (rows - 1) as f32) * 100.).round() as u8
}

/// The dial card's context: up to `n` characters of `text` before byte `at`,
/// led by 「…」 when cut. Never splits a character.
pub fn context_before(text: &str, at: usize, n: usize) -> String {
    let at = (0..=at.min(text.len())).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    let before = text[..at].trim_start();
    let count = before.chars().count();
    if count <= n { return before.to_string(); }
    format!("…{}", before.chars().skip(count - n).collect::<String>())
}

/// The keyword on a card: at most `n` characters, 「…」 when longer.
pub fn keyword(text: &str, n: usize) -> String {
    if text.chars().count() <= n { text.to_string() } else { format!("{}…", text.chars().take(n).collect::<String>()) }
}

/// Where a dragged bookmark lands: the row (of `starts`: row and the window y of
/// its paragraph start) whose start is nearest `y`.
pub fn snap(starts: &[(usize, f32)], y: f32) -> Option<usize> {
    starts.iter().min_by(|a, b| (a.1 - y).abs().total_cmp(&(b.1 - y).abs())).map(|s| s.0)
}

/// The speaker of a paragraph that starts with 「名字:」 / 「名字：」 (not a time).
pub fn speaker(text: &str) -> Option<&str> {
    let end = text.find([':', '：'])?;
    let name = text[..end].trim();
    let ok = !name.is_empty()
        && name.chars().count() <= 24
        && name.split_whitespace().count() <= 3
        && name.chars().next().is_some_and(char::is_alphabetic)
        && name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '.' || c == '·' || c == '-');
    ok.then_some(name)
}

/// Labels that are never a speaker (notes, Q/A, section words), as on the phone.
const NOT_SPEAKER: [&str; 22] = ["注", "按", "译注", "原注", "问", "答", "原文", "译文", "摘要", "关键词", "关键字", "题记", "附", "附注", "说明", "提示",
    "Note", "Notes", "Abstract", "Keywords", "Summary", "Q"];

fn label(text: &str) -> Option<&str> {
    speaker(text).filter(|n| !NOT_SPEAKER.contains(n) && *n != "A")
}

/// Speakers of the translation side, shown under their ORIGINAL names: a
/// translator may render 「Thorsten」 as 「托尔斯滕」.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslatedSpeakers {
    /// Translated labels at the start of at least two translation paragraphs.
    pub speakers: std::collections::HashSet<String>,
    /// Translated name → original name, only where every pairing agrees.
    pub names: std::collections::HashMap<String, String>,
    /// Original name → translated name, only where every pairing agrees.
    pub back: std::collections::HashMap<String, String>,
}

/// 「Thorsten」, 「托尔斯滕」 or 「Thorsten（托尔斯滕）」: a missing side shows the other.
fn compose(original: Option<&str>, translated: Option<&str>, mode: SpeakerNames) -> Option<String> {
    let s = match (mode, original, translated) {
        (SpeakerNames::Both, Some(o), Some(t)) if o != t => format!("{o}（{t}）"),
        (SpeakerNames::Translated, _, Some(t)) => t.to_string(),
        (_, Some(o), _) => o.to_string(),
        (_, None, t) => t?.to_string(),
    };
    Some(s)
}

/// Where the speaker's name sits in a paragraph's shown text.
pub fn speaker_range(text: &str) -> Option<std::ops::Range<usize>> {
    let name = label(text)?;
    let start = name.as_ptr() as usize - text.as_ptr() as usize;
    Some(start..start + name.len())
}

impl TranslatedSpeakers {
    /// Translated names → original names, only from `pairs`: the (original,
    /// translation) block texts of rows of an already recorded and validated
    /// one-to-one block correspondence. A name maps only when both blocks of a
    /// row carry an explicit label and every such row agrees. Paragraph counts or
    /// order are never used. `translation` (every translation paragraph) only
    /// decides which labels are speakers (named at least twice).
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>, translation: impl IntoIterator<Item = &'a str>) -> Self {
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        for name in translation.into_iter().filter_map(label) { *counts.entry(name.to_string()).or_default() += 1; }
        let mut names: std::collections::HashMap<String, String> = Default::default();
        let mut back: std::collections::HashMap<String, String> = Default::default();
        let (mut bad, mut bad_back) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for (o, t) in pairs {
            let (Some(o), Some(t)) = (label(o), label(t)) else { continue };
            match names.get(t) {
                Some(was) if was != o => { bad.insert(t.to_string()); }
                _ => { names.insert(t.to_string(), o.to_string()); }
            }
            match back.get(o) {
                Some(was) if was != t => { bad_back.insert(o.to_string()); }
                _ => { back.insert(o.to_string(), t.to_string()); }
            }
        }
        names.retain(|t, _| !bad.contains(t));
        back.retain(|o, _| !bad_back.contains(o));
        TranslatedSpeakers { speakers: counts.into_iter().filter(|(_, c)| *c >= 2).map(|(k, _)| k).collect(), names, back }
    }

    /// A translation-side speaker of `text` with no reliable original name (the
    /// label stays as written; the UI says the reference is unavailable).
    pub fn unmapped_t(&self, text: &str) -> bool {
        label(text).is_some_and(|n| self.speakers.contains(n) && !self.names.contains_key(n))
    }

    /// An original-side speaker with no reliable translated name.
    pub fn unmapped_o(&self, name: &str) -> bool {
        !self.back.contains_key(name)
    }

    /// The translation-side speaker of a paragraph led by `text`, as `mode` shows it.
    pub fn shown_t(&self, text: &str, mode: SpeakerNames) -> Option<String> {
        let name = label(text).filter(|n| self.speakers.contains(*n))?;
        compose(self.names.get(name).map(String::as_str), Some(name), mode)
    }

    /// An original-side speaker `name`, as `mode` shows it.
    pub fn shown_o(&self, name: &str, mode: SpeakerNames) -> String {
        compose(Some(name), self.back.get(name).map(String::as_str), mode).unwrap_or_else(|| name.to_string())
    }

    /// The display-only replacement of a translation paragraph's leading speaker
    /// label: where the name is in `text` and what to show instead (`None` when
    /// it stays as written).
    pub fn relabel_t(&self, text: &str, mode: SpeakerNames) -> Option<(std::ops::Range<usize>, String)> {
        let at = speaker_range(text)?;
        let shown = self.shown_t(text, mode)?;
        (shown != text[at.clone()]).then_some((at, shown))
    }
}

/// First letter of a speaker for the bubble.
pub fn initial(name: &str) -> String {
    name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default()
}

/// A paragraph led by a timestamp (`12:40`, `1:02:03`, `[12:40]`): the time and the rest.
pub fn timestamp(text: &str) -> Option<(String, String)> {
    let t = text.trim_start().trim_start_matches('[');
    let len = t.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(t.len());
    let time = &t[..len];
    let parts: Vec<&str> = time.split(':').collect();
    let valid = (2..=3).contains(&parts.len()) && parts.iter().enumerate().all(|(i, p)| !p.is_empty() && p.len() <= 2 && (i == 0 || p.len() == 2));
    if !valid { return None; }
    let rest = t[len..].trim_start_matches(']').trim_start_matches([' ', '-', '—', '·', '：', ':']).trim();
    Some((time.to_string(), keyword(rest, 40)))
}

/// What a key press marks: ⌘B 加粗, ⌘I 标红, ` 标蓝, ⌘U 下划线, ⌘Y 高亮 —
/// only with text selected in the reading area and no text field focused.
pub fn mark_for_key(key: &str, cmd: bool, other_modifiers: bool, has_selection: bool, input_focused: bool) -> Option<MarkStyle> {
    if !has_selection || input_focused || other_modifiers { return None; }
    match (key, cmd) {
        ("b", true) => Some(MarkStyle::Bold),
        ("i", true) => Some(MarkStyle::Red),
        ("`", false) => Some(MarkStyle::Blue),
        ("u", true) => Some(MarkStyle::Underline),
        ("y", true) => Some(MarkStyle::Yellow),
        _ => None,
    }
}

/// The shortcut shown under a mark button.
pub fn shortcut(style: MarkStyle) -> &'static str {
    match style {
        MarkStyle::Bold => "⌘B",
        MarkStyle::Red => "⌘I",
        MarkStyle::Blue => "`",
        MarkStyle::Underline => "⌘U",
        MarkStyle::Yellow => "⌘Y",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_counts_and_returns_to_all_after_a_mark() {
        assert_eq!(Filter::All.count(7, 3), Some(10));
        assert_eq!(Filter::Ai.count(7, 3), Some(7));
        assert_eq!(Filter::Mine.count(7, 3), Some(3));
        assert_eq!(Filter::Off.count(7, 3), None);
        assert!(Filter::Ai.shows_ai() && !Filter::Ai.shows_mine() && !Filter::Off.shows_ai());
        assert_eq!(Filter::Ai.after_mark(), Filter::All);
        assert_eq!(Filter::Off.after_mark(), Filter::All);
        assert_eq!(Filter::Mine.after_mark(), Filter::Mine);
    }

    #[test]
    fn knobs_move_between_the_four_stops_and_remember_where_reading_left_off() {
        let stops = [0., 1., 40., 41.];
        // From mid-reading: ▼ goes to 正文结尾 and the place is kept as 上次读到.
        assert_eq!(step(17.3, stops, false), (Stop::End, true));
        assert_eq!(step(17.3, stops, true), (Stop::Start, true));
        // On a stop: the neighbour, nothing remembered.
        assert_eq!(step(1., stops, false), (Stop::End, false));
        assert_eq!(step(40., stops, false), (Stop::Chat, false));
        assert_eq!(step(41., stops, false), (Stop::Chat, false));
        assert_eq!(step(0., stops, true), (Stop::Minutes, false));
        assert_eq!(step(1., stops, true), (Stop::Minutes, false));
        assert_eq!(Stop::ALL.map(Stop::label), ["纪要", "正文开头", "正文结尾", "AI 对话记录"]);
        assert_eq!(percent(0, 41), 0);
        assert_eq!(percent(20, 41), 50);
        assert_eq!(percent(40, 41), 100);
    }

    #[test]
    fn dial_cards_take_the_words_before_the_keyword_without_splitting_characters() {
        let text = "我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本。";
        let at = text.find("成本").unwrap();
        assert_eq!(context_before(text, at, 16), "…没有上限的时候，大家根本不会去想");
        assert_eq!(context_before(text, text.find("上限").unwrap(), 16), "我们一开始就定了");
        assert_eq!(context_before("a🌹b", 2, 16), "a");
        assert_eq!(keyword("Retrieval-augmented generation", 12), "Retrieval-au…");
        assert_eq!(keyword("成本", 12), "成本");
    }

    #[test]
    fn a_dragged_bookmark_snaps_to_the_nearest_paragraph_start() {
        let starts = [(3, 100.), (4, 180.), (5, 260.)];
        assert_eq!(snap(&starts, 175.), Some(4));
        assert_eq!(snap(&starts, 241.), Some(5));
        assert_eq!(snap(&starts, -20.), Some(3));
        assert_eq!(snap(&[], 10.), None);
    }

    #[test]
    fn speakers_and_timestamps_are_read_from_paragraph_starts() {
        assert_eq!(speaker("Quinn: we set a cap"), Some("Quinn"));
        assert_eq!(speaker("主持人：我们开始"), Some("主持人"));
        assert_eq!(speaker("12:40 cheaper models"), None);
        assert_eq!(speaker("Note that the ratio: 3 to 1 holds"), None);
        assert_eq!(speaker("plain text"), None);
        assert_eq!(initial("thorsten"), "T");
        assert_eq!(timestamp("12:40 便宜模型为什么最后更贵"), Some(("12:40".into(), "便宜模型为什么最后更贵".into())));
        assert_eq!(timestamp("[1:02:03] Orbs"), Some(("1:02:03".into(), "Orbs".into())));
        assert_eq!(timestamp("3:4 nope"), None);
        assert_eq!(timestamp("Quinn: hi"), None);
    }

    #[test]
    fn bold_speaker_labels_are_read_as_their_shown_text() {
        let b = crate::model::blocks("**Quinn:** we set a cap\n\n**Thorsten**: three months\n\n**托尔斯滕：** 三个月前\n");
        assert_eq!(b.iter().map(|b| speaker(&b.text)).collect::<Vec<_>>(), vec![Some("Quinn"), Some("Thorsten"), Some("托尔斯滕")]);
    }

    #[test]
    fn translated_speakers_map_only_through_recorded_one_to_one_rows() {
        let (orig, tr, both) = (SpeakerNames::Original, SpeakerNames::Translated, SpeakerNames::Both);
        let t = ["奎因：甲", "托尔斯滕：乙", "更多", "奎因：丙", "托尔斯滕：丁", "注：译者的话"];
        // Recorded rows: each translated label beside its original label, every time.
        let rows = [("Quinn: a", "奎因：甲"), ("Thorsten: b", "托尔斯滕：乙"), ("more", "更多"), ("Quinn: c", "奎因：丙"), ("Thorsten: d", "托尔斯滕：丁")];
        let s = TranslatedSpeakers::from_pairs(rows, t);
        assert_eq!(s.shown_t("托尔斯滕：乙", orig), Some("Thorsten".into()));
        assert_eq!(s.shown_t("奎因：丙", tr), Some("奎因".into()));
        assert_eq!(s.shown_t("奎因：丙", both), Some("Quinn（奎因）".into()));
        assert_eq!(s.shown_o("Thorsten", orig), "Thorsten");
        assert_eq!(s.shown_o("Thorsten", tr), "托尔斯滕");
        assert_eq!(s.shown_o("Thorsten", both), "Thorsten（托尔斯滕）");
        assert!(!s.unmapped_t("奎因：丙"));
        assert_eq!(s.shown_t("注：译者的话", orig), None, "a note is not a speaker");
        assert_eq!(s.shown_t("更多", orig), None);
        assert_eq!(s.relabel_t("托尔斯滕：乙", orig), Some((0.."托尔斯滕".len(), "Thorsten".into())));
        assert_eq!(s.relabel_t("托尔斯滕：乙", tr), None);
        // A name seen once is not a speaker.
        assert_eq!(TranslatedSpeakers::from_pairs(rows, ["奎因：甲", "托尔斯滕：乙"]).shown_t("奎因：甲", orig), None);
    }

    #[test]
    fn same_paragraph_counts_or_missing_rows_never_map_a_speaker() {
        let orig = SpeakerNames::Original;
        // Same number of labelled paragraphs on both sides, in another order, and no
        // recorded correspondence: nothing maps; each label stays as written.
        let t = ["托尔斯滕：乙", "奎因：甲", "托尔斯滕：丁", "奎因：丙"];
        let s = TranslatedSpeakers::from_pairs([], t);
        assert!(s.names.is_empty() && s.back.is_empty());
        assert_eq!(s.shown_t("托尔斯滕：乙", orig), Some("托尔斯滕".into()));
        assert!(s.unmapped_t("托尔斯滕：乙") && s.unmapped_o("Thorsten"));
        assert_eq!(s.relabel_t("托尔斯滕：乙", orig), None, "the label is not rewritten");
        assert_eq!(s.shown_o("Thorsten", SpeakerNames::Translated), "Thorsten");
        // A recorded row without a label on one side tells nothing.
        let s = TranslatedSpeakers::from_pairs([("Then a pause.", "托尔斯滕：乙"), ("Thorsten: b", "他说完了")], t);
        assert!(s.names.is_empty());
        // Rows that disagree (「奎因」 beside Quinn, then beside Thorsten): no mapping
        // for it, nor a reverse name for Thorsten (beside two names).
        let s = TranslatedSpeakers::from_pairs([("Quinn: a", "奎因：甲"), ("Thorsten: b", "奎因：乙"), ("Thorsten: c", "托尔斯滕：丙")], ["奎因：甲", "奎因：乙", "托尔斯滕：丙", "托尔斯滕：丁"]);
        assert_eq!(s.names.get("奎因"), None);
        assert_eq!(s.shown_t("奎因：甲", orig), Some("奎因".into()));
        assert_eq!(s.shown_t("托尔斯滕：丙", orig), Some("Thorsten".into()));
        assert_eq!(s.shown_o("Thorsten", SpeakerNames::Translated), "Thorsten");
    }

    #[test]
    fn mark_shortcuts_work_only_with_a_reading_selection_outside_text_fields() {
        assert_eq!(mark_for_key("b", true, false, true, false), Some(MarkStyle::Bold));
        assert_eq!(mark_for_key("i", true, false, true, false), Some(MarkStyle::Red));
        assert_eq!(mark_for_key("`", false, false, true, false), Some(MarkStyle::Blue));
        assert_eq!(mark_for_key("u", true, false, true, false), Some(MarkStyle::Underline));
        assert_eq!(mark_for_key("y", true, false, true, false), Some(MarkStyle::Yellow));
        // No selection, a focused text field, or extra modifiers: the key is left alone.
        assert_eq!(mark_for_key("b", true, false, false, false), None);
        assert_eq!(mark_for_key("`", false, false, true, true), None);
        assert_eq!(mark_for_key("b", true, true, true, false), None);
        assert_eq!(mark_for_key("b", false, false, true, false), None);
    }
}
