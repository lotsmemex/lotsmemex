//! Five-style marks on durable AI chapter translations
//! (`reader_annotations_ai_translation_v1`): the `reader` ops `annotations_list` /
//! `annotations_edit` with `annotation_space: "ai_translation"`, a ledger of its
//! own beside the edition marks of `reader_state`. A segment is a half-open UTF-8
//! byte range of one chapter's complete AI translation Markdown at one effective
//! translation version and hash: never an original offset, never chunk-local and
//! never found by searching for text.

use crate::book::{ReaderRequest, TranslationMeta};
use crate::reader_state::{EditOutcome, EditReceipt, MarkStyle, ANNOTATIONS_PAGE, DEFINITE_REFUSALS};
use crate::{Client, ClientError};
use serde::{Deserialize, Serialize};
use std::ops::Range;

pub const AI_ANNOTATIONS_FEATURE: &str = "reader_annotations_ai_translation_v1";
/// `annotation_space` of every AI request and response.
pub const AI_SPACE: &str = "ai_translation";
/// `anchor.side` of an AI mark.
pub const AI_SIDE: &str = "ai_translation";
const MAX_PAGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSegment {
    /// The `original_chapter` key from the toc.
    pub chapter: String,
    /// The effective `version` of `translate_metadata` / `translate_chunk`.
    pub translation_version: String,
    pub sha256: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiAnchor {
    pub revision_id: String,
    pub side: String,
    pub segments: Vec<AiSegment>,
}

/// A mark as sent in `put`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiAnnotation {
    pub id: String,
    pub style: MarkStyle,
    pub anchor: AiAnchor,
}

/// The complete identity of one chapter's AI translation (the list filter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiProjection {
    pub chapter: String,
    pub translation_version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectionStatus {
    pub chapter: String,
    pub translation_version: String,
    pub sha256: String,
    pub status: String,
}

/// A mark as listed: `stale` when the revision or any segment's version/hash is
/// no longer the current effective translation (the whole mark, every segment).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ListedAiAnnotation {
    pub id: String,
    pub style: MarkStyle,
    pub anchor: AiAnchor,
    pub status: String,
    /// Server mutation order (red/blue precedence).
    pub order: u64,
    #[serde(default)]
    pub projections: Vec<ProjectionStatus>,
}

impl ListedAiAnnotation {
    pub fn is_current(&self) -> bool {
        self.status == "current"
    }
    /// Whether a segment of the mark is in `chapter`.
    pub fn touches(&self, chapter: &str) -> bool {
        self.anchor.segments.iter().any(|s| s.chapter == chapter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiAnnotationChange {
    Put { annotation: AiAnnotation },
    Delete { id: String },
}

#[derive(Debug, Deserialize)]
struct AiPage {
    item_id: String,
    annotation_space: String,
    version: String,
    view_version: String,
    annotations: Vec<ListedAiAnnotation>,
    #[allow(dead_code)]
    total: usize,
    next_offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AiReceipt {
    item_id: String,
    annotation_space: String,
    version: String,
    id: String,
    outcome: String,
}

/// Every AI mark of one item, read at one ledger version and one view version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AiLedger {
    pub version: String,
    pub view_version: String,
    pub annotations: Vec<ListedAiAnnotation>,
}

impl Client {
    pub fn ai_annotations_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == AI_ANNOTATIONS_FEATURE))
    }

    fn require_ai_annotations(&self) -> Result<(), ClientError> {
        if self.capabilities()?.features.iter().any(|f| f == AI_ANNOTATIONS_FEATURE) { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    /// All AI marks of `item_id` (no projection filter). Page 1 sends both
    /// versions null; later pages both versions of page 1. A later page that is
    /// `stale_reader_annotations`, or answers other versions, restarts from page 1
    /// (at most three reads).
    pub fn ai_annotations_list(&self, item_id: &str) -> Result<AiLedger, ClientError> {
        self.require_ai_annotations()?;
        'read: for _ in 0..3 {
            let (mut all, mut bound) = (Vec::new(), None::<(String, String)>);
            for _ in 0..MAX_PAGES {
                let request = ReaderRequest::AiAnnotationsList {
                    item_id: item_id.into(), offset: all.len(), limit: ANNOTATIONS_PAGE, expected_version: bound.as_ref().map(|b| b.0.clone()),
                    annotation_space: AI_SPACE.into(), projection: None, expected_view_version: bound.as_ref().map(|b| b.1.clone()),
                };
                let page: AiPage = match self.reader(request) {
                    Ok(v) => serde_json::from_value(v).map_err(|_| ClientError::InvalidFrame)?,
                    Err(ClientError::Daemon(code)) if code == "stale_reader_annotations" && bound.is_some() => continue 'read,
                    Err(e) => return Err(e),
                };
                if page.item_id != item_id || page.annotation_space != AI_SPACE || page.annotations.iter().any(|a| a.anchor.side != AI_SIDE) {
                    return Err(ClientError::InvalidFrame);
                }
                if bound.as_ref().is_some_and(|(v, w)| *v != page.version || *w != page.view_version) { continue 'read; }
                bound = Some((page.version.clone(), page.view_version.clone()));
                let got = page.annotations.len();
                all.extend(page.annotations);
                match page.next_offset {
                    Some(next) if next == all.len() && got > 0 => continue,
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => return Ok(AiLedger { version: page.version, view_version: page.view_version, annotations: all }),
                }
            }
            return Err(ClientError::InvalidFrame);
        }
        Err(ClientError::Daemon("stale_reader_annotations".into()))
    }

    /// One `put` / `delete` in the AI space against `expected_version`. Never
    /// retried here: after `Unknown` the caller keeps exactly this request.
    pub fn ai_annotations_edit(&self, item_id: &str, expected_version: &str, change: &AiAnnotationChange) -> EditOutcome {
        if let Err(e) = self.require_ai_annotations() { return EditOutcome::Rejected(e.message()); }
        let request = ReaderRequest::AiAnnotationsEdit { item_id: item_id.into(), expected_version: expected_version.into(), annotation_space: AI_SPACE.into(), change: change.clone() };
        match self.reader(request) {
            Ok(v) => match serde_json::from_value::<AiReceipt>(v) {
                Ok(r) if r.item_id == item_id && r.annotation_space == AI_SPACE => EditOutcome::Saved(EditReceipt { version: r.version, id: r.id, outcome: r.outcome }),
                _ => EditOutcome::Unknown(ClientError::InvalidFrame.message()),
            },
            Err(ClientError::Daemon(code)) if code == "stale_reader_annotations" => EditOutcome::Stale,
            // Refused before anything was written; a changed translation too.
            Err(ClientError::Daemon(code)) if code == "stale_reader_translation" || DEFINITE_REFUSALS.contains(&code.as_str()) => EditOutcome::Rejected(code),
            Err(e) => EditOutcome::Unknown(e.message()),
        }
    }
}

/// The segment of `range` of the AI text `text` that `meta` describes: `None`
/// unless the range is nonempty, inside it and on UTF-8 boundaries.
pub fn ai_segment(meta: &TranslationMeta, text: &str, range: Range<usize>) -> Option<AiSegment> {
    let ok = text.len() == meta.bytes && range.start < range.end && range.end <= text.len() && text.is_char_boundary(range.start) && text.is_char_boundary(range.end);
    ok.then(|| AiSegment { chapter: meta.original_chapter.clone(), translation_version: meta.version.clone(), sha256: meta.sha256.clone(), start: range.start, end: range.end })
}

/// Where `mark` is drawn on the AI text `meta` describes (`text_len` bytes) at
/// `revision_id`: its ranges in this chapter (empty when it lies elsewhere), or
/// `None` when it is stale or does not bind — then it is drawn nowhere.
pub fn ai_ranges(meta: &TranslationMeta, text_len: usize, revision_id: &str, mark: &ListedAiAnnotation) -> Option<Vec<Range<usize>>> {
    if !mark.is_current() || mark.anchor.revision_id != revision_id || mark.anchor.side != AI_SIDE || meta.revision_id != revision_id { return None; }
    mark.anchor.segments.iter().filter(|s| s.chapter == meta.original_chapter).map(|s| {
        (s.translation_version == meta.version && s.sha256 == meta.sha256 && s.start < s.end && s.end <= text_len).then_some(s.start..s.end)
    }).collect()
}
