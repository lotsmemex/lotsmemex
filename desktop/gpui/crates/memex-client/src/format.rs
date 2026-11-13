//! 格式整理 (`reader_content_format_v1`, batches `reader_content_format_batch_v1`):
//! a local preview with a server-owned quote, one explicit confirmation, a
//! durable batch (one target for a single item), per-target results, the proof
//! changes, restore and resume. Every reply is checked against what was asked;
//! a mismatch is `InvalidFrame` (outcome unknown), never accepted. Nothing here
//! retries a request, restore or resume on its own.

use crate::book::{EstimatedCost, JobUsage, ReaderRequest, RevisionTarget, TranslationBudget, TranslationRoute};
use crate::{Client, ClientError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT_FEATURE: &str = "reader_content_format_v1";
pub const FORMAT_BATCH_FEATURE: &str = "reader_content_format_batch_v1";
/// Rows per `format_results` / `format_targets` page (contract: 1..100).
pub const RESULTS_PAGE: usize = 20;
/// Rows per `format_changes` page (contract: 1..64).
pub const CHANGES_PAGE: usize = 64;
/// `format_changes` view=text chunk (contract: 1..8192).
pub const CHANGE_TEXT_BYTES: usize = 8192;

/// What one formatting covers: one item's body or one side of one chapter, or a folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FormatSelection {
    Item { item_id: String, revision_id: String, target: RevisionTarget },
    Group { group_id: String, include_descendants: bool },
}

/// `format_preview` params: start with a selection, or read a running preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum FormatPreviewParams {
    Selection { selection: FormatSelection },
    Status { preview_id: String },
}

/// `format_changes` params: the proof list, or one part of one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum FormatChangesParams {
    List { batch_id: String, target_id: String, expected_version: String, offset: usize, limit: usize },
    Text { batch_id: String, target_id: String, expected_version: String, change_id: String, part: String, start_byte: usize, limit_bytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct FormatCounts {
    pub items: u64,
    pub targets: u64,
    pub eligible: u64,
    pub skipped: u64,
    pub private_targets: u64,
    #[serde(default)]
    pub skipped_by_reason: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatBudget {
    pub max_requests: u64,
    pub max_input_bytes: u64,
    pub max_completion_tokens: u64,
}

/// What the owner confirms: frozen scope, Low dial route (null when no model
/// request is planned) and the checked total budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatQuote {
    pub quote_id: String,
    pub expires_at: i64,
    pub selection: FormatSelection,
    pub policy_version: String,
    #[serde(default)]
    pub route: Option<TranslationRoute>,
    pub counts: FormatCounts,
    pub budget: FormatBudget,
    #[serde(default)]
    pub estimated_cost: Option<EstimatedCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatPreview {
    pub preview_id: String,
    /// preparing | ready | failed
    pub state: String,
    #[serde(default)]
    pub quote: Option<FormatQuote>,
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatTarget {
    pub target_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub target: RevisionTarget,
    #[serde(default)]
    pub source_sha256: Option<String>,
    #[serde(default)]
    pub source_bytes: Option<u64>,
    /// eligible | skipped
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub budget: Option<TranslationBudget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TargetsPage {
    pub preview_id: String,
    pub total: usize,
    pub offset: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub targets: Vec<FormatTarget>,
}

/// One frozen target's (and its child job's) state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Queued,
    Running,
    Succeeded,
    Failed,
    OutcomeUnknown,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct BatchCounts {
    pub total: u64,
    pub queued: u64,
    pub running: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub outcome_unknown: u64,
    pub skipped: u64,
}

impl BatchCounts {
    pub fn settled(&self) -> u64 {
        self.succeeded + self.failed + self.outcome_unknown + self.skipped
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatBatch {
    pub batch_id: String,
    pub request_id: String,
    /// queued | running | paused | done (done is not "every target succeeded").
    pub state: String,
    pub counts: BatchCounts,
    pub usage: JobUsage,
    #[serde(default)]
    pub error_code: Option<String>,
}

impl FormatBatch {
    pub fn is_settled(&self) -> bool {
        matches!(self.state.as_str(), "done" | "paused")
    }
}

#[derive(Deserialize)]
struct BatchReply {
    batch: FormatBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatJob {
    pub job_id: String,
    pub request_id: String,
    pub operation: String,
    pub item_id: String,
    pub revision_id: String,
    pub target: RevisionTarget,
    pub state: TargetState,
    pub stage: String,
    #[serde(default)]
    pub result_revision_id: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub failure_stage: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub usage: JobUsage,
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// The settled receipt of a succeeded target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Formatting {
    /// complete | partial
    pub status: String,
    pub changed: bool,
    pub applied_count: u64,
    pub correction_count: u64,
    pub deleted_blocks: u64,
    pub unresolved_count: u64,
    pub checked_bytes: u64,
    pub total_bytes: u64,
    pub changes_version: String,
    pub quality: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatResult {
    pub target_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub target: RevisionTarget,
    pub state: TargetState,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub job: Option<FormatJob>,
    #[serde(default)]
    pub formatting: Option<Formatting>,
}

impl FormatResult {
    /// What 查看修改 / 退回旧版 need, once a target succeeded.
    pub fn handle(&self, batch_id: &str) -> Option<TargetHandle> {
        let (job, f) = (self.job.as_ref()?, self.formatting.as_ref()?);
        if self.state != TargetState::Succeeded { return None; }
        Some(TargetHandle {
            batch_id: batch_id.into(), target_id: self.target_id.clone(), item_id: self.item_id.clone(), target: self.target.clone(),
            input_revision_id: self.revision_id.clone(), result_revision_id: job.result_revision_id.clone()?, changes_version: f.changes_version.clone(), changed: f.changed,
        })
    }
}

/// A known authorized succeeded target: enough to review and restore it
/// without reloading the whole batch (each request is still authorized).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetHandle {
    pub batch_id: String,
    pub target_id: String,
    pub item_id: String,
    pub target: RevisionTarget,
    pub input_revision_id: String,
    pub result_revision_id: String,
    pub changes_version: String,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResultsPage {
    pub batch_id: String,
    pub total: usize,
    pub offset: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub results: Vec<FormatResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChangeRow {
    pub id: String,
    pub kind: String,
    pub basis: String,
    pub reason_code: String,
    pub old_range: [usize; 2],
    pub new_range: [usize; 2],
    pub old_sha256: String,
    pub new_sha256: String,
    pub before_bytes: usize,
    pub after_bytes: usize,
    #[serde(default)]
    pub source_evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChangesPage {
    pub version: String,
    pub item_id: String,
    pub input_revision_id: String,
    pub result_revision_id: String,
    pub target: RevisionTarget,
    pub source_sha256: String,
    pub result_sha256: String,
    pub total: usize,
    pub offset: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub changes: Vec<ChangeRow>,
}

#[derive(Deserialize)]
struct ChangeTextChunk {
    version: String,
    change_id: String,
    part: String,
    total_bytes: usize,
    start_byte: usize,
    end_byte: usize,
    #[serde(default)]
    next_offset: Option<usize>,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RestoreReceipt {
    pub item_id: String,
    pub request_id: String,
    pub revision_id: String,
    pub changed: bool,
}

/// The read of a whole batch was refused (some member is no longer accessible,
/// or the batch is not visible). The batch and its receipts still exist.
pub fn whole_batch_denied(e: &ClientError) -> bool {
    matches!(e, ClientError::Daemon(c) if matches!(c.as_str(), "library_resource_not_found" | "item_not_found" | "permission_denied" | "content_format_batch_not_found"))
}

/// The send may or may not have reached the agent: keep the request id.
/// `library_loading` is not a definite rejection either, nor is a provider
/// outcome the agent itself could not settle.
pub fn outcome_unknown(e: &ClientError) -> bool {
    matches!(e, ClientError::Timeout | ClientError::Unavailable | ClientError::InvalidFrame | ClientError::TooLarge) || e.is_busy()
        || matches!(e, ClientError::Daemon(c) if c == "provider_outcome_unknown")
}

impl Client {
    pub fn format_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == FORMAT_FEATURE))
    }

    /// A folder needs both capabilities.
    pub fn format_batch_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == FORMAT_FEATURE) && c.features.iter().any(|f| f == FORMAT_BATCH_FEATURE))
    }

    fn require_format(&self, selection: Option<&FormatSelection>) -> Result<(), ClientError> {
        let caps = self.capabilities()?;
        let has = |f: &str| caps.features.iter().any(|x| x == f);
        let ok = has(FORMAT_FEATURE) && (!matches!(selection, Some(FormatSelection::Group { .. })) || has(FORMAT_BATCH_FEATURE));
        if ok { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    fn format_preview_reply(&self, value: serde_json::Value, expected: Option<&str>, selection: Option<&FormatSelection>) -> Result<FormatPreview, ClientError> {
        let p: FormatPreview = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        let state_ok = match p.state.as_str() {
            "ready" => p.quote.as_ref().is_some_and(|q| !q.quote_id.is_empty() && selection.is_none_or(|s| q.selection == *s)),
            "preparing" | "failed" => p.quote.is_none(),
            _ => false,
        };
        if p.preview_id.is_empty() || expected.is_some_and(|e| e != p.preview_id) || !state_ok { return Err(ClientError::InvalidFrame); }
        Ok(p)
    }

    /// `format_preview` {selection}: local work only, no model request.
    pub fn format_preview_start(&self, selection: &FormatSelection) -> Result<FormatPreview, ClientError> {
        self.require_format(Some(selection))?;
        let value = self.reader(ReaderRequest::FormatPreview(FormatPreviewParams::Selection { selection: selection.clone() }))?;
        self.format_preview_reply(value, None, Some(selection))
    }

    /// `format_preview` {preview_id}: the same preview, later.
    pub fn format_preview_read(&self, preview_id: &str, selection: &FormatSelection) -> Result<FormatPreview, ClientError> {
        self.require_format(Some(selection))?;
        let value = self.reader(ReaderRequest::FormatPreview(FormatPreviewParams::Status { preview_id: preview_id.into() }))?;
        self.format_preview_reply(value, Some(preview_id), Some(selection))
    }

    /// Starts a preview and reads it until it is ready or failed (bounded).
    pub fn format_preview(&self, selection: &FormatSelection, pause: std::time::Duration) -> Result<FormatPreview, ClientError> {
        let mut p = self.format_preview_start(selection)?;
        for _ in 0..120 {
            if p.state != "preparing" { return Ok(p); }
            std::thread::sleep(pause);
            p = self.format_preview_read(&p.preview_id, selection)?;
        }
        Err(ClientError::Timeout)
    }

    pub fn format_targets(&self, preview_id: &str, offset: usize, limit: usize) -> Result<TargetsPage, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatTargets { preview_id: preview_id.into(), offset, limit })?;
        let page: TargetsPage = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        if page.preview_id != preview_id || page.offset != offset || page.targets.len() > limit || page.next_offset.is_some_and(|n| n != offset + page.targets.len()) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(page)
    }

    /// `format_request` after the owner confirmed `quote_id` (and, for private
    /// text, agreed to send it). Persist `request_id` before calling; after an
    /// unknown outcome call again with both unchanged (the agent returns the
    /// same batch). A new attempt needs a fresh preview and id.
    pub fn format_request(&self, quote_id: &str, request_id: &str) -> Result<FormatBatch, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatRequest { quote_id: quote_id.into(), request_id: request_id.into(), accept_cost: true, accept_model_processing: true })?;
        let batch = serde_json::from_value::<BatchReply>(value).map_err(|_| ClientError::InvalidFrame)?.batch;
        if batch.request_id != request_id || batch.batch_id.is_empty() { return Err(ClientError::InvalidFrame); }
        Ok(batch)
    }

    pub fn format_status(&self, batch_id: &str) -> Result<FormatBatch, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatStatus { batch_id: batch_id.into() })?;
        let batch = serde_json::from_value::<BatchReply>(value).map_err(|_| ClientError::InvalidFrame)?.batch;
        if batch.batch_id != batch_id { return Err(ClientError::InvalidFrame); }
        Ok(batch)
    }

    pub fn format_results(&self, batch_id: &str, offset: usize, limit: usize) -> Result<ResultsPage, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatResults { batch_id: batch_id.into(), offset, limit })?;
        let page: ResultsPage = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        if page.batch_id != batch_id || page.offset != offset || page.results.len() > limit || page.offset + page.results.len() > page.total
            || page.next_offset != (offset + page.results.len() < page.total).then_some(offset + page.results.len())
        {
            return Err(ClientError::InvalidFrame);
        }
        Ok(page)
    }

    /// One page of the proof of `h` (a target-addressed read: no batch metadata).
    pub fn format_changes(&self, h: &TargetHandle, offset: usize) -> Result<ChangesPage, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatChanges(FormatChangesParams::List {
            batch_id: h.batch_id.clone(), target_id: h.target_id.clone(), expected_version: h.changes_version.clone(), offset, limit: CHANGES_PAGE,
        }))?;
        let page: ChangesPage = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        if page.version != h.changes_version || page.item_id != h.item_id || page.input_revision_id != h.input_revision_id || page.result_revision_id != h.result_revision_id
            || page.target != h.target || page.offset != offset || page.changes.len() > CHANGES_PAGE || page.offset + page.changes.len() > page.total
            || page.next_offset != (offset + page.changes.len() < page.total).then_some(offset + page.changes.len())
        {
            return Err(ClientError::InvalidFrame);
        }
        Ok(page)
    }

    /// The first `max` bytes (at most) of one part of one change, and its total size.
    pub fn format_change_text(&self, h: &TargetHandle, change_id: &str, part: &str, max: usize) -> Result<(String, usize), ClientError> {
        self.require_format(None)?;
        let mut text = String::new();
        loop {
            let limit = (max - text.len()).clamp(1, CHANGE_TEXT_BYTES);
            let value = self.reader(ReaderRequest::FormatChanges(FormatChangesParams::Text {
                batch_id: h.batch_id.clone(), target_id: h.target_id.clone(), expected_version: h.changes_version.clone(), change_id: change_id.into(), part: part.into(), start_byte: text.len(), limit_bytes: limit,
            }))?;
            let c: ChangeTextChunk = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            if c.version != h.changes_version || c.change_id != change_id || c.part != part || c.start_byte != text.len() || c.end_byte != c.start_byte + c.text.len()
                || c.end_byte > c.total_bytes || c.next_offset != (c.end_byte < c.total_bytes).then_some(c.end_byte)
            {
                return Err(ClientError::InvalidFrame);
            }
            text.push_str(&c.text);
            if c.next_offset.is_none() || text.len() + 4 > max || c.text.is_empty() { return Ok((text, c.total_bytes)); }
        }
    }

    /// `format_restore` against the result revision `h` produced (CAS). Persist
    /// `request_id` before calling; an exact replay returns the one receipt.
    pub fn format_restore(&self, h: &TargetHandle, request_id: &str) -> Result<RestoreReceipt, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatRestore { batch_id: h.batch_id.clone(), target_id: h.target_id.clone(), expected_revision_id: h.result_revision_id.clone(), request_id: request_id.into() })?;
        let r: RestoreReceipt = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        if r.item_id != h.item_id || r.request_id != request_id || r.revision_id.is_empty() || r.changed != (r.revision_id != h.result_revision_id) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(r)
    }

    /// `format_resume`: the same paused batch under its original budget. Never
    /// called automatically; a refusal leaves it as it was.
    pub fn format_resume(&self, batch_id: &str) -> Result<FormatBatch, ClientError> {
        self.require_format(None)?;
        let value = self.reader(ReaderRequest::FormatResume { batch_id: batch_id.into() })?;
        let batch = serde_json::from_value::<BatchReply>(value).map_err(|_| ClientError::InvalidFrame)?.batch;
        if batch.batch_id != batch_id { return Err(ClientError::InvalidFrame); }
        Ok(batch)
    }
}
