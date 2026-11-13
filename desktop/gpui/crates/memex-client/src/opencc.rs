//! 简繁: display-only Simplified ⇄ Traditional conversion with the OpenCC
//! dictionaries the phone ships (mobile/web/assets/opencc, Apache-2.0; read at
//! build time, never copied). OpenCC-style maximal forward matching over the
//! phrase table, then the character table; the first candidate of a key wins.
//! Every mapping keeps the character count (checked by a test against the data),
//! so a converted string lines up with its source character by character — but
//! not byte by byte: callers never use converted offsets as source offsets.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

const ST_PHRASES: &str = include_str!("../../../../../mobile/web/assets/opencc/STPhrases.txt");
const ST_CHARACTERS: &str = include_str!("../../../../../mobile/web/assets/opencc/STCharacters.txt");
const TS_PHRASES: &str = include_str!("../../../../../mobile/web/assets/opencc/TSPhrases.txt");
const TS_CHARACTERS: &str = include_str!("../../../../../mobile/web/assets/opencc/TSCharacters.txt");
/// Shipped with the app beside the dictionaries' use.
pub const OPENCC_LICENSE: &str = include_str!("../../../../../mobile/web/assets/opencc/LICENSE-OpenCC.txt");

/// Which script the reader shows: the text as stored, or converted for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Script {
    #[default]
    Original,
    Simplified,
    Traditional,
}

impl Script {
    pub const ALL: [Script; 3] = [Script::Original, Script::Simplified, Script::Traditional];

    pub fn label(self) -> &'static str {
        crate::tx(match self {
            Script::Original => "原文",
            Script::Simplified => "简体",
            Script::Traditional => "繁体",
        })
    }
}

struct Table {
    phrases: HashMap<String, String>,
    chars: HashMap<char, char>,
    /// Longest phrase key, in characters.
    longest: usize,
}

impl Table {
    fn build(phrases: &'static str, chars: &'static str) -> Self {
        let entries = |src: &'static str| src.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).filter_map(|l| {
            let (k, v) = l.split_once('\t')?;
            Some((k, v.split(' ').next().unwrap_or(v)))
        });
        let mut table = Table { phrases: HashMap::new(), chars: HashMap::new(), longest: 1 };
        for (k, v) in entries(phrases) {
            if k.chars().count() == v.chars().count() {
                table.longest = table.longest.max(k.chars().count());
                table.phrases.entry(k.to_string()).or_insert_with(|| v.to_string());
            }
        }
        for (k, v) in entries(chars) {
            let (mut kc, mut vc) = (k.chars(), v.chars());
            if let (Some(a), None, Some(b), None) = (kc.next(), kc.next(), vc.next(), vc.next()) {
                table.chars.entry(a).or_insert(b);
            }
        }
        table
    }

    fn convert(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let mut key = String::new();
        while i < chars.len() {
            let mut matched = false;
            // Phrases only start at a character that is not ASCII (a cheap skip).
            if !chars[i].is_ascii() {
                for len in (2..=self.longest.min(chars.len() - i)).rev() {
                    key.clear();
                    key.extend(&chars[i..i + len]);
                    if let Some(v) = self.phrases.get(&key) {
                        out.push_str(v);
                        i += len;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                let c = chars[i];
                out.push(*self.chars.get(&c).unwrap_or(&c));
                i += 1;
            }
        }
        out
    }
}

fn s2t() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| Table::build(ST_PHRASES, ST_CHARACTERS))
}

fn t2s() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| Table::build(TS_PHRASES, TS_CHARACTERS))
}

/// `text` in `script` (unchanged for `Original`). Same number of characters.
pub fn convert(text: &str, script: Script) -> String {
    match script {
        Script::Original => text.to_string(),
        Script::Traditional => s2t().convert(text),
        Script::Simplified => t2s().convert(text),
    }
}

/// Byte offsets of each character boundary of `text` (0, …, len).
fn boundaries(text: &str) -> Vec<usize> {
    text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect()
}

/// Maps a byte offset of `source` to the same character position in `converted`
/// (the two have the same character count). Offsets inside a character round down.
pub fn map_offset(source: &str, converted: &str, offset: usize) -> usize {
    let from = boundaries(source);
    let to = boundaries(converted);
    let ix = match from.binary_search(&offset) { Ok(i) => i, Err(i) => i.saturating_sub(1) };
    to.get(ix).copied().unwrap_or(converted.len())
}

/// `text` converted, with byte `ranges` of `text` moved onto the converted string.
pub fn convert_with_ranges(text: &str, script: Script, ranges: &[std::ops::Range<usize>]) -> (String, Vec<std::ops::Range<usize>>) {
    let out = convert(text, script);
    if out.len() == text.len() && script == Script::Original { return (out, ranges.to_vec()); }
    let (from, to) = (boundaries(text), boundaries(&out));
    let map = |o: usize| { let ix = match from.binary_search(&o) { Ok(i) => i, Err(i) => i.saturating_sub(1) }; to.get(ix).copied().unwrap_or(out.len()) };
    let mapped = ranges.iter().map(|r| map(r.start)..map(r.end)).collect();
    (out, mapped)
}

/// The vertical presentation form of CJK punctuation (U+FE10–FE19, FE30–FE4F),
/// for 竖排; other characters are returned unchanged.
pub fn vertical_form(c: char) -> char {
    match c {
        '，' | ',' => '︐',
        '、' => '︑',
        '。' => '︒',
        '：' => '︓',
        '；' => '︔',
        '！' => '︕',
        '？' => '︖',
        '〖' => '︗',
        '〗' => '︘',
        '…' => '︙',
        '‥' => '︰',
        '—' | '―' => '︱',
        '–' => '︲',
        '（' | '(' => '︵',
        '）' | ')' => '︶',
        '｛' => '︷',
        '｝' => '︸',
        '〔' => '︹',
        '〕' => '︺',
        '【' => '︻',
        '】' => '︼',
        '《' => '︽',
        '》' => '︾',
        '〈' => '︿',
        '〉' => '﹀',
        '「' | '“' => '﹁',
        '」' | '”' => '﹂',
        '『' | '‘' => '﹃',
        '』' | '’' => '﹄',
        '［' => '﹇',
        '］' => '﹈',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first value of `key` in a dictionary file, read straight from the data.
    fn data(file: &str, key: &str) -> Option<String> {
        file.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
            let (k, v) = l.split_once('\t')?;
            (k == key).then(|| v.split(' ').next().unwrap_or(v).to_string())
        })
    }

    #[test]
    fn expectations_come_from_the_dictionaries() {
        // Phrase entries the data actually has (asymmetric: 发 differs by word).
        for word in ["头发", "发展", "皇后"] {
            if let Some(v) = data(ST_PHRASES, word) { assert_eq!(convert(word, Script::Traditional), v, "{word}"); }
        }
        // Single characters fall back to the character table's first candidate.
        let fa = data(ST_CHARACTERS, "发").expect("发 in STCharacters");
        assert_eq!(convert("发", Script::Traditional), fa);
        let hou = data(ST_CHARACTERS, "后").expect("后 in STCharacters");
        assert_eq!(convert("后", Script::Traditional), hou);
        // 头发 and 发展 really differ in the data, and the converter follows it.
        let (a, b) = (convert("头发", Script::Traditional), convert("发展", Script::Traditional));
        assert_ne!(a.chars().nth(1), b.chars().next(), "{a} / {b}");
        // 皇后 keeps 后 while 后来 / single 后 become 後 (whatever the data says).
        assert_eq!(convert("皇后", Script::Traditional), data(ST_PHRASES, "皇后").unwrap_or_else(|| format!("皇{hou}")));
        // Back: the traditional characters map to simplified ones.
        assert_eq!(convert("道德經", Script::Simplified), format!("道德{}", data(TS_CHARACTERS, "經").unwrap()));
        // Longest match wins over a shorter phrase inside it.
        assert_eq!(convert("一了百当", Script::Traditional), data(ST_PHRASES, "一了百当").unwrap());
        // Latin, digits and unknown characters stay; Original is the identity.
        assert_eq!(convert("Tao 1 🌹", Script::Traditional), "Tao 1 🌹");
        assert_eq!(convert("头发", Script::Original), "头发");
    }

    #[test]
    fn conversion_keeps_character_count_but_not_always_bytes() {
        let check = |src: &str| {
            for (k, v) in src.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).filter_map(|l| l.split_once('\t')) {
                assert_eq!(k.chars().count(), v.split(' ').next().unwrap().chars().count(), "{k}");
            }
        };
        for f in [ST_PHRASES, ST_CHARACTERS, TS_PHRASES, TS_CHARACTERS] { check(f); }
        let text = "道可道，非常道。**名**可名";
        let out = convert(text, Script::Traditional);
        assert_eq!(out.chars().count(), text.chars().count());
        // A bold range of the source lands on the same characters.
        let start = text.find("名").unwrap();
        let (conv, ranges) = convert_with_ranges(text, Script::Traditional, &[start..start + "名".len()]);
        assert_eq!(&conv[ranges[0].clone()], &convert("名", Script::Traditional));
        assert_eq!(map_offset(text, &conv, text.len()), conv.len());
    }

    #[test]
    fn vertical_punctuation() {
        let s: String = "道，可道。「名」（一）《經》！？".chars().map(vertical_form).collect();
        assert_eq!(s, "道︐可道︒﹁名﹂︵一︶︽經︾︕︖");
        assert_eq!(vertical_form('A'), 'A');
        assert_eq!(vertical_form('道'), '道');
    }
}
