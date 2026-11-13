//! Explicit speaker labels and the version-bound speaker view of a chapter
//! translation. Detection is conservative and byte-exact: a label is the leading
//! `Name:` (optionally `**Name:**`) of a block, kept as it appears in the source.
//! No identity is inferred from paragraph counts, order or substring search.
use super::*;

/// The exact source label of one block, if it has the shape of a speaker label.
pub fn shape(block: &str) -> Option<&str> {
    let body = block.strip_prefix("**").unwrap_or(block);
    let colon = body.find(':')?;
    let name = &body[..colon];
    let after = body[colon + 1..].strip_prefix("**").unwrap_or(&body[colon + 1..]);
    let words: Vec<&str> = name.split(' ').collect();
    let first = name.chars().next()?;
    (name.len() <= 48
        && (1..=4).contains(&words.len())
        && words.iter().all(|w| !w.is_empty() && w.chars().all(|c| c.is_alphabetic() || matches!(c, '.' | '\'' | '-')))
        && first.is_alphabetic()
        && (!first.is_ascii() || first.is_ascii_uppercase())
        && (after.is_empty() || after.starts_with([' ', '\n'])))
    .then_some(name)
}

/// Labels of a whole chapter/document, block by block. A single-word label must
/// recur at least twice to count, so one-off `Note:` prose is not a speaker.
pub fn labels<'a>(blocks: impl IntoIterator<Item = &'a str>) -> Vec<Option<String>> {
    let found: Vec<Option<&str>> = blocks.into_iter().map(shape).collect();
    let mut counts = BTreeMap::<&str, usize>::new();
    for name in found.iter().flatten() {
        *counts.entry(name).or_default() += 1;
    }
    found
        .iter()
        .map(|n| n.filter(|n| n.contains(' ') || counts[n] >= 2).map(str::to_owned))
        .collect()
}

/// Whether a translated block keeps the exact source label (ASCII or full-width colon).
pub fn preserved(label: &str, translated: &str) -> bool {
    let body = translated.trim_start();
    let body = body.strip_prefix("**").unwrap_or(body);
    body.strip_prefix(label).is_some_and(|rest| {
        let rest = rest.strip_prefix("**").unwrap_or(rest);
        rest.starts_with(':') || rest.starts_with('：')
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Speaker {
    pub block_id: String,
    pub source_range: [usize; 2],
    pub target_range: Option<[usize; 2]>,
    /// Exact source spelling; never an invented or transliterated name.
    pub original: String,
    /// Chinese display reference; null when no reliable reference exists.
    pub reference: Option<String>,
    /// `verified` (label kept inside a recorded, still-bound block) or `unavailable`.
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct View {
    pub item_id: String,
    pub revision_id: String,
    pub original_chapter: String,
    pub translation_version: String,
    pub translation_sha256: String,
    pub schema: String,
    /// `verified_recorded_correspondence`; references are never model-inferred here.
    pub basis: String,
    /// `available` if at least one label verified, otherwise `unavailable`.
    pub status: String,
    pub speakers: Vec<Speaker>,
}

/// Derived read: only labels inside recorded (and, after owner edits, still proven)
/// block correspondence of this exact effective version. No write, no model call.
pub fn read(library: &Path, item: &str, revision: &str, chapter: &str, expected_version: &str, ceiling: Sensitivity) -> Result<View> {
    with_consistent_snapshot(library, |root| {
        let source = source_locked(root, item, revision, chapter, ceiling)?;
        let (p, m) = effective(root, &source, ceiling)?.context("reader_translation_unavailable")?;
        anyhow::ensure!(m.version == expected_version, "stale_reader_translation");
        let rows: Vec<(String, [usize; 2], Option<[usize; 2]>)> = match p.owner.as_deref() {
            Some(v) => v.blocks.iter().flatten().map(|b| (b.id.clone(), b.source_range, b.target_range)).collect(),
            None => p.blocks.iter().flatten().map(|b| (b.id.clone(), b.source_range, Some(b.target_range))).collect(),
        };
        let found = labels(rows.iter().map(|(_, [s, e], _)| &source.markdown[*s..*e]));
        let speakers: Vec<Speaker> = rows
            .iter()
            .zip(found)
            .filter_map(|((id, o, t), label)| {
                let label = label?;
                let ok = t.is_some_and(|[s, e]| preserved(&label, &p.markdown[s..e]));
                Some(Speaker {
                    block_id: id.clone(),
                    source_range: *o,
                    target_range: t.filter(|_| ok),
                    original: label,
                    reference: None,
                    status: if ok { "verified" } else { "unavailable" }.into(),
                })
            })
            .collect();
        Ok(View {
            item_id: item.into(),
            revision_id: revision.into(),
            original_chapter: chapter.into(),
            translation_version: m.version,
            translation_sha256: m.sha256,
            schema: "reader-speaker-names-v1".into(),
            basis: "verified_recorded_correspondence".into(),
            status: if speakers.iter().any(|s| s.status == "verified") { "available" } else { "unavailable" }.into(),
            speakers,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_exact_conservative_and_never_invented() {
        assert_eq!(shape("Lex Fridman: hello"), Some("Lex Fridman"));
        assert_eq!(shape("**Hélène Dupont:** bonjour"), Some("Hélène Dupont"));
        assert_eq!(shape("Чайковский: да"), Some("Чайковский"));
        for no in ["note: lower", "http://x: y", "A very long label with too many words: x", "Time 12:30 here", "Lex(1): x"] {
            assert_eq!(shape(no), None, "{no}");
        }
        let l = labels(["Note: once", "Host: hi", "Host: again", "Sam Altman: yes"]);
        assert_eq!(l, [None, Some("Host".into()), Some("Host".into()), Some("Sam Altman".into())]);
        assert!(preserved("Sam Altman", "Sam Altman：是的"));
        assert!(preserved("Sam Altman", "**Sam Altman:** 是的"));
        assert!(!preserved("Sam Altman", "萨姆·奥尔特曼：是的"));
        assert!(!preserved("Sam", "Samuel: x"));
    }
}
