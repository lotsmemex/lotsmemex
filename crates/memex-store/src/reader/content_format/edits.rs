//! Pure, source-bound edit validation; no Store, provider, or publication authority.
//!
//! Classification is a TRUSTED CALLER input, not model output to deserialize into
//! authority. The caller resolves ambiguity and verifies Evidence ownership and
//! semantic support first. This module checks anchors/structure, not semantic truth,
//! model confidence, secret detection, or whether a short correction is correct.
//! Deletion currently supports only standalone closed-vocabulary controls in plain
//! top-level paragraphs. Other furniture (including ads/account widgets) stays unavailable;
//! a classification alone cannot authorize arbitrary prose deletion.
//! Corrections likewise require plain top-level paragraphs; Markdown containers
//! remain protected while unrelated headings/notes do not exclude the whole target.
//! Revisions, target identity, policy/model/profile/prompt and publication belong in
//! the later immutable Store proof envelope. Keep this result and its text private.

use crate::reader::paragraphs;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_EDITS: usize = 65536;
pub const MAX_CORRECTIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Whitespace,
    ParagraphBoundary,
    GarbageDelete,
    Typo,
    ExtractionGarble,
    Punctuation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Deterministic,
    SourceVerified,
    ContextInferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Navigation,
    Advertisement,
    SavedControl,
    AccountWidget,
    ParagraphLayout,
    ShortCorrection,
    SourceGarble,
    Punctuation,
}

/// No strings, source excerpts, or caller-controlled values in errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Limit,
    SourceHash,
    Range,
    Overlap,
    SpanHash,
    Evidence,
    Classification,
    UnsupportedDeletion,
    InvalidEdit,
    AdjacentCorrections,
    BodyLost,
    StructureChanged,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("content_format_invalid_result")
    }
}
impl std::error::Error for Error {}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A complete paragraphs::blocks range, including its trailing blank separators.
#[derive(Clone, PartialEq, Eq)]
pub struct Block {
    pub id: String,
    pub range: [usize; 2],
    pub sha256: String,
}

pub fn blocks(source: &str) -> Result<Vec<Block>, Error> {
    if source.len() > MAX_BYTES {
        return Err(Error::Limit);
    }
    let source_hash = sha256(source.as_bytes());
    Ok(paragraphs::blocks(source)
        .into_iter()
        .map(|(range, _)| Block {
            id: sha256(
                format!("format-block-v1:{source_hash}:{}:{}", range[0], range[1]).as_bytes(),
            ),
            range,
            sha256: sha256(source[range[0]..range[1]].as_bytes()),
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Body,
    Protected,
    /// Absence of a decision, not permission to discard.
    Unresolved,
    Furniture {
        reason: Reason,
        basis: Basis,
    },
}

/// Complete source partition, prepared independently of proposed edits. An exact
/// anchor match is required even for protected/unresolved blocks. Evidence IDs are
/// already verified local Evidence links, never arbitrary model citations.
#[derive(Clone)]
pub struct ClassifiedBlock {
    pub block: Block,
    pub role: Role,
    pub source_evidence_ids: Vec<String>,
}

#[derive(Clone)]
pub struct ProposedEdit {
    pub old_range: [usize; 2],
    pub old_sha256: String,
    pub replacement: String,
    pub kind: Kind,
    pub basis: Basis,
    pub reason_code: Reason,
    pub source_evidence_ids: Vec<String>,
    /// Required only for deletion; must name exactly one classified block.
    pub block_id: Option<String>,
}

/// Private text-bearing immutable value. No mutable accessors or Deserialize.
#[derive(Clone)]
pub struct Change {
    id: String,
    proposal: ProposedEdit,
    new_range: [usize; 2],
    new_sha256: String,
    before: String,
}

impl Change {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn proposal(&self) -> &ProposedEdit {
        &self.proposal
    }
    pub fn new_range(&self) -> [usize; 2] {
        self.new_range
    }
    pub fn new_sha256(&self) -> &str {
        &self.new_sha256
    }
    pub fn before(&self) -> &str {
        &self.before
    }
    pub fn after(&self) -> &str {
        &self.proposal.replacement
    }
}

#[derive(Clone)]
pub struct Applied {
    output: String,
    source_sha256: String,
    result_sha256: String,
    changes: Vec<Change>,
}

impl Applied {
    pub fn output(&self) -> &str {
        &self.output
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn result_sha256(&self) -> &str {
        &self.result_sha256
    }
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
}

// Even IDs/hashes may be supplied by an untrusted caller. Redact entire values.
macro_rules! private_debug {
    ($($ty:ty),*) => { $(impl fmt::Debug for $ty {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(concat!(stringify!($ty), "([private])"))
        }
    })* };
}
private_debug!(Block, ClassifiedBlock, ProposedEdit, Change, Applied);

fn correction(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Typo | Kind::ExtractionGarble | Kind::Punctuation
    )
}

fn evidence(ids: &[String], allowed: &BTreeSet<&str>) -> bool {
    ids.len() <= 64
        && ids
            .iter()
            .all(|id| !id.is_empty() && id.len() <= 256 && allowed.contains(id.as_str()))
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}

fn typed_text(text: &str) -> [String; 3] {
    let mut parts = [String::new(), String::new(), String::new()];
    for ([start, end], kind) in paragraphs::blocks(text) {
        let slot = match kind {
            paragraphs::Kind::Paragraph => 0,
            paragraphs::Kind::Heading => 1,
            paragraphs::Kind::Note => 2,
        };
        parts[slot].extend(text[start..end].chars().filter(|c| !c.is_whitespace()));
    }
    parts
}

// Paragraph boundaries/ordinary whitespace may change; Markdown containers,
// inline syntax and exact code bytes may not. Keep this private, never diagnostic.
fn markdown_structure(
    source: &str,
    preserve_prose: bool,
) -> (Vec<pulldown_cmark::Event<'static>>, Vec<String>) {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};
    let mut events = Vec::new();
    let mut code = false;
    let mut code_bytes = Vec::new();
    let mut prose = String::new();
    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::Paragraph)
            | Event::End(TagEnd::Paragraph)
            | Event::SoftBreak
            | Event::HardBreak => {}
            Event::Text(text) if !code => {
                if preserve_prose {
                    prose.extend(text.chars().filter(|c| !c.is_whitespace()));
                }
            }
            event => {
                if !prose.is_empty() {
                    events.push(Event::Text(CowStr::Boxed(
                        std::mem::take(&mut prose).into_boxed_str(),
                    )));
                }
                if matches!(event, Event::Start(Tag::CodeBlock(_)) | Event::Code(_)) {
                    code_bytes.push(sha256(source[range].as_bytes()));
                }
                if matches!(event, Event::Start(Tag::CodeBlock(_))) {
                    code = true;
                }
                if matches!(event, Event::End(TagEnd::CodeBlock)) {
                    code = false;
                }
                events.push(event.into_static());
            }
        }
    }
    if !prose.is_empty() {
        events.push(Event::Text(CowStr::Boxed(prose.into_boxed_str())));
    }
    (events, code_bytes)
}

/// Deliberately narrow structural support. Reject documents with ambiguous Markdown
/// containers instead of pretending paragraphs::blocks is a full Markdown parser.
fn plain_document(source: &str) -> bool {
    source.lines().all(|line| {
        let s = line.trim();
        !line.starts_with([' ', '\t'])
            && !s.contains([
                '`', '~', '|', '[', ']', '<', '>', '#', '*', '_', '"', '“', '”',
            ])
            && !s.starts_with(['-', '+', '=', ':'])
    })
}

fn plain_paragraphs(source: &str) -> Vec<std::ops::Range<usize>> {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let mut depth = 0usize;
    let mut ranges = Vec::new();
    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0
                    && matches!(tag, Tag::Paragraph)
                    && plain_document(&source[range.clone()])
                {
                    ranges.push(range);
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    ranges
}

fn supported_control(text: &str, reason: Reason) -> bool {
    let text = text.trim_end_matches(['\r', '\n']);
    match reason {
        Reason::Navigation => matches!(text, "Next" | "Previous" | "下一页" | "上一页"),
        Reason::SavedControl => matches!(text, "Print" | "打印"),
        _ => false,
    }
}

/// Apply a complete, sorted, bounded source-relative proposal atomically in memory.
/// `classified` must cover every source block in order. At least one identified
/// Body letter must remain outside deletions and corrections (layout changes may
/// conserve it). This conservative byte-level witness is not
/// a semantic certification. Empty/identical edits reject rather than inflate counts.
/// `verified_evidence_ids` is supplied by the later authorized Evidence resolver;
/// membership here does NOT independently establish source_verified semantics.
pub fn apply(
    source: &str,
    expected_sha256: &str,
    classified: &[ClassifiedBlock],
    verified_evidence_ids: &[String],
    edits: &[ProposedEdit],
) -> Result<Applied, Error> {
    if source.len() > MAX_BYTES
        || edits.len() > MAX_EDITS
        || verified_evidence_ids.len() > MAX_EDITS
    {
        return Err(Error::Limit);
    }
    if sha256(source.as_bytes()) != expected_sha256 {
        return Err(Error::SourceHash);
    }
    let anchors = blocks(source)?;
    let allowed: BTreeSet<_> = verified_evidence_ids.iter().map(String::as_str).collect();
    if classified.len() != anchors.len()
        || classified
            .iter()
            .zip(&anchors)
            .any(|(c, b)| c.block != *b || !evidence(&c.source_evidence_ids, &allowed))
    {
        return Err(Error::Classification);
    }
    let mut cursor = 0;
    let mut previous_start = None;
    let mut last_correction_end = None;
    let mut corrections = 0;
    let mut output = String::new();
    // Apply semantic edits but NOT layout edits to a control document, so layout
    // cannot silently move text between heading/note/prose categories.
    let mut control = String::new();
    let mut changes = Vec::new();
    let plain = plain_paragraphs(source);
    for e in edits {
        let [start, end] = e.old_range;
        let old = source.get(start..end).ok_or(Error::Range)?;
        if start < cursor || previous_start == Some(start) {
            return Err(Error::Overlap);
        }
        if sha256(old.as_bytes()) != e.old_sha256 {
            return Err(Error::SpanHash);
        }
        if old == e.replacement || e.replacement.contains('\0') {
            return Err(Error::InvalidEdit);
        }
        if !evidence(&e.source_evidence_ids, &allowed)
            || (e.basis == Basis::SourceVerified && e.source_evidence_ids.is_empty())
        {
            return Err(Error::Evidence);
        }
        if e.kind != Kind::GarbageDelete && e.block_id.is_some() {
            return Err(Error::InvalidEdit);
        }
        match e.kind {
            Kind::Whitespace | Kind::ParagraphBoundary => {
                if e.reason_code != Reason::ParagraphLayout
                    || !old
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .eq(e.replacement.chars().filter(|c| !c.is_whitespace()))
                    || (e.kind == Kind::Whitespace
                        && (!old.chars().all(char::is_whitespace)
                            || !e.replacement.chars().all(char::is_whitespace)))
                {
                    return Err(Error::InvalidEdit);
                }
            }
            Kind::GarbageDelete => {
                let c = classified
                    .iter()
                    .find(|c| Some(&c.block.id) == e.block_id.as_ref())
                    .ok_or(Error::Classification)?;
                if c.block.range != e.old_range
                    || c.role
                        != (Role::Furniture {
                            reason: e.reason_code,
                            basis: e.basis,
                        })
                    || c.source_evidence_ids != e.source_evidence_ids
                    || e.basis == Basis::Deterministic
                    || !e.replacement.is_empty()
                {
                    return Err(Error::Classification);
                }
                if !plain.iter().any(|r| {
                    r.start == start
                        && r.end <= end
                        && source[r.end..end].chars().all(char::is_whitespace)
                }) || !supported_control(old, e.reason_code)
                {
                    return Err(Error::UnsupportedDeletion);
                }
            }
            _ => {
                corrections += 1;
                if corrections > MAX_CORRECTIONS || old.len() > 64 || e.replacement.len() > 64 {
                    return Err(Error::Limit);
                }
                if old.is_empty()
                    || old.contains(['\r', '\n'])
                    || e.replacement.contains(['\r', '\n'])
                    || !plain_document(&e.replacement)
                    || !plain.iter().any(|r| start >= r.start && end <= r.end)
                    || !classified.iter().any(|c| {
                        c.role == Role::Body
                            && start >= c.block.range[0]
                            && end <= c.block.range[1]
                            && plain_document(&source[c.block.range[0]..c.block.range[1]])
                    })
                    || (e.basis == Basis::ContextInferred
                        && old
                            .chars()
                            .chain(e.replacement.chars())
                            .any(char::is_numeric))
                {
                    return Err(Error::InvalidEdit);
                }
                let reason = match e.kind {
                    Kind::Typo => Reason::ShortCorrection,
                    Kind::ExtractionGarble => Reason::SourceGarble,
                    _ => Reason::Punctuation,
                };
                if e.reason_code != reason || e.basis == Basis::Deterministic {
                    return Err(Error::InvalidEdit);
                }
                let non_punctuation = |c: &char| {
                    !c.is_ascii_punctuation() && !"，。；：！？、（）【】《》〈〉…—·".contains(*c)
                };
                if e.kind == Kind::Punctuation
                    && !old
                        .chars()
                        .filter(non_punctuation)
                        .eq(e.replacement.chars().filter(non_punctuation))
                {
                    return Err(Error::InvalidEdit);
                }
                // Also reject a whitespace gap (even if separately edited), rather
                // than allowing a long rewrite to be divided into adjacent words.
                if last_correction_end
                    .is_some_and(|last| source[last..start].chars().all(char::is_whitespace))
                {
                    return Err(Error::AdjacentCorrections);
                }
                last_correction_end = Some(end);
            }
        }
        let new_len = output
            .len()
            .checked_add(start - cursor)
            .and_then(|n| n.checked_add(e.replacement.len()))
            .ok_or(Error::Limit)?;
        if new_len > MAX_BYTES {
            return Err(Error::Limit);
        }
        output.push_str(&source[cursor..start]);
        control.push_str(&source[cursor..start]);
        control.push_str(
            if matches!(e.kind, Kind::Whitespace | Kind::ParagraphBoundary) {
                old
            } else {
                &e.replacement
            },
        );
        let new_start = output.len();
        output.push_str(&e.replacement);
        changes.push(Change {
            id: sha256(
                format!(
                    "format-change-v1:{expected_sha256}:{start}:{end}:{}:{:?}:{:?}:{:?}",
                    sha256(e.replacement.as_bytes()),
                    e.kind,
                    e.basis,
                    e.reason_code
                )
                .as_bytes(),
            ),
            proposal: e.clone(),
            new_range: [new_start, output.len()],
            new_sha256: sha256(e.replacement.as_bytes()),
            before: old.into(),
        });
        cursor = end;
        previous_start = Some(start);
    }
    if output
        .len()
        .checked_add(source.len() - cursor)
        .ok_or(Error::Limit)?
        > MAX_BYTES
    {
        return Err(Error::Limit);
    }
    output.push_str(&source[cursor..]);
    control.push_str(&source[cursor..]);
    if typed_text(&control) != typed_text(&output)
        || markdown_structure(&control, true) != markdown_structure(&output, true)
        || markdown_structure(source, false) != markdown_structure(&control, false)
    {
        return Err(Error::StructureChanged);
    }
    // A short correction is not a second deletion channel. Check each retained
    // paragraph after ALL corrections, not just a target-wide surviving witness.
    for range in &plain {
        let mut retained = String::new();
        let mut at = range.start;
        for e in edits.iter().filter(|e| {
            correction(e.kind) && e.old_range[0] >= range.start && e.old_range[1] <= range.end
        }) {
            retained.push_str(&source[at..e.old_range[0]]);
            retained.push_str(&e.replacement);
            at = e.old_range[1];
        }
        retained.push_str(&source[at..range.end]);
        if source[range.clone()].chars().any(char::is_alphanumeric)
            && !retained.chars().any(char::is_alphanumeric)
        {
            return Err(Error::BodyLost);
        }
    }
    let semantic: Vec<_> = edits
        .iter()
        .filter(|e| correction(e.kind) || e.kind == Kind::GarbageDelete)
        .collect();
    let witness = classified.iter().any(|c| {
        if c.role != Role::Body {
            return false;
        }
        let [mut at, end] = c.block.range;
        let first = semantic.partition_point(|e| e.old_range[1] <= at);
        for e in semantic[first..]
            .iter()
            .take_while(|e| e.old_range[0] < end)
        {
            let gap_end = e.old_range[0].max(at).min(end);
            if source[at..gap_end].chars().any(char::is_alphabetic) {
                return true;
            }
            at = e.old_range[1].max(at).min(end);
        }
        source[at..end].chars().any(char::is_alphabetic)
    });
    if output.trim().is_empty()
        || (!edits.is_empty() && (!witness || !output.chars().any(char::is_alphabetic)))
    {
        return Err(Error::BodyLost);
    }
    Ok(Applied {
        result_sha256: sha256(output.as_bytes()),
        source_sha256: expected_sha256.into(),
        output,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classified(s: &str) -> Vec<ClassifiedBlock> {
        blocks(s)
            .unwrap()
            .into_iter()
            .map(|block| ClassifiedBlock {
                block,
                role: Role::Body,
                source_evidence_ids: vec![],
            })
            .collect()
    }
    fn edit(s: &str, range: [usize; 2], replacement: &str, kind: Kind) -> ProposedEdit {
        ProposedEdit {
            old_range: range,
            old_sha256: sha256(s[range[0]..range[1]].as_bytes()),
            replacement: replacement.into(),
            kind,
            basis: Basis::ContextInferred,
            reason_code: match kind {
                Kind::GarbageDelete => Reason::Navigation,
                Kind::Typo => Reason::ShortCorrection,
                _ => Reason::ParagraphLayout,
            },
            source_evidence_ids: vec![],
            block_id: None,
        }
    }
    #[test]
    fn utf8_crlf_exact_bytes_and_insertions() {
        let s = "中文  text\r\n\r\nKeep β.\r\n";
        let e = edit(s, [6, 8], "\r\n", Kind::Whitespace);
        let a = apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).unwrap();
        assert_eq!(a.output(), "中文\r\ntext\r\n\r\nKeep β.\r\n");
        assert_eq!(a.changes()[0].new_range(), [6, 8]);
        assert_eq!(a.changes()[0].before(), "  ");
        let e = edit(s, [6, 6], "\n", Kind::ParagraphBoundary);
        let a = apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).unwrap();
        assert_eq!(a.output(), "中文\n  text\r\n\r\nKeep β.\r\n");
    }
    #[test]
    fn deletion_is_exact_and_anchored() {
        let s = "Next\r\n\r\n保留 body.\r\n";
        let mut c = classified(s);
        c[0].role = Role::Furniture {
            reason: Reason::Navigation,
            basis: Basis::ContextInferred,
        };
        let mut e = edit(s, c[0].block.range, "", Kind::GarbageDelete);
        e.block_id = Some(c[0].block.id.clone());
        let a = apply(s, &sha256(s.as_bytes()), &c, &[], &[e.clone()]).unwrap();
        assert_eq!(a.output(), "保留 body.\r\n");
        assert_eq!(a.changes()[0].new_range(), [0, 0]);
        assert_eq!(a.changes()[0].new_sha256(), sha256(b""));
        e.replacement = "replacement".into();
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[e]).unwrap_err(),
            Error::Classification
        );
        c[0].block.id = "forged".into();
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[]).unwrap_err(),
            Error::Classification
        );
    }
    #[test]
    fn protected_and_ambiguous_blocks_cannot_be_deleted() {
        for protected in [
            "# Heading",
            "[^1]: note",
            "```\nNext\n```",
            "| a | b |",
            "> Next",
            "By A. Author",
            "Source: A",
            "2026",
            "Advertising is discussed here.",
            "Next\n====",
            "    Next",
        ] {
            let s = format!("{protected}\n\nRetained prose.\n");
            let mut c = classified(&s);
            c[0].role = Role::Furniture {
                reason: Reason::Navigation,
                basis: Basis::ContextInferred,
            };
            let mut e = edit(&s, c[0].block.range, "", Kind::GarbageDelete);
            e.block_id = Some(c[0].block.id.clone());
            assert!(apply(&s, &sha256(s.as_bytes()), &c, &[], &[e]).is_err());
        }
    }
    #[test]
    fn bad_ranges_hashes_overlap_and_debug() {
        let s = "秘密  prose\n\nRetained body.";
        let c = classified(s);
        let e = edit(s, [6, 8], " ", Kind::Whitespace);
        assert_eq!(
            apply(s, "wrong", &c, &[], &[]).unwrap_err(),
            Error::SourceHash
        );
        let mut bad = e.clone();
        bad.old_range = [1, 8];
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[bad]).unwrap_err(),
            Error::Range
        );
        let mut bad = e.clone();
        bad.old_sha256 = "secret".into();
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[bad.clone()]).unwrap_err(),
            Error::SpanHash
        );
        assert!(!format!("{bad:?}").contains("secret"));
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[e.clone(), e]).unwrap_err(),
            Error::Overlap
        );
    }

    #[test]
    fn markdown_neighbours_do_not_disable_body_corrections_or_expose_containers() {
        let s = "# Title\n\nteh body.\n\n[^1]: retained note.\n";
        let start = s.find("teh").unwrap();
        let e = edit(s, [start, start + 3], "the", Kind::Typo);
        let mut c = classified(s);
        let result = apply(s, &sha256(s.as_bytes()), &c, &[], &[e.clone()]).unwrap();
        assert_eq!(
            result.output(),
            "# Title\n\nthe body.\n\n[^1]: retained note.\n"
        );
        c.iter_mut()
            .find(|c| c.block.range[0] == start)
            .unwrap()
            .role = Role::Protected;
        assert!(apply(s, &sha256(s.as_bytes()), &c, &[], &[e]).is_err());
        for container in [
            "   ```\n\nteh body.\n\n   ```",
            "> quote\n\nteh body.\n===",
            "<script>\n\nteh body.\n\n</script>",
        ] {
            let s = format!("{container}\n\nRetained prose.\n");
            let start = s.find("teh").unwrap();
            let e = edit(&s, [start, start + 3], "the", Kind::Typo);
            assert!(apply(&s, &sha256(s.as_bytes()), &classified(&s), &[], &[e]).is_err());
        }
    }

    #[test]
    fn corrections_cannot_erase_a_retained_paragraph() {
        let s = "Important warning.\n\nRetained body.\n";
        for replacement in ["", " ", ".", "！！"] {
            let e = edit(s, [0, 18], replacement, Kind::Typo);
            assert!(
                apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).is_err(),
                "correction erased a paragraph"
            );
        }
        let s = "Spelliing warning.\n\nRetained body.\n";
        let e = edit(s, [5, 6], "", Kind::Typo);
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e])
                .unwrap()
                .output(),
            "Spelling warning.\n\nRetained body.\n"
        );
        for s in [
            "x    return false;\n\nRetained body.",
            "x- retained prose.\n\nTail.",
        ] {
            let e = edit(s, [0, 1], "", Kind::Typo);
            assert!(
                apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).is_err(),
                "correction introduced a Markdown container"
            );
        }
    }

    #[test]
    fn layout_preserves_code_bytes_and_markdown_containers() {
        let s = "```js\nreturn false;\n```\n\nRetained body.\n";
        let e = edit(s, [12, 13], "", Kind::Whitespace);
        assert!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).is_err(),
            "layout changed code semantics"
        );
        let s = "Ordinary prose.\n\nRetained body.\n";
        let e = edit(s, [0, 0], "    ", Kind::Whitespace);
        assert!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).is_err(),
            "layout changed prose into code"
        );
    }

    #[test]
    fn correction_bounds_and_no_adjacent_rewrite() {
        let s = format!("{}\n\nRetained body.", "a".repeat(65));
        let c = classified(&s);
        let e = edit(&s, [0, 65], "b", Kind::Typo);
        assert_eq!(
            apply(&s, &sha256(s.as_bytes()), &c, &[], &[e]).unwrap_err(),
            Error::Limit
        );
        let e = edit(&s, [0, 64], &"b".repeat(64), Kind::Typo);
        assert!(apply(&s, &sha256(s.as_bytes()), &c, &[], &[e]).is_ok());
        let e = edit(&s, [0, 1], &"b".repeat(65), Kind::Typo);
        assert_eq!(
            apply(&s, &sha256(s.as_bytes()), &c, &[], &[e]).unwrap_err(),
            Error::Limit
        );
        let s = "bad word\n\nRetained body.";
        let edits = [
            edit(s, [0, 3], "new", Kind::Typo),
            edit(s, [4, 8], "text", Kind::Typo),
        ];
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &edits).unwrap_err(),
            Error::AdjacentCorrections
        );
        let s = format!("{}\n\nRetained body.", "a;".repeat(65));
        let edits: Vec<_> = (0..65)
            .map(|i| edit(&s, [i * 2, i * 2 + 1], "b", Kind::Typo))
            .collect();
        assert!(apply(
            &s,
            &sha256(s.as_bytes()),
            &classified(&s),
            &[],
            &edits[..64]
        )
        .is_ok());
        assert_eq!(
            apply(&s, &sha256(s.as_bytes()), &classified(&s), &[], &edits).unwrap_err(),
            Error::Limit
        );
    }
    #[test]
    fn body_witness_and_structure_are_required() {
        let s = "Next";
        let mut c = classified(s);
        c[0].role = Role::Furniture {
            reason: Reason::Navigation,
            basis: Basis::ContextInferred,
        };
        let mut e = edit(s, [0, 4], "", Kind::GarbageDelete);
        e.block_id = Some(c[0].block.id.clone());
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &c, &[], &[e]).unwrap_err(),
            Error::BodyLost
        );
        let s = "# Heading\n\nBody text.";
        let e = edit(s, [9, 11], " ", Kind::Whitespace);
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).unwrap_err(),
            Error::StructureChanged
        );
    }

    #[test]
    fn evidence_noop_and_exact_multi_edit_map() {
        let s = "teh  文。\r\n\r\nTail.";
        let mut e = edit(s, [0, 3], "the", Kind::Typo);
        e.basis = Basis::SourceVerified;
        e.source_evidence_ids = vec!["evidence_fixture".into()];
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e.clone()]).unwrap_err(),
            Error::Evidence
        );
        let es = [e, edit(s, [3, 5], " ", Kind::Whitespace)];
        let a = apply(
            s,
            &sha256(s.as_bytes()),
            &classified(s),
            &["evidence_fixture".into()],
            &es,
        )
        .unwrap();
        assert_eq!(a.output(), "the 文。\r\n\r\nTail.");
        assert_eq!(a.source_sha256(), sha256(s.as_bytes()));
        assert_eq!(a.result_sha256(), sha256(a.output().as_bytes()));
        let (mut old, mut new) = (0, 0);
        for change in a.changes() {
            let [start, end] = change.proposal().old_range;
            let [ns, ne] = change.new_range();
            assert_eq!(&s[old..start], &a.output()[new..ns]);
            assert_eq!(&s[start..end], change.before());
            assert_eq!(&a.output()[ns..ne], change.after());
            assert_eq!(change.new_sha256(), sha256(change.after().as_bytes()));
            assert_eq!(change.id().len(), 64);
            old = end;
            new = ne;
        }
        assert_eq!(&s[old..], &a.output()[new..]);
        let no_op = apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[]).unwrap();
        assert_eq!(no_op.output(), s);
        assert!(no_op.changes().is_empty());
        assert!(!format!("{a:?}").contains("Tail"));
        let mut protected = classified(s);
        for c in &mut protected {
            c.role = Role::Protected;
        }
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &protected, &[], &[])
                .unwrap()
                .output(),
            s
        );
    }

    #[test]
    fn correction_structure_and_conservation() {
        let s = "bad word";
        for (range, replacement) in [
            ([0, 0], "x"),
            ([0, 3], "new\nline"),
            ([0, 3], "# title"),
            ([0, 3], "2026"),
        ] {
            assert!(apply(
                s,
                &sha256(s.as_bytes()),
                &classified(s),
                &[],
                &[edit(s, range, replacement, Kind::Typo)]
            )
            .is_err());
        }
        assert!(apply(
            s,
            &sha256(s.as_bytes()),
            &classified(s),
            &[],
            &[edit(s, [0, 3], "new", Kind::ParagraphBoundary)]
        )
        .is_err());
        for (kind, reason) in [
            (Kind::ExtractionGarble, Reason::SourceGarble),
            (Kind::Punctuation, Reason::Punctuation),
        ] {
            let mut e = edit(s, [0, 3], "good", kind);
            e.reason_code = reason;
            assert_eq!(
                apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e]).is_ok(),
                kind == Kind::ExtractionGarble
            );
        }
        let s = "Body, text.";
        let mut e = edit(s, [4, 5], "，", Kind::Punctuation);
        e.reason_code = Reason::Punctuation;
        assert_eq!(
            apply(s, &sha256(s.as_bytes()), &classified(s), &[], &[e])
                .unwrap()
                .output(),
            "Body， text."
        );
        let s = "Next\n\nBody.";
        for role in [Role::Protected, Role::Unresolved] {
            let mut c = classified(s);
            c[0].role = role;
            let mut e = edit(s, c[0].block.range, "", Kind::GarbageDelete);
            e.block_id = Some(c[0].block.id.clone());
            assert_eq!(
                apply(s, &sha256(s.as_bytes()), &c, &[], &[e]).unwrap_err(),
                Error::Classification
            );
        }
    }
}
