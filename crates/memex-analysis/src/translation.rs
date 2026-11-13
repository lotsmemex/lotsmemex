#[cfg(test)]
use super::apply_record;
use super::{
    generated_text_prompt, list_records, prepare_record, revision_ref, ActorKind, AnalysisKind,
    AnalysisRequest, GeneratedTextDescriptor, GeneratedTextPurpose, TextLengthUnit,
    TextOutputLimit, GENERATED_TEXT_PROMPT_VERSION,
};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use memex_core::{IngestReceipt, ItemRevision, Sensitivity, SCHEMA_VERSION};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const DATABASE_NAME: &str = "translation-jobs.sqlite3";
#[path = "translation_preservation.rs"]
mod preservation;
pub use preservation::{preserve_completed_translation, TranslationPreservation};
#[path = "translation_tables.rs"]
mod tables;
pub use tables::*;
const TARGET_LANGUAGE: &str = "zh-CN";
const CLASSIFIER_VERSION: &str = "memex-bounded-language-v1";
pub const MAX_TRANSLATION_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_TRANSLATION_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_TITLE_CHARACTERS: usize = 512;
const MAX_ATTEMPTS: u32 = 3;
/// Terminal reason of a job whose revision was replaced after work started.
pub(crate) const SUPERSEDED: &str = "superseded_by_revision";
/// Terminal reason of a duplicate job of a metadata-only revision: a same-body revision already
/// has the validated result, which this revision inherits (no model request is sent).
pub const INHERITED: &str = "inherited_from_revision";
/// SQL condition: no other job of the same table for the row's item is running. A replacement
/// revision's job waits for the superseded one to stop, so in-flight work is never sent twice
/// (finished batches are stored and reused by content).
pub(crate) fn no_running_sibling(table: &str) -> String {
    format!(
        "NOT EXISTS(SELECT 1 FROM {table} sibling WHERE sibling.item_id={table}.item_id
           AND sibling.state='running')"
    )
}
/// Covers one provider request (120 s) with margin; multi-request documents renew it through
/// `TranslationProgress::heartbeat` while requests are in flight, so a live job is never
/// recovered (and re-dispatched) by another worker.
const LEASE_SECONDS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationJobView {
    pub job_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub source_language: String,
    pub target_language: String,
    pub state: TranslationJobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub attempt_count: u32,
    pub retryable: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationAnalysisPair {
    pub title_analysis_id: String,
    pub document_analysis_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationSkipReason {
    AlreadyChinese,
    ChallengeOrLogin,
    SourceLanguageUnknown,
    SourceTooLarge,
    StaleRevision,
    UnsupportedItemType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TranslationEnqueueOutcome {
    Enqueued {
        job: TranslationJobView,
    },
    Skipped {
        item_id: String,
        revision_id: String,
        reason: TranslationSkipReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationModelDescriptor {
    pub model_id: String,
    pub model_sha256: String,
    pub adapter_id: String,
    pub adapter_version: String,
    /// The concrete adapter must enforce this deadline around each provider call.
    pub request_timeout: Duration,
}

impl TranslationModelDescriptor {
    pub fn is_valid(&self) -> bool {
        let timeout_ms = self.request_timeout.as_millis();
        valid_token(&self.model_id)
            && valid_sha256(&self.model_sha256)
            && valid_token(&self.adapter_id)
            && valid_token(&self.adapter_version)
            // Up to the provider generation deadline (120 s).
            && (1..=120_000).contains(&timeout_ms)
            && self.request_timeout == Duration::from_millis(timeout_ms as u64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationModelError {
    ProviderFailure,
    Timeout,
    /// The request was dispatched and may have been billed; never retried automatically.
    OutcomeUnknown,
    /// The provider answered but the output broke the adapter contract; not retried.
    MalformedOutput,
    /// The source cannot be sent through this adapter (for example over its input bound).
    InputUnsupported,
}

impl std::fmt::Display for TranslationModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ProviderFailure => "translation_provider_failure",
            Self::Timeout => "translation_provider_timeout",
            Self::OutcomeUnknown => "translation_provider_outcome_unknown",
            Self::MalformedOutput => "translation_malformed_model_output",
            Self::InputUnsupported => "translation_provider_input_unsupported",
        })
    }
}

impl std::error::Error for TranslationModelError {}

/// Provider input deliberately implements neither `Debug` nor `Serialize`: source text must not
/// enter generic logs, status payloads, or instrumentation by accident. Only the revision-checking
/// queue can construct it; provider implementations receive read-only accessors.
pub struct TranslationModelInput<'a> {
    purpose: GeneratedTextPurpose,
    source_language: &'a str,
    target_language: &'a str,
    prompt: &'static str,
    source_text: &'a str,
    document_title: &'a str,
    document_markdown: &'a str,
    progress: Option<&'a mut (dyn TranslationProgress + 'a)>,
}

/// Persistence and liveness for a multi-request document translation. Finished batch outputs
/// are kept per item and exact source content (title, Markdown, source language), so an explicit
/// re-request resumes from the first unfinished batch instead of re-sending finished work.
/// Keys are opaque here; the adapter binds them to its prompt, model identity and batch bytes.
/// Only the queue implements it; nothing here carries source or output text to logs.
pub trait TranslationProgress {
    /// The stored outputs of a finished batch.
    fn completed(&mut self, key: &str) -> Option<Vec<String>>;
    /// Stores one finished batch; false when the job's lease was lost.
    fn record(&mut self, key: &str, outputs: &[String]) -> bool;
    /// Renews the job lease and publishes `done` of `total` batches; false when the lease was
    /// lost (the adapter must stop dispatching new requests).
    fn heartbeat(&mut self, done: usize, total: usize) -> bool;
    /// Diagnostic of the failure the adapter is about to return: a fixed code and numbers only
    /// (never source or output text). The queue stores it with the failed job.
    fn failure(&mut self, _failure: TranslationFailure) {}
}

/// Privacy-safe diagnostic of one failed translation attempt: `detail` is a fixed code such
/// as `batch_2_of_5:count_mismatch` or `title:cjk_ratio`; `context` holds numbers only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationFailure {
    pub detail: String,
    pub context: TranslationFailureContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationFailureContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batches_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_blocks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_blocks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_chars: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl TranslationFailure {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            context: TranslationFailureContext::default(),
        }
    }
}

/// `scope:check` with a fixed scope (`title`, `document`, `glossary`, `provider`, `worker`,
/// `batch_N` or `batch_N_of_M`) and a lowercase snake-case check.
pub fn valid_failure_detail(detail: &str) -> bool {
    let Some((scope, check)) = detail.split_once(':') else {
        return false;
    };
    let numbered = |value: &str| {
        !value.is_empty() && value.len() <= 6 && value.bytes().all(|b| b.is_ascii_digit())
    };
    let scope_valid = matches!(scope, "title" | "document" | "glossary" | "provider" | "worker")
        || scope.strip_prefix("batch_").is_some_and(|rest| match rest.split_once("_of_") {
            Some((index, total)) => numbered(index) && numbered(total),
            None => numbered(rest),
        });
    detail.len() <= 64
        && scope_valid
        && (1..=40).contains(&check.len())
        && check.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

impl<'a> TranslationModelInput<'a> {
    /// Batch persistence and lease renewal; present for document translation run by the queue.
    pub fn progress(&mut self) -> Option<&mut dyn TranslationProgress> {
        match &mut self.progress {
            Some(progress) => Some(&mut **progress),
            None => None,
        }
    }

    /// Read-only context: the revision's title (also for the document request).
    pub fn document_title(&self) -> &'a str {
        self.document_title
    }

    /// Read-only context: the revision's whole Markdown (also for the title request).
    pub fn document_markdown(&self) -> &'a str {
        self.document_markdown
    }

    pub fn purpose(&self) -> GeneratedTextPurpose {
        self.purpose
    }

    pub fn source_language(&self) -> &'a str {
        self.source_language
    }

    pub fn target_language(&self) -> &'a str {
        self.target_language
    }

    pub fn prompt(&self) -> &'static str {
        self.prompt
    }

    pub fn source_text(&self) -> &'a str {
        self.source_text
    }
}

/// A provider returns only the requested translated text. Provider diagnostics are intentionally
/// reduced to the bounded `TranslationModelError` enum at this boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum TableTranslationOutput {
    Complete(String),
    /// Known partial response: retain locally, never validate as a complete table.
    IncompleteLength(String),
}

pub trait TranslationModel {
    fn descriptor(&self) -> TranslationModelDescriptor;
    fn translate(
        &mut self,
        input: TranslationModelInput<'_>,
    ) -> std::result::Result<String, TranslationModelError>;
    /// One table-only request. The caller owns durable intent, validation and private result
    /// retention; an adapter must not replace a rejected response with the source table.
    fn translate_table(
        &mut self,
        input: TranslationModelInput<'_>,
    ) -> std::result::Result<TableTranslationOutput, TranslationModelError> {
        self.translate(input).map(TableTranslationOutput::Complete)
    }
}

struct TranslationJobRecord {
    view: TranslationJobView,
    revision_sha256: String,
    lease_owner: String,
    translated_title: Option<String>,
    translated_document: Option<String>,
    model: Option<TranslationModelDescriptor>,
}

pub struct TranslationQueue {
    connection: Connection,
    root: PathBuf,
}

impl TranslationQueue {
    pub fn open(library: impl AsRef<Path>) -> Result<Self> {
        let root = super::safe_library_root(library.as_ref())?;
        let analysis = super::safe_create_dir(&root, "analysis")?;
        let database = analysis.join(DATABASE_NAME);
        ensure_database_file(&database)?;
        validate_database_family(&database)?;
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let database_version =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        if database_version > 1 {
            bail!("unsupported translation database version")
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             PRAGMA secure_delete=ON;
             PRAGMA trusted_schema=OFF;
             CREATE TABLE IF NOT EXISTS translation_jobs(
               job_id TEXT PRIMARY KEY,
               item_id TEXT NOT NULL,
               revision_id TEXT NOT NULL,
               revision_sha256 TEXT NOT NULL,
               source_language TEXT NOT NULL,
               target_language TEXT NOT NULL,
               classifier_version TEXT NOT NULL,
               state TEXT NOT NULL CHECK(state IN ('queued','running','succeeded','failed')),
               reason_code TEXT,
               attempt_count INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               lease_owner TEXT,
               lease_until INTEGER,
               model_id TEXT,
               model_sha256 TEXT,
               adapter_id TEXT,
               adapter_version TEXT,
               request_timeout_ms INTEGER,
               translated_title TEXT,
               translated_document TEXT,
               title_analysis_id TEXT,
               document_analysis_id TEXT,
               UNIQUE(item_id,revision_id,source_language,target_language)
             );
             CREATE TABLE IF NOT EXISTS translation_conflicts(
               id INTEGER PRIMARY KEY,
               job_id TEXT NOT NULL,
               reason_code TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS translation_jobs_order
               ON translation_jobs(created_at,job_id);
             CREATE TABLE IF NOT EXISTS metadata_jobs(
               item_id TEXT NOT NULL,
               revision_id TEXT NOT NULL,
               state TEXT NOT NULL
                 CHECK(state IN ('queued','running','succeeded','skipped','failed')),
               reason_code TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               lease_until INTEGER,
               result_revision_id TEXT,
               PRIMARY KEY(item_id,revision_id)
             );
             CREATE TABLE IF NOT EXISTS translation_batches(
               item_id TEXT NOT NULL,
               content_sha256 TEXT NOT NULL,
               batch_key TEXT NOT NULL,
               outputs_json TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(item_id,content_sha256,batch_key)
             );",
        )?;
        // Assigning even the SAME user_version commits a SQLite write. The
        // reconciler opens this queue for every eligible revision, so doing it
        // on every open needlessly churns WAL/checkpoints beside library reads.
        if database_version == 0 {
            connection.pragma_update(None, "user_version", 1)?;
        }
        // Additive: cloud-provider work is opt-in per job (explicit owner request or a new
        // capture); background reconciliation never sets it.
        add_columns(
            &connection,
            "translation_jobs",
            "provider_authorized",
            "ALTER TABLE translation_jobs
               ADD COLUMN provider_authorized INTEGER NOT NULL DEFAULT 0;",
        )?;
        // Additive: progress of a multi-request document translation.
        add_columns(
            &connection,
            "translation_jobs",
            "batches_done",
            "ALTER TABLE translation_jobs ADD COLUMN batches_done INTEGER;
             ALTER TABLE translation_jobs ADD COLUMN batches_total INTEGER;",
        )?;
        // Additive: privacy-safe failure diagnostics (fixed code + numbers), the start of the
        // current attempt, and whether the title kept its original wording after two invalid
        // answers.
        add_columns(
            &connection,
            "translation_jobs",
            "failure_detail",
            "ALTER TABLE translation_jobs ADD COLUMN failure_detail TEXT;
             ALTER TABLE translation_jobs ADD COLUMN failure_context TEXT;
             ALTER TABLE translation_jobs ADD COLUMN started_at TEXT;
             ALTER TABLE translation_jobs ADD COLUMN title_untranslated INTEGER;",
        )?;
        validate_database_family(&database)?;
        secure_database_family(&database)?;
        Ok(Self { connection, root })
    }

    /// Marks the current job for this revision as eligible for a cloud provider.
    /// Returns false when no job exists for the revision.
    pub fn authorize(&mut self, item_id: &str, revision_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE translation_jobs SET provider_authorized=1
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
        )? == 1)
    }

    /// Explicit owner re-request of a failed job (any reason, including exhausted or unknown
    /// outcome): a fresh attempt budget. A frozen title and stored batch outputs are kept and
    /// reused when the model is unchanged (finished work is not re-sent); a different model
    /// discards the frozen title when the job runs (see `process_leased`).
    pub fn requeue_failed(&mut self, item_id: &str, revision_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE translation_jobs
                SET state='queued',reason_code='manual_retry',attempt_count=0,
                    lease_owner=NULL,lease_until=NULL,translated_document=NULL,
                    provider_authorized=1,updated_at=?4
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3
                AND state='failed' AND lease_owner IS NULL",
            params![
                item_id,
                revision_id,
                TARGET_LANGUAGE,
                Utc::now().to_rfc3339()
            ],
        )? == 1)
    }

    /// Queued explicitly authorized work, so the provider worker resolves settings and
    /// credentials only when a job will actually be sent.
    pub fn has_authorized_work(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM translation_jobs WHERE provider_authorized=1
                   AND state='queued' AND lease_owner IS NULL AND {} AND {})",
                metadata_gate("translation_jobs", Utc::now()),
                no_running_sibling("translation_jobs")
            ),
            [],
            |row| row.get(0),
        )?)
    }

    pub fn process_next_authorized<M: TranslationModel>(&mut self, model: &mut M) -> Result<bool> {
        self.process_leased(model, true)
    }

    pub fn retry_one_eligible_authorized(&mut self, base_delay: Duration) -> Result<bool> {
        self.retry_eligible(base_delay, true)
    }

    pub fn get(&self, job_id: &str) -> Result<TranslationJobView> {
        self.connection
            .query_row(
                "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                        reason_code,attempt_count,created_at,updated_at
                   FROM translation_jobs WHERE job_id=?1",
                [job_id],
                job_view_row,
            )
            .context("translation_job_not_found")
    }

    pub fn retry(&mut self, job_id: &str) -> Result<TranslationJobView> {
        let current = self.get(job_id)?;
        if current.state != TranslationJobState::Failed
            || !current.retryable
            || current.attempt_count >= MAX_ATTEMPTS
        {
            bail!("translation_job_not_retryable")
        }
        let changed = self.connection.execute(
            "UPDATE translation_jobs
                SET state='queued',reason_code='manual_retry',updated_at=?2,
                    lease_owner=NULL,lease_until=NULL
              WHERE job_id=?1 AND state='failed' AND lease_owner IS NULL",
            params![job_id, Utc::now().to_rfc3339()],
        )?;
        if changed != 1 {
            bail!("translation_job_not_retryable")
        }
        self.get(job_id)
    }

    /// Requeues at most one retryable failure after a bounded exponential delay.
    ///
    /// The persisted failure timestamp is the retry clock, so restart cannot lose or accelerate the
    /// retry budget. `attempt_count` remains authoritative and `lease` enforces `MAX_ATTEMPTS`.
    pub fn retry_one_eligible(&mut self, base_delay: Duration) -> Result<bool> {
        self.retry_eligible(base_delay, false)
    }

    fn retry_eligible(&mut self, base_delay: Duration, authorized_only: bool) -> Result<bool> {
        if base_delay > Duration::from_secs(60)
            || !base_delay.subsec_nanos().is_multiple_of(1_000_000)
        {
            bail!("invalid_translation_retry_delay")
        }
        let base_delay_ms = i64::try_from(base_delay.as_millis())?;
        let now = Utc::now().to_rfc3339();
        let candidate = self
            .connection
            .query_row(
                "SELECT job_id,attempt_count,updated_at FROM translation_jobs
                  WHERE state='failed' AND lease_owner IS NULL AND attempt_count<?1
                    AND reason_code IN ('provider_failure','provider_timeout',
                                        'provider_outcome_unknown','malformed_model_output')
                    AND (?4=0 OR provider_authorized=1)
                    AND (julianday(?3)-julianday(updated_at))*86400000.0
                          >= CAST(?2 AS REAL)*CAST((1 << (attempt_count-1)) AS REAL)
                  ORDER BY julianday(updated_at)
                           + (CAST(?2 AS REAL)*CAST((1 << (attempt_count-1)) AS REAL))
                             / 86400000.0,
                           job_id
                  LIMIT 1",
                params![
                    i64::from(MAX_ATTEMPTS),
                    base_delay_ms,
                    now,
                    i64::from(authorized_only)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((job_id, attempt_count, updated_at)) = candidate else {
            return Ok(false);
        };
        if !(1..i64::from(MAX_ATTEMPTS)).contains(&attempt_count) {
            bail!("invalid_translation_retry_state")
        }
        chrono::DateTime::parse_from_rfc3339(&updated_at)
            .context("invalid_translation_retry_state")?;
        let changed = self.connection.execute(
            "UPDATE translation_jobs SET state='queued',reason_code='automatic_retry',
                    lease_owner=NULL,lease_until=NULL,updated_at=?4
              WHERE job_id=?1 AND state='failed' AND attempt_count=?2 AND updated_at=?3
                AND reason_code IN ('provider_failure','provider_timeout',
                                        'provider_outcome_unknown','malformed_model_output')",
            params![job_id, attempt_count, updated_at, Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    pub fn process_next<M: TranslationModel>(&mut self, model: &mut M) -> Result<bool> {
        self.process_leased(model, false)
    }

    fn process_leased<M: TranslationModel>(
        &mut self,
        model: &mut M,
        authorized_only: bool,
    ) -> Result<bool> {
        let Some(mut job) = self.lease(authorized_only)? else {
            return Ok(false);
        };
        if self.inherited(&job)? {
            self.fail(&job, INHERITED, false)?;
            return Ok(true);
        }
        let descriptor = model.descriptor();
        if !descriptor.is_valid() {
            self.fail(&job, "invalid_provider_configuration", false)?;
            return Ok(true);
        }
        if job
            .model
            .as_ref()
            .is_some_and(|frozen| !same_model(frozen, &descriptor))
        {
            match (&job.translated_title, &job.translated_document) {
                // A complete frozen pair is applied as frozen: no request is sent, and its
                // records name the model that produced it.
                (Some(_), Some(_)) => {}
                (_, Some(_)) => {
                    self.fail(&job, "provider_model_mismatch", false)?;
                    return Ok(true);
                }
                // Only a title is frozen (an explicitly re-requested job): the new model redoes
                // it, so one pair never mixes two models.
                _ => {
                    self.discard_frozen_title(&job)?;
                    job = self.record(&job.view.job_id)?;
                }
            }
        }
        let revision = match self.current_revision(&job)? {
            Some(revision) => revision,
            None => {
                self.fail(&job, "stale_revision", false)?;
                return Ok(true);
            }
        };
        let content_sha256 = content_digest(&revision, &job.view.source_language);
        let started = std::time::Instant::now();
        let elapsed = |mut failure: TranslationFailure| {
            failure
                .context
                .elapsed_ms
                .get_or_insert(started.elapsed().as_millis() as u64);
            failure
        };
        if job.translated_title.is_none() {
            let (_, prompt, _) = generated_text_prompt(GeneratedTextPurpose::TitleTranslation);
            // One immediate re-ask after an invalid reply; a second invalid reply keeps the
            // original title (a valid kept title, flagged `title_untranslated`) instead of
            // failing the whole translation.
            let mut valid = None;
            for _ in 0..2 {
                let mut progress = FailureSink::default();
                let translated = match model.translate(TranslationModelInput {
                    purpose: GeneratedTextPurpose::TitleTranslation,
                    source_language: &job.view.source_language,
                    target_language: &job.view.target_language,
                    prompt,
                    source_text: &revision.title,
                    document_title: &revision.title,
                    document_markdown: &revision.content_markdown,
                    progress: Some(&mut progress),
                }) {
                    Ok(translated) => translated,
                    Err(error) => {
                        let failure = progress.0.take().unwrap_or_else(|| {
                            TranslationFailure::new(format!("title:{}", model_error_check(error)))
                        });
                        self.fail_dispatched(&job, error, Some(elapsed(failure)))?;
                        return Ok(true);
                    }
                };
                if translation_output_failure(
                    &translated,
                    &revision.title,
                    GeneratedTextPurpose::TitleTranslation,
                )
                .is_none()
                {
                    valid = Some(translated);
                    break;
                }
            }
            let kept = valid.is_none();
            let translated = valid.unwrap_or_else(|| revision.title.clone());
            self.freeze_output(&job, &descriptor, true, translated.trim())?;
            if kept {
                self.connection.execute(
                    "UPDATE translation_jobs SET title_untranslated=1
                      WHERE job_id=?1 AND state='running' AND lease_owner=?2",
                    params![job.view.job_id, job.lease_owner],
                )?;
            }
            job = self.record(&job.view.job_id)?;
        }
        if job.translated_document.is_none() {
            let (_, prompt, _) = generated_text_prompt(GeneratedTextPurpose::DocumentTranslation);
            let mut progress = JobProgress {
                connection: &self.connection,
                root: &self.root,
                job_id: &job.view.job_id,
                item_id: &job.view.item_id,
                revision_id: &job.view.revision_id,
                revision_sha256: &job.revision_sha256,
                lease_owner: &job.lease_owner,
                content_sha256: &content_sha256,
                failure: None,
            };
            // One immediate re-ask after a malformed document (its stored batches are dropped
            // first, so the re-ask is a fresh translation); then the bounded automatic retry.
            let mut valid = None;
            let mut invalid = None;
            for reask in [false, true] {
                if reask {
                    self.connection.execute(
                        "DELETE FROM translation_batches WHERE item_id=?1 AND content_sha256=?2",
                        params![job.view.item_id, content_sha256],
                    )?;
                }
                let translated = match model.translate(TranslationModelInput {
                    purpose: GeneratedTextPurpose::DocumentTranslation,
                    source_language: &job.view.source_language,
                    target_language: &job.view.target_language,
                    prompt,
                    source_text: &revision.content_markdown,
                    document_title: &revision.title,
                    document_markdown: &revision.content_markdown,
                    progress: Some(&mut progress),
                }) {
                    Ok(translated) => translated,
                    Err(error) => {
                        let failure = progress.failure.take().unwrap_or_else(|| {
                            TranslationFailure::new(format!(
                                "document:{}",
                                model_error_check(error)
                            ))
                        });
                        self.fail_dispatched(&job, error, Some(elapsed(failure)))?;
                        return Ok(true);
                    }
                };
                match translation_output_failure(
                    &translated,
                    &revision.content_markdown,
                    GeneratedTextPurpose::DocumentTranslation,
                ) {
                    None => {
                        valid = Some(translated);
                        break;
                    }
                    Some(check) => {
                        let mut failure = TranslationFailure::new(format!("document:{check}"));
                        failure.context.output_chars = Some(translated.chars().count() as u64);
                        failure.context.actual_blocks =
                            Some(untranslated_block_count(&translated) as u32);
                        invalid = Some(failure);
                    }
                }
            }
            let Some(translated) = valid else {
                self.connection.execute(
                    "DELETE FROM translation_batches WHERE item_id=?1 AND content_sha256=?2",
                    params![job.view.item_id, content_sha256],
                )?;
                self.fail_with(&job, "malformed_model_output", true, invalid.map(elapsed))?;
                return Ok(true);
            };
            self.freeze_output(&job, &descriptor, false, translated.trim())?;
            job = self.record(&job.view.job_id)?;
        }
        if self.current_revision(&job)?.is_none() {
            self.fail(&job, SUPERSEDED, false)?;
            return Ok(true);
        }
        let frozen_model = job.model.as_ref().context("translation_model_not_frozen")?;
        let title = match translation_record(
            &self.root,
            &job,
            frozen_model,
            GeneratedTextPurpose::TitleTranslation,
            job.translated_title
                .as_deref()
                .context("translation_title_not_frozen")?,
        ) {
            Ok(record) => record,
            Err(error) => {
                if self.current_revision(&job)?.is_none() {
                    self.fail(&job, SUPERSEDED, false)?;
                    return Ok(true);
                }
                return Err(error);
            }
        };
        let document = match translation_record(
            &self.root,
            &job,
            frozen_model,
            GeneratedTextPurpose::DocumentTranslation,
            job.translated_document
                .as_deref()
                .context("translation_document_not_frozen")?,
        ) {
            Ok(record) => record,
            Err(error) => {
                if self.current_revision(&job)?.is_none() {
                    self.fail(&job, SUPERSEDED, false)?;
                    return Ok(true);
                }
                return Err(error);
            }
        };
        if competing_translation_records(&self.root, &job, &title, &document)? {
            self.fail(&job, "analysis_conflict", false)?;
            return Ok(true);
        }
        let (title_receipt, document_receipt) =
            match super::apply_record_pair(&self.root, &title, &document) {
                Ok(receipts) => receipts,
                Err(error) => {
                    if self.current_revision(&job)?.is_none() {
                        self.fail(&job, SUPERSEDED, false)?;
                        return Ok(true);
                    }
                    return Err(error);
                }
            };
        if title_receipt
            .warning_codes
            .iter()
            .chain(&document_receipt.warning_codes)
            .any(|warning| warning == "analysis_json_index_projection_pending")
        {
            bail!("translation_analysis_index_pending")
        }
        if self.current_revision(&job)?.is_none() {
            self.fail(&job, SUPERSEDED, false)?;
            return Ok(true);
        }
        if competing_translation_records(&self.root, &job, &title, &document)? {
            self.fail(&job, "analysis_conflict", false)?;
            return Ok(true);
        }
        self.succeed(&job, &title.analysis_id, &document.analysis_id)?;
        // The applied pair supersedes the stored batches of this exact source.
        self.connection.execute(
            "DELETE FROM translation_batches WHERE item_id=?1 AND content_sha256=?2",
            params![job.view.item_id, content_sha256],
        )?;
        Ok(true)
    }

    /// A failed request whose revision was replaced meanwhile (e.g. a metadata-only revision)
    /// ends `superseded_by_revision`: the replacement revision's job owns the work and reuses
    /// the stored batches, so this is not a failure of the current translation.
    fn fail_dispatched(
        &mut self,
        job: &TranslationJobRecord,
        error: TranslationModelError,
        failure: Option<TranslationFailure>,
    ) -> Result<()> {
        if self.current_revision(job)?.is_none() {
            return self.fail(job, SUPERSEDED, false);
        }
        let (reason, retryable) = model_error_reason(error);
        self.fail_with(job, reason, retryable, failure)
    }

    fn discard_frozen_title(&mut self, job: &TranslationJobRecord) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE translation_jobs
                SET translated_title=NULL,model_id=NULL,model_sha256=NULL,adapter_id=NULL,
                    adapter_version=NULL,request_timeout_ms=NULL,updated_at=?2
              WHERE job_id=?1 AND state='running' AND lease_owner=?3
                AND translated_document IS NULL",
            params![job.view.job_id, Utc::now().to_rfc3339(), job.lease_owner],
        )?;
        if changed != 1 {
            bail!("translation_lease_lost")
        }
        Ok(())
    }

    fn enqueue_verified(
        &mut self,
        item_id: &str,
        revision: &ItemRevision,
        source_language: &str,
    ) -> Result<TranslationJobView> {
        let input = revision_ref(item_id, revision)?;
        let key = format!(
            "{item_id}\0{}\0{source_language}\0{TARGET_LANGUAGE}",
            revision.revision_id
        );
        let job_id = format!("translation_{:x}", Sha256::digest(key.as_bytes()));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = transaction
            .query_row(
                "SELECT revision_sha256 FROM translation_jobs WHERE job_id=?1",
                [&job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing == input.revision_sha256 {
                transaction.commit()?;
                return self.get(&job_id);
            }
            transaction.execute(
                "INSERT INTO translation_conflicts(job_id,reason_code,created_at)
                 VALUES(?1,'translation_job_identity_conflict',?2)",
                params![job_id, Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
            bail!("translation_job_identity_conflict")
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO translation_jobs(
               job_id,item_id,revision_id,revision_sha256,source_language,target_language,
               classifier_version,state,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?8)",
            params![
                job_id,
                item_id,
                revision.revision_id,
                input.revision_sha256,
                source_language,
                TARGET_LANGUAGE,
                CLASSIFIER_VERSION,
                now
            ],
        )?;
        transaction.commit()?;
        self.get(&job_id)
    }

    fn recover_expired(&mut self) -> Result<usize> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now();
        let exhausted = transaction.execute(
            "UPDATE translation_jobs
                SET state='failed',reason_code='retry_exhausted',lease_owner=NULL,
                    lease_until=NULL,updated_at=?3,failure_detail='worker:lease_expired',
                    failure_context=NULL
              WHERE state='running' AND lease_until<?1 AND attempt_count>=?2
                AND (translated_title IS NULL OR translated_document IS NULL)",
            params![now.timestamp(), i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        let recovered = transaction.execute(
            "UPDATE translation_jobs
                SET state='queued',reason_code='worker_recovered',lease_owner=NULL,
                    lease_until=NULL,updated_at=?3
              WHERE state='running' AND lease_until<?1
                AND (attempt_count<?2
                     OR (translated_title IS NOT NULL AND translated_document IS NOT NULL))",
            params![now.timestamp(), i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(exhausted + recovered)
    }

    fn lease(&mut self, authorized_only: bool) -> Result<Option<TranslationJobRecord>> {
        self.recover_expired()?;
        let now = Utc::now();
        let owner = format!(
            "worker_{:x}",
            Sha256::digest(format!(
                "{}:{}",
                std::process::id(),
                now.timestamp_nanos_opt().unwrap_or(0)
            ))
        );
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE translation_jobs
                SET state='failed',reason_code='retry_exhausted',updated_at=?2
              WHERE state='queued' AND lease_owner IS NULL AND attempt_count>=?1
                AND (translated_title IS NULL OR translated_document IS NULL)",
            params![i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        let job_id = transaction
            .query_row(
                &format!(
                    "SELECT job_id FROM translation_jobs
                      WHERE state='queued' AND lease_owner IS NULL
                        AND (?2=0 OR provider_authorized=1)
                        AND (attempt_count<?1
                             OR (translated_title IS NOT NULL AND translated_document IS NOT NULL))
                        AND (?2=0 OR {})
                        AND {}
                      ORDER BY created_at,job_id LIMIT 1",
                    metadata_gate("translation_jobs", now),
                    no_running_sibling("translation_jobs")
                ),
                params![i64::from(MAX_ATTEMPTS), i64::from(authorized_only)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(job_id) = job_id else {
            transaction.commit()?;
            return Ok(None);
        };
        let changed = transaction.execute(
            "UPDATE translation_jobs
                SET state='running',reason_code=NULL,started_at=?4,
                    failure_detail=NULL,failure_context=NULL,
                    attempt_count=attempt_count
                      + CASE WHEN translated_title IS NOT NULL
                                  AND translated_document IS NOT NULL THEN 0 ELSE 1 END,
                    lease_owner=?2,lease_until=?3,updated_at=?4
              WHERE job_id=?1 AND state='queued' AND lease_owner IS NULL",
            params![
                job_id,
                owner,
                now.timestamp() + LEASE_SECONDS,
                now.to_rfc3339()
            ],
        )?;
        if changed != 1 {
            bail!("translation_lease_lost")
        }
        transaction.commit()?;
        Ok(Some(self.record(&job_id)?))
    }

    fn record(&self, job_id: &str) -> Result<TranslationJobRecord> {
        self.connection
            .query_row(
                "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                        reason_code,attempt_count,created_at,updated_at,revision_sha256,
                        lease_owner,translated_title,translated_document,model_id,model_sha256,
                        adapter_id,adapter_version,request_timeout_ms
                   FROM translation_jobs WHERE job_id=?1",
                [job_id],
                |row| {
                    let model_id = row.get::<_, Option<String>>(14)?;
                    let model_sha256 = row.get::<_, Option<String>>(15)?;
                    let adapter_id = row.get::<_, Option<String>>(16)?;
                    let adapter_version = row.get::<_, Option<String>>(17)?;
                    let request_timeout_ms = row.get::<_, Option<i64>>(18)?;
                    let model = match (
                        model_id,
                        model_sha256,
                        adapter_id,
                        adapter_version,
                        request_timeout_ms,
                    ) {
                        (
                            Some(model_id),
                            Some(model_sha256),
                            Some(adapter_id),
                            Some(adapter_version),
                            Some(request_timeout_ms),
                        ) if request_timeout_ms > 0 => Some(TranslationModelDescriptor {
                            model_id,
                            model_sha256,
                            adapter_id,
                            adapter_version,
                            request_timeout: Duration::from_millis(request_timeout_ms as u64),
                        }),
                        (None, None, None, None, None) => None,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    Ok(TranslationJobRecord {
                        view: job_view_columns(row)?,
                        revision_sha256: row.get(10)?,
                        lease_owner: row
                            .get::<_, Option<String>>(11)?
                            .ok_or(rusqlite::Error::InvalidQuery)?,
                        translated_title: row.get(12)?,
                        translated_document: row.get(13)?,
                        model,
                    })
                },
            )
            .context("translation_job_not_found")
    }

    /// The job's revision while its body is still the current body (a metadata-only revision
    /// since does not stop or supersede it; its records stay bound to the job's revision).
    fn current_revision(&self, job: &TranslationJobRecord) -> Result<Option<ItemRevision>> {
        crate::lineage::job_source(
            &self.root,
            &job.view.item_id,
            &job.view.revision_id,
            &job.revision_sha256,
        )
    }

    /// Whether a same-body revision already has a validated translation this job's revision
    /// inherits (then the job is a duplicate and sends nothing).
    fn inherited(&self, job: &TranslationJobRecord) -> Result<bool> {
        Ok(effective_translation(&self.root, &job.view.item_id, &job.view.revision_id)?
            .is_some_and(|(_, source)| source.revision_id != job.view.revision_id))
    }

    fn freeze_output(
        &mut self,
        job: &TranslationJobRecord,
        model: &TranslationModelDescriptor,
        title: bool,
        translated: &str,
    ) -> Result<()> {
        let column = if title {
            "translated_title"
        } else {
            "translated_document"
        };
        let statement = format!(
            "UPDATE translation_jobs
                SET {column}=?2,model_id=COALESCE(model_id,?3),
                    model_sha256=COALESCE(model_sha256,?4),adapter_id=COALESCE(adapter_id,?5),
                    adapter_version=COALESCE(adapter_version,?6),
                    request_timeout_ms=COALESCE(request_timeout_ms,?7),updated_at=?8,lease_until=?9
              WHERE job_id=?1 AND state='running' AND lease_owner=?10 AND {column} IS NULL
                AND (model_id IS NULL OR (model_id=?3 AND model_sha256=?4
                     AND adapter_id=?5 AND adapter_version=?6 AND request_timeout_ms=?7))"
        );
        let now = Utc::now();
        let changed = self.connection.execute(
            &statement,
            params![
                job.view.job_id,
                translated,
                model.model_id,
                model.model_sha256,
                model.adapter_id,
                model.adapter_version,
                model.request_timeout.as_millis() as i64,
                now.to_rfc3339(),
                now.timestamp() + LEASE_SECONDS,
                job.lease_owner
            ],
        )?;
        if changed != 1 {
            bail!("translation_lease_lost")
        }
        Ok(())
    }

    fn fail(&mut self, job: &TranslationJobRecord, reason: &str, retryable: bool) -> Result<()> {
        self.fail_with(job, reason, retryable, None)
    }

    /// Fails the running job; `failure` (fixed code + numbers) is stored as its diagnostic.
    fn fail_with(
        &mut self,
        job: &TranslationJobRecord,
        reason: &str,
        retryable: bool,
        failure: Option<TranslationFailure>,
    ) -> Result<()> {
        let reason = if retryable && job.view.attempt_count >= MAX_ATTEMPTS {
            "retry_exhausted"
        } else {
            reason
        };
        let failure = failure.filter(|failure| valid_failure_detail(&failure.detail));
        let context = match &failure {
            Some(failure) => Some(serde_json::to_string(&failure.context)?),
            None => None,
        };
        let changed = self.connection.execute(
            "UPDATE translation_jobs
                SET state='failed',reason_code=?2,lease_owner=NULL,lease_until=NULL,updated_at=?3,
                    failure_detail=?5,failure_context=?6
              WHERE job_id=?1 AND state='running' AND lease_owner=?4",
            params![
                job.view.job_id,
                reason,
                Utc::now().to_rfc3339(),
                job.lease_owner,
                failure.as_ref().map(|failure| failure.detail.as_str()),
                context
            ],
        )?;
        if changed != 1 {
            bail!("translation_lease_lost")
        }
        Ok(())
    }

    fn succeed(
        &mut self,
        job: &TranslationJobRecord,
        title_analysis_id: &str,
        document_analysis_id: &str,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE translation_jobs
                SET state='succeeded',reason_code=NULL,lease_owner=NULL,lease_until=NULL,
                    failure_detail=NULL,failure_context=NULL,
                    translated_title=NULL,translated_document=NULL,title_analysis_id=?2,
                    document_analysis_id=?3,updated_at=?4
              WHERE job_id=?1 AND state='running' AND lease_owner=?5",
            params![
                job.view.job_id,
                title_analysis_id,
                document_analysis_id,
                Utc::now().to_rfc3339(),
                job.lease_owner
            ],
        )?;
        if changed != 1 {
            bail!("translation_lease_lost")
        }
        Ok(())
    }
}

/// SQL condition: no metadata job of the row's item is running, or was queued/requeued in the
/// last `METADATA_GATE_SECONDS`. Metadata enrichment may write a new (metadata-only) revision, so
/// generated text for the item waits for it; the time bound keeps a metadata job without a
/// route from holding the item forever.
pub(crate) fn metadata_gate(table: &str, now: chrono::DateTime<Utc>) -> String {
    let cutoff = (now - chrono::Duration::seconds(METADATA_GATE_SECONDS)).to_rfc3339();
    format!(
        "NOT EXISTS(SELECT 1 FROM metadata_jobs gate WHERE gate.item_id={table}.item_id
           AND (gate.state='running' OR (gate.state='queued' AND gate.updated_at>'{cutoff}')))"
    )
}

const METADATA_GATE_SECONDS: i64 = 600;

/// Identity of the exact source a stored batch belongs to.
fn content_digest(revision: &ItemRevision, source_language: &str) -> String {
    let mut digest = Sha256::new();
    for part in [
        revision.title.as_str(),
        revision.content_markdown.as_str(),
        source_language,
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

/// The queue's `TranslationProgress` for one leased job; every write is lease-owner checked.
struct JobProgress<'c> {
    connection: &'c Connection,
    root: &'c Path,
    job_id: &'c str,
    item_id: &'c str,
    revision_id: &'c str,
    revision_sha256: &'c str,
    lease_owner: &'c str,
    content_sha256: &'c str,
    /// The adapter's diagnostic of the failure it returns.
    failure: Option<TranslationFailure>,
}

/// Progress of the title request: only collects the adapter's failure diagnostic.
#[derive(Default)]
struct FailureSink(Option<TranslationFailure>);

impl TranslationProgress for FailureSink {
    fn completed(&mut self, _: &str) -> Option<Vec<String>> {
        None
    }
    fn record(&mut self, _: &str, _: &[String]) -> bool {
        false
    }
    fn heartbeat(&mut self, _: usize, _: usize) -> bool {
        true
    }
    fn failure(&mut self, failure: TranslationFailure) {
        self.0 = Some(failure);
    }
}

impl TranslationProgress for JobProgress<'_> {
    fn failure(&mut self, failure: TranslationFailure) {
        self.failure = Some(failure);
    }

    fn completed(&mut self, key: &str) -> Option<Vec<String>> {
        let json = self
            .connection
            .query_row(
                "SELECT outputs_json FROM translation_batches
                  WHERE item_id=?1 AND content_sha256=?2 AND batch_key=?3",
                params![self.item_id, self.content_sha256, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()??;
        serde_json::from_str(&json).ok()
    }

    fn record(&mut self, key: &str, outputs: &[String]) -> bool {
        let Ok(json) = serde_json::to_string(outputs) else {
            return false;
        };
        self.owned()
            && self
                .connection
                .execute(
                    "INSERT OR REPLACE INTO translation_batches(
                       item_id,content_sha256,batch_key,outputs_json,created_at)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![
                        self.item_id,
                        self.content_sha256,
                        key,
                        json,
                        Utc::now().to_rfc3339()
                    ],
                )
                .is_ok()
    }

    fn heartbeat(&mut self, done: usize, total: usize) -> bool {
        // A changed body ends dispatching; a metadata-only revision (same body) does not.
        let current = crate::lineage::job_source(
            self.root,
            self.item_id,
            self.revision_id,
            self.revision_sha256,
        )
        .is_ok_and(|source| source.is_some());
        if !current {
            return false;
        }
        let now = Utc::now();
        self.connection
            .execute(
                "UPDATE translation_jobs
                    SET lease_until=?2,batches_done=?3,batches_total=?4
                  WHERE job_id=?1 AND state='running' AND lease_owner=?5",
                params![
                    self.job_id,
                    now.timestamp() + LEASE_SECONDS,
                    done as i64,
                    total as i64,
                    self.lease_owner
                ],
            )
            .is_ok_and(|changed| changed == 1)
    }
}

impl JobProgress<'_> {
    fn owned(&self) -> bool {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM translation_jobs
                  WHERE job_id=?1 AND state='running' AND lease_owner=?2)",
                params![self.job_id, self.lease_owner],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }
}

/// Carries generated text across a metadata-only revision (`apply_model_metadata_suggestion`
/// copies title and Markdown unchanged, so outputs for `previous` are outputs for `current`).
/// A succeeded pair (read from its Analysis records) or a frozen title moves into the
/// `current` job together with its frozen model; the provider worker then applies it without a
/// model request, and stored batches (keyed by content, not revision) resume unfinished work.
/// Only succeeded or explicitly authorized work is carried (cost guard). Returns whether a
/// `current` job exists afterwards.
pub fn carry_translation(
    library: impl AsRef<Path>,
    item_id: &str,
    previous_revision_id: &str,
    current_revision_id: &str,
) -> Result<bool> {
    let library = library.as_ref();
    let mut queue = TranslationQueue::open(library)?;
    type Row = (
        String,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<TranslationModelDescriptor>,
    );
    let previous: Option<Row> = queue
        .connection
        .query_row(
            "SELECT state,provider_authorized,translated_title,title_analysis_id,
                    document_analysis_id,model_id,model_sha256,adapter_id,adapter_version,
                    request_timeout_ms
               FROM translation_jobs WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, previous_revision_id, TARGET_LANGUAGE],
            |row| {
                let model = match (
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ) {
                    (
                        Some(model_id),
                        Some(model_sha256),
                        Some(adapter_id),
                        Some(adapter_version),
                        Some(ms),
                    ) if ms > 0 => Some(TranslationModelDescriptor {
                        model_id,
                        model_sha256,
                        adapter_id,
                        adapter_version,
                        request_timeout: Duration::from_millis(ms as u64),
                    }),
                    _ => None,
                };
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)? == 1,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    model,
                ))
            },
        )
        .optional()?;
    let Some((state, authorized, title, title_id, document_id, model)) = previous else {
        return Ok(false);
    };
    let succeeded = state == "succeeded";
    if !succeeded && !authorized {
        return Ok(false);
    }
    let TranslationEnqueueOutcome::Enqueued { job } =
        enqueue_item_translation(library, item_id, current_revision_id)?
    else {
        return Ok(false);
    };
    let frozen = match (succeeded, title_id, document_id) {
        (true, Some(title_id), Some(document_id)) => Some((
            super::read_record(library, &title_id)?.body_markdown,
            Some(super::read_record(library, &document_id)?.body_markdown),
        )),
        _ => title.map(|title| (title, None)),
    };
    if job.state == TranslationJobState::Queued {
        if let (Some((title, document)), Some(model)) = (frozen, model.filter(|m| m.is_valid())) {
            queue.connection.execute(
                "UPDATE translation_jobs
                    SET translated_title=?2,translated_document=?3,model_id=?4,model_sha256=?5,
                        adapter_id=?6,adapter_version=?7,request_timeout_ms=?8,updated_at=?9
                  WHERE job_id=?1 AND state='queued' AND lease_owner IS NULL
                    AND translated_title IS NULL AND translated_document IS NULL
                    AND model_id IS NULL",
                params![
                    job.job_id,
                    title,
                    document,
                    model.model_id,
                    model.model_sha256,
                    model.adapter_id,
                    model.adapter_version,
                    model.request_timeout.as_millis() as i64,
                    Utc::now().to_rfc3339()
                ],
            )?;
        }
    }
    queue.authorize(item_id, current_revision_id)?;
    Ok(true)
}

pub fn enqueue_item_translation(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<TranslationEnqueueOutcome> {
    let (item, revision) = memex_store::read_item(library.as_ref(), item_id)?;
    if item.current_revision != revision_id || revision.revision_id != revision_id {
        return Ok(TranslationEnqueueOutcome::Skipped {
            item_id: item_id.to_owned(),
            revision_id: revision_id.to_owned(),
            reason: TranslationSkipReason::StaleRevision,
        });
    }
    if !matches!(
        item.item_type.as_str(),
        "article" | "journal_article" | "webpage"
    ) {
        return Ok(skipped(
            &revision,
            TranslationSkipReason::UnsupportedItemType,
        ));
    }
    let source_language = match classify_source(&revision.title, &revision.content_markdown) {
        SourceClassification::Chinese => {
            return Ok(skipped(&revision, TranslationSkipReason::AlreadyChinese))
        }
        SourceClassification::English | SourceClassification::Other => {
            source_language(&revision.title, &revision.content_markdown)
        }
        SourceClassification::ChallengeOrLogin => {
            return Ok(skipped(&revision, TranslationSkipReason::ChallengeOrLogin))
        }
        SourceClassification::TooLarge => {
            return Ok(skipped(&revision, TranslationSkipReason::SourceTooLarge))
        }
        SourceClassification::Unknown => {
            return Ok(skipped(
                &revision,
                TranslationSkipReason::SourceLanguageUnknown,
            ))
        }
    };
    // A metadata-only revision reuses the succeeded job of a same-body revision: no duplicate.
    if let Some(view) = translation_status(library.as_ref(), item_id, revision_id)?.filter(|view| {
        view.state == TranslationJobState::Succeeded && view.revision_id != revision_id
    }) {
        if effective_translation(library.as_ref(), item_id, revision_id)?.is_some() {
            return Ok(TranslationEnqueueOutcome::Enqueued { job: view });
        }
    }
    let mut queue = TranslationQueue::open(library)?;
    Ok(TranslationEnqueueOutcome::Enqueued {
        job: queue.enqueue_verified(item_id, &revision, source_language)?,
    })
}

pub fn enqueue_ingest_translations(
    library: impl AsRef<Path>,
    receipt: &IngestReceipt,
) -> Result<Vec<TranslationEnqueueOutcome>> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.item_ids.is_empty()
        || receipt.item_ids.len() != receipt.revision_ids.len()
    {
        bail!("invalid_ingest_receipt_for_translation")
    }
    // Only the loopback capture intake calls this (bulk imports never do): each captured item
    // gets one title-only translation; its body waits for an explicit `translation_request`.
    if let Ok(mut titles) = crate::TitleTranslationQueue::open(library.as_ref()) {
        for (item_id, revision_id) in receipt.item_ids.iter().zip(&receipt.revision_ids) {
            let _ = titles.request_capture(item_id, revision_id);
        }
    }
    receipt
        .item_ids
        .iter()
        .zip(&receipt.revision_ids)
        .map(|(item_id, revision_id)| {
            enqueue_item_translation(library.as_ref(), item_id, revision_id)
        })
        .collect()
}

/// Read-only, current-revision generated text projection. Queue success alone
/// is insufficient; both Analysis records must validate against the source.
#[derive(Debug, Serialize, Deserialize)]
pub struct TranslationProjection {
    pub state: String,
    pub reason: Option<String>,
    pub revision_id: String,
    /// Additive: the translated title as soon as the title step succeeded for this revision,
    /// independent of the document (the paged `title` stays success-only).
    #[serde(default)]
    pub translated_title: Option<String>,
    /// Additive: document batches finished so far (queued/running/failed jobs only).
    #[serde(default)]
    pub progress: Option<TranslationProgressView>,
    /// Additive: the same-body revision a metadata-only revision inherits its translation from
    /// (absent when the translation is the revision's own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision_id: Option<String>,
    /// Additive: fixed diagnostic code of the last failed attempt (`batch_2_of_5:cjk_ratio`,
    /// `title:json_parse`, ...); failed jobs only, never text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
    /// Additive: numbers only (expected/actual blocks, output tokens, elapsed ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_context: Option<TranslationFailureContext>,
    /// Additive, succeeded only: document blocks kept in the original language
    /// (`UNTRANSLATED_BLOCK_MARKER`); 0 for a complete translation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untranslated_blocks: Option<usize>,
    /// Additive, succeeded only: the title kept its original wording after invalid answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_untranslated: Option<bool>,
    #[serde(flatten)]
    pub page: Option<TranslationPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationProgressView {
    pub done_batches: usize,
    pub total_batches: usize,
    /// Additive: start of the current attempt (RFC 3339), running jobs only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// Failure diagnostic and title flag of the revision's job row (never text).
fn job_diagnostics(
    connection: &Connection,
    item_id: &str,
    revision_id: &str,
) -> Result<(Option<String>, Option<TranslationFailureContext>, bool)> {
    if !has_column(connection, "translation_jobs", "failure_detail")? {
        return Ok((None, None, false));
    }
    Ok(connection
        .query_row(
            "SELECT failure_detail,failure_context,title_untranslated FROM translation_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| {
                let detail = row
                    .get::<_, Option<String>>(0)?
                    .filter(|detail| valid_failure_detail(detail));
                let context = row
                    .get::<_, Option<String>>(1)?
                    .and_then(|json| serde_json::from_str(&json).ok());
                Ok((
                    detail,
                    context,
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0) == 1,
                ))
            },
        )
        .optional()?
        .unwrap_or((None, None, false)))
}

/// The revision's frozen translated title (before the document succeeded) and document batch
/// progress; `(None, None)` without a job.
pub fn translation_partial(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<(Option<String>, Option<TranslationProgressView>)> {
    match open_status_database(library.as_ref())? {
        Some((root, connection)) => {
            let Some(status) = effective_job_view(&connection, &root, item_id, revision_id)? else {
                return Ok((None, None));
            };
            let (mut title, progress) = job_partial(&connection, item_id, &status.revision_id)?;
            if status.revision_id != revision_id {
                let source = memex_store::same_body_revision(&root, item_id, revision_id, &status.revision_id)?;
                let requested = memex_store::same_body_revision(&root, item_id, &status.revision_id, revision_id)?;
                if !source.zip(requested).is_some_and(|(source, requested)| source.title == requested.title) {
                    title = None;
                }
            }
            Ok((title, progress))
        }
        None => Ok((None, None)),
    }
}

/// Frozen title and batch progress of the revision's job row (never source text).
fn job_partial(
    connection: &Connection,
    item_id: &str,
    revision_id: &str,
) -> Result<(Option<String>, Option<TranslationProgressView>)> {
    if !has_column(connection, "translation_jobs", "batches_done")? {
        return Ok((None, None));
    }
    let started = if has_column(connection, "translation_jobs", "started_at")? {
        "CASE WHEN state='running' THEN started_at END"
    } else {
        "NULL"
    };
    Ok(connection
        .query_row(
            &format!(
                "SELECT translated_title,batches_done,batches_total,{started} FROM translation_jobs
                  WHERE item_id=?1 AND revision_id=?2 AND target_language=?3"
            ),
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| {
                let done = row.get::<_, Option<i64>>(1)?;
                let total = row.get::<_, Option<i64>>(2)?;
                let started_at = row
                    .get::<_, Option<String>>(3)?
                    .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok());
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    match (done, total) {
                        (Some(done), Some(total)) if done >= 0 && total >= done => {
                            Some(TranslationProgressView {
                                done_batches: done as usize,
                                total_batches: total as usize,
                                started_at,
                            })
                        }
                        _ => None,
                    },
                ))
            },
        )
        .optional()?
        .unwrap_or((None, None)))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranslationPage {
    pub title: String,
    pub content_markdown: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub next_byte: Option<usize>,
    pub total_bytes: usize,
    pub content_sha256: String,
    pub coordinate_space: String,
    pub title_analysis_id: String,
    pub document_analysis_id: String,
}

pub fn read_translation_projection(
    library: &Path,
    item_id: &str,
    ceiling: Sensitivity,
    range: Option<(&str, usize)>,
) -> Result<TranslationProjection> {
    memex_store::with_consistent_snapshot(library, |root| {
        let (item, revision) = memex_store::read_item_at_sensitivity(root, item_id, ceiling)?
            .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= ceiling)
            .context("library_resource_not_found")?;
        if range.is_some_and(|(expected, _)| expected != revision.revision_id) {
            bail!("stale_item_revision");
        }
        let mut projection = TranslationProjection {
            state: "not_requested".into(),
            reason: None,
            revision_id: revision.revision_id.clone(),
            translated_title: None,
            progress: None,
            source_revision_id: None,
            failure_detail: None,
            failure_context: None,
            untranslated_blocks: None,
            title_untranslated: None,
            page: None,
        };
        // A corrupt/unavailable derived projection never becomes a successful
        // translation and does not prevent reading the canonical original.
        let read = (|| -> Result<()> {
            let Some(status) = translation_status(root, &item.item_id, &revision.revision_id)?
            else {
                return Ok(());
            };
            projection.state = match status.state {
                TranslationJobState::Queued => "queued",
                TranslationJobState::Running => "running",
                TranslationJobState::Failed => "failed",
                TranslationJobState::Succeeded => "unavailable",
            }
            .into();
            projection.reason = status.reason_code;
            if status.state != TranslationJobState::Succeeded {
                let (title, progress) = translation_partial(root, &item.item_id, &revision.revision_id)?;
                projection.translated_title = title;
                projection.progress = progress;
                if status.revision_id != revision.revision_id {
                    projection.source_revision_id = Some(status.revision_id.clone());
                }
                if let Some((_, connection)) = open_status_database(root)? {
                    if status.state == TranslationJobState::Failed {
                        let (detail, context, _) =
                            job_diagnostics(&connection, &item.item_id, &status.revision_id)?;
                        projection.failure_detail = detail;
                        projection.failure_context = context;
                    }
                }
                return Ok(());
            }
            let (pair, source) = effective_translation(root, item_id, &revision.revision_id)?
                .context("translation_unavailable")?;
            let title = super::read_record_at_sensitivity(root, &pair.title_analysis_id, ceiling)?
                .context("translation_unavailable")?;
            let document =
                super::read_record_at_sensitivity(root, &pair.document_analysis_id, ceiling)?
                    .context("translation_unavailable")?;
            projection.state = "succeeded".into();
            projection.untranslated_blocks = Some(untranslated_block_count(&document.body_markdown));
            if let Some((_, connection)) = open_status_database(root)? {
                let (_, _, kept) = job_diagnostics(&connection, &item.item_id, &source.revision_id)?;
                projection.title_untranslated = Some(kept);
            }
            // The body translation is inherited across metadata edits; a translated title only
            // while the title is unchanged (else the new title's own title-only translation).
            let translated_title = if source.title == revision.title {
                Some(title.body_markdown.clone())
            } else {
                crate::titles::retitled_translation(root, item_id, &revision.title)?
            };
            if source.revision_id != revision.revision_id {
                projection.source_revision_id = Some(source.revision_id.clone());
            }
            projection.translated_title = translated_title.clone();
            if let Some((_, start)) = range {
                let text = &document.body_markdown;
                if start >= text.len() || !text.is_char_boundary(start) {
                    bail!("invalid_context_range");
                }
                let mut end = start.saturating_add(16 * 1024).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                projection.page = Some(TranslationPage {
                    title: translated_title.unwrap_or_else(|| revision.title.clone()),
                    content_markdown: text[start..end].to_owned(),
                    start_byte: start,
                    end_byte: end,
                    next_byte: (end < text.len()).then_some(end),
                    total_bytes: text.len(),
                    content_sha256: format!("sha256:{:x}", Sha256::digest(text)),
                    coordinate_space: "analysis_translation_utf8_v1".into(),
                    title_analysis_id: pair.title_analysis_id,
                    document_analysis_id: pair.document_analysis_id,
                });
            }
            Ok(())
        })();
        if let Err(error) = read {
            if error.to_string() == "invalid_context_range" {
                return Err(error);
            }
            projection.state = "unavailable".into();
            projection.reason = Some("translation_unavailable".into());
            projection.translated_title = None;
            projection.progress = None;
            projection.source_revision_id = None;
            projection.failure_detail = None;
            projection.failure_context = None;
            projection.untranslated_blocks = None;
            projection.title_untranslated = None;
            projection.page = None;
        }
        if range.is_some() && projection.page.is_none() {
            bail!("translation_unavailable");
        }
        Ok(projection)
    })
}

pub fn translation_status(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<TranslationJobView>> {
    let Some((root, connection)) = open_status_database(library.as_ref())? else {
        return Ok(None);
    };
    effective_job_view(&connection, &root, item_id, revision_id)
}

fn effective_job_view(
    connection: &Connection,
    root: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<TranslationJobView>> {
    let own = job_view(connection, item_id, revision_id)?;
    if own
        .as_ref()
        .is_some_and(|view| view.state == TranslationJobState::Succeeded)
    {
        return Ok(own);
    }
    // A metadata-only revision reports the succeeded job of its newest same-body revision.
    for source in inherited_translation_revisions(connection, root, item_id, revision_id)? {
        if let Some(view) = job_view(connection, item_id, &source.revision_id)? {
            return Ok(Some(view));
        }
    }
    // A queued metadata revision must not hide its same-body job still running.
    // This is a read-only projection: jobs, authorization and leases stay untouched.
    if !own.as_ref().is_some_and(|view| view.state == TranslationJobState::Running)
        && connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM translation_jobs WHERE item_id=?1
              AND revision_id<>?2 AND target_language=?3 AND state='running')",
            params![item_id, revision_id, TARGET_LANGUAGE], |row| row.get::<_, bool>(0),
        )?
    {
        for source in memex_store::same_body_revisions(root, item_id, revision_id)? {
            if let Some(view) = job_view(connection, item_id, &source.revision_id)?
                .filter(|view| view.state == TranslationJobState::Running)
            {
                return Ok(Some(view));
            }
        }
    }
    Ok(own)
}

fn job_view(
    connection: &Connection,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<TranslationJobView>> {
    connection
        .query_row(
            "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                    reason_code,attempt_count,created_at,updated_at
               FROM translation_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            job_view_row,
        )
        .optional()
        .map_err(Into::into)
}

fn inherited_translation_revisions(
    connection: &Connection,
    root: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<Vec<ItemRevision>> {
    crate::lineage::inherited_revisions(
        connection,
        root,
        "translation_jobs",
        "AND target_language=?3",
        TARGET_LANGUAGE,
        item_id,
        revision_id,
    )
}

/// The validated translation pair `revision_id` shows: its own, or the one of its newest
/// same-body revision (title or other metadata edited since).
pub fn validated_translation_pair(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<TranslationAnalysisPair>> {
    Ok(effective_translation(library, item_id, revision_id)?.map(|(pair, _)| pair))
}

/// The validated pair `revision_id` shows and the revision it was produced from and validated
/// against (`revision_id` itself, or a same-body revision whose title may differ).
pub fn effective_translation(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<(TranslationAnalysisPair, ItemRevision)>> {
    let Some((mut pair, source)) = base_effective_translation(library.as_ref(), item_id, revision_id)? else {
        return Ok(None);
    };
    tables::apply_table_repair_projection(library.as_ref(), item_id, revision_id, &mut pair, &source)?;
    Ok(Some((pair, source)))
}

fn base_effective_translation(
    library: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<(TranslationAnalysisPair, ItemRevision)>> {
    let Some((root, connection)) = open_status_database(library)? else {
        return Ok(None);
    };
    let Some(requested) = crate::lineage::job_source_revision(&root, item_id, revision_id)? else {
        return Ok(None);
    };
    let (_, current) = memex_store::read_item(&root, item_id)?;
    let inherited = inherited_translation_revisions(&connection, &root, item_id, revision_id)?;
    for source in std::iter::once(requested).chain(inherited) {
        let historical = source.revision_id != current.revision_id;
        if let Some(pair) = validated_pair_at(&root, &connection, item_id, &source, historical)? {
            return Ok(Some((pair, source)));
        }
    }
    Ok(None)
}

/// Validates the succeeded pair of exactly `revision` (the current revision, or a same-body
/// historical one): prompt digests, model, outputs and the record bytes are all recomputed.
fn validated_pair_at(
    root: &Path,
    connection: &Connection,
    item_id: &str,
    revision: &ItemRevision,
    historical: bool,
) -> Result<Option<TranslationAnalysisPair>> {
    let revision_id = revision.revision_id.as_str();
    let binding = connection
        .query_row(
            "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                    reason_code,attempt_count,created_at,updated_at,revision_sha256,
                    title_analysis_id,document_analysis_id,model_id,model_sha256,
                    adapter_id,adapter_version,request_timeout_ms
               FROM translation_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3 AND state='succeeded'",
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| {
                Ok((
                    job_view_columns(row)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    TranslationModelDescriptor {
                        model_id: row.get(13)?,
                        model_sha256: row.get(14)?,
                        adapter_id: row.get(15)?,
                        adapter_version: row.get(16)?,
                        request_timeout: Duration::from_millis(
                            u64::try_from(row.get::<_, i64>(17)?)
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        ),
                    },
                ))
            },
        )
        .optional()?;
    let Some((view, revision_sha256, title_analysis_id, document_analysis_id, model)) = binding
    else {
        return Ok(None);
    };
    if title_analysis_id == document_analysis_id
        || !super::valid_analysis_id(&title_analysis_id)
        || !super::valid_analysis_id(&document_analysis_id)
        || !valid_sha256(&revision_sha256)
        || !model.is_valid()
    {
        bail!("invalid succeeded translation binding")
    }
    if revision_ref(item_id, revision)?.revision_sha256 != revision_sha256 {
        return Ok(None);
    }
    let title = super::read_record(root, &title_analysis_id)?;
    let document = super::read_record(root, &document_analysis_id)?;
    let job = TranslationJobRecord {
        view,
        revision_sha256,
        lease_owner: String::new(),
        translated_title: Some(title.body_markdown.clone()),
        translated_document: Some(document.body_markdown.clone()),
        model: Some(model.clone()),
    };
    if !valid_translation_output(
        &title.body_markdown,
        &revision.title,
        GeneratedTextPurpose::TitleTranslation,
    ) || !valid_translation_output(
        &document.body_markdown,
        &revision.content_markdown,
        GeneratedTextPurpose::DocumentTranslation,
    ) {
        return Ok(None);
    }
    let historical = historical.then_some(revision);
    let expected_title = translation_record_at(
        root,
        &job,
        &model,
        GeneratedTextPurpose::TitleTranslation,
        &title.body_markdown,
        historical,
    )?;
    let expected_document = translation_record_at(
        root,
        &job,
        &model,
        GeneratedTextPurpose::DocumentTranslation,
        &document.body_markdown,
        historical,
    )?;
    if title != expected_title
        || document != expected_document
        || competing_translation_records(root, &job, &title, &document)?
    {
        return Ok(None);
    }
    Ok(Some(TranslationAnalysisPair {
        title_analysis_id,
        document_analysis_id,
    }))
}

pub(crate) fn open_status_database(library: &Path) -> Result<Option<(PathBuf, Connection)>> {
    let root = super::safe_library_root(library)?;
    let analysis = root.join("analysis");
    match fs::symlink_metadata(&analysis) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => bail!("unsafe analysis directory"),
        Err(error) => return Err(error.into()),
    }
    let database = analysis.join(DATABASE_NAME);
    match fs::symlink_metadata(&database) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {}
        Ok(_) => bail!("unsafe translation database"),
        Err(error) => return Err(error.into()),
    }
    validate_database_family(&database)?;
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let database_version =
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
    if database_version != 1 {
        bail!("unsupported translation database version")
    }
    Ok(Some((root, connection)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceClassification {
    Chinese,
    English,
    /// Any other language with enough letters; translatable to Simplified Chinese.
    Other,
    ChallengeOrLogin,
    TooLarge,
    Unknown,
}

/// Mirrors the capture admission rule (`LONG_FORM_TEXT_CHARS` in memex-capture): login and
/// common challenge words inside a long document are content, not a wall.
const LONG_FORM_TEXT_CHARS: usize = 3_000;
const MIN_LANGUAGE_LETTERS: usize = 80;

/// Bounded, script-based language evidence for one title + document.
struct LanguageSample {
    han: usize,
    kana: usize,
    hangul: usize,
    ascii_letters: usize,
    cyrillic: usize,
    greek: usize,
    arabic: usize,
    hebrew: usize,
    thai: usize,
    devanagari: usize,
    letters: usize,
    english_words: usize,
    latin_language: Option<&'static str>,
}

fn language_sample(sample: &str) -> LanguageSample {
    let mut out = LanguageSample {
        han: 0,
        kana: 0,
        hangul: 0,
        ascii_letters: 0,
        cyrillic: 0,
        greek: 0,
        arabic: 0,
        hebrew: 0,
        thai: 0,
        devanagari: 0,
        letters: 0,
        english_words: 0,
        latin_language: None,
    };
    for character in sample.chars() {
        let code = character as u32;
        if is_han(character) {
            out.han += 1;
        } else if matches!(code, 0x3040..=0x30ff | 0x31f0..=0x31ff | 0xff66..=0xff9f) {
            out.kana += 1;
        } else if matches!(code, 0xac00..=0xd7af | 0x1100..=0x11ff | 0x3130..=0x318f) {
            out.hangul += 1;
        } else if character.is_ascii_alphabetic() {
            out.ascii_letters += 1;
        } else if !character.is_alphabetic() {
            continue;
        } else if matches!(code, 0x0400..=0x052f) {
            out.cyrillic += 1;
        } else if matches!(code, 0x0370..=0x03ff) {
            out.greek += 1;
        } else if matches!(code, 0x0600..=0x06ff | 0x0750..=0x077f) {
            out.arabic += 1;
        } else if matches!(code, 0x0590..=0x05ff) {
            out.hebrew += 1;
        } else if matches!(code, 0x0e00..=0x0e7f) {
            out.thai += 1;
        } else if matches!(code, 0x0900..=0x097f) {
            out.devanagari += 1;
        }
        if character.is_alphabetic() {
            out.letters += 1;
        }
    }
    let common_english = [
        "the", "and", "that", "this", "with", "from", "for", "was", "were", "are", "has", "have",
        "not", "but", "their", "into", "about", "more", "when", "which",
    ];
    // Best-effort code for common Latin-script languages; the model identifies the rest.
    let latin: [(&str, &[&str]); 6] = [
        (
            "fr",
            &[
                "le", "la", "les", "des", "est", "une", "dans", "pour", "qui", "pas", "avec",
                "sur", "nous", "vous", "du", "au",
            ],
        ),
        (
            "de",
            &[
                "der", "die", "das", "und", "ist", "nicht", "mit", "ein", "eine", "auf", "sich",
                "dem", "den", "auch", "wir",
            ],
        ),
        (
            "es",
            &[
                "el", "los", "las", "una", "del", "por", "con", "para", "que", "como", "pero",
                "sus", "muy", "es", "se",
            ],
        ),
        (
            "it",
            &[
                "il", "che", "della", "gli", "una", "sono", "per", "non", "con", "anche", "come",
                "nel", "alla", "di",
            ],
        ),
        (
            "pt",
            &[
                "os", "uma", "não", "com", "para", "que", "dos", "das", "como", "mais", "mas",
                "foi", "ao", "em",
            ],
        ),
        (
            "nl",
            &[
                "het", "een", "van", "en", "niet", "zijn", "met", "voor", "dat", "ook", "maar",
                "wij", "wordt",
            ],
        ),
    ];
    let mut latin_counts = [0usize; 6];
    for word in sample
        .split(|character: char| !character.is_alphabetic())
        .filter(|word| !word.is_empty())
        .take(20_000)
    {
        let word = word.to_lowercase();
        if common_english.contains(&word.as_str()) {
            out.english_words += 1;
        }
        for (index, (_, words)) in latin.iter().enumerate() {
            if words.contains(&word.as_str()) {
                latin_counts[index] += 1;
            }
        }
    }
    out.latin_language = latin_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .filter(|(_, count)| **count >= 5 && **count > out.english_words)
        .map(|(index, _)| latin[index].0);
    out
}

fn classify_sample(title: &str, document: &str) -> (SourceClassification, &'static str) {
    if title.len().saturating_add(document.len()) > MAX_TRANSLATION_SOURCE_BYTES {
        return (SourceClassification::TooLarge, "und");
    }
    let sample = format!("{title}\n{document}");
    let lowercase = sample.to_lowercase();
    // Interactive/challenge widgets: length-independent.
    let strong_markers = [
        "captcha",
        "cf-challenge",
        "challenge-form",
        "verify you are human",
        "人机验证",
        "安全验证",
    ];
    // Ordinary words (a transcript may say "I logged in"): only a short page is a wall.
    let wall_markers = [
        "login",
        "log in",
        "sign in",
        "security check",
        "enable javascript",
        "登录后继续",
        "请先登录",
        "扫码",
        "二维码",
    ];
    let short = document
        .chars()
        .filter(|character| !character.is_whitespace())
        .take(LONG_FORM_TEXT_CHARS)
        .count()
        < LONG_FORM_TEXT_CHARS;
    if strong_markers
        .iter()
        .any(|marker| lowercase.contains(marker))
        || (short && wall_markers.iter().any(|marker| lowercase.contains(marker)))
    {
        return (SourceClassification::ChallengeOrLogin, "und");
    }
    let language = language_sample(&sample);
    let letters = language.letters;
    if letters < MIN_LANGUAGE_LETTERS && language.han < 20 {
        return (SourceClassification::Unknown, "und");
    }
    // Japanese mixes kanji and kana; any real kana share is not Chinese.
    let japanese =
        language.kana >= 20 && language.kana.saturating_mul(100) >= letters.saturating_mul(5);
    if !japanese
        && language.han >= 20
        && language.han.saturating_mul(100) >= letters.saturating_mul(35)
    {
        return (SourceClassification::Chinese, "zh-CN");
    }
    if letters < MIN_LANGUAGE_LETTERS {
        return (SourceClassification::Unknown, "und");
    }
    if language.ascii_letters.saturating_mul(100) >= letters.saturating_mul(90)
        && language.english_words >= 3
        && language.latin_language.is_none()
    {
        return (SourceClassification::English, "en");
    }
    let scripts = [
        (
            language.kana + language.han,
            if japanese { "ja" } else { "und-Hani" },
        ),
        (language.hangul, "ko"),
        (language.cyrillic, "und-Cyrl"),
        (language.greek, "el"),
        (language.arabic, "und-Arab"),
        (language.hebrew, "und-Hebr"),
        (language.thai, "th"),
        (language.devanagari, "und-Deva"),
        (
            language.ascii_letters,
            language.latin_language.unwrap_or("und-Latn"),
        ),
    ];
    let code = scripts
        .iter()
        .max_by_key(|(count, _)| *count)
        .filter(|(count, _)| count.saturating_mul(2) >= letters)
        .map_or("und", |(_, code)| *code);
    (SourceClassification::Other, code)
}

pub fn classify_source(title: &str, document: &str) -> SourceClassification {
    classify_sample(title, document).0
}

/// Best-effort BCP 47 source language for a translatable or summarizable document: `en`,
/// `zh-CN`, a script-level guess such as `ja`, `fr` or `und-Cyrl`, or `und` when unknown.
pub fn source_language(title: &str, document: &str) -> &'static str {
    classify_sample(title, document).1
}

pub(crate) fn valid_translation_output(translated: &str, source: &str, purpose: GeneratedTextPurpose) -> bool {
    translation_output_failure(translated, source, purpose).is_none()
}

/// First line of a document block the adapter kept in its original language because its
/// translation failed validation (or a single-block request still failed). The rest of the
/// block is the exact source block; the whole-document check verifies that and bounds how much
/// of a document may be kept this way, so source text is never accepted as a translation.
pub const UNTRANSLATED_BLOCK_MARKER: &str = "<!-- memex:untranslated -->";

/// At most this share (1/4) of the source bytes may be kept as untranslated blocks.
const MAX_UNTRANSLATED_SHARE_DIVISOR: usize = 4;

/// A translated document block of `source`, kept in the original language.
pub fn untranslated_block(source: &str) -> String {
    format!("{UNTRANSLATED_BLOCK_MARKER}\n{source}")
}

/// Untranslated blocks of a (validated) document translation.
pub fn untranslated_block_count(document: &str) -> usize {
    document
        .split("\n\n")
        .filter(|block| block.starts_with(UNTRANSLATED_BLOCK_MARKER))
        .count()
}

/// The document without its untranslated blocks, plus their byte and ASCII-letter totals.
/// `None` when a marker is malformed or its block is not text of the source.
fn untranslated_parts(translated: &str, source: &str) -> Option<(String, usize, usize)> {
    // A source that itself contains the marker string cannot carry trustworthy marks: every
    // block is then judged as translated text.
    if !translated.contains(UNTRANSLATED_BLOCK_MARKER) || source.contains(UNTRANSLATED_BLOCK_MARKER)
    {
        return Some((translated.to_owned(), 0, 0));
    }
    let mut rest = String::new();
    let (mut bytes, mut letters) = (0, 0);
    for block in translated.split("\n\n") {
        match block.strip_prefix(UNTRANSLATED_BLOCK_MARKER) {
            Some(body) => {
                let body = body.strip_prefix('\n')?;
                if body.trim().is_empty()
                    || body.contains(UNTRANSLATED_BLOCK_MARKER)
                    || !source.contains(body)
                {
                    return None;
                }
                bytes += body.len();
                letters += body.chars().filter(char::is_ascii_alphabetic).count();
            }
            None if block.contains(UNTRANSLATED_BLOCK_MARKER) => return None,
            None => {
                rest.push_str(block);
                rest.push_str("\n\n");
            }
        }
    }
    Some((rest, bytes, letters))
}

/// Why one translated document block is not a translation of its source block, or `None`.
/// Short blocks (speaker labels, names, timestamps, fewer than 24 ASCII letters or 4 words)
/// are only checked for being non-empty: keeping them as written is legitimate.
pub fn translated_block_failure(translated: &str, source: &str) -> Option<&'static str> {
    let translated = translated.trim();
    let source = source.trim();
    if translated.is_empty() {
        return Some("empty");
    }
    if translated.contains(UNTRANSLATED_BLOCK_MARKER)
        || translated
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Some("control_chars");
    }
    let source_letters = source.chars().filter(char::is_ascii_alphabetic).count();
    let source_words = source
        .split_whitespace()
        .filter(|word| word.chars().any(|character| character.is_ascii_alphabetic()))
        .count();
    if source_letters < 24 || source_words < 4 {
        return None;
    }
    if translated == source {
        return Some("block_identical");
    }
    let han = translated.chars().filter(|character| is_han(*character)).count();
    let letters = translated.chars().filter(|character| character.is_alphabetic()).count();
    // A Chinese rendering needs roughly one Han character per 2–4 English letters; keeping
    // English names and terms is fine, an essentially English answer is not.
    if han.saturating_mul(16) < source_letters || han.saturating_mul(5) < letters {
        return Some("cjk_ratio");
    }
    None
}

/// Why `translated` is not a valid translation of `source`, as a fixed check name, or `None`.
pub fn translation_output_failure(
    translated: &str,
    source: &str,
    purpose: GeneratedTextPurpose,
) -> Option<&'static str> {
    let raw_translated = translated;
    let translated = translated.trim();
    let source = source.trim();
    // A title that is itself a proper name (product, person, publication) is legitimately
    // kept as is; it still passes the generic shape checks but needs no Chinese characters.
    let kept_title = purpose == GeneratedTextPurpose::TitleTranslation && translated == source;
    if translated.is_empty() {
        return Some("empty");
    }
    if raw_translated.contains('\0')
        || raw_translated
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Some("control_chars");
    }
    if raw_translated.len() > MAX_TRANSLATION_OUTPUT_BYTES {
        return Some("too_large");
    }
    // Untranslated document blocks are exact source blocks: verified, bounded, and left out of
    // the Chinese-share check below.
    let (rest, untranslated_bytes, untranslated_letters) =
        if purpose == GeneratedTextPurpose::DocumentTranslation {
            match untranslated_parts(translated, source) {
                Some(parts) => parts,
                None => return Some("invalid_untranslated_mark"),
            }
        } else {
            (translated.to_owned(), 0, 0)
        };
    if untranslated_bytes.saturating_mul(MAX_UNTRANSLATED_SHARE_DIVISOR) > source.len() {
        return Some("too_many_untranslated");
    }
    if translated == source && !kept_title {
        return Some("identical");
    }
    if source.chars().count() >= 32 && translated.contains(source) && !kept_title {
        return Some("contains_source");
    }
    let translated_characters = translated.chars().count();
    let source_characters = source.chars().count();
    match purpose {
        GeneratedTextPurpose::TitleTranslation => {
            if translated.contains('\n') {
                return Some("multiline");
            }
            if translated.starts_with('#') {
                return Some("heading");
            }
            if translated_characters > MAX_TITLE_CHARACTERS
                || translated_characters > source_characters.saturating_mul(3).saturating_add(32)
            {
                return Some("too_long");
            }
            if translated_characters < source_characters.saturating_add(4) / 5 {
                return Some("too_short");
            }
        }
        GeneratedTextPurpose::DocumentTranslation => {
            if translated_characters < source_characters.saturating_add(19) / 20 {
                return Some("too_short");
            }
            if translated_characters > source_characters.saturating_mul(3).saturating_add(1024) {
                return Some("too_long");
            }
            if (translated.starts_with("```") && translated.ends_with("```"))
                || (translated.starts_with("~~~") && translated.ends_with("~~~"))
            {
                return Some("code_fence");
            }
            if !balanced_markdown_fences(translated) {
                return Some("unbalanced_fence");
            }
        }
        GeneratedTextPurpose::Summary => return Some("invalid_purpose"),
    }
    if kept_title {
        return None;
    }
    let han = rest.chars().filter(|character| is_han(*character)).count();
    let letters = rest.chars().filter(|character| character.is_alphabetic()).count();
    let valid = if purpose == GeneratedTextPurpose::TitleTranslation {
        // At least one Han character, and either Han outweighs the rest or every Latin word
        // left is a word of the source title (proper names kept, e.g. `Orbs 与 Jellyware`).
        let source_words: Vec<String> = latin_words(source).collect();
        han >= 1
            && (han.saturating_mul(2) >= letters
                || latin_words(translated).all(|word| source_words.contains(&word)))
    } else {
        let source_letters = source
            .chars()
            .filter(char::is_ascii_alphabetic)
            .count()
            .saturating_sub(untranslated_letters);
        // Proper names, product names and code terms commonly stay in English in a podcast or
        // technical transcript (and passthrough code/URL blocks stay byte-identical), so the
        // Chinese share is bounded at 1 Han character per 4 other letters (was 1 per 1).
        han >= (source_letters.saturating_add(63) / 64).max(8)
            && han.saturating_mul(5) >= letters
    };
    (!valid).then_some("cjk_ratio")
}

/// Lowercase ASCII-letter words of `text`.
fn latin_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| word.chars().any(|character| character.is_ascii_alphabetic()))
        .map(str::to_ascii_lowercase)
}

fn balanced_markdown_fences(markdown: &str) -> bool {
    let mut backticks_open = false;
    let mut tildes_open = false;
    for line in markdown.lines().map(str::trim_start) {
        if line.starts_with("```") {
            backticks_open = !backticks_open;
        } else if line.starts_with("~~~") {
            tildes_open = !tildes_open;
        }
    }
    !backticks_open && !tildes_open
}

fn is_han(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff)
}

fn skipped(revision: &ItemRevision, reason: TranslationSkipReason) -> TranslationEnqueueOutcome {
    TranslationEnqueueOutcome::Skipped {
        item_id: revision.item_id.clone(),
        revision_id: revision.revision_id.clone(),
        reason,
    }
}

/// The job's record, bound to the job's revision: the current one, or a same-body revision
/// that only metadata edits have replaced since.
fn translation_record(
    library: &Path,
    job: &TranslationJobRecord,
    model: &TranslationModelDescriptor,
    purpose: GeneratedTextPurpose,
    body: &str,
) -> Result<super::AnalysisRecord> {
    let source = crate::lineage::job_source(
        library,
        &job.view.item_id,
        &job.view.revision_id,
        &job.revision_sha256,
    )?;
    let historical = match &source {
        Some(source) if crate::lineage::historical(library, source)? => Some(source),
        _ => None,
    };
    translation_record_at(library, job, model, purpose, body, historical)
}

fn translation_record_at(
    library: &Path,
    job: &TranslationJobRecord,
    model: &TranslationModelDescriptor,
    purpose: GeneratedTextPurpose,
    body: &str,
    historical: Option<&ItemRevision>,
) -> Result<super::AnalysisRecord> {
    let (prompt_id, _, prompt_sha256) = generated_text_prompt(purpose);
    let config = format!(
        "translation-v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        job.view.item_id,
        job.view.revision_id,
        job.revision_sha256,
        job.view.source_language,
        job.view.target_language,
        model.model_id,
        model.model_sha256,
        prompt_sha256
    );
    let mut record = prepare_record(
        library,
        &AnalysisRequest {
            schema_version: SCHEMA_VERSION,
            analysis_kind: AnalysisKind::Translation,
            title: match purpose {
                GeneratedTextPurpose::TitleTranslation => "Chinese title translation",
                GeneratedTextPurpose::DocumentTranslation => "Chinese document translation",
                GeneratedTextPurpose::Summary => bail!("invalid translation purpose"),
            }
            .into(),
            body_markdown: body.into(),
            item_ids: vec![job.view.item_id.clone()],
            topics: Vec::new(),
            requested_sensitivity: None,
            created_at: job.view.created_at.clone(),
            actor_kind: ActorKind::Agent,
            actor_id: model.adapter_id.clone(),
            tool_id: model.adapter_id.clone(),
            tool_version: model.adapter_version.clone(),
            config_digest: format!("sha256:{:x}", Sha256::digest(config.as_bytes())),
            generated_text: Some(GeneratedTextDescriptor {
                purpose,
                source_language: job.view.source_language.clone(),
                target_language: job.view.target_language.clone(),
                output_limit: (purpose == GeneratedTextPurpose::TitleTranslation).then_some(
                    TextOutputLimit {
                        unit: TextLengthUnit::Characters,
                        maximum: MAX_TITLE_CHARACTERS as u32,
                    },
                ),
                summary_job_id: None,
                translation_job_id: Some(job.view.job_id.clone()),
                model_id: model.model_id.clone(),
                model_sha256: Some(model.model_sha256.clone()),
                prompt_id: prompt_id.into(),
                prompt_version: GENERATED_TEXT_PROMPT_VERSION.into(),
                prompt_sha256,
            }),
        },
    )?;
    if let Some(revision) = historical {
        record.inputs = vec![revision_ref(&job.view.item_id, revision)?];
        record.sensitivity = revision.sensitivity;
        record.analysis_id = super::analysis_id(&record)?;
        super::validate_record(&record)?;
    }
    if record.inputs.len() != 1
        || record.inputs[0].item_id != job.view.item_id
        || record.inputs[0].revision_id != job.view.revision_id
        || record.inputs[0].revision_sha256 != job.revision_sha256
    {
        bail!("translation record revision changed")
    }
    Ok(record)
}

fn competing_translation_records(
    library: &Path,
    job: &TranslationJobRecord,
    title: &super::AnalysisRecord,
    document: &super::AnalysisRecord,
) -> Result<bool> {
    for record in list_records(library)? {
        if record.inputs.len() != 1
            || record.inputs[0].item_id != job.view.item_id
            || record.inputs[0].revision_id != job.view.revision_id
            || record.inputs[0].revision_sha256 != job.revision_sha256
        {
            continue;
        }
        let Some(generated) = record.generated_text else {
            continue;
        };
        if !same_language_family(&generated.source_language, &job.view.source_language)
            || !same_language_family(&generated.target_language, &job.view.target_language)
        {
            continue;
        }
        let expected = match generated.purpose {
            GeneratedTextPurpose::TitleTranslation => Some(&title.analysis_id),
            GeneratedTextPurpose::DocumentTranslation => Some(&document.analysis_id),
            GeneratedTextPurpose::Summary => None,
        };
        if expected.is_some_and(|expected| expected != &record.analysis_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn same_language_family(left: &str, right: &str) -> bool {
    left.split('-')
        .next()
        .zip(right.split('-').next())
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn same_model(left: &TranslationModelDescriptor, right: &TranslationModelDescriptor) -> bool {
    left.model_id == right.model_id
        && left.model_sha256 == right.model_sha256
        && left.adapter_id == right.adapter_id
        && left.adapter_version == right.adapter_version
        && left.request_timeout == right.request_timeout
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_state(value: &str) -> rusqlite::Result<TranslationJobState> {
    match value {
        "queued" => Ok(TranslationJobState::Queued),
        "running" => Ok(TranslationJobState::Running),
        "succeeded" => Ok(TranslationJobState::Succeeded),
        "failed" => Ok(TranslationJobState::Failed),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn valid_reason(reason: &str) -> bool {
    matches!(
        reason,
        "analysis_conflict"
            | "automatic_retry"
            | "invalid_provider_configuration"
            | "malformed_model_output"
            | "manual_retry"
            | "provider_failure"
            | "provider_model_mismatch"
            | "provider_timeout"
            | "provider_outcome_unknown"
            | "provider_input_unsupported"
            | "retry_exhausted"
            | "stale_revision"
            | "superseded_by_revision"
            | "inherited_from_revision"
            | "worker_recovered"
    )
}

/// Failures, timeouts, uncertain outcomes and malformed replies are all retried automatically
/// (bounded by `MAX_ATTEMPTS`, with backoff): the owner's route is a subscription, so a request
/// that may have been sent is retried instead of waiting for a click. Input the provider
/// rejects as unsupported is deterministic and is not retried.
/// Fixed check name of a bounded model error (when the adapter reported no finer detail).
pub(crate) fn model_error_check(error: TranslationModelError) -> &'static str {
    match error {
        TranslationModelError::ProviderFailure => "provider_failure",
        TranslationModelError::Timeout => "timeout",
        TranslationModelError::OutcomeUnknown => "outcome_unknown",
        TranslationModelError::MalformedOutput => "malformed",
        TranslationModelError::InputUnsupported => "input_unsupported",
    }
}

pub(crate) fn model_error_reason(error: TranslationModelError) -> (&'static str, bool) {
    match error {
        TranslationModelError::ProviderFailure => ("provider_failure", true),
        TranslationModelError::Timeout => ("provider_timeout", true),
        TranslationModelError::OutcomeUnknown => ("provider_outcome_unknown", true),
        TranslationModelError::MalformedOutput => ("malformed_model_output", true),
        TranslationModelError::InputUnsupported => ("provider_input_unsupported", false),
    }
}

/// Additive migration safe against concurrent openers (the provider workers open their queues
/// at the same time): the column check and `alter` run in one IMMEDIATE transaction, so a
/// second opener sees the first one's columns instead of failing on a duplicate column.
pub(crate) fn add_columns(
    connection: &Connection,
    table: &str,
    marker_column: &str,
    alter: &str,
) -> Result<()> {
    if has_column(connection, table, marker_column)? {
        return Ok(());
    }
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        if !has_column(connection, table, marker_column)? {
            connection.execute_batch(alter)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the current job for this revision was explicitly authorized for a cloud provider.
pub fn translation_authorized(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<bool> {
    let Some((_, connection)) = open_status_database(library.as_ref())? else {
        return Ok(false);
    };
    if !has_column(&connection, "translation_jobs", "provider_authorized")? {
        return Ok(false);
    }
    Ok(connection
        .query_row(
            "SELECT provider_authorized FROM translation_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        == Some(1))
}

fn reason_retryable(reason: Option<&str>, attempts: u32) -> bool {
    attempts < MAX_ATTEMPTS
        && matches!(
            reason,
            Some(
                "provider_failure"
                    | "provider_timeout"
                    | "provider_outcome_unknown"
                    | "malformed_model_output"
            )
        )
}

fn job_view_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationJobView> {
    job_view_columns(row)
}

fn job_view_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationJobView> {
    let state = parse_state(&row.get::<_, String>(5)?)?;
    let reason_code = row.get::<_, Option<String>>(6)?;
    if reason_code
        .as_deref()
        .is_some_and(|reason| !valid_reason(reason))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let attempt_count = row.get::<_, i64>(7)?;
    if attempt_count < 0 || attempt_count > u32::MAX as i64 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let attempt_count = attempt_count as u32;
    let job_id = row.get::<_, String>(0)?;
    let item_id = row.get::<_, String>(1)?;
    let revision_id = row.get::<_, String>(2)?;
    let source_language = row.get::<_, String>(3)?;
    let target_language = row.get::<_, String>(4)?;
    let created_at = row.get::<_, String>(8)?;
    let updated_at = row.get::<_, String>(9)?;
    let state_reason_valid = match state {
        TranslationJobState::Queued => matches!(
            reason_code.as_deref(),
            None | Some("automatic_retry" | "manual_retry" | "worker_recovered")
        ),
        TranslationJobState::Running | TranslationJobState::Succeeded => reason_code.is_none(),
        TranslationJobState::Failed => reason_code.is_some(),
    };
    if !valid_prefixed_digest(&job_id, "translation_")
        || !valid_component(&item_id, "item_")
        || !valid_component(&revision_id, "revision_")
        || !valid_source_language(&source_language)
        || target_language != TARGET_LANGUAGE
        || !state_reason_valid
        || chrono::DateTime::parse_from_rfc3339(&created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&updated_at).is_err()
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(TranslationJobView {
        job_id,
        item_id,
        revision_id,
        source_language,
        target_language,
        state,
        retryable: state == TranslationJobState::Failed
            && reason_retryable(reason_code.as_deref(), attempt_count),
        reason_code,
        attempt_count,
        created_at,
        updated_at,
    })
}

/// A short BCP 47 tag other than the Chinese target (`en`, `ja`, `und-Latn`, ...).
pub(crate) fn valid_source_language(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && !value.eq_ignore_ascii_case(TARGET_LANGUAGE)
        && value.split('-').all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn valid_prefixed_digest(value: &str, prefix: &str) -> bool {
    value.len() == prefix.len() + 64
        && value.starts_with(prefix)
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_component(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(unix)]
fn ensure_database_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(()),
        Ok(_) => bail!("unsafe translation database"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?
                .sync_all()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn ensure_database_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(()),
        Ok(_) => bail!("unsafe translation database"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .sync_all()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_database_family(database: &Path) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let parent = database.parent().context("unsafe translation database")?;
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .context("unsafe translation database")?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = parent.join(format!("{name}{suffix}"));
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if !metadata.file_type().is_symlink() && metadata.is_file() && {
                    #[cfg(unix)]
                    {
                        metadata.uid() == unsafe { libc::geteuid() }
                            && metadata.permissions().mode() & 0o077 == 0
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                } => {}
            Ok(_) => bail!("unsafe translation database family"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn secure_database_family(database: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = database.parent().context("unsafe translation database")?;
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .context("unsafe translation database")?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = parent.join(format!("{name}{suffix}"));
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.uid() == unsafe { libc::geteuid() } =>
            {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(_) => bail!("unsafe translation database family"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn secure_database_family(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ItemDraft, Sensitivity, SourceProfile, VerifiedBlob,
        VerifiedCapture,
    };
    use std::collections::VecDeque;

    fn english_document(marker: &str) -> String {
        format!(
            "# A careful report about the synthetic {marker} case\n\n\
             This is the synthetic source article, and the report was written for the test. \
             The article has more detail about the result, which was checked with a bounded \
             fixture and not with private content. This report has the facts from the source, \
             and it was prepared for deterministic verification."
        )
    }

    fn ingest_article(root: &Path, marker: &str, title: &str, body: String) -> IngestReceipt {
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: SCHEMA_VERSION,
                source_id: "translation-test-source".into(),
                name: "Synthetic translation test source".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: Sensitivity::Internal,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: SCHEMA_VERSION,
                adapter_id: "synthetic-test-adapter".into(),
                adapter_version: "1".into(),
                external_capture_id: format!("translation-{marker}"),
                source_id: "translation-test-source".into(),
                captured_at: "2026-08-11T00:00:00Z".into(),
                capture_method: "synthetic".into(),
                sensitivity: Sensitivity::Internal,
                route_summary: None,
                title: None,
                source_url: format!("https://example.invalid/{marker}"),
                canonical_url: None,
                artifacts: vec![BlobDescriptor {
                    schema_version: SCHEMA_VERSION,
                    locator: format!("memory:{marker}"),
                    media_type: "text/plain".into(),
                    byte_size: 4,
                    sha256: "sha256:body".into(),
                    source_url: None,
                }],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: SCHEMA_VERSION,
                    locator: format!("memory:{marker}"),
                    media_type: "text/plain".into(),
                    byte_size: 4,
                    sha256: "sha256:body".into(),
                    source_url: None,
                },
                blob_id: format!("blob-{marker}"),
                source_backend_ref: Some("synthetic:test".into()),
                bytes: b"body".to_vec(),
            }],
        };
        memex_store::ingest(
            root,
            &capture,
            ItemDraft {
                item_type: "webpage".into(),
                title: title.into(),
                source_url: format!("https://example.invalid/{marker}"),
                canonical_url: None,
                content_markdown: body,
                sensitivity: Sensitivity::Internal,
                translator_id: "synthetic-test-translator".into(),
                translator_version: "1".into(),
                metadata: Default::default(),
                attachments: vec![],
            },
        )
        .unwrap()
    }

    fn enqueue_fixture(root: &Path, marker: &str) -> (IngestReceipt, TranslationJobView) {
        let receipt = ingest_article(
            root,
            marker,
            "A careful synthetic report about the test",
            english_document(marker),
        );
        let outcomes = enqueue_ingest_translations(root, &receipt).unwrap();
        let TranslationEnqueueOutcome::Enqueued { job } = outcomes.into_iter().next().unwrap()
        else {
            panic!("synthetic English article was not enqueued")
        };
        (receipt, job)
    }

    struct SyntheticModel {
        script: VecDeque<std::result::Result<String, TranslationModelError>>,
        calls: Vec<GeneratedTextPurpose>,
    }

    impl SyntheticModel {
        fn successful() -> Self {
            Self {
                script: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn scripted(
            script: impl IntoIterator<Item = std::result::Result<String, TranslationModelError>>,
        ) -> Self {
            Self {
                script: script.into_iter().collect(),
                calls: Vec::new(),
            }
        }
    }

    impl TranslationModel for SyntheticModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-test-only-v1".into(),
                model_sha256: format!("sha256:{}", "7".repeat(64)),
                adapter_id: "memex.synthetic-test-only".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn translate(
            &mut self,
            input: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            self.calls.push(input.purpose());
            if let Some(result) = self.script.pop_front() {
                return result;
            }
            Ok(match input.purpose() {
                GeneratedTextPurpose::TitleTranslation => "用于测试的中文标题译文".into(),
                GeneratedTextPurpose::DocumentTranslation => {
                    "# 测试文档译文\n\n这是只用于测试的确定性中文译文。它验证成功路径，不代表生产翻译。".into()
                }
                GeneratedTextPurpose::Summary => unreachable!(),
            })
        }
    }

    struct FractionalTimeoutModel {
        calls: usize,
    }

    impl TranslationModel for FractionalTimeoutModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-invalid-timeout".into(),
                model_sha256: format!("sha256:{}", "6".repeat(64)),
                adapter_id: "memex.synthetic-invalid-timeout".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_micros(1_500),
            }
        }

        fn translate(
            &mut self,
            _: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            self.calls += 1;
            unreachable!("invalid descriptor must stop before model invocation")
        }
    }

    const REPAIR_TABLE: &str = "| Task description | Estimated cost |\n|---|---:|\n| Read the detailed report | $2.50 |";
    const REPAIRED_TABLE: &str = "| 任务说明文字 | 预估费用 |\n|---|---:|\n| 阅读这份详细的报告 | $2.50 |";

    fn table_repair_fixture(root: &Path) -> (String, String, TranslationAnalysisPair, String) {
        let source = format!("{}\n\n{REPAIR_TABLE}\n\n{REPAIR_TABLE}", english_document("table-repair"));
        let capture = ingest_article(root, "table-repair", "A careful report about synthetic tables", source.clone());
        enqueue_ingest_translations(root, &capture).unwrap();
        let body = format!("{}\r\n\r\n{REPAIR_TABLE}\n\n已有的中间译文🙂\n\n{REPAIR_TABLE}\r\n\r\n已有的结尾译文", "已有的中文正文，不应重译或重排。".repeat(20));
        let mut model = SyntheticModel::scripted([Ok("一份关于合成表格的详细报告".into()), Ok(body.clone())]);
        let mut queue = TranslationQueue::open(root).unwrap();
        assert!(queue.process_next(&mut model).unwrap());
        let item = capture.item_ids[0].clone();
        let (_, revision) = memex_store::read_item(root, &item).unwrap();
        let pair = validated_translation_pair(root, &item, &revision.revision_id).unwrap().unwrap();
        (item, revision.revision_id, pair, body)
    }

    #[test]
    fn table_repair_preserves_base_and_prose_and_replays_without_spending() {
        let root = tempfile::tempdir().unwrap();
        let (item, revision, pair, body) = table_repair_fixture(root.path());
        let base = super::super::read_record(root.path(), &pair.document_analysis_id).unwrap();
        let title = super::super::read_record(root.path(), &pair.title_analysis_id).unwrap();
        let before_job = translation_status(root.path(), &item, &revision).unwrap();
        let plan = translation_table_repair_plan(root.path(), &item, &pair.document_analysis_id).unwrap();
        assert_eq!(plan.plan.tables, 2);
        let mut model = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into()), Ok(REPAIRED_TABLE.into())]);
        let done = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.requests_started, 2);
        assert_eq!(model.calls.len(), 2);
        assert_eq!(super::super::read_record(root.path(), &pair.document_analysis_id).unwrap(), base);
        assert_eq!(super::super::read_record(root.path(), &pair.title_analysis_id).unwrap(), title);
        assert_eq!(translation_status(root.path(), &item, &revision).unwrap(), before_job);
        let projected = read_translation_projection(root.path(), &item, Sensitivity::Private, Some((&revision, 0))).unwrap();
        let page = projected.page.unwrap();
        assert_eq!(page.content_markdown, body.replace(REPAIR_TABLE, REPAIRED_TABLE));
        assert_eq!(page.title_analysis_id, pair.title_analysis_id);
        assert_eq!(Some(page.document_analysis_id.clone()), done.document_analysis_id);
        assert_ne!(page.document_analysis_id, pair.document_analysis_id);
        let replay = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(replay.document_analysis_id, done.document_analysis_id);
        assert_eq!(model.calls.len(), 2);

        // Simulate a crash after immutable record install, before publishing its receipt.
        let queue = TranslationQueue::open(root.path()).unwrap();
        queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(record_json,'$.state','ready','$.document_analysis_id',NULL)", []).unwrap();
        let recovered = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(recovered.document_analysis_id, done.document_analysis_id);
        assert_eq!(model.calls.len(), 2, "cached tables survive the publish crash");

        let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
        let edit = memex_store::revise_item_metadata(root.path(), &item, memex_store::ItemMetadataUpdate {
            expected_revision_id: current.revision_id.clone(), title: current.title,
            manual_tags: vec!["metadata-only".into()],
        }).unwrap();
        enqueue_item_translation(root.path(), &item, &edit.revision_id).unwrap();
        let inherited = read_translation_projection(root.path(), &item, Sensitivity::Private, Some((&edit.revision_id, 0))).unwrap().page.unwrap();
        assert_eq!(inherited.document_analysis_id, page.document_analysis_id);
        assert_eq!(inherited.content_markdown, page.content_markdown);
        let TranslationPreservation::Preserved(rebound) = preserve_completed_translation(root.path(), &item, &revision, &edit.revision_id).unwrap() else { panic!("preservation failed") };
        let preserved = read_translation_projection(root.path(), &item, Sensitivity::Private, Some((&edit.revision_id, 0))).unwrap().page.unwrap();
        assert_eq!(preserved.content_markdown, page.content_markdown);
        assert_eq!(preserved.document_analysis_id, page.document_analysis_id);
        let replay = repair_translation_tables(root.path(), &item, &rebound.document_analysis_id, &mut model).unwrap();
        assert_eq!(replay.document_analysis_id, done.document_analysis_id);
        assert_eq!(model.calls.len(), 2, "metadata preservation cannot pay for tables again");
        assert_eq!(preserve_completed_translation(root.path(), &item, &revision, &edit.revision_id).unwrap(), TranslationPreservation::Preserved(rebound));
    }

    #[test]
    fn table_repair_never_retries_unknown_or_interrupted_calls_and_rejects_stale_base() {
        for interrupted in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (item, revision, pair, body) = table_repair_fixture(root.path());
            let mut model = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into()), Err(TranslationModelError::OutcomeUnknown)]);
            let unknown = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
            assert_eq!(unknown.state, "outcome_unknown");
            assert_eq!(unknown.completed_tables, 1);
            assert_eq!(unknown.requests_started, 2);
            if interrupted {
                let queue = TranslationQueue::open(root.path()).unwrap();
                queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(record_json,'$.state','in_flight_or_unknown')", []).unwrap();
            }
            let replay = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
            assert_ne!(replay.state, "succeeded");
            assert_eq!(model.calls.len(), 2, "unknown outcome cannot be spent twice");
            assert!(repair_translation_tables(root.path(), &item, &format!("analysis_{}", "1".repeat(64)), &mut model).is_err());
            assert_eq!(model.calls.len(), 2);
            assert_eq!(read_translation_projection(root.path(), &item, Sensitivity::Private, Some((&revision, 0))).unwrap().page.unwrap().content_markdown, body);
            assert_eq!(validated_translation_pair(root.path(), &item, &revision).unwrap().unwrap(), pair);

            let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
            let edit = memex_store::revise_item_metadata(root.path(), &item, memex_store::ItemMetadataUpdate {
                expected_revision_id: current.revision_id.clone(), title: current.title,
                manual_tags: vec!["metadata-only".into()],
            }).unwrap();
            let TranslationPreservation::Preserved(rebound) = preserve_completed_translation(root.path(), &item, &revision, &edit.revision_id).unwrap() else { panic!("preservation failed") };
            let status = translation_table_repair_plan(root.path(), &item, &rebound.document_analysis_id).unwrap();
            assert_eq!(status.plan.repair_id, unknown.plan.repair_id);
            assert_eq!(status.completed_tables, 1);
            assert!(resume_invalid_table_repair(root.path(), &item, &rebound.document_analysis_id, &status.plan.repair_id).is_err());
            let replay = repair_translation_tables(root.path(), &item, &rebound.document_analysis_id, &mut model).unwrap();
            assert_eq!(replay.state, status.state);
            assert_eq!(model.calls.len(), 2, "a new metadata base cannot resend an unknown paid table");

            // A cached, known response can resume, but only the remaining table is dispatched
            // and the durable paid identity stays the ancestor's identity.
            let queue = TranslationQueue::open(root.path()).unwrap();
            queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(record_json,'$.state','ready')", []).unwrap();
            let mut remaining = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into())]);
            let done = repair_translation_tables(root.path(), &item, &rebound.document_analysis_id, &mut remaining).unwrap();
            assert_eq!(done.state, "succeeded");
            assert_eq!(done.plan.repair_id, unknown.plan.repair_id);
            assert_eq!(done.requests_started, 3);
            assert_eq!(remaining.calls.len(), 1);
            assert_eq!(read_translation_projection(root.path(), &item, Sensitivity::Private, Some((&edit.revision_id, 0))).unwrap().page.unwrap().content_markdown, body.replace(REPAIR_TABLE, REPAIRED_TABLE));
        }
    }

    #[test]
    fn table_repair_known_invalid_continuation_is_once_bounded_and_legacy_compatible() {
        for legacy in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (item, _, pair, _) = table_repair_fixture(root.path());
            let mut bad = SyntheticModel::scripted([Ok("合成已知无效响应".into())]);
            let failed = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut bad).unwrap();
            assert_eq!(failed.state, "invalid_output");
            assert_eq!(failed.requests_started, 1);
            assert_eq!(failed.last_check.as_deref(), Some("table_shape"));
            assert!(!serde_json::to_string(&failed).unwrap().contains("合成已知无效响应"));
            if legacy {
                let queue = TranslationQueue::open(root.path()).unwrap();
                queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_remove(record_json,'$.attempts','$.continuation_limit')", []).unwrap();
            }
            assert!(resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, "wrong-repair").is_err());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2).map(|_| {
                let (path, item, base, repair, barrier) = (root.path().to_path_buf(), item.clone(), pair.document_analysis_id.clone(), failed.plan.repair_id.clone(), barrier.clone());
                std::thread::spawn(move || { barrier.wait(); resume_invalid_table_repair(&path, &item, &base, &repair).is_ok() })
            }).collect();
            assert_eq!(handles.into_iter().map(|h| h.join().unwrap() as usize).sum::<usize>(), 1);
            // A crash after the grant but before dispatch leaves ready + the same bounded
            // grant. Normal execution resumes it; no second grant or identity is created.
            let mut good = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into()), Ok(REPAIRED_TABLE.into())]);
            let done = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut good).unwrap();
            assert_eq!(done.state, "succeeded"); assert_eq!(done.requests_started, 3);
            assert_eq!(done.plan.repair_id, failed.plan.repair_id); assert_eq!(good.calls.len(), 2);
            assert!(resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &done.plan.repair_id).is_err());
        }
    }

    #[test]
    fn table_repair_continuation_stops_on_another_invalid_answer_and_reuses_known_cache() {
        for reuse in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (item, _, pair, _) = table_repair_fixture(root.path());
            let mut bad = SyntheticModel::scripted([Ok(REPAIRED_TABLE.replace("2.50", "9.00"))]);
            let failed = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut bad).unwrap();
            if reuse {
                // Simulate a complete answer rejected by an older validator, now acceptable.
                let queue = TranslationQueue::open(root.path()).unwrap();
                queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(record_json,'$.attempts[0].output',?1,'$.attempts[0].check','table_alignment')", [REPAIRED_TABLE]).unwrap();
            }
            let ready = resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &failed.plan.repair_id).unwrap();
            assert_eq!(ready.completed_tables, usize::from(reuse));
            assert_eq!(ready.requests_started, 1);
            let mut next = SyntheticModel::scripted([Ok(if reuse { REPAIRED_TABLE.into() } else { "再次无效响应".into() })]);
            let result = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut next).unwrap();
            assert_eq!(result.state, if reuse { "succeeded" } else { "invalid_output" });
            assert_eq!(result.requests_started, 2); assert_eq!(next.calls.len(), 1);
            assert!(resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &result.plan.repair_id).is_err());
            repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut next).unwrap();
            assert_eq!(next.calls.len(), 1);
        }
    }

    #[test]
    fn table_repair_cached_revalidation_is_hash_bound_once_and_never_buys_the_first_table_again() {
        for case in ["valid", "unknown", "inflight", "length", "missing", "invalid", "wrong_hash", "wrong_repair", "wrong_base", "budget", "without_grant"] {
            let root = tempfile::tempdir().unwrap();
            let (item, revision, pair, body) = table_repair_fixture(root.path());
            let mut bad = SyntheticModel::scripted([Ok("合成无效响应".into()), Ok("合成无效响应".into())]);
            let first = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut bad).unwrap();
            resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &first.plan.repair_id).unwrap();
            let failed = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut bad).unwrap();
            assert_eq!(failed.requests_started, 2);
            let queue = TranslationQueue::open(root.path()).unwrap();
            // A known complete answer rejected by the preceding validator; never production SQL.
            queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(record_json,'$.attempts[1].output',?1,'$.attempts[1].check','table_values')", [REPAIRED_TABLE]).unwrap();
            let mut hash = format!("{:x}", Sha256::digest(REPAIRED_TABLE.as_bytes()));
            let mut repair = failed.plan.repair_id.clone();
            let mut expected = pair.document_analysis_id.clone();
            let patch = match case {
                "unknown" => Some("'$.state','outcome_unknown'"),
                "inflight" => Some("'$.state','in_flight_or_unknown'"),
                "length" => Some("'$.attempts[1].check','incomplete_length'"),
                "missing" => Some("'$.attempts[1].output',NULL"),
                "invalid" => { hash = format!("{:x}", Sha256::digest("仍然无效的已知响应".as_bytes())); Some("'$.attempts[1].output','仍然无效的已知响应'") },
                "budget" => Some("'$.requests_started',3"),
                "without_grant" => Some("'$.continuation_limit',NULL"),
                "wrong_hash" => { hash = "0".repeat(64); None },
                "wrong_repair" => { repair = "wrong-repair".into(); None },
                "wrong_base" => { expected = "wrong-analysis".into(); None },
                _ => None,
            };
            if let Some(patch) = patch {
                queue.connection.execute(&format!("UPDATE translation_table_repairs SET record_json=json_set(record_json,{patch})"), []).unwrap();
            }
            if case == "valid" {
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                let handles: Vec<_> = (0..2).map(|_| {
                    let (path,item,expected,repair,hash,barrier) = (root.path().to_path_buf(),item.clone(),expected.clone(),repair.clone(),hash.clone(),barrier.clone());
                    std::thread::spawn(move || { barrier.wait(); revalidate_cached_table_repair(&path,&item,&expected,&repair,&hash).is_ok() })
                }).collect();
                assert_eq!(handles.into_iter().map(|h|h.join().unwrap() as usize).sum::<usize>(),1);
                assert!(revalidate_cached_table_repair(root.path(),&item,&expected,&repair,&hash).is_err());
                let ready = translation_table_repair_plan(root.path(),&item,&expected).unwrap();
                assert_eq!(ready.requests_started,2); assert_eq!(ready.completed_tables,1);
                let raw: String = queue.connection.query_row("SELECT record_json FROM translation_table_repairs",[],|r|r.get(0)).unwrap();
                let history: serde_json::Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(history["attempts"][0]["state"],"invalid_output");
                assert_eq!(history["attempts"][1]["state"],"invalid_output");
                assert_eq!(history["attempts"][1]["reused"],true);
                // Crash after zero-call cache install: normal execution consumes only remaining table.
                let mut remaining = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into())]);
                let done = repair_translation_tables(root.path(),&item,&expected,&mut remaining).unwrap();
                assert_eq!(done.state,"succeeded"); assert_eq!(done.requests_started,3);
                assert_eq!(remaining.calls.len(),1);
                repair_translation_tables(root.path(),&item,&expected,&mut remaining).unwrap();
                assert_eq!(remaining.calls.len(),1);
                assert_eq!(read_translation_projection(root.path(),&item,Sensitivity::Private,Some((&revision,0))).unwrap().page.unwrap().content_markdown,body.replace(REPAIR_TABLE,REPAIRED_TABLE));
            } else {
                let before: String = queue.connection.query_row("SELECT record_json FROM translation_table_repairs",[],|r|r.get(0)).unwrap();
                assert!(revalidate_cached_table_repair(root.path(),&item,&expected,&repair,&hash).is_err(),"{case}");
                let after: String = queue.connection.query_row("SELECT record_json FROM translation_table_repairs",[],|r|r.get(0)).unwrap();
                assert_eq!(before,after,"rejection must not mutate or authorize {case}");
            }
            assert_eq!(bad.calls.len(),2);
        }
    }

    #[test]
    fn table_repair_retains_length_response_but_never_installs_or_reuses_it() {
        struct LengthModel(SyntheticModel);
        impl TranslationModel for LengthModel {
            fn descriptor(&self) -> TranslationModelDescriptor { self.0.descriptor() }
            fn translate(&mut self, input: TranslationModelInput<'_>) -> std::result::Result<String, TranslationModelError> {
                self.0.translate(input)
            }
            fn translate_table(&mut self, input: TranslationModelInput<'_>) -> std::result::Result<TableTranslationOutput, TranslationModelError> {
                self.0.translate(input).map(TableTranslationOutput::IncompleteLength)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (item, _, pair, _) = table_repair_fixture(root.path());
        // Even a syntactically complete table cannot be trusted after length truncation.
        let mut model = LengthModel(SyntheticModel::scripted([Ok(REPAIRED_TABLE.into()), Ok(REPAIRED_TABLE.into())]));
        let failed = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(failed.state, "invalid_output");
        assert_eq!(failed.last_check.as_deref(), Some("incomplete_length"));
        assert_eq!(failed.completed_tables, 0);
        assert!(failed.document_analysis_id.is_none());
        let queue = TranslationQueue::open(root.path()).unwrap();
        let stored: String = queue.connection.query_row("SELECT record_json FROM translation_table_repairs WHERE repair_id=?1", [&failed.plan.repair_id], |row| row.get(0)).unwrap();
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["attempts"][0]["output"], REPAIRED_TABLE);
        repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(model.0.calls.len(), 1, "no automatic length retry");
        let ready = resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &failed.plan.repair_id).unwrap();
        assert_eq!(ready.completed_tables, 0, "a known partial answer cannot be reused");
        let stopped = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(stopped.requests_started, 2);
        assert_eq!(stopped.state, "invalid_output");
        assert_eq!(stopped.completed_tables, 0);
        assert!(resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &failed.plan.repair_id).is_err());
        repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut model).unwrap();
        assert_eq!(model.0.calls.len(), 2);
    }

    #[test]
    fn table_repair_continuation_never_dispatches_past_the_total_budget() {
        let root = tempfile::tempdir().unwrap();
        let (item, _, pair, _) = table_repair_fixture(root.path());
        let mut bad = SyntheticModel::scripted([Ok("已知无效响应".into())]);
        let failed = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut bad).unwrap();
        // A legacy receipt with two spent intents has only one request left, not two.
        let queue = TranslationQueue::open(root.path()).unwrap();
        queue.connection.execute("UPDATE translation_table_repairs SET record_json=json_set(json_remove(record_json,'$.attempts'),'$.requests_started',2)", []).unwrap();
        resume_invalid_table_repair(root.path(), &item, &pair.document_analysis_id, &failed.plan.repair_id).unwrap();
        let mut good = SyntheticModel::scripted([Ok(REPAIRED_TABLE.into())]);
        let stopped = repair_translation_tables(root.path(), &item, &pair.document_analysis_id, &mut good).unwrap();
        assert_eq!(stopped.state, "budget_exhausted");
        assert_eq!(stopped.requests_started, 3); assert_eq!(stopped.completed_tables, 1);
        assert_eq!(good.calls.len(), 1);
    }

    #[test]
    fn bounded_classifier_is_conservative_about_chinese_login_and_unknown_text() {
        assert_eq!(
            serde_json::to_string(&TranslationSkipReason::ChallengeOrLogin).unwrap(),
            "\"challenge_or_login\""
        );
        assert_eq!(
            serde_json::to_string(&TranslationSkipReason::SourceLanguageUnknown).unwrap(),
            "\"source_language_unknown\""
        );
        assert_eq!(
            classify_source(
                "中文测试文章",
                "这是一篇中文测试文章，其中包含足够多的中文字符，用于确认无需翻译。"
            ),
            SourceClassification::Chinese
        );
        assert_eq!(
            classify_source(
                "Sign in to continue",
                "Security check: verify you are human with CAPTCHA"
            ),
            SourceClassification::ChallengeOrLogin
        );
        // Asymmetric pair: a long transcript that mentions logging in is content; a short
        // "please log in" page is a wall.
        let transcript = format!(
            "{} Then I logged in with the login it gave me, and the security check passed.",
            "This is the report and it has the source facts with more detail. ".repeat(300)
        );
        assert_eq!(
            classify_source("A careful report about account security", &transcript),
            SourceClassification::English
        );
        assert_eq!(
            source_language("A careful report about account security", &transcript),
            "en"
        );
        assert_eq!(
            classify_source(
                "Account",
                "Please log in to continue reading this article. Sign in with your account, \
                 or create a new one. Enable JavaScript to continue with the login form."
            ),
            SourceClassification::ChallengeOrLogin
        );
        // Strong challenge widgets stay length-independent.
        assert_eq!(
            classify_source("A careful report", &format!("{transcript} captcha"),),
            SourceClassification::ChallengeOrLogin
        );
        assert_eq!(
            classify_source(
                "长文",
                &format!(
                    "{}请先登录",
                    "这是一篇很长的中文文章，内容包括很多段落。".repeat(300)
                )
            ),
            SourceClassification::Chinese
        );
        assert_eq!(
            classify_source("Rapport synthétique", "Ceci est un texte bref et ambigu."),
            SourceClassification::Unknown
        );
        assert_eq!(
            classify_source("Synthetic report", &english_document("classifier")),
            SourceClassification::English
        );
    }

    #[test]
    fn non_english_documents_are_translatable_with_a_best_effort_source_language() {
        let french = "Le rapport décrit une expérience menée dans un laboratoire. Les chercheurs \
            ont observé que la méthode est plus rapide pour les petites données, mais elle n'est \
            pas toujours fiable. Nous avons comparé les résultats avec des mesures publiées dans \
            une revue, et la différence est faible pour la plupart des cas.";
        assert_eq!(
            classify_source("Rapport de laboratoire", french),
            SourceClassification::Other
        );
        assert_eq!(source_language("Rapport de laboratoire", french), "fr");
        let japanese = "これは合成された日本語の記事です。研究者たちは新しい方法を試して、\
            その結果を詳しく説明しました。この方法は小さなデータではとても速いですが、\
            いつも正しいとは限りません。私たちは結果を公開された測定と比べました。";
        assert_eq!(
            classify_source("日本語の記事", japanese),
            SourceClassification::Other
        );
        assert_eq!(source_language("日本語の記事", japanese), "ja");
        // Too few letters stays unknown; Chinese stays untranslated.
        assert_eq!(
            classify_source("Note", "Merci beaucoup."),
            SourceClassification::Unknown
        );
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_article(
            root.path(),
            "french",
            "Rapport de laboratoire",
            french.into(),
        );
        let TranslationEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_translations(root.path(), &receipt)
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
        else {
            panic!("a French article must be translatable")
        };
        assert_eq!(job.source_language, "fr");
        assert_eq!(job.target_language, "zh-CN");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(
            queue.get(&job.job_id).unwrap().state,
            TranslationJobState::Succeeded
        );
    }

    #[test]
    fn a_title_kept_as_its_proper_name_is_a_valid_translation() {
        for title in ["Stratechery", "GPT-5", "  The Innovator's Dilemma, Revisited Again  "] {
            assert!(valid_translation_output(title, title, GeneratedTextPurpose::TitleTranslation));
        }
        // Only an exact echo is exempt: other English output still needs Chinese text,
        // and copying the document body is still rejected.
        assert!(!valid_translation_output("Stratechery Weekly", "Stratechery", GeneratedTextPurpose::TitleTranslation));
        assert!(!valid_translation_output("Stratechery\nX", "Stratechery\nX", GeneratedTextPurpose::TitleTranslation));
        let document = english_document("kept-title");
        assert!(!valid_translation_output(&document, &document, GeneratedTextPurpose::DocumentTranslation));
    }

    #[test]
    fn output_validation_rejects_truncation_copying_and_unbalanced_markdown() {
        let title = "A careful synthetic report about the test";
        let document = english_document("output-validation");
        assert!(!valid_translation_output(
            "中",
            title,
            GeneratedTextPurpose::TitleTranslation,
        ));
        assert!(!valid_translation_output(
            "中文",
            &document,
            GeneratedTextPurpose::DocumentTranslation,
        ));
        assert!(!valid_translation_output(
            &format!("{document}\n\n这是附加的中文文本，用于确认原文复制不会被当成翻译。"),
            &document,
            GeneratedTextPurpose::DocumentTranslation,
        ));
        assert!(!valid_translation_output(
            "# 中文文档\n\n```rust\n这是没有关闭围栏的输出。",
            &document,
            GeneratedTextPurpose::DocumentTranslation,
        ));
    }

    #[test]
    fn reopening_initialized_queues_does_not_commit_database_writes() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, first) = enqueue_fixture(root.path(), "reopen");
        drop(crate::SummaryQueue::open(root.path()).unwrap());
        let observer = Connection::open(root.path().join("analysis").join(DATABASE_NAME)).unwrap();
        let version = || {
            observer
                .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        let before = version();
        drop(TranslationQueue::open(root.path()).unwrap());
        drop(crate::SummaryQueue::open(root.path()).unwrap());
        assert_eq!(
            version(),
            before,
            "opening an existing queue must be read-only"
        );
        let replay = enqueue_ingest_translations(root.path(), &receipt).unwrap();
        let TranslationEnqueueOutcome::Enqueued { job } = &replay[0] else {
            panic!("existing job lost")
        };
        assert_eq!(job, &first);
        assert_eq!(
            version(),
            before,
            "idempotent reconciliation must not write"
        );
        enqueue_fixture(root.path(), "new-job");
        assert_ne!(version(), before, "observer must detect a real enqueue");
    }

    #[test]
    fn duplicate_enqueue_is_stable_and_public_status_contains_no_source_or_output() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, first) = enqueue_fixture(root.path(), "duplicate");
        let second = enqueue_ingest_translations(root.path(), &receipt).unwrap();
        let TranslationEnqueueOutcome::Enqueued { job: second } = &second[0] else {
            panic!("duplicate request was not replayed")
        };
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(first.created_at, second.created_at);
        let queue = TranslationQueue::open(root.path()).unwrap();
        assert_eq!(
            queue
                .connection
                .query_row("SELECT COUNT(*) FROM translation_jobs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let public = serde_json::to_string(&first).unwrap();
        assert!(!public.contains("careful synthetic report"));
        assert!(!public.contains("用于测试的中文标题译文"));
        assert!(!public.contains("prompt"));
        assert!(!public.contains("model"));
    }

    #[test]
    fn preservation_completed_zero_provider_replay_concurrent_and_gaps() {
        struct ForbiddenModel;
        impl TranslationModel for ForbiddenModel {
            // The existing worker reads local configuration before rejecting
            // stale authorized jobs; only inference is forbidden here.
            fn descriptor(&self) -> TranslationModelDescriptor { SyntheticModel::successful().descriptor() }
            fn translate(&mut self, _: TranslationModelInput<'_>) -> std::result::Result<String, TranslationModelError> {
                panic!("provider inference forbidden")
            }
        }
        for scenario in ["success", "authorized", "missing_model", "missing_record", "title_changed", "body_changed", "broken_chain", "foreign", "target_authorized_queued"] {
            let root = tempfile::tempdir().unwrap();
            let (receipt, queued) = enqueue_fixture(root.path(), scenario);
            let item = &receipt.item_ids[0];
            let previous = &receipt.revision_ids[0];
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            if scenario == "authorized" {
                queue.authorize(item, previous).unwrap();
            } else {
                queue.process_next(&mut SyntheticModel::successful()).unwrap();
            }
            let old_records = list_records(root.path()).unwrap();
            let (_, old) = memex_store::read_item(root.path(), item).unwrap();
            let mut edit = memex_store::revise_item_metadata(root.path(), item, memex_store::ItemMetadataUpdate {
                expected_revision_id: previous.clone(),
                title: if scenario == "title_changed" { "Changed title".into() } else { old.title.clone() },
                manual_tags: vec!["metadata-copy".into()],
            }).unwrap();
            if scenario == "body_changed" {
                edit = memex_store::edit_markdown(root.path(), memex_store::MarkdownEdit {
                    item_id: item.clone(), expected_revision_id: edit.revision_id.clone(),
                    request_id: "preservation-body-change".into(), markdown: format!("{} changed", old.content_markdown),
                }, Sensitivity::Internal).unwrap();
            }
            if scenario == "target_authorized_queued" {
                // A legacy placeholder (current enqueue reuses the inherited job instead).
                let (_, current) = memex_store::read_item(root.path(), item).unwrap();
                queue.enqueue_verified(item, &current, source_language(&current.title, &current.content_markdown)).unwrap();
                queue.authorize(item, &edit.revision_id).unwrap();
            }
            if scenario == "missing_model" {
                queue.connection.execute("UPDATE translation_jobs SET model_id=NULL WHERE job_id=?1", [&queued.job_id]).unwrap();
            }
            if scenario == "missing_record" {
                fs::remove_file(root.path().join("analysis/records").join(format!("{}.json", old_records[0].analysis_id))).unwrap();
            }
            if scenario == "broken_chain" {
                let path = root.path().join("items").join(item).join("revisions").join(format!("{previous}.json"));
                let mut altered = old.clone();
                altered.evidence_ids.clear();
                fs::write(path, serde_json::to_vec_pretty(&altered).unwrap()).unwrap();
            }
            let foreign = if scenario == "foreign" { Some(enqueue_fixture(root.path(), "other").0.revision_ids[0].clone()) } else { None };
            let result = preserve_completed_translation(root.path(), item, foreign.as_deref().unwrap_or(previous), &edit.revision_id);
            if scenario == "success" {
                let expected = result.unwrap();
                assert!(matches!(expected, TranslationPreservation::Preserved(_)));
                // Simulate loss of the terminal SQLite commit after immutable
                // records were published: concurrent restart must repair it.
                queue.connection.execute("DELETE FROM translation_jobs WHERE revision_id=?1", [&edit.revision_id]).unwrap();
                drop(queue);
                std::thread::scope(|scope| {
                    let handles: Vec<_> = (0..3).map(|_| scope.spawn(|| preserve_completed_translation(root.path(), item, previous, &edit.revision_id).unwrap())).collect();
                    for handle in handles { assert_eq!(handle.join().unwrap(), expected); }
                });
                queue = TranslationQueue::open(root.path()).unwrap();
                assert!(!queue.process_next(&mut ForbiddenModel).unwrap());
                assert!(!queue.process_next_authorized(&mut ForbiddenModel).unwrap());
                let pair = validated_translation_pair(root.path(), item, &edit.revision_id).unwrap().unwrap();
                assert_eq!(read_translation_projection(root.path(), item, Sensitivity::Internal, None).unwrap().state, "succeeded");
                let carried = crate::read_record(root.path(), &pair.document_analysis_id).unwrap();
                assert!(old_records.iter().any(|r| r.body_markdown == carried.body_markdown && r.created_at == carried.created_at && r.provenance.actor_id == carried.provenance.actor_id));
                for old in &old_records { assert_eq!(crate::read_record(root.path(), &old.analysis_id).unwrap(), *old); }
                assert_eq!(queue.connection.query_row("SELECT provider_authorized+attempt_count FROM translation_jobs WHERE revision_id=?1", [&edit.revision_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
            } else {
                assert!(!matches!(result, Ok(TranslationPreservation::Preserved(_))), "{scenario}");
                assert_eq!(queue.connection.query_row("SELECT COUNT(*) FROM translation_jobs WHERE revision_id=?1", [&edit.revision_id], |r| r.get::<_, i64>(0)).unwrap(), i64::from(scenario == "target_authorized_queued"));
                // The old authorized job is retained, not migrated or reset. A metadata-only
                // edit does not make it stale: it runs and the new revision inherits it.
                if scenario == "authorized" {
                    assert!(queue.process_next_authorized(&mut SyntheticModel::successful()).unwrap());
                    assert_eq!(read_translation_projection(root.path(), item, Sensitivity::Internal, None).unwrap().state, "succeeded");
                }
            }
        }
    }

    #[test]
    fn preservation_reconcile_placeholder_reopen_and_conflicts() {
        struct Forbidden;
        impl TranslationModel for Forbidden {
            fn descriptor(&self) -> TranslationModelDescriptor { panic!("no provider access") }
            fn translate(&mut self, _: TranslationModelInput<'_>) -> std::result::Result<String, TranslationModelError> {
                panic!("no inference")
            }
        }
        for mutation in ["", "provider_authorized=1", "translated_title='已有输出'",
            "translated_document='已有正文'", "state='running'", "attempt_count=1",
            "lease_owner='owner'", "lease_until=1", "batches_done=0", "batches_total=2",
            "title_analysis_id='analysis_existing'", "model_id='frozen'",
            "revision_sha256='wrong'", "job_id='wrong'", "source_language='fr'"] {
            let root = tempfile::tempdir().unwrap();
            let (receipt, _) = enqueue_fixture(root.path(), "reconcile-preservation");
            let item = &receipt.item_ids[0];
            let previous = &receipt.revision_ids[0];
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            queue.process_next(&mut SyntheticModel::successful()).unwrap();
            let records_before = list_records(root.path()).unwrap();
            let (_, old) = memex_store::read_item(root.path(), item).unwrap();
            // Deterministic startup order: metadata commits R1, reconciliation
            // creates its empty job, then preservation sees that committed row.
            let edit = memex_store::revise_item_metadata(root.path(), item, memex_store::ItemMetadataUpdate {
                expected_revision_id: previous.clone(), title: old.title.clone(),
                manual_tags: vec!["metadata-only".into()],
            }).unwrap();
            // A placeholder an earlier version's reconciler created for the metadata-only
            // revision (current enqueue reuses the inherited job instead).
            let (_, current) = memex_store::read_item(root.path(), item).unwrap();
            let job = queue.enqueue_verified(item, &current, source_language(&current.title, &current.content_markdown)).unwrap();
            let content_hash = content_digest(&old, &job.source_language);
            // Content-bound cache can belong to the old revision, not to the
            // empty target job. Preservation must neither reject nor delete it.
            queue.connection.execute("INSERT INTO translation_batches VALUES(?1,?2,'old-batch','[\"旧缓存\"]','2026-01-01T00:00:00Z')",
                params![item, content_hash]).unwrap();
            if !mutation.is_empty() {
                queue.connection.execute(&format!("UPDATE translation_jobs SET {mutation} WHERE job_id=?1"), [&job.job_id]).unwrap();
            }
            let before: String = queue.connection.query_row("SELECT json_object('state',state,'authorized',provider_authorized,'attempts',attempt_count,'title',translated_title,'body',translated_document,'hash',revision_sha256,'job',job_id,'source',source_language,'owner',lease_owner,'until',lease_until,'done',batches_done,'total',batches_total,'analysis',title_analysis_id,'model',model_id) FROM translation_jobs WHERE revision_id=?1", [&edit.revision_id], |r| r.get(0)).unwrap();
            let result = preserve_completed_translation(root.path(), item, previous, &edit.revision_id).unwrap();
            if mutation.is_empty() {
                assert!(matches!(result, TranslationPreservation::Preserved(_)), "{result:?}");
                drop(queue);
                queue = TranslationQueue::open(root.path()).unwrap();
                assert_eq!(preserve_completed_translation(root.path(), item, previous, &edit.revision_id).unwrap(), result);
                assert!(!queue.process_next(&mut Forbidden).unwrap());
                assert!(!queue.process_next_authorized(&mut Forbidden).unwrap());
                assert_eq!(read_translation_projection(root.path(), item, Sensitivity::Internal, None).unwrap().state, "succeeded");
                assert_eq!(queue.connection.query_row("SELECT provider_authorized+attempt_count FROM translation_jobs WHERE job_id=?1", [&job.job_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
            } else {
                assert_eq!(result, TranslationPreservation::Gap { reason: "target_job_conflict" }, "{mutation}");
                let after: String = queue.connection.query_row("SELECT json_object('state',state,'authorized',provider_authorized,'attempts',attempt_count,'title',translated_title,'body',translated_document,'hash',revision_sha256,'job',job_id,'source',source_language,'owner',lease_owner,'until',lease_until,'done',batches_done,'total',batches_total,'analysis',title_analysis_id,'model',model_id) FROM translation_jobs WHERE revision_id=?1", [&edit.revision_id], |r| r.get(0)).unwrap();
                assert_eq!(before, after, "conflicting job must stay untouched");
                assert_eq!(list_records(root.path()).unwrap(), records_before);
            }
            assert_eq!(queue.connection.query_row("SELECT outputs_json FROM translation_batches WHERE item_id=?1 AND content_sha256=?2 AND batch_key='old-batch'", params![item, content_hash], |r| r.get::<_, String>(0)).unwrap(), "[\"旧缓存\"]");
            for old in records_before { assert_eq!(crate::read_record(root.path(), &old.analysis_id).unwrap(), old); }
        }
    }

    #[test]
    fn title_edits_inherit_body_translation_and_body_edits_invalidate_it() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, _) = enqueue_fixture(root.path(), "retitle");
        let item = &receipt.item_ids[0];
        let first = &receipt.revision_ids[0];
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        let pair = validated_translation_pair(root.path(), item, first).unwrap().unwrap();
        // Two owner title edits, like the reported item (revisions 3 -> 4 -> 5).
        let mut revision = first.clone();
        for title in ["Episode 7 renamed", "第七期：改过两次的标题"] {
            revision = memex_store::revise_item_metadata(
                root.path(),
                item,
                memex_store::ItemMetadataUpdate {
                    expected_revision_id: revision.clone(),
                    title: title.into(),
                    manual_tags: vec![],
                },
            )
            .unwrap()
            .revision_id;
        }
        // Reads of the newest revision see the first revision's validated pair.
        assert_eq!(
            validated_translation_pair(root.path(), item, &revision).unwrap(),
            Some(pair.clone())
        );
        let status = translation_status(root.path(), item, &revision).unwrap().unwrap();
        assert_eq!(status.state, TranslationJobState::Succeeded);
        let projection =
            read_translation_projection(root.path(), item, Sensitivity::Internal, Some((&revision, 0)))
                .unwrap();
        assert_eq!(projection.state, "succeeded");
        assert_eq!(projection.revision_id, revision);
        assert_eq!(projection.source_revision_id.as_deref(), Some(first.as_str()));
        let page = projection.page.unwrap();
        assert!(page.content_markdown.contains("测试文档译文"));
        // The old title's translation is not shown for the new (already Chinese) title.
        assert_eq!(projection.translated_title.as_deref(), Some("第七期：改过两次的标题"));
        assert_eq!(page.title, "第七期：改过两次的标题");
        // A new request queues nothing and sends nothing.
        let TranslationEnqueueOutcome::Enqueued { job } =
            enqueue_item_translation(root.path(), item, &revision).unwrap()
        else {
            panic!("expected the inherited job")
        };
        assert_eq!(job.state, TranslationJobState::Succeeded);
        let rows: i64 = queue
            .connection
            .query_row("SELECT COUNT(*) FROM translation_jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        // A duplicate queued job an earlier version created is settled without a request.
        let (_, current) = memex_store::read_item(root.path(), item).unwrap();
        let duplicate = queue
            .enqueue_verified(item, &current, &job.source_language)
            .unwrap();
        queue.authorize(item, &revision).unwrap();
        assert!(queue.process_next_authorized(&mut model).unwrap());
        assert_eq!(model.calls.len(), 2, "no provider request for the duplicate");
        let settled = queue.get(&duplicate.job_id).unwrap();
        assert_eq!(settled.reason_code.as_deref(), Some(INHERITED));
        assert!(!settled.retryable);
        assert_eq!(
            translation_status(root.path(), item, &revision).unwrap().unwrap().state,
            TranslationJobState::Succeeded
        );
        // A body edit invalidates the inherited translation.
        let edited = memex_store::edit_markdown(
            root.path(),
            memex_store::MarkdownEdit {
                item_id: item.clone(),
                expected_revision_id: revision.clone(),
                request_id: "retitle-body-change".into(),
                markdown: format!("{} changed", current.content_markdown),
            },
            Sensitivity::Internal,
        )
        .unwrap();
        assert_eq!(
            validated_translation_pair(root.path(), item, &edited.revision_id).unwrap(),
            None
        );
        assert!(translation_status(root.path(), item, &edited.revision_id)
            .unwrap()
            .is_none());
        assert_eq!(
            read_translation_projection(root.path(), item, Sensitivity::Internal, None)
                .unwrap()
                .state,
            "not_requested"
        );
    }

    #[test]
    fn running_translation_survives_a_title_edit_and_is_inherited() {
        struct RenameDuring<'a> {
            inner: SyntheticModel,
            root: &'a Path,
            item: &'a str,
        }
        impl TranslationModel for RenameDuring<'_> {
            fn descriptor(&self) -> TranslationModelDescriptor {
                self.inner.descriptor()
            }
            fn translate(
                &mut self,
                mut input: TranslationModelInput<'_>,
            ) -> std::result::Result<String, TranslationModelError> {
                if input.purpose() == GeneratedTextPurpose::DocumentTranslation {
                    assert!(input.progress().unwrap().heartbeat(1, 3));
                    let (_, current) = memex_store::read_item(self.root, self.item).unwrap();
                    let source = current.revision_id.clone();
                    // A metadata-only revision with its own queued placeholder must
                    // still show the older job's live progress, not "queued".
                    let edited = memex_store::revise_item_metadata(
                        self.root,
                        self.item,
                        memex_store::ItemMetadataUpdate {
                            expected_revision_id: current.revision_id,
                            title: current.title,
                            manual_tags: vec!["metadata-only".into()],
                        },
                    ).unwrap();
                    enqueue_item_translation(self.root, self.item, &edited.revision_id).unwrap();
                    let running = read_translation_projection(self.root, self.item, Sensitivity::Internal, None).unwrap();
                    assert_eq!(running.state, "running");
                    assert_eq!(running.revision_id, edited.revision_id);
                    assert_eq!(running.source_revision_id.as_deref(), Some(source.as_str()));
                    assert_eq!(running.translated_title.as_deref(), Some("用于测试的中文标题译文"));
                    let progress = running.progress.unwrap();
                    assert_eq!((progress.done_batches, progress.total_batches), (1, 3));
                    assert!(progress.started_at.is_some());
                    let partial = translation_partial(self.root, self.item, &edited.revision_id).unwrap();
                    assert_eq!(partial.1, Some(progress.clone()));
                    memex_store::revise_item_metadata(
                        self.root,
                        self.item,
                        memex_store::ItemMetadataUpdate {
                            expected_revision_id: edited.revision_id,
                            title: "Renamed while translating".into(),
                            manual_tags: vec![],
                        },
                    )
                    .unwrap();
                    let renamed = read_translation_projection(self.root, self.item, Sensitivity::Internal, None).unwrap();
                    assert_eq!(renamed.state, "running");
                    assert_eq!(renamed.progress, Some(progress));
                    assert_eq!(renamed.translated_title, None, "the old title does not translate the renamed title");
                }
                self.inner.translate(input)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (receipt, queued) = enqueue_fixture(root.path(), "rename-during");
        let item = &receipt.item_ids[0];
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = RenameDuring {
            inner: SyntheticModel::successful(),
            root: root.path(),
            item,
        };
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(queue.get(&queued.job_id).unwrap().state, TranslationJobState::Succeeded);
        let (_, current) = memex_store::read_item(root.path(), item).unwrap();
        assert_ne!(current.revision_id, receipt.revision_ids[0]);
        let projection =
            read_translation_projection(root.path(), item, Sensitivity::Internal, None).unwrap();
        assert_eq!(projection.state, "succeeded");
        // The old title's translation is withheld until the new title has its own.
        assert_eq!(projection.translated_title, None);
        let mut titles = crate::TitleTranslationQueue::open(root.path()).unwrap();
        assert!(titles.request_capture(item, &current.revision_id).unwrap());
    }

    #[test]
    fn success_applies_one_revision_bound_title_and_document_pair() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, queued) = enqueue_fixture(root.path(), "success");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        assert!(!queue.process_next(&mut model).unwrap());
        assert_eq!(
            model.calls,
            vec![
                GeneratedTextPurpose::TitleTranslation,
                GeneratedTextPurpose::DocumentTranslation
            ]
        );
        let status = queue.get(&queued.job_id).unwrap();
        assert_eq!(status.state, TranslationJobState::Succeeded);
        assert!(!status.retryable);
        let view = crate::translated_title(
            root.path(),
            &receipt.item_ids[0],
            &receipt.revision_ids[0],
            "A careful synthetic report about the test",
        )
        .unwrap()
        .expect("completed contextual title must remain displayable");
        assert_eq!(view.title_source, crate::TITLE_SOURCE_WITH_CONTEXT);
        assert_eq!(view.translated_title, "用于测试的中文标题译文");
        assert_eq!(
            crate::title_backfill_candidates(
                root.path(),
                &[(
                    &receipt.item_ids[0],
                    &receipt.revision_ids[0],
                    "A careful synthetic report about the test"
                )],
            )
            .unwrap(),
            [false]
        );

        let pair =
            validated_translation_pair(root.path(), &receipt.item_ids[0], &receipt.revision_ids[0])
                .unwrap()
                .unwrap();
        assert_ne!(pair.title_analysis_id, pair.document_analysis_id);
        let records = list_records(root.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records
            .iter()
            .any(|record| record.analysis_id == pair.title_analysis_id));
        assert!(records
            .iter()
            .any(|record| record.analysis_id == pair.document_analysis_id));
        assert_eq!(
            records
                .iter()
                .filter(|record| {
                    record.generated_text.as_ref().unwrap().purpose
                        == GeneratedTextPurpose::TitleTranslation
                })
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| {
                    record.generated_text.as_ref().unwrap().purpose
                        == GeneratedTextPurpose::DocumentTranslation
                })
                .count(),
            1
        );
        for record in records {
            assert_eq!(record.inputs[0].item_id, receipt.item_ids[0]);
            assert_eq!(record.inputs[0].revision_id, receipt.revision_ids[0]);
            let generated = record.generated_text.unwrap();
            assert_eq!(generated.source_language, "en");
            assert_eq!(generated.target_language, TARGET_LANGUAGE);
            assert_eq!(
                generated.model_sha256,
                Some(format!("sha256:{}", "7".repeat(64)))
            );
            assert!(valid_sha256(&generated.prompt_sha256));
            assert!(valid_sha256(&record.provenance.config_digest));
        }
        let frozen_content = queue
            .connection
            .query_row(
                "SELECT translated_title,translated_document FROM translation_jobs WHERE job_id=?1",
                [&queued.job_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(frozen_content, (None, None));

        struct SyntheticHighlights;
        impl crate::InsightModel for SyntheticHighlights {
            fn descriptor(&self) -> TranslationModelDescriptor {
                SyntheticModel::successful().descriptor()
            }
            fn generate(
                &mut self,
                input: crate::InsightModelInput<'_>,
            ) -> std::result::Result<crate::InsightOutput, TranslationModelError> {
                assert!(input.translation().is_some());
                Ok(crate::InsightOutput {
                    items: serde_json::json!([]),
                    dropped: 0,
                    translation_used: true,
                })
            }
        }
        let item = &receipt.item_ids[0];
        let (_, previous) = memex_store::read_item(root.path(), item).unwrap();
        let mut highlights = crate::InsightQueue::open(root.path()).unwrap();
        highlights
            .request(&previous, crate::InsightKind::Highlights)
            .unwrap();
        assert!(highlights
            .process_next(crate::InsightKind::Highlights, &mut SyntheticHighlights)
            .unwrap());
        let before =
            crate::insight_view(root.path(), &previous, crate::InsightKind::Highlights).unwrap();
        assert_eq!(before.sides.as_deref(), Some("original_and_translation"));
        let edit = memex_store::revise_item_metadata(
            root.path(),
            item,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: previous.revision_id.clone(),
                title: previous.title,
                manual_tags: vec!["metadata-only-copy".into()],
            },
        )
        .unwrap();
        // The metadata-only revision inherits the pair by body identity: carry finds nothing
        // to copy, no job is queued and no provider work happens.
        carry_translation(
            root.path(),
            item,
            &edit.previous_revision_id,
            &edit.revision_id,
        )
        .unwrap();
        assert!(!queue.process_next(&mut model).unwrap());
        assert_eq!(model.calls.len(), 2, "no provider work for a metadata-only revision");
        let (_, current) = memex_store::read_item(root.path(), item).unwrap();
        let inherited_pair = validated_translation_pair(root.path(), item, &current.revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(inherited_pair, pair);
        highlights
            .carry(&edit.previous_revision_id, &current)
            .unwrap();
        assert_eq!(
            crate::insight_view(root.path(), &current, crate::InsightKind::Highlights).unwrap(),
            before
        );
        highlights
            .request(&current, crate::InsightKind::Highlights)
            .unwrap();
        assert!(
            highlights.queued_kinds().unwrap().is_empty(),
            "identical translated text is not paid for again"
        );

    }

    #[test]
    fn expired_running_lease_is_recovered_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "restart");
        {
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            let leased = queue.lease(false).unwrap().unwrap();
            assert_eq!(leased.view.state, TranslationJobState::Running);
            queue
                .connection
                .execute(
                    "UPDATE translation_jobs SET lease_until=0 WHERE job_id=?1",
                    [&queued.job_id],
                )
                .unwrap();
        }
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        let status = queue.get(&queued.job_id).unwrap();
        assert_eq!(status.state, TranslationJobState::Succeeded);
        assert_eq!(status.attempt_count, 2);
    }

    #[test]
    fn expired_incomplete_third_attempt_is_exhausted_without_another_model_call() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "restart-exhausted");
        {
            let queue = TranslationQueue::open(root.path()).unwrap();
            queue
                .connection
                .execute(
                    "UPDATE translation_jobs
                        SET state='running',attempt_count=?2,lease_owner='expired-test-worker',
                            lease_until=0
                      WHERE job_id=?1",
                    params![queued.job_id, i64::from(MAX_ATTEMPTS)],
                )
                .unwrap();
        }
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(!queue.process_next(&mut model).unwrap());
        assert!(model.calls.is_empty());
        let status = queue.get(&queued.job_id).unwrap();
        assert_eq!(status.state, TranslationJobState::Failed);
        assert_eq!(status.reason_code.as_deref(), Some("retry_exhausted"));
        assert_eq!(status.attempt_count, MAX_ATTEMPTS);
        assert!(!status.retryable);
    }

    #[test]
    fn retry_reuses_frozen_title_instead_of_invoking_it_twice() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "frozen-retry");
        {
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            let mut first = SyntheticModel::scripted([
                Ok("用于测试的中文标题译文".into()),
                Err(TranslationModelError::ProviderFailure),
            ]);
            assert!(queue.process_next(&mut first).unwrap());
            assert_eq!(
                first.calls,
                vec![
                    GeneratedTextPurpose::TitleTranslation,
                    GeneratedTextPurpose::DocumentTranslation
                ]
            );
            let failed = queue.get(&queued.job_id).unwrap();
            assert_eq!(failed.reason_code.as_deref(), Some("provider_failure"));
            assert!(failed.retryable);
            queue.retry(&queued.job_id).unwrap();
        }
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut second = SyntheticModel::successful();
        assert!(queue.process_next(&mut second).unwrap());
        assert_eq!(
            second.calls,
            vec![GeneratedTextPurpose::DocumentTranslation]
        );
        assert_eq!(
            queue.get(&queued.job_id).unwrap().state,
            TranslationJobState::Succeeded
        );
    }

    #[test]
    fn restart_after_analysis_application_replays_the_same_frozen_pair() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "application-replay");
        {
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            let descriptor = SyntheticModel::successful().descriptor();
            let mut job = queue.lease(false).unwrap().unwrap();
            queue
                .freeze_output(&job, &descriptor, true, "用于测试的中文标题译文")
                .unwrap();
            job = queue.record(&queued.job_id).unwrap();
            queue
                .freeze_output(
                    &job,
                    &descriptor,
                    false,
                    "# 测试文档译文\n\n这是用于重放验证的中文译文。",
                )
                .unwrap();
            job = queue.record(&queued.job_id).unwrap();
            let title = translation_record(
                root.path(),
                &job,
                job.model.as_ref().unwrap(),
                GeneratedTextPurpose::TitleTranslation,
                job.translated_title.as_deref().unwrap(),
            )
            .unwrap();
            let document = translation_record(
                root.path(),
                &job,
                job.model.as_ref().unwrap(),
                GeneratedTextPurpose::DocumentTranslation,
                job.translated_document.as_deref().unwrap(),
            )
            .unwrap();
            apply_record(root.path(), &title).unwrap();
            apply_record(root.path(), &document).unwrap();
            queue
                .connection
                .execute(
                    "UPDATE translation_jobs
                        SET attempt_count=?2,lease_until=0 WHERE job_id=?1",
                    params![queued.job_id, i64::from(MAX_ATTEMPTS)],
                )
                .unwrap();
        }
        let first_ids = list_records(root.path())
            .unwrap()
            .into_iter()
            .map(|record| record.analysis_id)
            .collect::<Vec<_>>();
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        assert!(model.calls.is_empty());
        assert_eq!(
            queue.get(&queued.job_id).unwrap().state,
            TranslationJobState::Succeeded
        );
        assert_eq!(
            queue.get(&queued.job_id).unwrap().attempt_count,
            MAX_ATTEMPTS
        );
        let second_ids = list_records(root.path())
            .unwrap()
            .into_iter()
            .map(|record| record.analysis_id)
            .collect::<Vec<_>>();
        assert_eq!(first_ids, second_ids);
    }

    #[cfg(unix)]
    #[test]
    fn index_projection_failure_keeps_job_running_and_frozen_pair_replayable() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let (receipt, queued) = enqueue_fixture(root.path(), "index-replay");
        let sentinel = root.path().join("sentinel");
        fs::write(&sentinel, b"sentinel").unwrap();
        symlink(&sentinel, root.path().join("analysis/index.json")).unwrap();

        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut first = SyntheticModel::successful();
        assert_eq!(
            queue.process_next(&mut first).unwrap_err().to_string(),
            "translation_analysis_index_pending"
        );
        assert_eq!(
            queue.get(&queued.job_id).unwrap().state,
            TranslationJobState::Running
        );
        assert!(validated_translation_pair(
            root.path(),
            &receipt.item_ids[0],
            &receipt.revision_ids[0],
        )
        .unwrap()
        .is_none());

        fs::remove_file(root.path().join("analysis/index.json")).unwrap();
        queue
            .connection
            .execute(
                "UPDATE translation_jobs SET lease_until=0 WHERE job_id=?1",
                [&queued.job_id],
            )
            .unwrap();
        let mut second = SyntheticModel::successful();
        assert!(queue.process_next(&mut second).unwrap());
        assert!(second.calls.is_empty());
        assert_eq!(
            queue.get(&queued.job_id).unwrap().state,
            TranslationJobState::Succeeded
        );
        assert!(validated_translation_pair(
            root.path(),
            &receipt.item_ids[0],
            &receipt.revision_ids[0],
        )
        .unwrap()
        .is_some());
        assert_eq!(fs::read(sentinel).unwrap(), b"sentinel");
    }

    #[test]
    fn provider_failure_and_timeout_are_bounded_retryable_states() {
        for (index, (error, reason)) in [
            (TranslationModelError::ProviderFailure, "provider_failure"),
            (TranslationModelError::Timeout, "provider_timeout"),
        ]
        .into_iter()
        .enumerate()
        {
            let root = tempfile::tempdir().unwrap();
            let (_, queued) = enqueue_fixture(root.path(), &format!("provider-{index}"));
            let mut queue = TranslationQueue::open(root.path()).unwrap();
            let mut model = SyntheticModel::scripted([Err(error)]);
            assert!(queue.process_next(&mut model).unwrap());
            let failed = queue.get(&queued.job_id).unwrap();
            assert_eq!(failed.state, TranslationJobState::Failed);
            assert_eq!(failed.reason_code.as_deref(), Some(reason));
            assert!(failed.retryable);
            assert!(list_records(root.path()).unwrap().is_empty());
        }
    }

    #[test]
    fn automatic_retry_backoff_survives_restart_selects_only_due_work_and_does_not_spin() {
        let root = tempfile::tempdir().unwrap();
        let (_, first) = enqueue_fixture(root.path(), "automatic-retry-first");
        let (_, second) = enqueue_fixture(root.path(), "automatic-retry-second");
        let failed_at = (Utc::now() - chrono::Duration::seconds(61)).to_rfc3339();
        {
            let queue = TranslationQueue::open(root.path()).unwrap();
            queue
                .connection
                .execute(
                    "UPDATE translation_jobs
                        SET state='failed',reason_code='provider_timeout',attempt_count=2,
                            updated_at=?2
                      WHERE job_id=?1",
                    params![first.job_id, failed_at],
                )
                .unwrap();
            queue
                .connection
                .execute(
                    "UPDATE translation_jobs
                        SET state='failed',reason_code='provider_failure',attempt_count=1,
                            updated_at=?2
                      WHERE job_id=?1",
                    params![second.job_id, failed_at],
                )
                .unwrap();
        }

        let mut restarted = TranslationQueue::open(root.path()).unwrap();
        assert!(restarted
            .retry_one_eligible(Duration::from_secs(60))
            .unwrap());
        assert_eq!(
            restarted.get(&second.job_id).unwrap().state,
            TranslationJobState::Queued
        );
        let not_yet_due = restarted.get(&first.job_id).unwrap();
        assert_eq!(not_yet_due.state, TranslationJobState::Failed);
        assert_eq!(not_yet_due.attempt_count, 2);
        let unchanged = not_yet_due.updated_at.clone();
        for _ in 0..3 {
            assert!(!restarted
                .retry_one_eligible(Duration::from_secs(60))
                .unwrap());
        }
        assert_eq!(restarted.get(&first.job_id).unwrap().updated_at, unchanged);
        assert!(restarted
            .retry_one_eligible(Duration::from_secs(61))
            .is_err());
        assert!(restarted
            .retry_one_eligible(Duration::from_micros(1_500))
            .is_err());
    }

    #[test]
    fn automatic_retry_exhausts_the_persisted_attempt_budget() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "automatic-retry-exhausted");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::scripted([
            Err(TranslationModelError::ProviderFailure),
            Err(TranslationModelError::Timeout),
            Err(TranslationModelError::ProviderFailure),
        ]);

        for attempt in 1..=MAX_ATTEMPTS {
            assert!(queue.process_next(&mut model).unwrap());
            let status = queue.get(&queued.job_id).unwrap();
            assert_eq!(status.attempt_count, attempt);
            if attempt < MAX_ATTEMPTS {
                assert!(status.retryable);
                assert!(queue.retry_one_eligible(Duration::ZERO).unwrap());
            }
        }
        let exhausted = queue.get(&queued.job_id).unwrap();
        assert_eq!(exhausted.state, TranslationJobState::Failed);
        assert_eq!(exhausted.reason_code.as_deref(), Some("retry_exhausted"));
        assert!(!exhausted.retryable);
        assert!(!queue.retry_one_eligible(Duration::ZERO).unwrap());
        assert_eq!(model.calls.len(), MAX_ATTEMPTS as usize);
    }

    #[test]
    fn fractional_millisecond_timeout_is_rejected_before_provider_invocation() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "fractional-timeout");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = FractionalTimeoutModel { calls: 0 };

        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(model.calls, 0);
        let failed = queue.get(&queued.job_id).unwrap();
        assert_eq!(failed.state, TranslationJobState::Failed);
        assert_eq!(
            failed.reason_code.as_deref(),
            Some("invalid_provider_configuration")
        );
        assert!(!failed.retryable);
    }

    #[test]
    fn a_title_returned_unchanged_no_longer_fails_the_whole_translation() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_article(root.path(), "kept-title", "Stratechery", english_document("kept-title"));
        let TranslationEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_translations(root.path(), &receipt).unwrap().into_iter().next().unwrap()
        else {
            panic!("synthetic English article was not enqueued")
        };
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::scripted([Ok("Stratechery".into())]);
        while queue.process_next(&mut model).unwrap() {}
        let done = queue.get(&job.job_id).unwrap();
        assert_eq!(done.state, TranslationJobState::Succeeded, "{:?}", done.reason_code);
        assert_eq!(
            model.calls,
            [GeneratedTextPurpose::TitleTranslation, GeneratedTextPurpose::DocumentTranslation]
        );
        let pair = validated_translation_pair(root.path(), &job.item_id, &job.revision_id)
            .unwrap()
            .expect("kept title is a readable translation");
        assert_eq!(super::super::read_record(root.path(), &pair.title_analysis_id).unwrap().body_markdown, "Stratechery");
    }

    #[test]
    fn malformed_output_is_reasked_and_retried_with_a_bound_and_never_applied() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "malformed");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        // Every answer is malformed. The title is re-asked once, then keeps its original
        // wording (frozen once, flagged); the document is re-asked once per attempt, retried
        // automatically until MAX_ATTEMPTS, and nothing is ever applied.
        let mut model = SyntheticModel::scripted(
            (0..2 + 2 * MAX_ATTEMPTS).map(|_| Ok("plain untranslated output".into())),
        );
        assert!(queue.process_next(&mut model).unwrap());
        let failed = queue.get(&queued.job_id).unwrap();
        assert_eq!(failed.state, TranslationJobState::Failed);
        assert_eq!(failed.reason_code.as_deref(), Some("malformed_model_output"));
        assert!(failed.retryable);
        assert_eq!(model.calls.len(), 4, "one same-call re-ask each for title and document");
        // The failure names its step and check, with numbers only.
        let (detail, context, kept): (Option<String>, Option<String>, Option<i64>) = queue
            .connection
            .query_row(
                "SELECT failure_detail,failure_context,title_untranslated FROM translation_jobs
                  WHERE job_id=?1",
                [&queued.job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(detail.as_deref(), Some("document:cjk_ratio"));
        let context: TranslationFailureContext =
            serde_json::from_str(context.as_deref().unwrap()).unwrap();
        assert_eq!(context.output_chars, Some(25));
        assert!(context.elapsed_ms.is_some());
        assert_eq!(kept, Some(1));
        let projection = read_translation_projection(
            root.path(),
            &queued.item_id,
            Sensitivity::Restricted,
            None,
        )
        .unwrap();
        assert_eq!(projection.failure_detail.as_deref(), Some("document:cjk_ratio"));
        assert!(!serde_json::to_string(&projection).unwrap().contains("plain untranslated"));
        while queue.retry_one_eligible(Duration::ZERO).unwrap() {
            assert!(queue.process_next(&mut model).unwrap());
        }
        let exhausted = queue.get(&queued.job_id).unwrap();
        assert_eq!(exhausted.state, TranslationJobState::Failed);
        assert_eq!(exhausted.reason_code.as_deref(), Some("retry_exhausted"));
        assert_eq!(exhausted.attempt_count, MAX_ATTEMPTS);
        assert!(!exhausted.retryable);
        assert_eq!(model.calls.len(), 2 + 2 * MAX_ATTEMPTS as usize);
        assert!(list_records(root.path()).unwrap().is_empty());
    }

    #[test]
    fn adapter_failure_detail_is_stored_and_reported_without_text() {
        struct Detailed;
        impl TranslationModel for Detailed {
            fn descriptor(&self) -> TranslationModelDescriptor {
                SyntheticModel::successful().descriptor()
            }
            fn translate(
                &mut self,
                mut input: TranslationModelInput<'_>,
            ) -> std::result::Result<String, TranslationModelError> {
                if input.purpose() == GeneratedTextPurpose::TitleTranslation {
                    return Ok("用于测试的中文标题译文".into());
                }
                let mut failure = TranslationFailure::new("batch_3_of_5:count_mismatch");
                failure.context.expected_blocks = Some(12);
                failure.context.actual_blocks = Some(11);
                failure.context.output_tokens = Some(9000);
                input.progress().unwrap().failure(failure);
                Err(TranslationModelError::MalformedOutput)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "detail");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        assert!(queue.process_next(&mut Detailed).unwrap());
        let projection = read_translation_projection(
            root.path(),
            &queued.item_id,
            Sensitivity::Restricted,
            None,
        )
        .unwrap();
        assert_eq!(projection.state, "failed");
        assert_eq!(projection.failure_detail.as_deref(), Some("batch_3_of_5:count_mismatch"));
        let context = projection.failure_context.unwrap();
        assert_eq!((context.expected_blocks, context.actual_blocks), (Some(12), Some(11)));
        assert_eq!(context.output_tokens, Some(9000));
        // A new attempt clears it; an invalid code is never stored.
        assert!(valid_failure_detail("batch_2:cjk_ratio") && valid_failure_detail("provider:empty"));
        assert!(!valid_failure_detail("batch_x:cjk") && !valid_failure_detail("title:Some Text"));
        assert!(queue.requeue_failed(&queued.item_id, &queued.revision_id).unwrap());
        assert!(queue.process_next(&mut SyntheticModel::successful()).unwrap());
        let detail: Option<String> = queue
            .connection
            .query_row(
                "SELECT failure_detail FROM translation_jobs WHERE job_id=?1",
                [&queued.job_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(detail, None);
    }

    #[test]
    fn a_document_with_marked_untranslated_blocks_succeeds_partially_and_still_verifies() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_article(
            root.path(),
            "partial",
            "A careful synthetic report about the test",
            english_document("partial"),
        );
        let TranslationEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_translations(root.path(), &receipt).unwrap().into_iter().next().unwrap()
        else {
            panic!("not enqueued")
        };
        let document = english_document("partial");
        let (heading, body) = document.split_once("\n\n").unwrap();
        // Heading translated, the one long paragraph too; an extra marked copy of the short
        // heading block stands for a block kept in the original.
        let partial = format!(
            "# 关于合成部分案例的审慎报告\n\n{}\n\n这是测试用的合成源文章。报告为测试而写，文章对结果有更多细节，结果由有界夹具而非私人内容核对。本报告包含来源中的事实，为确定性验证而准备。",
            untranslated_block(heading)
        );
        assert_eq!(untranslated_block_count(&partial), 1);
        assert_eq!(
            translation_output_failure(&partial, &document, GeneratedTextPurpose::DocumentTranslation),
            None
        );
        // A marked block that is not source text, or most of the source kept, is rejected.
        assert_eq!(
            translation_output_failure(
                &format!("{}\n\n中文段落。", untranslated_block("Invented English text not in the source at all.")),
                &document,
                GeneratedTextPurpose::DocumentTranslation
            ),
            Some("invalid_untranslated_mark")
        );
        assert_eq!(
            translation_output_failure(
                &format!("# 中文标题\n\n{}", untranslated_block(body)),
                &document,
                GeneratedTextPurpose::DocumentTranslation
            ),
            Some("too_many_untranslated")
        );
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::scripted([Ok("用于测试的中文标题译文".into()), Ok(partial)]);
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(queue.get(&job.job_id).unwrap().state, TranslationJobState::Succeeded);
        // The read-time validation (prompt digests, record bytes) accepts the marked pair.
        assert!(validated_translation_pair(root.path(), &job.item_id, &job.revision_id)
            .unwrap()
            .is_some());
        let projection = read_translation_projection(
            root.path(),
            &job.item_id,
            Sensitivity::Restricted,
            None,
        )
        .unwrap();
        assert_eq!(projection.state, "succeeded");
        assert_eq!(projection.untranslated_blocks, Some(1));
        assert_eq!(projection.title_untranslated, Some(false));
    }

    #[test]
    fn a_podcast_block_with_many_english_names_is_a_valid_translation() {
        let source = "Speaker 2: We talked with Andrej Karpathy, Jensen Huang and the OpenAI, Anthropic and Google DeepMind teams about CUDA, PyTorch and JAX.";
        let translated = "Speaker 2：这期节目我们和 Andrej Karpathy、Jensen Huang 以及 OpenAI、Anthropic、Google DeepMind 的团队一起，聊了聊 CUDA、PyTorch 和 JAX 这些工具的来龙去脉。";
        assert_eq!(translated_block_failure(translated, source), None);
        assert_eq!(translated_block_failure(source, source), Some("block_identical"));
        assert_eq!(
            translated_block_failure("Speaker 2: We talked with Andrej Karpathy 和 Jensen Huang about CUDA.", source),
            Some("cjk_ratio")
        );
        // Short labels and names are kept as written.
        assert_eq!(translated_block_failure("Jensen Huang:", "Jensen Huang:"), None);
        // Document level: the old bound (Han ≥ all other letters) rejected this; Han ≥ 1/4
        // of the other letters accepts it.
        let document = [source; 8].join("\n\n");
        let output = [translated; 8].join("\n\n");
        assert_eq!(
            translation_output_failure(&output, &document, GeneratedTextPurpose::DocumentTranslation),
            None
        );
        // A title translated with its proper names kept.
        assert_eq!(
            translation_output_failure("Orbs 与 Jellyware", "Orbs and Jellyware", GeneratedTextPurpose::TitleTranslation),
            None
        );
    }

    #[test]
    fn one_malformed_answer_is_reasked_in_the_same_attempt_and_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "reask");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::scripted([Ok("plain untranslated output".into())]);
        assert!(queue.process_next(&mut model).unwrap());
        let done = queue.get(&queued.job_id).unwrap();
        assert_eq!(done.state, TranslationJobState::Succeeded, "{:?}", done.reason_code);
        assert_eq!(done.attempt_count, 1);
    }

    #[test]
    fn stale_revision_digest_fails_before_provider_invocation_or_application() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "stale");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        queue
            .connection
            .execute(
                "UPDATE translation_jobs SET revision_sha256=?2 WHERE job_id=?1",
                params![queued.job_id, format!("sha256:{}", "0".repeat(64))],
            )
            .unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        assert!(model.calls.is_empty());
        let failed = queue.get(&queued.job_id).unwrap();
        assert_eq!(failed.reason_code.as_deref(), Some("stale_revision"));
        assert!(!failed.retryable);
        assert!(list_records(root.path()).unwrap().is_empty());
    }

    #[test]
    fn record_preparation_rejects_a_revision_that_no_longer_matches_the_job() {
        let root = tempfile::tempdir().unwrap();
        let (_, queued) = enqueue_fixture(root.path(), "prepare-race");
        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut job = queue.lease(false).unwrap().unwrap();
        job.view.revision_id = format!("revision_{}", "0".repeat(32));
        let descriptor = SyntheticModel::successful().descriptor();

        assert_eq!(
            translation_record(
                root.path(),
                &job,
                &descriptor,
                GeneratedTextPurpose::TitleTranslation,
                "用于测试的中文标题译文",
            )
            .unwrap_err()
            .to_string(),
            "translation record revision changed"
        );
        assert!(list_records(root.path()).unwrap().is_empty());
        assert_eq!(
            queue.get(&queued.job_id).unwrap().state,
            TranslationJobState::Running
        );
    }

    #[test]
    fn competing_translation_record_fails_closed_without_adding_another_pair() {
        let root = tempfile::tempdir().unwrap();
        let (receipt, queued) = enqueue_fixture(root.path(), "conflict");
        let (prompt_id, _, prompt_sha256) =
            generated_text_prompt(GeneratedTextPurpose::TitleTranslation);
        let competing = prepare_record(
            root.path(),
            &AnalysisRequest {
                schema_version: SCHEMA_VERSION,
                analysis_kind: AnalysisKind::Translation,
                title: "Competing title translation".into(),
                body_markdown: "既有中文标题".into(),
                item_ids: receipt.item_ids.clone(),
                topics: vec![],
                requested_sensitivity: None,
                created_at: "2026-08-11T00:01:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "synthetic.competing".into(),
                tool_id: "synthetic.competing".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "8".repeat(64)),
                generated_text: Some(GeneratedTextDescriptor {
                    purpose: GeneratedTextPurpose::TitleTranslation,
                    source_language: "en".into(),
                    target_language: "zh-Hans".into(),
                    output_limit: Some(TextOutputLimit {
                        unit: TextLengthUnit::Characters,
                        maximum: MAX_TITLE_CHARACTERS as u32,
                    }),
                    summary_job_id: None,
                    translation_job_id: None,
                    model_id: "synthetic-competing-v1".into(),
                    model_sha256: Some(format!("sha256:{}", "8".repeat(64))),
                    prompt_id: prompt_id.into(),
                    prompt_version: GENERATED_TEXT_PROMPT_VERSION.into(),
                    prompt_sha256,
                }),
            },
        )
        .unwrap();
        apply_record(root.path(), &competing).unwrap();

        let mut queue = TranslationQueue::open(root.path()).unwrap();
        let mut model = SyntheticModel::successful();
        assert!(queue.process_next(&mut model).unwrap());
        let failed = queue.get(&queued.job_id).unwrap();
        assert_eq!(failed.reason_code.as_deref(), Some("analysis_conflict"));
        assert!(!failed.retryable);
        let records = list_records(root.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].analysis_id, competing.analysis_id);
    }
}
