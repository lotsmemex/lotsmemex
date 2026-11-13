//! `reader_v1`: chapter-bounded reads of long documents and paired books.
//!
//! A paired book keeps two independent editions: `original` (e.g. the English
//! text) and `translation` (e.g. a published Chinese translation). The canonical
//! Markdown of such an item is the translation, so a book must never be shown
//! through `item_read` as its own original. `counterpart` links chapters by
//! declared chapter order only; it says nothing about paragraphs.

use crate::{Client, ClientError, Command, Sensitivity};
use serde::{Deserialize, Serialize};

pub const READER_FEATURE: &str = "reader_v1";
/// Paragraph correspondence of one chapter (`alignment`).
pub const ALIGNMENT_FEATURE: &str = "reader_paragraph_alignment_v1";
/// Owner-requested chapter formatting repair + paragraph matching (`align_request`).
pub const REPAIR_FEATURE: &str = "reader_chapter_repair_v1";
/// Owner Markdown edit of one chapter of one real edition (`edit_begin/chunk/commit`).
pub const EDIT_FEATURE: &str = "reader_chapter_edit_v1";
/// AI translation of one English original-only chapter (`translate_preview/request/status/chunk`).
pub const TRANSLATION_FEATURE: &str = "reader_chapter_translation_v1";
/// Credential-free discovery of a completed AI chapter translation.
pub const TRANSLATION_METADATA_FEATURE: &str = "reader_chapter_translation_metadata_v1";
/// Read-only paging of the recorded source-to-AI block correspondence.
pub const TRANSLATION_ALIGNMENT_FEATURE: &str = "reader_chapter_translation_alignment_v1";
/// Rows per `translate_alignment` page (the contract allows 1..256).
pub const AI_ALIGNMENT_PAGE: usize = 256;
/// Exact historical text of any retained revision on the verified chain (`revision_chunk`).
pub const REVISION_TEXT_FEATURE: &str = "reader_revision_text_v1";
/// Largest `revision_chunk` the agent serves (`limit_bytes` = 1..8192).
pub const REVISION_CHUNK_BYTES: usize = 8192;
/// Largest `edit_chunk` the agent accepts.
pub const EDIT_CHUNK_BYTES: usize = 8192;
/// Largest chapter body an edit may propose (the existing editor's limit).
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;
/// Rows per alignment page.
pub const ALIGNMENT_PAGE: usize = 128;
const MAX_ALIGNMENT_PAGES: usize = 64;
/// Largest chunk the agent serves.
pub const CHUNK_LIMIT: usize = 32 * 1024;
/// A chapter larger than this is refused rather than read without bound.
const MAX_CHAPTER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReaderRequest {
    Toc { item_id: String, revision_id: String },
    Chunk { item_id: String, revision_id: String, side: String, chapter: String, offset: usize, limit: usize },
    Alignment { item_id: String, revision_id: String, chapter: String, offset: usize, limit: usize, expected_version: String },
    AlignRequest { item_id: String, revision_id: String, chapter: String, request_id: String, expected_alignment_version: String },
    AlignStatus { item_id: String, job_id: String },
    EditBegin { item_id: String, revision_id: String, side: String, chapter: String, expected_chapter_sha256: String, request_id: String, bytes: usize, sha256: String },
    EditChunk { item_id: String, edit_id: String, offset: usize, markdown: String },
    EditCommit { item_id: String, edit_id: String },
    TranslatePreview { item_id: String, revision_id: String, chapter: String },
    TranslateRequest { item_id: String, revision_id: String, chapter: String, request_id: String, quote_id: String, accept_cost: bool, expected_translation_version: String },
    TranslateStatus { item_id: String, job_id: String },
    TranslateChunk { item_id: String, revision_id: String, chapter: String, expected_version: String, offset: usize, limit: usize },
    TranslateMetadata { item_id: String, revision_id: String, chapter: String },
    TranslateAlignment { item_id: String, revision_id: String, chapter: String, expected_version: String, offset: usize, limit: usize },
    RevisionChunk { item_id: String, revision_id: String, target: RevisionTarget, start_byte: usize, limit_bytes: usize, expected_sha256: String },
    FormatPreview(crate::format::FormatPreviewParams),
    FormatTargets { preview_id: String, offset: usize, limit: usize },
    FormatRequest { quote_id: String, request_id: String, accept_cost: bool, accept_model_processing: bool },
    FormatStatus { batch_id: String },
    FormatResults { batch_id: String, offset: usize, limit: usize },
    FormatChanges(crate::format::FormatChangesParams),
    FormatRestore { batch_id: String, target_id: String, expected_revision_id: String, request_id: String },
    FormatResume { batch_id: String },
    /// B2 reader state (position and bookmarks).
    State { item_id: String },
    Save { item_id: String, expected_version: String, state: crate::reader_state::ReaderState },
    Resume,
    /// `reader_annotations_v1`: `expected_version` is null only on the first page.
    AnnotationsList { item_id: String, offset: usize, limit: usize, expected_version: Option<String> },
    AnnotationsEdit { item_id: String, expected_version: String, change: crate::reader_state::AnnotationChange },
    /// `reader_annotations_ai_translation_v1`: the same ops in the AI space. Both
    /// versions are null only on the first page; `projection` is never sent.
    #[serde(rename = "annotations_list")]
    AiAnnotationsList {
        item_id: String, offset: usize, limit: usize, expected_version: Option<String>, annotation_space: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        projection: Option<crate::ai_marks::AiProjection>,
        expected_view_version: Option<String>,
    },
    #[serde(rename = "annotations_edit")]
    AiAnnotationsEdit { item_id: String, expected_version: String, annotation_space: String, change: crate::ai_marks::AiAnnotationChange },
    /// `reader_passes_v1`: `expected_version` / `expected_timing_version` are null only at offset 0.
    PassesList { item_id: String, offset: usize, limit: usize, expected_version: Option<String> },
    PassesStats { item_id: String, pass_id: String, offset: usize, limit: usize, expected_timing_version: Option<String> },
    PassesEdit { item_id: String, expected_version: String, change: crate::passes::PassChange },
    PassesTimingStart { item_id: String, pass_id: String },
    PassesTimingReport { item_id: String, pass_id: String, session_id: String, segments: Vec<crate::passes::TimingSegment> },
}

/// What one revision text read addresses: an ordinary item's whole canonical
/// body, or one chapter of one side of a book.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevisionTarget {
    Body {},
    Chapter { side: String, chapter: String },
}

#[derive(Deserialize)]
struct RevisionChunk {
    item_id: String,
    revision_id: String,
    target: RevisionTarget,
    sha256: String,
    total_bytes: usize,
    start_byte: usize,
    end_byte: usize,
    next_offset: Option<usize>,
    text: String,
}

/// The complete text of one target at one revision, every byte checked against
/// the agent's stated length and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionText {
    pub item_id: String,
    pub revision_id: String,
    pub target: RevisionTarget,
    pub sha256: String,
    pub text: String,
}

/// A revision text that was not read completely: how far it got, never the
/// partial text itself (a prefix must not be shown as the whole).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionReadError {
    pub error: ClientError,
    pub read_bytes: usize,
    pub total_bytes: Option<usize>,
}

/// The saved Translation route a preview resolved (no secret).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationRoute {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// Ceilings of one confirmed chapter job, not expected consumption.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationBudget {
    pub max_requests: u64,
    pub max_input_bytes_per_request: u64,
    pub max_completion_tokens_per_request: u64,
    #[serde(default)]
    pub max_total_input_bytes: Option<u64>,
    #[serde(default)]
    pub max_total_completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EstimatedCost {
    pub amount: String,
    pub currency: String,
    pub kind: String,
}

/// What the owner confirms before a chapter is translated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationQuote {
    pub quote_id: String,
    /// Unix epoch milliseconds.
    pub expires_at: i64,
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub source_sha256: String,
    pub target_language: String,
    pub settings_revision: u64,
    pub route: TranslationRoute,
    pub budget: TranslationBudget,
    pub source_bytes: u64,
    pub planned_requests: u64,
    #[serde(default)]
    pub estimated_cost: Option<EstimatedCost>,
}

/// A published AI translation of one original chapter (a projection: not the
/// human translation edition, no new revision, counterpart stays null).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationMeta {
    pub version: String,
    pub item_id: String,
    pub revision_id: String,
    pub original_chapter: String,
    pub source_sha256: String,
    pub target_language: String,
    pub sha256: String,
    pub bytes: usize,
    pub provenance: String,
    pub route: TranslationRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct TranslationMetadataReply {
    item_id: String,
    revision_id: String,
    chapter: String,
    translation: Option<TranslationMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AiAlignmentPage {
    item_id: String,
    revision_id: String,
    version: String,
    original_chapter: String,
    translation_chapter: Option<String>,
    original_sha256: String,
    translation_sha256: String,
    original_bytes: usize,
    translation_bytes: usize,
    status: String,
    quality: String,
    basis: String,
    offset: usize,
    total: usize,
    next_offset: Option<usize>,
    pairs: Vec<Pair>,
}

/// The recorded correspondence of one AI translation: generation blocks, not a
/// semantic alignment and not certified (`quality=not_certified`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiAlignment {
    /// A legacy projection recorded no map: the text stays readable, unsynced.
    Missing,
    /// Every row is matched; `o` indexes the original chapter, `t` the AI body.
    Aligned(Alignment),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationPreview {
    pub eligible: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub quote: Option<TranslationQuote>,
    #[serde(default)]
    pub translation: Option<TranslationMeta>,
}

/// One dispatch's provider-reported usage; each part may be unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct DispatchUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Job usage. Aggregate input/output are the agent's (null when any part is
/// unknown); they are never derived from totals here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct JobUsage {
    pub requests_dispatched: u64,
    pub reported_total_tokens: u64,
    pub unknown_usage_requests: u64,
    #[serde(default)]
    pub dispatches: Option<Vec<DispatchUsage>>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

impl JobUsage {
    /// Total tokens only when every dispatch reported its total.
    pub fn known_total(&self) -> Option<u64> {
        (self.unknown_usage_requests == 0).then_some(self.reported_total_tokens)
    }
}

/// A chapter translation job as the agent reports it (no text).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationJob {
    pub job_id: String,
    pub request_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    #[serde(default)]
    pub quote_id: Option<String>,
    pub state: JobState,
    pub stage: String,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub failure_stage: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<JobUsage>,
    #[serde(default)]
    pub completed_blocks: usize,
    #[serde(default)]
    pub total_blocks: usize,
    #[serde(default)]
    pub translation: Option<TranslationMeta>,
}

#[derive(Deserialize)]
struct TranslationJobReply {
    job: TranslationJob,
}

#[derive(Deserialize)]
struct TranslationChunk {
    #[serde(flatten)]
    meta: TranslationMeta,
    start: usize,
    end: usize,
    markdown: String,
    #[serde(default)]
    next_offset: Option<usize>,
}

/// One owner save of a whole chapter of one side. Keep `request_id` (persisted)
/// until the outcome is known: replaying it resumes the same upload or returns
/// the committed receipt, never a second revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterEdit {
    pub item_id: String,
    /// The revision the chapter text was read at.
    pub revision_id: String,
    /// `original` or `translation`.
    pub side: String,
    pub chapter: String,
    /// SHA-256 of the chapter text as read (the edit's base).
    pub expected_chapter_sha256: String,
    pub request_id: String,
    pub markdown: String,
}

/// `edit_commit` (or a replayed `edit_begin`): `changed=false` means the body was
/// identical and the revision did not move.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EditReceipt {
    pub item_id: String,
    pub request_id: String,
    pub revision_id: String,
    pub side: String,
    pub chapter: String,
    pub chapter_sha256: String,
    pub changed: bool,
}

#[derive(Deserialize)]
struct EditUpload {
    edit_id: String,
    received_bytes: usize,
    max_chunk_bytes: usize,
    state: String,
    #[serde(default)]
    receipt: Option<EditReceipt>,
}

#[derive(Deserialize)]
struct EditProgress {
    edit_id: String,
    received_bytes: usize,
}

pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Consecutive byte ranges of `text` from `from`, each at most `max` bytes and
/// ending on a code point boundary.
pub fn edit_chunks(text: &str, from: usize, max: usize) -> Vec<std::ops::Range<usize>> {
    let (mut out, mut at) = (Vec::new(), from);
    while at < text.len() {
        let mut end = (at + max).min(text.len());
        while !text.is_char_boundary(end) { end -= 1; }
        if end == at { break; }
        out.push(at..end);
        at = end;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairKind {
    Paragraph,
    Heading,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Matched,
    OriginalOnly,
    TranslationOnly,
    /// Unknown matching: never an assertion that the other side lacks the text.
    Unresolved,
}

/// One row: chapter-relative UTF-8 byte ranges on each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Pair {
    pub o: Option<[usize; 2]>,
    pub t: Option<[usize; 2]>,
    pub kind: PairKind,
    pub relation: Relation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentStatus {
    Aligned,
    Partial,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AlignmentPage {
    item_id: String,
    revision_id: String,
    version: String,
    original_chapter: String,
    translation_chapter: Option<String>,
    original_sha256: String,
    translation_sha256: Option<String>,
    original_bytes: usize,
    translation_bytes: Option<usize>,
    status: AlignmentStatus,
    offset: usize,
    total: usize,
    next_offset: Option<usize>,
    pairs: Vec<Pair>,
}

/// A chapter's whole paragraph index (all pages, one version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alignment {
    /// `"missing"` when the chapter has no index.
    pub version: String,
    pub status: AlignmentStatus,
    pub original_sha256: String,
    pub translation_sha256: Option<String>,
    pub original_bytes: usize,
    pub translation_bytes: Option<usize>,
    pub pairs: Vec<Pair>,
}

impl Alignment {
    pub fn is_missing(&self) -> bool {
        self.version == "missing"
    }

    /// Checks the index against the chapter texts it claims to describe: same
    /// bytes, every byte of each side covered exactly once, in order, on block
    /// boundaries. A failure means the index is not used at all.
    pub fn validate(&self, original: &str, translation: Option<&str>) -> Result<(), ClientError> {
        use sha2::{Digest, Sha256};
        let sha = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
        if self.original_bytes != original.len() || self.original_sha256 != sha(original) {
            return Err(ClientError::Daemon("stale_chapter_alignment".into()));
        }
        match (translation, &self.translation_sha256, self.translation_bytes) {
            (Some(t), Some(h), Some(n)) if n == t.len() && *h == sha(t) => {}
            (None, None, None) => {}
            _ => return Err(ClientError::Daemon("stale_chapter_alignment".into())),
        }
        if self.is_missing() {
            return if self.pairs.is_empty() { Ok(()) } else { Err(ClientError::InvalidFrame) };
        }
        let (mut at_o, mut at_t) = (0, 0);
        for p in &self.pairs {
            let ok = match p.relation {
                Relation::Matched => p.o.is_some() && p.t.is_some(),
                Relation::OriginalOnly => p.o.is_some() && p.t.is_none(),
                Relation::TranslationOnly => p.o.is_none() && p.t.is_some(),
                Relation::Unresolved => p.o.is_some() != p.t.is_some(),
            };
            if !ok {
                return Err(ClientError::InvalidFrame);
            }
            for (range, at, text) in [(p.o, &mut at_o, Some(original)), (p.t, &mut at_t, translation)] {
                let Some([a, b]) = range else { continue };
                let Some(text) = text else { return Err(ClientError::InvalidFrame) };
                if a != *at || b <= a || b > text.len() || !text.is_char_boundary(b) {
                    return Err(ClientError::InvalidFrame);
                }
                *at = b;
            }
        }
        if at_o != original.len() || at_t != translation.map_or(0, str::len) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(())
    }

    pub fn matched(&self) -> usize {
        self.pairs.iter().filter(|p| p.relation == Relation::Matched).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    OutcomeUnknown,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Succeeded | JobState::Failed | JobState::OutcomeUnknown)
    }
}

/// A chapter repair job as the agent reports it (no text).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepairJob {
    pub job_id: String,
    pub request_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub state: JobState,
    pub stage: String,
    #[serde(default)]
    pub result_revision_id: Option<String>,
    #[serde(default)]
    pub alignment_version: Option<String>,
    #[serde(default)]
    pub alignment_status: Option<AlignmentStatus>,
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Deserialize)]
struct JobReply {
    job: RepairJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Chapter {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub level: u8,
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub counterpart: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Edition {
    pub side: String,
    pub language: String,
    pub sha256: String,
    pub bytes: usize,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReaderDocument {
    pub item_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub default_side: String,
    #[serde(default)]
    pub alignment: String,
    pub editions: Vec<Edition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReaderChunk {
    pub item_id: String,
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub chapter: String,
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub markdown: String,
}

impl ReaderDocument {
    pub fn edition(&self, side: &str) -> Option<&Edition> {
        self.editions.iter().find(|e| e.side == side)
    }

    /// The original chapter `key` may get an AI translation: an English (`en`,
    /// `en-*`) original edition, the chapter exists and has no counterpart. The
    /// agent checks again; a paired chapter is never offered (it would waste tokens).
    pub fn translation_eligible(&self, key: &str) -> bool {
        let Some(o) = self.edition("original") else { return false };
        let lang = o.language.to_ascii_lowercase();
        (lang == "en" || lang.starts_with("en-")) && o.chapters.iter().any(|c| c.key == key && c.counterpart.is_none())
    }

    /// A paired book: both an original and a translation edition.
    pub fn is_book(&self) -> bool {
        self.edition("original").is_some() && self.edition("translation").is_some()
    }

    /// Reading order of a book: chapters of both editions merged, counterparts on
    /// one entry. Chapters without a counterpart (front matter, appendices) keep
    /// their own entry so they stay reachable.
    pub fn entries(&self) -> Vec<Entry> {
        let (Some(o), Some(t)) = (self.edition("original"), self.edition("translation")) else { return Vec::new() };
        let paired = |c: &Chapter, other: &Edition| c.counterpart.as_ref().and_then(|k| other.chapters.iter().position(|x| &x.key == k));
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        while i < o.chapters.len() || j < t.chapters.len() {
            let oi = o.chapters.get(i);
            let tj = t.chapters.get(j);
            match (oi, tj) {
                (Some(a), _) if paired(a, t).is_none() => { out.push(Entry { original: Some(i), translation: None }); i += 1; }
                (_, Some(b)) if paired(b, o).is_none() => { out.push(Entry { original: None, translation: Some(j) }); j += 1; }
                (Some(a), Some(_)) if paired(a, t) == Some(j) => { out.push(Entry { original: Some(i), translation: Some(j) }); i += 1; j += 1; }
                // Out-of-order counterparts: never guess; show each side on its own.
                (Some(_), Some(_)) => { out.push(Entry { original: Some(i), translation: None }); i += 1; }
                (Some(_), None) => { out.push(Entry { original: Some(i), translation: None }); i += 1; }
                (None, Some(_)) => { out.push(Entry { original: None, translation: Some(j) }); j += 1; }
                (None, None) => break,
            }
        }
        out
    }
}

/// One stop in a book: a chapter of the original, of the translation, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub original: Option<usize>,
    pub translation: Option<usize>,
}

impl Client {
    fn require_reader(&self) -> Result<(), ClientError> {
        if self.capabilities()?.features.iter().any(|f| f == READER_FEATURE) { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    pub(crate) fn reader(&self, request: ReaderRequest) -> Result<serde_json::Value, ClientError> {
        self.backend
            .send(&Command::Reader { max_sensitivity: Sensitivity::Restricted, request })?
            .reader
            .ok_or(ClientError::InvalidFrame)
    }

    /// The document's editions and chapters at `revision_id` (no body text).
    pub fn reader_toc(&self, item_id: &str, revision_id: &str) -> Result<ReaderDocument, ClientError> {
        self.require_reader()?;
        let doc: ReaderDocument = serde_json::from_value(self.reader(ReaderRequest::Toc { item_id: item_id.into(), revision_id: revision_id.into() })?)
            .map_err(|_| ClientError::InvalidFrame)?;
        if doc.item_id != item_id || doc.revision_id != revision_id {
            return Err(ClientError::Daemon("stale_item_revision".into()));
        }
        Ok(doc)
    }

    /// The paragraph index of one chapter (keyed by its original chapter), every
    /// page at one version; a page from another version restarts the read.
    /// `Unsupported` without `reader_paragraph_alignment_v1`.
    pub fn reader_alignment(&self, doc: &ReaderDocument, chapter: &str) -> Result<Alignment, ClientError> {
        if !self.capabilities()?.features.iter().any(|f| f == ALIGNMENT_FEATURE) {
            return Err(ClientError::Unsupported);
        }
        'read: for _attempt in 0..3 {
            let (mut pairs, mut version, mut first) = (Vec::new(), "any".to_string(), None::<AlignmentPage>);
            for _ in 0..MAX_ALIGNMENT_PAGES {
                let request = ReaderRequest::Alignment { item_id: doc.item_id.clone(), revision_id: doc.revision_id.clone(), chapter: chapter.into(), offset: pairs.len(), limit: ALIGNMENT_PAGE, expected_version: version.clone() };
                let page: AlignmentPage = match self.reader(request) {
                    Ok(v) => serde_json::from_value(v).map_err(|_| ClientError::InvalidFrame)?,
                    Err(ClientError::Daemon(c)) if c == "stale_chapter_alignment" => continue 'read,
                    Err(e) => return Err(e),
                };
                if page.item_id != doc.item_id || page.revision_id != doc.revision_id || page.original_chapter != chapter || page.offset != pairs.len() {
                    return Err(ClientError::InvalidFrame);
                }
                if let Some(f) = &first {
                    if f.version != page.version || f.total != page.total || f.translation_chapter != page.translation_chapter { continue 'read; }
                }
                version = page.version.clone();
                pairs.extend(page.pairs.iter().copied());
                let next = page.next_offset;
                first.get_or_insert(page);
                match next {
                    Some(n) if n == pairs.len() => {}
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => {
                        let f = first.expect("first page");
                        if pairs.len() != f.total { return Err(ClientError::InvalidFrame); }
                        return Ok(Alignment { version: f.version, status: f.status, original_sha256: f.original_sha256, translation_sha256: f.translation_sha256, original_bytes: f.original_bytes, translation_bytes: f.translation_bytes, pairs });
                    }
                }
            }
            return Err(ClientError::InvalidFrame);
        }
        Err(ClientError::Daemon("stale_chapter_alignment".into()))
    }

    /// Whether the agent offers chapter repair at all (not model readiness).
    pub fn reader_repair_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == REPAIR_FEATURE))
    }

    /// Admits one owner-requested repair of `chapter`. Reuse `request_id` after an
    /// unknown outcome; a new attempt after a failure needs a new id.
    pub fn reader_align_request(&self, doc: &ReaderDocument, chapter: &str, request_id: &str, expected_alignment_version: &str) -> Result<RepairJob, ClientError> {
        if !self.reader_repair_supported() { return Err(ClientError::Unsupported); }
        let value = self.reader(ReaderRequest::AlignRequest { item_id: doc.item_id.clone(), revision_id: doc.revision_id.clone(), chapter: chapter.into(), request_id: request_id.into(), expected_alignment_version: expected_alignment_version.into() })?;
        let job = serde_json::from_value::<JobReply>(value).map_err(|_| ClientError::InvalidFrame)?.job;
        if job.request_id != request_id || job.item_id != doc.item_id { return Err(ClientError::InvalidFrame); }
        Ok(job)
    }

    pub fn reader_align_status(&self, item_id: &str, job_id: &str) -> Result<RepairJob, ClientError> {
        let value = self.reader(ReaderRequest::AlignStatus { item_id: item_id.into(), job_id: job_id.into() })?;
        let job = serde_json::from_value::<JobReply>(value).map_err(|_| ClientError::InvalidFrame)?.job;
        if job.job_id != job_id || job.item_id != item_id { return Err(ClientError::InvalidFrame); }
        Ok(job)
    }

    /// Whether the agent offers owner chapter edits (`reader_chapter_edit_v1`).
    pub fn reader_edit_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == EDIT_FEATURE))
    }

    /// Saves one chapter of one side: `edit_begin`, the remaining chunks from
    /// what the agent already received, then `edit_commit`. Calling it again with
    /// the same `edit` (same request id) after a lost response resumes the same
    /// upload or returns the committed receipt. Every reply is checked against the
    /// request; a mismatch is `InvalidFrame` (outcome unknown), never accepted.
    pub fn reader_edit_chapter(&self, edit: &ChapterEdit) -> Result<EditReceipt, ClientError> {
        // A failed capability probe is a transport error (outcome unknown), not "unsupported".
        if !self.capabilities()?.features.iter().any(|f| f == EDIT_FEATURE) { return Err(ClientError::Unsupported); }
        let text = &edit.markdown;
        // As the Markdown editor: refused here, nothing is sent.
        if text.len() > MAX_EDIT_BYTES { return Err(ClientError::Daemon("reader_edit_too_large".into())); }
        if text.trim().is_empty() || text.contains('\0') || !matches!(edit.side.as_str(), "original" | "translation") {
            return Err(ClientError::Daemon("invalid_reader_edit".into()));
        }
        let sha256 = sha256_hex(text);
        let check = |r: EditReceipt| -> Result<EditReceipt, ClientError> {
            let moved = r.revision_id != edit.revision_id;
            if r.item_id != edit.item_id || r.request_id != edit.request_id || r.side != edit.side || r.chapter != edit.chapter
                || r.chapter_sha256 != sha256 || r.changed != moved || r.revision_id.is_empty()
            {
                return Err(ClientError::InvalidFrame);
            }
            Ok(r)
        };
        let begin = self.reader(ReaderRequest::EditBegin {
            item_id: edit.item_id.clone(), revision_id: edit.revision_id.clone(), side: edit.side.clone(), chapter: edit.chapter.clone(),
            expected_chapter_sha256: edit.expected_chapter_sha256.clone(), request_id: edit.request_id.clone(), bytes: text.len(), sha256: sha256.clone(),
        })?;
        let upload: EditUpload = serde_json::from_value(begin).map_err(|_| ClientError::InvalidFrame)?;
        if upload.edit_id.is_empty() || upload.received_bytes > text.len() || !text.is_char_boundary(upload.received_bytes) || upload.max_chunk_bytes < 4 {
            return Err(ClientError::InvalidFrame);
        }
        match (upload.state.as_str(), upload.receipt) {
            ("committed", Some(r)) if upload.received_bytes == text.len() => return check(r),
            ("uploading", None) => {}
            _ => return Err(ClientError::InvalidFrame),
        }
        let max = upload.max_chunk_bytes.min(EDIT_CHUNK_BYTES);
        let mut received = upload.received_bytes;
        for range in edit_chunks(text, received, max) {
            let value = self.reader(ReaderRequest::EditChunk { item_id: edit.item_id.clone(), edit_id: upload.edit_id.clone(), offset: range.start, markdown: text[range.clone()].to_string() })?;
            let progress: EditProgress = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            if progress.edit_id != upload.edit_id || progress.received_bytes != range.end { return Err(ClientError::InvalidFrame); }
            received = range.end;
        }
        if received != text.len() { return Err(ClientError::InvalidFrame); }
        let value = self.reader(ReaderRequest::EditCommit { item_id: edit.item_id.clone(), edit_id: upload.edit_id })?;
        check(serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?)
    }

    /// Whether the agent offers chapter translation at all (not model readiness).
    pub fn reader_translation_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == TRANSLATION_FEATURE))
    }

    /// `translate_preview`: no inference. Either an existing AI result (read
    /// without cost), a quote to confirm, or why the chapter is not eligible.
    pub fn reader_translate_preview(&self, doc: &ReaderDocument, chapter: &str) -> Result<TranslationPreview, ClientError> {
        if !self.reader_translation_supported() { return Err(ClientError::Unsupported); }
        let value = self.reader(ReaderRequest::TranslatePreview { item_id: doc.item_id.clone(), revision_id: doc.revision_id.clone(), chapter: chapter.into() })?;
        let p: TranslationPreview = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        let q_ok = p.quote.as_ref().is_none_or(|q| q.item_id == doc.item_id && q.revision_id == doc.revision_id && q.chapter == chapter && !q.quote_id.is_empty());
        let t_ok = p.translation.as_ref().is_none_or(|t| t.item_id == doc.item_id && t.revision_id == doc.revision_id && t.original_chapter == chapter);
        // An eligible preview has exactly one of a quote or a result; an ineligible one neither.
        let shape = if p.eligible { p.quote.is_some() != p.translation.is_some() } else { p.quote.is_none() && p.translation.is_none() };
        if !q_ok || !t_ok || !shape { return Err(ClientError::InvalidFrame); }
        Ok(p)
    }

    /// `translate_request` after the owner confirmed `quote_id`. Persist
    /// `request_id` and `quote_id` before calling; after an unknown outcome call
    /// again with both unchanged (the agent returns the original job, never a
    /// second dispatch). A new attempt after a failure needs a new preview and id.
    pub fn reader_translate_request(&self, doc: &ReaderDocument, chapter: &str, request_id: &str, quote_id: &str) -> Result<TranslationJob, ClientError> {
        if !self.reader_translation_supported() { return Err(ClientError::Unsupported); }
        let value = self.reader(ReaderRequest::TranslateRequest {
            item_id: doc.item_id.clone(), revision_id: doc.revision_id.clone(), chapter: chapter.into(), request_id: request_id.into(),
            quote_id: quote_id.into(), accept_cost: true, expected_translation_version: "missing".into(),
        })?;
        let job = serde_json::from_value::<TranslationJobReply>(value).map_err(|_| ClientError::InvalidFrame)?.job;
        if job.request_id != request_id || job.item_id != doc.item_id || job.chapter != chapter { return Err(ClientError::InvalidFrame); }
        Ok(job)
    }

    pub fn reader_translate_status(&self, item_id: &str, job_id: &str) -> Result<TranslationJob, ClientError> {
        let value = self.reader(ReaderRequest::TranslateStatus { item_id: item_id.into(), job_id: job_id.into() })?;
        let job = serde_json::from_value::<TranslationJobReply>(value).map_err(|_| ClientError::InvalidFrame)?.job;
        if job.job_id != job_id || job.item_id != item_id { return Err(ClientError::InvalidFrame); }
        Ok(job)
    }

    /// The whole AI translation `meta` describes, chunk by chunk, checked against
    /// its length and hash. Reading never starts work.
    pub fn reader_translation_text(&self, meta: &TranslationMeta) -> Result<String, ClientError> {
        if meta.bytes > MAX_CHAPTER_BYTES { return Err(ClientError::Daemon("result_too_large".into())); }
        let mut markdown = String::with_capacity(meta.bytes);
        loop {
            let value = self.reader(ReaderRequest::TranslateChunk {
                item_id: meta.item_id.clone(), revision_id: meta.revision_id.clone(), chapter: meta.original_chapter.clone(),
                expected_version: meta.version.clone(), offset: markdown.len(), limit: CHUNK_LIMIT,
            })?;
            let chunk: TranslationChunk = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            if chunk.meta != *meta || chunk.start != markdown.len() || chunk.end != chunk.start + chunk.markdown.len() || chunk.end > meta.bytes {
                return Err(ClientError::InvalidFrame);
            }
            markdown.push_str(&chunk.markdown);
            match chunk.next_offset {
                Some(next) if next == markdown.len() && !chunk.markdown.is_empty() => {}
                Some(_) => return Err(ClientError::InvalidFrame),
                None if markdown.len() == meta.bytes && sha256_hex(&markdown) == meta.sha256 => return Ok(markdown),
                None => return Err(ClientError::InvalidFrame),
            }
        }
    }

    /// `translate_metadata`: the completed AI translation of this exact chapter,
    /// if any. No preview, quote, job or credential read; `None` means none is
    /// published now (not a failure and no reason to translate).
    pub fn reader_translate_metadata(&self, doc: &ReaderDocument, chapter: &str) -> Result<Option<TranslationMeta>, ClientError> {
        if !self.capabilities()?.features.iter().any(|f| f == TRANSLATION_METADATA_FEATURE) { return Err(ClientError::Unsupported); }
        let value = self.reader(ReaderRequest::TranslateMetadata { item_id: doc.item_id.clone(), revision_id: doc.revision_id.clone(), chapter: chapter.into() })?;
        let r: TranslationMetadataReply = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        if r.item_id != doc.item_id || r.revision_id != doc.revision_id || r.chapter != chapter { return Err(ClientError::InvalidFrame); }
        if r.translation.as_ref().is_some_and(|t| t.item_id != doc.item_id || t.revision_id != doc.revision_id || t.original_chapter != chapter || t.version.is_empty()) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(r.translation)
    }

    /// `translate_alignment` of the projection `meta` describes, every page at
    /// that version (`stale_reader_translation` when it changed: the caller
    /// reads the metadata again). `limit` is the page size (1..=256).
    pub fn reader_translate_alignment(&self, meta: &TranslationMeta, limit: usize) -> Result<AiAlignment, ClientError> {
        if !self.capabilities()?.features.iter().any(|f| f == TRANSLATION_ALIGNMENT_FEATURE) { return Err(ClientError::Unsupported); }
        let limit = limit.clamp(1, AI_ALIGNMENT_PAGE);
        let (mut pairs, mut first) = (Vec::<Pair>::new(), None::<AiAlignmentPage>);
        for _ in 0..(MAX_ALIGNMENT_PAGES * ALIGNMENT_PAGE / limit.min(ALIGNMENT_PAGE)).max(MAX_ALIGNMENT_PAGES) {
            let value = self.reader(ReaderRequest::TranslateAlignment {
                item_id: meta.item_id.clone(), revision_id: meta.revision_id.clone(), chapter: meta.original_chapter.clone(),
                expected_version: meta.version.clone(), offset: pairs.len(), limit,
            })?;
            let page: AiAlignmentPage = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            let same = page.item_id == meta.item_id && page.revision_id == meta.revision_id && page.version == meta.version
                && page.original_chapter == meta.original_chapter && page.translation_chapter.is_none()
                && page.translation_sha256 == meta.sha256 && page.translation_bytes == meta.bytes
                && page.quality == "not_certified" && page.basis == "translation_blocks" && page.offset == pairs.len()
                && page.pairs.len() <= limit && page.pairs.iter().all(|p| p.relation == Relation::Matched && p.o.is_some() && p.t.is_some());
            if !same { return Err(ClientError::InvalidFrame); }
            if let Some(f) = &first {
                if f.status != page.status || f.total != page.total || f.original_sha256 != page.original_sha256 || f.original_bytes != page.original_bytes { return Err(ClientError::InvalidFrame); }
            }
            match page.status.as_str() {
                "missing" if page.offset == 0 && page.total == 0 && page.pairs.is_empty() && page.next_offset.is_none() => return Ok(AiAlignment::Missing),
                "aligned" => {}
                _ => return Err(ClientError::InvalidFrame),
            }
            pairs.extend(page.pairs.iter().copied());
            let next = page.next_offset;
            first.get_or_insert(page);
            match next {
                Some(n) if n == pairs.len() && n < first.as_ref().map_or(0, |f| f.total) => {}
                Some(_) => return Err(ClientError::InvalidFrame),
                None => {
                    let f = first.expect("first page");
                    if pairs.len() != f.total || pairs.is_empty() { return Err(ClientError::InvalidFrame); }
                    return Ok(AiAlignment::Aligned(Alignment { version: f.version, status: AlignmentStatus::Aligned, original_sha256: f.original_sha256,
                        translation_sha256: Some(f.translation_sha256), original_bytes: f.original_bytes, translation_bytes: Some(f.translation_bytes), pairs }));
                }
            }
        }
        Err(ClientError::InvalidFrame)
    }

    /// Whether the agent serves exact historical revision text (`reader_revision_text_v1`).
    pub fn reader_revision_text_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == REVISION_TEXT_FEATURE))
    }

    /// The complete text of `target` at `revision_id`, read by bounded
    /// `revision_chunk`s: the first with `expected_sha256="any"`, every later one
    /// bound to the hash the first returned. Each chunk must continue exactly
    /// where the last ended; the whole must match the stated length and hash.
    /// `progress(read, total)` follows each chunk. Anything short of the
    /// complete, verified text is an error, never a prefix.
    pub fn reader_revision_text(&self, item_id: &str, revision_id: &str, target: &RevisionTarget, progress: &dyn Fn(usize, usize)) -> Result<RevisionText, RevisionReadError> {
        let fail = |error, read_bytes, total_bytes| RevisionReadError { error, read_bytes, total_bytes };
        match self.capabilities() {
            Ok(c) if c.features.iter().any(|f| f == REVISION_TEXT_FEATURE) => {}
            Ok(_) => return Err(fail(ClientError::Unsupported, 0, None)),
            Err(e) => return Err(fail(e, 0, None)),
        }
        let mut text = String::new();
        let mut first: Option<(String, usize)> = None;
        loop {
            let (expected, total) = match &first { Some((s, t)) => (s.clone(), Some(*t)), None => ("any".to_string(), None) };
            let at = text.len();
            let value = self
                .reader(ReaderRequest::RevisionChunk { item_id: item_id.into(), revision_id: revision_id.into(), target: target.clone(), start_byte: at, limit_bytes: REVISION_CHUNK_BYTES, expected_sha256: expected })
                .map_err(|e| fail(e, at, total))?;
            let chunk: RevisionChunk = serde_json::from_value(value).map_err(|_| fail(ClientError::InvalidFrame, at, total))?;
            let hex = chunk.sha256.len() == 64 && chunk.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            let same = first.as_ref().is_none_or(|(s, t)| *s == chunk.sha256 && *t == chunk.total_bytes);
            if chunk.item_id != item_id || chunk.revision_id != revision_id || chunk.target != *target || !hex || !same
                || chunk.start_byte != at || chunk.end_byte != at + chunk.text.len() || chunk.end_byte > chunk.total_bytes
                || chunk.next_offset != (chunk.end_byte < chunk.total_bytes).then_some(chunk.end_byte)
                || chunk.text.len() > REVISION_CHUNK_BYTES || (chunk.text.is_empty() && chunk.end_byte < chunk.total_bytes)
            {
                return Err(fail(ClientError::InvalidFrame, at, total.or(Some(chunk.total_bytes))));
            }
            if chunk.total_bytes > MAX_EDIT_BYTES {
                return Err(fail(ClientError::Daemon("result_too_large".into()), at, Some(chunk.total_bytes)));
            }
            let total = chunk.total_bytes;
            if first.is_none() {
                text.reserve(total);
                first = Some((chunk.sha256.clone(), total));
            }
            text.push_str(&chunk.text);
            progress(text.len(), total);
            if chunk.next_offset.is_none() {
                let sha256 = first.map(|f| f.0).unwrap_or_default();
                if text.len() != total || sha256_hex(&text) != sha256 {
                    return Err(fail(ClientError::InvalidFrame, text.len(), Some(total)));
                }
                return Ok(RevisionText { item_id: item_id.into(), revision_id: revision_id.into(), target: target.clone(), sha256, text });
            }
        }
    }

    /// One whole chapter of one side, following `next_offset` chunk by chunk.
    pub fn reader_chapter(&self, doc: &ReaderDocument, side: &str, chapter: &str) -> Result<String, ClientError> {
        let edition = doc.edition(side).ok_or(ClientError::InvalidFrame)?;
        let ch = edition.chapters.iter().find(|c| c.key == chapter).ok_or(ClientError::InvalidFrame)?;
        let size = ch.end.checked_sub(ch.start).ok_or(ClientError::InvalidFrame)?;
        if size > MAX_CHAPTER_BYTES {
            return Err(ClientError::Daemon("result_too_large".into()));
        }
        let mut markdown = String::with_capacity(size);
        let mut offset = 0;
        loop {
            let value = self.reader(ReaderRequest::Chunk {
                item_id: doc.item_id.clone(),
                revision_id: doc.revision_id.clone(),
                side: side.into(),
                chapter: chapter.into(),
                offset,
                limit: CHUNK_LIMIT,
            })?;
            let chunk: ReaderChunk = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            // Each chunk must continue exactly where the last one ended, on this
            // revision and edition; anything else is refused, never stitched.
            if chunk.item_id != doc.item_id || chunk.revision_id != doc.revision_id || chunk.side != side || chunk.chapter != chapter
                || chunk.sha256 != edition.sha256 || chunk.start != ch.start + offset || chunk.end != chunk.start + chunk.markdown.len()
                || chunk.end > ch.end
            {
                return Err(ClientError::InvalidFrame);
            }
            markdown.push_str(&chunk.markdown);
            match chunk.next_offset {
                Some(next) if next == offset + chunk.markdown.len() && next > offset => offset = next,
                Some(_) => return Err(ClientError::InvalidFrame),
                None if chunk.end == ch.end => return Ok(markdown),
                None => return Err(ClientError::InvalidFrame),
            }
        }
    }
}
