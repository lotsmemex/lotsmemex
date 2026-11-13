//! Owner UDS reader state (B2: position and bookmarks) and manual reader
//! annotations (`reader_annotations_v1`, five-style marks). Both are `reader`
//! command ops; anchors are UTF-8 byte offsets within a real toc chapter of a
//! real edition, never pixels, percentages or AI-translation offsets.

use crate::book::{Edition, ReaderRequest, READER_FEATURE};
use crate::{Client, ClientError};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// New capability of five-style marks (not the legacy `reading_annotations_v1`).
pub const ANNOTATIONS_FEATURE: &str = "reader_annotations_v1";
/// Largest `annotations_list` page.
pub const ANNOTATIONS_PAGE: usize = 100;
const MAX_ANNOTATION_PAGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderAnchor {
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub chapter: String,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub id: String,
    pub label: String,
    pub anchor: ReaderAnchor,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderState {
    pub visit_id: String,
    pub position: Option<ReaderAnchor>,
    pub bookmarks: Vec<Bookmark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VersionedState {
    pub version: String,
    pub state: ReaderState,
}

/// One chapter-relative half-open byte range of a mark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub chapter: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkAnchor {
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkStyle {
    Bold,
    Red,
    Blue,
    Underline,
    Yellow,
}

impl MarkStyle {
    pub const ALL: [MarkStyle; 5] = [MarkStyle::Bold, MarkStyle::Red, MarkStyle::Blue, MarkStyle::Underline, MarkStyle::Yellow];
    pub fn label(self) -> &'static str {
        match self {
            MarkStyle::Bold => "加粗",
            MarkStyle::Red => "标红",
            MarkStyle::Blue => "标蓝（负面）",
            MarkStyle::Underline => "下划线",
            MarkStyle::Yellow => "黄色高亮",
        }
    }
}

/// A mark as sent in `put`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub id: String,
    pub style: MarkStyle,
    pub anchor: MarkAnchor,
}

/// A mark as listed: the server derives `status` (`current` / `stale`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ListedAnnotation {
    pub id: String,
    pub style: MarkStyle,
    pub anchor: MarkAnchor,
    pub status: String,
}

impl ListedAnnotation {
    pub fn is_current(&self) -> bool {
        self.status == "current"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnnotationChange {
    Put { annotation: Annotation },
    Delete { id: String },
}

#[derive(Debug, Deserialize)]
struct AnnotationPage {
    item_id: String,
    version: String,
    annotations: Vec<ListedAnnotation>,
    #[allow(dead_code)]
    total: usize,
    next_offset: Option<usize>,
}

/// Every mark of one item at one ledger version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnnotationLedger {
    pub version: String,
    pub annotations: Vec<ListedAnnotation>,
}

/// What `annotations_edit` answered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EditReceipt {
    pub version: String,
    pub id: String,
    pub outcome: String,
}

/// How a mark edit ended, for the caller's next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Saved(EditReceipt),
    /// `stale_reader_annotations`: read and reconcile; never resend blindly.
    Stale,
    /// Rejected as invalid / duplicate: nothing changed.
    Rejected(String),
    /// The reply was lost: the attempt is kept as it was until read/reconciled.
    Unknown(String),
}

impl Client {
    fn reader_feature(&self, feature: &str) -> Result<(), ClientError> {
        if self.capabilities()?.features.iter().any(|f| f == feature) { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    /// B2 `state`: `{version,state}` of one item (empty when never saved).
    pub fn reader_state(&self, item_id: &str) -> Result<VersionedState, ClientError> {
        self.reader_feature(READER_FEATURE)?;
        serde_json::from_value(self.reader(ReaderRequest::State { item_id: item_id.into() })?).map_err(|_| ClientError::InvalidFrame)
    }

    /// B2 `save`: the complete next state against the version read. A conflict
    /// is `Daemon("stale_reader_state")`; the caller reads and reconciles.
    pub fn reader_save_state(&self, item_id: &str, expected_version: &str, state: &ReaderState) -> Result<VersionedState, ClientError> {
        self.reader_feature(READER_FEATURE)?;
        let request = ReaderRequest::Save { item_id: item_id.into(), expected_version: expected_version.into(), state: state.clone() };
        serde_json::from_value(self.reader(request)?).map_err(|_| ClientError::InvalidFrame)
    }

    pub fn annotations_supported(&self) -> bool {
        self.reader_feature(ANNOTATIONS_FEATURE).is_ok()
    }

    /// All marks of an item, page by page at one ledger version; a page from
    /// another version restarts the read (at most three times).
    pub fn annotations_list(&self, item_id: &str) -> Result<AnnotationLedger, ClientError> {
        self.reader_feature(ANNOTATIONS_FEATURE)?;
        'read: for _ in 0..3 {
            let (mut all, mut version) = (Vec::new(), None::<String>);
            for _ in 0..MAX_ANNOTATION_PAGES {
                let request = ReaderRequest::AnnotationsList { item_id: item_id.into(), offset: all.len(), limit: ANNOTATIONS_PAGE, expected_version: version.clone() };
                let page: AnnotationPage = match self.reader(request) {
                    Ok(v) => serde_json::from_value(v).map_err(|_| ClientError::InvalidFrame)?,
                    Err(ClientError::Daemon(code)) if code == "stale_reader_annotations" && version.is_some() => continue 'read,
                    Err(e) => return Err(e),
                };
                if page.item_id != item_id { return Err(ClientError::InvalidFrame); }
                if version.as_ref().is_some_and(|v| *v != page.version) { continue 'read; }
                version = Some(page.version.clone());
                let got = page.annotations.len();
                all.extend(page.annotations);
                match page.next_offset {
                    Some(next) if next == all.len() && got > 0 => continue,
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => return Ok(AnnotationLedger { version: page.version, annotations: all }),
                }
            }
            return Err(ClientError::InvalidFrame);
        }
        Err(ClientError::Daemon("stale_reader_annotations".into()))
    }

    /// One `put` / `delete` against `expected_version`. Never retried here.
    pub fn annotations_edit(&self, item_id: &str, expected_version: &str, change: &AnnotationChange) -> EditOutcome {
        if let Err(e) = self.reader_feature(ANNOTATIONS_FEATURE) { return EditOutcome::Rejected(e.message()); }
        let request = ReaderRequest::AnnotationsEdit { item_id: item_id.into(), expected_version: expected_version.into(), change: change.clone() };
        match self.reader(request) {
            Ok(v) => match serde_json::from_value::<EditReceipt>(v) {
                Ok(r) => EditOutcome::Saved(r),
                Err(_) => EditOutcome::Unknown(ClientError::InvalidFrame.message()),
            },
            Err(ClientError::Daemon(code)) if code == "stale_reader_annotations" => EditOutcome::Stale,
            // Only refusals the daemon makes before anything is written are definite.
            // internal_error (e.g. a failed directory sync after the rename),
            // outcome_unknown and anything unrecognised may have been committed: keep
            // the write unknown and reconcile under the original request.
            Err(ClientError::Daemon(code)) if DEFINITE_REFUSALS.contains(&code.as_str()) => EditOutcome::Rejected(code),
            Err(e) => EditOutcome::Unknown(e.message()),
        }
    }
}

pub(crate) const DEFINITE_REFUSALS: &[&str] = &[
    "invalid_reader_annotation", "invalid_reader_anchor", "reader_annotation_conflict", "reader_annotation_limit_exceeded",
    "reader_annotation_not_found", "reader_annotations_unavailable", "invalid_request",
    "library_resource_not_found", "item_not_found", "stale_item_revision", "permission_denied",
];

/// The chapter holding canonical body byte `body` of `edition` and the offset in
/// it: at a boundary the following chapter at zero; EOF the last chapter's end.
pub fn anchor_at(edition: &Edition, revision_id: &str, body: usize) -> Option<ReaderAnchor> {
    let ch = edition.chapters.iter().find(|c| c.start <= body && body < c.end).or_else(|| edition.chapters.last().filter(|c| body == c.end))?;
    Some(ReaderAnchor { revision_id: revision_id.into(), side: edition.side.clone(), sha256: edition.sha256.clone(), chapter: ch.key.clone(), offset: body - ch.start })
}

/// The canonical body byte of `anchor`, when it binds to this edition and revision.
pub fn body_at(edition: &Edition, revision_id: &str, anchor: &ReaderAnchor) -> Option<usize> {
    if anchor.revision_id != revision_id || anchor.side != edition.side || anchor.sha256 != edition.sha256 { return None; }
    let ch = edition.chapters.iter().find(|c| c.key == anchor.chapter)?;
    (anchor.offset <= ch.end - ch.start).then_some(ch.start + anchor.offset)
}

/// A body byte range split into ordered chapter segments (one mark across chapters).
pub fn segments_of(edition: &Edition, range: Range<usize>) -> Vec<Segment> {
    edition.chapters.iter().filter_map(|c| {
        let (s, e) = (range.start.max(c.start), range.end.min(c.end));
        (s < e).then(|| Segment { chapter: c.key.clone(), start: s - c.start, end: e - c.start })
    }).collect()
}

/// The body ranges of a mark bound to this edition and revision (`None`: stale or elsewhere).
pub fn ranges_of(edition: &Edition, revision_id: &str, anchor: &MarkAnchor) -> Option<Vec<Range<usize>>> {
    if anchor.revision_id != revision_id || anchor.side != edition.side || anchor.sha256 != edition.sha256 { return None; }
    anchor.segments.iter().map(|s| {
        let c = edition.chapters.iter().find(|c| c.key == s.chapter)?;
        (s.start < s.end && s.end <= c.end - c.start).then(|| c.start + s.start..c.start + s.end)
    }).collect()
}
