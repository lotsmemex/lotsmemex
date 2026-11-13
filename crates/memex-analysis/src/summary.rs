use super::{
    ai_minutes_prompt, generated_text_prompt, list_records, prepare_record, revision_ref,
    ActorKind, AnalysisKind, AnalysisRequest, GeneratedTextDescriptor, GeneratedTextPurpose,
    TextLengthUnit, TextOutputLimit, TranslationModelDescriptor, AI_MINUTES_PROMPT_VERSION,
    GENERATED_TEXT_PROMPT_VERSION,
};
use crate::translation::{
    classify_source, has_column, model_error_reason, open_status_database, secure_database_family,
    source_language, valid_source_language, validate_database_family, SourceClassification,
    TranslationModelError, TranslationQueue, DATABASE_NAME,
};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use memex_core::{IngestReceipt, ItemRevision, SCHEMA_VERSION};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const TARGET_LANGUAGE: &str = "zh-CN";
const CLASSIFIER_VERSION: &str = "memex-bounded-language-v1";
const MAX_ATTEMPTS: u32 = 3;
/// Covers one provider request (120 s) with margin, so a live job is never recovered.
const LEASE_SECONDS: i64 = 300;
pub const MAX_SUMMARY_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_SUMMARY_OUTPUT_BYTES: usize = 16 * 1024;
/// Output limit of the current summary prompt (AI minutes, about 300–800 Chinese characters).
pub const MAX_SUMMARY_CHARACTERS: u32 = 1200;
/// Version-1 faithful summaries keep their original binding so they stay verifiable.
const LEGACY_SUMMARY_CHARACTERS: u32 = 600;
const LEGACY_PROMPT_VERSION: &str = "1";

pub type SummaryModelDescriptor = TranslationModelDescriptor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryJobView {
    pub job_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub source_language: String,
    pub target_language: String,
    pub state: SummaryJobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub attempt_count: u32,
    pub retryable: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryAnalysis {
    pub analysis_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySkipReason {
    ChallengeOrLogin,
    SourceLanguageUnknown,
    SourceTooLarge,
    StaleRevision,
    UnsupportedItemType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SummaryEnqueueOutcome {
    Enqueued {
        job: SummaryJobView,
    },
    Skipped {
        item_id: String,
        revision_id: String,
        reason: SummarySkipReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryModelError {
    ProviderFailure,
    Timeout,
    /// Dispatched and possibly billed; never retried automatically.
    OutcomeUnknown,
    MalformedOutput,
    InputUnsupported,
}

impl std::fmt::Display for SummaryModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ProviderFailure => "summary_provider_failure",
            Self::Timeout => "summary_provider_timeout",
            Self::OutcomeUnknown => "summary_provider_outcome_unknown",
            Self::MalformedOutput => "summary_malformed_model_output",
            Self::InputUnsupported => "summary_provider_input_unsupported",
        })
    }
}

impl std::error::Error for SummaryModelError {}

/// Source text deliberately implements neither `Debug` nor `Serialize` so it cannot enter a
/// generic worker log or status response. Only a revision-checking queue can construct this input.
pub struct SummaryModelInput<'a> {
    document_title: &'a str,
    source_language: &'a str,
    target_language: &'a str,
    prompt: &'static str,
    source_text: &'a str,
    output_limit: TextOutputLimit,
}

impl<'a> SummaryModelInput<'a> {
    pub fn document_title(&self) -> &'a str {
        self.document_title
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

    pub fn output_limit(&self) -> &TextOutputLimit {
        &self.output_limit
    }
}

pub trait SummaryModel {
    fn descriptor(&self) -> SummaryModelDescriptor;
    fn summarize(
        &mut self,
        input: SummaryModelInput<'_>,
    ) -> std::result::Result<String, SummaryModelError>;
}

struct SummaryJobRecord {
    view: SummaryJobView,
    prompt_version: String,
    revision_sha256: String,
    lease_owner: String,
    generated_summary: Option<String>,
    model: Option<SummaryModelDescriptor>,
}

pub struct SummaryQueue {
    connection: Connection,
    root: PathBuf,
}

impl SummaryQueue {
    pub fn open(library: impl AsRef<Path>) -> Result<Self> {
        // The established queue owner creates and validates the shared owner-only SQLite family.
        drop(TranslationQueue::open(library.as_ref())?);
        let root = super::safe_library_root(library.as_ref())?;
        let database = root.join("analysis").join(DATABASE_NAME);
        validate_database_family(&database)?;
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             PRAGMA secure_delete=ON;
             PRAGMA trusted_schema=OFF;
             CREATE TABLE IF NOT EXISTS summary_jobs(
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
               generated_summary TEXT,
               analysis_id TEXT,
               UNIQUE(item_id,revision_id,target_language)
             );
             CREATE TABLE IF NOT EXISTS summary_conflicts(
               id INTEGER PRIMARY KEY,
               job_id TEXT NOT NULL,
               reason_code TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS summary_jobs_order
               ON summary_jobs(created_at,job_id);",
        )?;
        crate::translation::add_columns(
            &connection,
            "summary_jobs",
            "provider_authorized",
            "ALTER TABLE summary_jobs ADD COLUMN provider_authorized INTEGER NOT NULL DEFAULT 0;",
        )?;
        // Additive: rows written before AI minutes are version-1 faithful summaries.
        crate::translation::add_columns(
            &connection,
            "summary_jobs",
            "prompt_version",
            "ALTER TABLE summary_jobs ADD COLUMN prompt_version TEXT NOT NULL DEFAULT '1';",
        )?;
        validate_database_family(&database)?;
        secure_database_family(&database)?;
        Ok(Self { connection, root })
    }

    /// See `TranslationQueue::authorize`.
    pub fn authorize(&mut self, item_id: &str, revision_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE summary_jobs SET provider_authorized=1
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
        )? == 1)
    }

    /// See `TranslationQueue::requeue_failed`.
    pub fn requeue_failed(&mut self, item_id: &str, revision_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE summary_jobs
                SET state='queued',reason_code='manual_retry',attempt_count=0,
                    lease_owner=NULL,lease_until=NULL,generated_summary=NULL,model_id=NULL,
                    model_sha256=NULL,adapter_id=NULL,adapter_version=NULL,
                    request_timeout_ms=NULL,provider_authorized=1,updated_at=?4
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

    /// One-time migration for manual-only AI minutes (`minutes_request`): minutes jobs that the
    /// capture hook or `translation_request` authorized under the old rule but that never
    /// started lose that authorization. Recorded in `analysis_migrations`, so a job the owner
    /// requests afterwards is never touched by a later start. Returns the jobs de-authorized.
    pub fn deauthorize_unstarted_minutes_once(&mut self) -> Result<usize> {
        const MIGRATION: &str = "manual_minutes_v1";
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS analysis_migrations(
               name TEXT PRIMARY KEY,
               applied_at TEXT NOT NULL
             );
             BEGIN IMMEDIATE;",
        )?;
        let result = (|| -> Result<usize> {
            let applied: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM analysis_migrations WHERE name=?1)",
                [MIGRATION],
                |row| row.get(0),
            )?;
            if applied {
                return Ok(0);
            }
            let changed = self.connection.execute(
                "UPDATE summary_jobs SET provider_authorized=0
                  WHERE provider_authorized=1 AND state='queued' AND lease_owner IS NULL
                    AND attempt_count=0 AND prompt_version IN ('2','3')",
                [],
            )?;
            self.connection.execute(
                "INSERT INTO analysis_migrations(name,applied_at) VALUES(?1,?2)",
                params![MIGRATION, Utc::now().to_rfc3339()],
            )?;
            Ok(changed)
        })();
        match result {
            Ok(changed) => {
                self.connection.execute_batch("COMMIT")?;
                Ok(changed)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Explicit owner request for AI minutes of this revision. A version-1 summary row is
    /// replaced in place by a fresh, authorized minutes job (its old Analysis record is never
    /// shown as minutes); a minutes job is authorized, or requeued after a failure.
    pub fn request_minutes(&mut self, item_id: &str, revision_id: &str) -> Result<()> {
        let row = self
            .connection
            .query_row(
                "SELECT prompt_version,state,source_language FROM summary_jobs
                  WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
                params![item_id, revision_id, TARGET_LANGUAGE],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((prompt_version, state, source_language)) = row else {
            return Ok(());
        };
        // Succeeded version-2 minutes stay current; unfinished older work moves to the
        // current prompt (which also yields a suggested title).
        if prompt_version != AI_MINUTES_PROMPT_VERSION
            && !(crate::is_ai_minutes_version(&prompt_version) && state == "succeeded")
        {
            self.connection.execute(
                "UPDATE summary_jobs
                    SET job_id=?4,prompt_version=?5,state='queued',reason_code=NULL,
                        attempt_count=0,lease_owner=NULL,lease_until=NULL,generated_summary=NULL,
                        analysis_id=NULL,model_id=NULL,model_sha256=NULL,adapter_id=NULL,
                        adapter_version=NULL,request_timeout_ms=NULL,provider_authorized=1,
                        updated_at=?6
                  WHERE item_id=?1 AND revision_id=?2 AND target_language=?3
                    AND prompt_version<>?5 AND lease_owner IS NULL",
                params![
                    item_id,
                    revision_id,
                    TARGET_LANGUAGE,
                    minutes_job_id(item_id, revision_id, &source_language),
                    AI_MINUTES_PROMPT_VERSION,
                    Utc::now().to_rfc3339()
                ],
            )?;
            return Ok(());
        }
        match state.as_str() {
            "failed" => {
                self.requeue_failed(item_id, revision_id)?;
            }
            "queued" | "running" => {
                self.authorize(item_id, revision_id)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn has_authorized_work(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM summary_jobs WHERE provider_authorized=1
                   AND state='queued' AND lease_owner IS NULL AND {} AND {})",
                crate::translation::metadata_gate("summary_jobs", Utc::now()),
                crate::translation::no_running_sibling("summary_jobs")
            ),
            [],
            |row| row.get(0),
        )?)
    }

    pub fn process_next_authorized<M: SummaryModel>(&mut self, model: &mut M) -> Result<bool> {
        self.process_leased(model, true)
    }

    pub fn retry_one_eligible_authorized(&mut self, base_delay: Duration) -> Result<bool> {
        self.retry_eligible(base_delay, true)
    }

    pub fn get(&self, job_id: &str) -> Result<SummaryJobView> {
        self.connection
            .query_row(
                "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                        reason_code,attempt_count,created_at,updated_at
                   FROM summary_jobs WHERE job_id=?1",
                [job_id],
                job_view_row,
            )
            .context("summary_job_not_found")
    }

    pub fn retry(&mut self, job_id: &str) -> Result<SummaryJobView> {
        let current = self.get(job_id)?;
        if current.state != SummaryJobState::Failed
            || !current.retryable
            || current.attempt_count >= MAX_ATTEMPTS
        {
            bail!("summary_job_not_retryable")
        }
        let changed = self.connection.execute(
            "UPDATE summary_jobs
                SET state='queued',reason_code='manual_retry',updated_at=?2,
                    lease_owner=NULL,lease_until=NULL
              WHERE job_id=?1 AND state='failed' AND lease_owner IS NULL",
            params![job_id, Utc::now().to_rfc3339()],
        )?;
        if changed != 1 {
            bail!("summary_job_not_retryable")
        }
        self.get(job_id)
    }

    pub fn retry_one_eligible(&mut self, base_delay: Duration) -> Result<bool> {
        self.retry_eligible(base_delay, false)
    }

    fn retry_eligible(&mut self, base_delay: Duration, authorized_only: bool) -> Result<bool> {
        if base_delay > Duration::from_secs(60)
            || !base_delay.subsec_nanos().is_multiple_of(1_000_000)
        {
            bail!("invalid_summary_retry_delay")
        }
        let delay_ms = i64::try_from(base_delay.as_millis())?;
        let now = Utc::now().to_rfc3339();
        let candidate = self
            .connection
            .query_row(
                "SELECT job_id,attempt_count,updated_at FROM summary_jobs
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
                    delay_ms,
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
        if !(1..i64::from(MAX_ATTEMPTS)).contains(&attempt_count)
            || chrono::DateTime::parse_from_rfc3339(&updated_at).is_err()
        {
            bail!("invalid_summary_retry_state")
        }
        let changed = self.connection.execute(
            "UPDATE summary_jobs SET state='queued',reason_code='automatic_retry',
                    lease_owner=NULL,lease_until=NULL,updated_at=?4
              WHERE job_id=?1 AND state='failed' AND attempt_count=?2 AND updated_at=?3
                AND reason_code IN ('provider_failure','provider_timeout',
                                        'provider_outcome_unknown','malformed_model_output')",
            params![job_id, attempt_count, updated_at, Utc::now().to_rfc3339()],
        )?;
        Ok(changed == 1)
    }

    pub fn process_next<M: SummaryModel>(&mut self, model: &mut M) -> Result<bool> {
        self.process_leased(model, false)
    }

    fn process_leased<M: SummaryModel>(
        &mut self,
        model: &mut M,
        authorized_only: bool,
    ) -> Result<bool> {
        let Some(mut job) = self.lease(authorized_only)? else {
            return Ok(false);
        };
        // A duplicate job of a metadata-only revision: a same-body revision already has the
        // validated minutes it inherits. Nothing is sent.
        if crate::is_ai_minutes_version(&job.prompt_version)
            && effective_summary(&self.root, &job.view.item_id, &job.view.revision_id)?
                .is_some_and(|(_, source)| source.revision_id != job.view.revision_id)
        {
            self.fail(&job, crate::translation::INHERITED, false)?;
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
            self.fail(&job, "provider_model_mismatch", false)?;
            return Ok(true);
        }
        let revision = match self.current_revision(&job)? {
            Some(revision) => revision,
            None => {
                self.fail(&job, "stale_revision", false)?;
                return Ok(true);
            }
        };
        if job.generated_summary.is_none() {
            let profile = SummaryProfile::of(&job.prompt_version)?;
            // One immediate re-ask after a malformed reply; then the bounded automatic retry.
            let mut valid = None;
            for _ in 0..2 {
                let output = match model.summarize(SummaryModelInput {
                    document_title: &revision.title,
                    source_language: &job.view.source_language,
                    target_language: &job.view.target_language,
                    prompt: profile.prompt,
                    source_text: &revision.content_markdown,
                    output_limit: TextOutputLimit {
                        unit: TextLengthUnit::Characters,
                        maximum: profile.maximum,
                    },
                }) {
                    Ok(output) => output,
                    Err(error) => {
                        self.fail_dispatched(&job, error)?;
                        return Ok(true);
                    }
                };
                if valid_summary_output(&output, &revision.content_markdown, profile.maximum) {
                    valid = Some(output);
                    break;
                }
            }
            let Some(output) = valid else {
                self.fail(&job, "malformed_model_output", true)?;
                return Ok(true);
            };
            self.freeze_output(&job, &descriptor, output.trim())?;
            job = self.record(&job.view.job_id)?;
        }
        if self.current_revision(&job)?.is_none() {
            self.supersede(&job, &revision)?;
            return Ok(true);
        }
        let frozen_model = job.model.as_ref().context("summary_model_not_frozen")?;
        let record = match summary_record(
            &self.root,
            &job,
            frozen_model,
            job.generated_summary
                .as_deref()
                .context("summary_output_not_frozen")?,
        ) {
            Ok(record) => record,
            Err(error) => {
                if self.current_revision(&job)?.is_none() {
                    self.supersede(&job, &revision)?;
                    return Ok(true);
                }
                return Err(error);
            }
        };
        if competing_summary_records(&self.root, &job, &record)? {
            self.fail(&job, "analysis_conflict", false)?;
            return Ok(true);
        }
        let receipt = match super::apply_record(&self.root, &record) {
            Ok(receipt) => receipt,
            Err(error) => {
                if self.current_revision(&job)?.is_none() {
                    self.supersede(&job, &revision)?;
                    return Ok(true);
                }
                return Err(error);
            }
        };
        if receipt
            .warning_codes
            .iter()
            .any(|warning| warning == "analysis_json_index_projection_pending")
        {
            bail!("summary_analysis_index_pending")
        }
        if self.current_revision(&job)?.is_none() {
            self.supersede(&job, &revision)?;
            return Ok(true);
        }
        if competing_summary_records(&self.root, &job, &record)? {
            self.fail(&job, "analysis_conflict", false)?;
            return Ok(true);
        }
        self.succeed(&job, &record.analysis_id)?;
        Ok(true)
    }

    fn fail_dispatched(&mut self, job: &SummaryJobRecord, error: SummaryModelError) -> Result<()> {
        if self.current_revision(job)?.is_none() {
            return self.fail(job, crate::translation::SUPERSEDED, false);
        }
        self.fail_model(job, error)
    }

    /// The job's revision was replaced after its request. When the replacement has the same
    /// title and Markdown (a metadata-only revision), the already generated minutes are handed
    /// to its queued job as frozen output (applied without another request); this job ends
    /// `superseded_by_revision`.
    fn supersede(&mut self, job: &SummaryJobRecord, old: &ItemRevision) -> Result<()> {
        let (_, current) = memex_store::read_item(&self.root, &job.view.item_id)?;
        if let (Some(output), Some(model), true) = (
            job.generated_summary.as_deref(),
            job.model.as_ref(),
            current.title == old.title && current.content_markdown == old.content_markdown,
        ) {
            self.connection.execute(
                "UPDATE summary_jobs
                    SET generated_summary=?3,model_id=?4,model_sha256=?5,adapter_id=?6,
                        adapter_version=?7,request_timeout_ms=?8,updated_at=?9
                  WHERE item_id=?1 AND revision_id=?2 AND target_language=?10
                    AND state='queued' AND lease_owner IS NULL AND generated_summary IS NULL
                    AND model_id IS NULL AND prompt_version=?11 AND source_language=?12",
                params![
                    job.view.item_id,
                    current.revision_id,
                    output,
                    model.model_id,
                    model.model_sha256,
                    model.adapter_id,
                    model.adapter_version,
                    model.request_timeout.as_millis() as i64,
                    Utc::now().to_rfc3339(),
                    TARGET_LANGUAGE,
                    job.prompt_version,
                    job.view.source_language
                ],
            )?;
        }
        self.fail(job, crate::translation::SUPERSEDED, false)
    }

    fn enqueue_verified(
        &mut self,
        item_id: &str,
        revision: &ItemRevision,
        source_language: &str,
    ) -> Result<SummaryJobView> {
        let input = revision_ref(item_id, revision)?;
        let job_id = minutes_job_id(item_id, &revision.revision_id, source_language);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // One row per revision: an older-version row is kept as is (only an explicit owner
        // request upgrades it, see `request_minutes`).
        if let Some((existing_id, existing)) = transaction
            .query_row(
                "SELECT job_id,revision_sha256 FROM summary_jobs
                  WHERE job_id=?1 OR (item_id=?2 AND revision_id=?3 AND target_language=?4)",
                params![job_id, item_id, revision.revision_id, TARGET_LANGUAGE],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing == input.revision_sha256 {
                transaction.commit()?;
                return self.get(&existing_id);
            }
            transaction.execute(
                "INSERT INTO summary_conflicts(job_id,reason_code,created_at)
                 VALUES(?1,'summary_job_identity_conflict',?2)",
                params![job_id, Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
            bail!("summary_job_identity_conflict")
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO summary_jobs(
               job_id,item_id,revision_id,revision_sha256,source_language,target_language,
               classifier_version,state,created_at,updated_at,prompt_version
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?8,?9)",
            params![
                job_id,
                item_id,
                revision.revision_id,
                input.revision_sha256,
                source_language,
                TARGET_LANGUAGE,
                CLASSIFIER_VERSION,
                now,
                AI_MINUTES_PROMPT_VERSION
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
            "UPDATE summary_jobs
                SET state='failed',reason_code='retry_exhausted',lease_owner=NULL,
                    lease_until=NULL,updated_at=?3
              WHERE state='running' AND lease_until<?1 AND attempt_count>=?2
                AND generated_summary IS NULL",
            params![now.timestamp(), i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        let recovered = transaction.execute(
            "UPDATE summary_jobs
                SET state='queued',reason_code='worker_recovered',lease_owner=NULL,
                    lease_until=NULL,updated_at=?3
              WHERE state='running' AND lease_until<?1
                AND (attempt_count<?2 OR generated_summary IS NOT NULL)",
            params![now.timestamp(), i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(exhausted + recovered)
    }

    fn lease(&mut self, authorized_only: bool) -> Result<Option<SummaryJobRecord>> {
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
            "UPDATE summary_jobs
                SET state='failed',reason_code='retry_exhausted',updated_at=?2
              WHERE state='queued' AND lease_owner IS NULL AND attempt_count>=?1
                AND generated_summary IS NULL",
            params![i64::from(MAX_ATTEMPTS), now.to_rfc3339()],
        )?;
        let job_id = transaction
            .query_row(
                &format!(
                    "SELECT job_id FROM summary_jobs
                      WHERE state='queued' AND lease_owner IS NULL
                        AND (?2=0 OR provider_authorized=1)
                        AND (attempt_count<?1 OR generated_summary IS NOT NULL)
                        AND (?2=0 OR {})
                        AND {}
                      ORDER BY created_at,job_id LIMIT 1",
                    crate::translation::metadata_gate("summary_jobs", now),
                    crate::translation::no_running_sibling("summary_jobs")
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
            "UPDATE summary_jobs
                SET state='running',reason_code=NULL,
                    attempt_count=attempt_count + CASE WHEN generated_summary IS NULL THEN 1 ELSE 0 END,
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
            bail!("summary_lease_lost")
        }
        transaction.commit()?;
        Ok(Some(self.record(&job_id)?))
    }

    fn record(&self, job_id: &str) -> Result<SummaryJobRecord> {
        self.connection
            .query_row(
                "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                        reason_code,attempt_count,created_at,updated_at,revision_sha256,
                        lease_owner,generated_summary,model_id,model_sha256,adapter_id,
                        adapter_version,request_timeout_ms,prompt_version
                   FROM summary_jobs WHERE job_id=?1",
                [job_id],
                |row| {
                    let fields = (
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, Option<String>>(15)?,
                        row.get::<_, Option<String>>(16)?,
                        row.get::<_, Option<i64>>(17)?,
                    );
                    let model = match fields {
                        (
                            Some(model_id),
                            Some(model_sha256),
                            Some(adapter_id),
                            Some(adapter_version),
                            Some(request_timeout_ms),
                        ) if request_timeout_ms > 0 => Some(SummaryModelDescriptor {
                            model_id,
                            model_sha256,
                            adapter_id,
                            adapter_version,
                            request_timeout: Duration::from_millis(request_timeout_ms as u64),
                        }),
                        (None, None, None, None, None) => None,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    Ok(SummaryJobRecord {
                        view: job_view_columns(row)?,
                        prompt_version: row.get(18)?,
                        revision_sha256: row.get(10)?,
                        lease_owner: row
                            .get::<_, Option<String>>(11)?
                            .ok_or(rusqlite::Error::InvalidQuery)?,
                        generated_summary: row.get(12)?,
                        model,
                    })
                },
            )
            .context("summary_job_not_found")
    }

    /// The job's revision while its body is still the current body (see
    /// `TranslationQueue::current_revision`).
    fn current_revision(&self, job: &SummaryJobRecord) -> Result<Option<ItemRevision>> {
        crate::lineage::job_source(
            &self.root,
            &job.view.item_id,
            &job.view.revision_id,
            &job.revision_sha256,
        )
    }

    fn freeze_output(
        &mut self,
        job: &SummaryJobRecord,
        model: &SummaryModelDescriptor,
        output: &str,
    ) -> Result<()> {
        let now = Utc::now();
        let changed = self.connection.execute(
            "UPDATE summary_jobs
                SET generated_summary=?2,model_id=COALESCE(model_id,?3),
                    model_sha256=COALESCE(model_sha256,?4),adapter_id=COALESCE(adapter_id,?5),
                    adapter_version=COALESCE(adapter_version,?6),
                    request_timeout_ms=COALESCE(request_timeout_ms,?7),updated_at=?8,lease_until=?9
              WHERE job_id=?1 AND state='running' AND lease_owner=?10
                AND generated_summary IS NULL
                AND (model_id IS NULL OR (model_id=?3 AND model_sha256=?4
                     AND adapter_id=?5 AND adapter_version=?6 AND request_timeout_ms=?7))",
            params![
                job.view.job_id,
                output,
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
            bail!("summary_lease_lost")
        }
        Ok(())
    }

    fn fail_model(&mut self, job: &SummaryJobRecord, error: SummaryModelError) -> Result<()> {
        let (reason, retryable) = model_error_reason(match error {
            SummaryModelError::ProviderFailure => TranslationModelError::ProviderFailure,
            SummaryModelError::Timeout => TranslationModelError::Timeout,
            SummaryModelError::OutcomeUnknown => TranslationModelError::OutcomeUnknown,
            SummaryModelError::MalformedOutput => TranslationModelError::MalformedOutput,
            SummaryModelError::InputUnsupported => TranslationModelError::InputUnsupported,
        });
        self.fail(job, reason, retryable)
    }

    fn fail(&mut self, job: &SummaryJobRecord, reason: &str, retryable: bool) -> Result<()> {
        let reason = if retryable && job.view.attempt_count >= MAX_ATTEMPTS {
            "retry_exhausted"
        } else {
            reason
        };
        let changed = self.connection.execute(
            "UPDATE summary_jobs
                SET state='failed',reason_code=?2,lease_owner=NULL,lease_until=NULL,updated_at=?3
              WHERE job_id=?1 AND state='running' AND lease_owner=?4",
            params![
                job.view.job_id,
                reason,
                Utc::now().to_rfc3339(),
                job.lease_owner
            ],
        )?;
        if changed != 1 {
            bail!("summary_lease_lost")
        }
        Ok(())
    }

    fn succeed(&mut self, job: &SummaryJobRecord, analysis_id: &str) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE summary_jobs
                SET state='succeeded',reason_code=NULL,lease_owner=NULL,lease_until=NULL,
                    generated_summary=NULL,analysis_id=?2,updated_at=?3
              WHERE job_id=?1 AND state='running' AND lease_owner=?4",
            params![
                job.view.job_id,
                analysis_id,
                Utc::now().to_rfc3339(),
                job.lease_owner
            ],
        )?;
        if changed != 1 {
            bail!("summary_lease_lost")
        }
        Ok(())
    }
}

/// See `carry_translation`: AI minutes (current prompt version only) of a metadata-only
/// revision. A succeeded output is frozen into the `current` job and applied without a model
/// request; queued/running/failed authorized work is re-requested for `current`.
pub fn carry_summary(
    library: impl AsRef<Path>,
    item_id: &str,
    previous_revision_id: &str,
    current_revision_id: &str,
) -> Result<bool> {
    let library = library.as_ref();
    let mut queue = SummaryQueue::open(library)?;
    let previous = queue
        .connection
        .query_row(
            "SELECT state,provider_authorized,prompt_version,analysis_id,model_id,model_sha256,
                    adapter_id,adapter_version,request_timeout_ms
               FROM summary_jobs WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, previous_revision_id, TARGET_LANGUAGE],
            |row| {
                let model = match (
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ) {
                    (
                        Some(model_id),
                        Some(model_sha256),
                        Some(adapter_id),
                        Some(adapter_version),
                        Some(ms),
                    ) if ms > 0 => Some(SummaryModelDescriptor {
                        model_id,
                        model_sha256,
                        adapter_id,
                        adapter_version,
                        request_timeout: Duration::from_millis(ms as u64),
                    }),
                    _ => None,
                };
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? == 1,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    model,
                ))
            },
        )
        .optional()?;
    let Some((state, authorized, prompt_version, analysis_id, model)) = previous else {
        return Ok(false);
    };
    if prompt_version != AI_MINUTES_PROMPT_VERSION || (state != "succeeded" && !authorized) {
        return Ok(false);
    }
    let SummaryEnqueueOutcome::Enqueued { job } =
        enqueue_item_summary(library, item_id, current_revision_id)?
    else {
        return Ok(false);
    };
    if let (true, Some(analysis_id), Some(model), SummaryJobState::Queued) =
        (state == "succeeded", analysis_id, model, job.state)
    {
        let output = super::read_record(library, &analysis_id)?.body_markdown;
        queue.connection.execute(
            "UPDATE summary_jobs
                SET generated_summary=?2,model_id=?3,model_sha256=?4,adapter_id=?5,
                    adapter_version=?6,request_timeout_ms=?7,updated_at=?8
              WHERE job_id=?1 AND state='queued' AND lease_owner IS NULL
                AND generated_summary IS NULL AND model_id IS NULL AND prompt_version=?9",
            params![
                job.job_id,
                output,
                model.model_id,
                model.model_sha256,
                model.adapter_id,
                model.adapter_version,
                model.request_timeout.as_millis() as i64,
                Utc::now().to_rfc3339(),
                AI_MINUTES_PROMPT_VERSION
            ],
        )?;
    }
    queue.request_minutes(item_id, current_revision_id)?;
    Ok(true)
}

pub fn enqueue_item_summary(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<SummaryEnqueueOutcome> {
    let (item, revision) = memex_store::read_item(library.as_ref(), item_id)?;
    if item.current_revision != revision_id || revision.revision_id != revision_id {
        return Ok(skipped(&revision, SummarySkipReason::StaleRevision));
    }
    if !matches!(
        item.item_type.as_str(),
        "article" | "journal_article" | "webpage" | "document"
    ) {
        return Ok(skipped(&revision, SummarySkipReason::UnsupportedItemType));
    }
    let source_language = match classify_source(&revision.title, &revision.content_markdown) {
        SourceClassification::Chinese
        | SourceClassification::English
        | SourceClassification::Other => {
            source_language(&revision.title, &revision.content_markdown)
        }
        SourceClassification::ChallengeOrLogin => {
            return Ok(skipped(&revision, SummarySkipReason::ChallengeOrLogin))
        }
        SourceClassification::TooLarge => {
            return Ok(skipped(&revision, SummarySkipReason::SourceTooLarge))
        }
        SourceClassification::Unknown => {
            return Ok(skipped(&revision, SummarySkipReason::SourceLanguageUnknown))
        }
    };
    if revision.content_markdown.is_empty()
        || revision.content_markdown.len() > MAX_SUMMARY_SOURCE_BYTES
    {
        return Ok(skipped(&revision, SummarySkipReason::SourceTooLarge));
    }
    // A metadata-only revision reuses the validated minutes of a same-body revision.
    if effective_summary(library.as_ref(), item_id, revision_id)?
        .is_some_and(|(_, source)| source.revision_id != revision_id)
    {
        if let Some(job) = summary_status(library.as_ref(), item_id, revision_id)? {
            return Ok(SummaryEnqueueOutcome::Enqueued { job });
        }
    }
    let mut queue = SummaryQueue::open(library)?;
    Ok(SummaryEnqueueOutcome::Enqueued {
        job: queue.enqueue_verified(item_id, &revision, source_language)?,
    })
}

pub fn enqueue_ingest_summaries(
    library: impl AsRef<Path>,
    receipt: &IngestReceipt,
) -> Result<Vec<SummaryEnqueueOutcome>> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.item_ids.is_empty()
        || receipt.item_ids.len() != receipt.revision_ids.len()
    {
        bail!("invalid_ingest_receipt_for_summary")
    }
    receipt
        .item_ids
        .iter()
        .zip(&receipt.revision_ids)
        .map(|(item_id, revision_id)| enqueue_item_summary(library.as_ref(), item_id, revision_id))
        .collect()
}

/// The revision whose minutes `revision_id` shows: itself, unless it has no succeeded AI-minutes
/// row and a same-body revision (title or other metadata edited since) has one.
fn minutes_revision(
    root: &Path,
    connection: &Connection,
    item_id: &str,
    revision_id: &str,
) -> Result<String> {
    if !has_column(connection, "summary_jobs", "prompt_version")? {
        return Ok(revision_id.to_owned());
    }
    let own: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM summary_jobs WHERE item_id=?1 AND revision_id=?2
           AND target_language=?3 AND state='succeeded' AND prompt_version IN ('2','3'))",
        params![item_id, revision_id, TARGET_LANGUAGE],
        |row| row.get(0),
    )?;
    if own {
        return Ok(revision_id.to_owned());
    }
    Ok(crate::lineage::inherited_revisions(
        connection,
        root,
        "summary_jobs",
        "AND target_language='zh-CN' AND prompt_version IN ('2','3')",
        "",
        item_id,
        revision_id,
    )?
    .into_iter()
    .next()
    .map_or_else(|| revision_id.to_owned(), |source| source.revision_id))
}

pub fn summary_status(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<SummaryJobView>> {
    let Some((root, connection)) = open_summary_status_database(library.as_ref())? else {
        return Ok(None);
    };
    let revision_id = &minutes_revision(&root, &connection, item_id, revision_id)?;
    connection
        .query_row(
            "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                    reason_code,attempt_count,created_at,updated_at
               FROM summary_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            job_view_row,
        )
        .optional()
        .map_err(Into::into)
}

/// See `translation_authorized`.
pub fn summary_authorized(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<bool> {
    let Some((root, connection)) = open_summary_status_database(library.as_ref())? else {
        return Ok(false);
    };
    if !has_column(&connection, "summary_jobs", "provider_authorized")? {
        return Ok(false);
    }
    let revision_id = &minutes_revision(&root, &connection, item_id, revision_id)?;
    Ok(connection
        .query_row(
            "SELECT provider_authorized FROM summary_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        == Some(1))
}

pub fn validated_summary(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<SummaryAnalysis>> {
    Ok(effective_summary(library, item_id, revision_id)?.map(|(summary, _)| summary))
}

/// The validated summary `revision_id` shows and the revision it was produced from and is
/// validated against: `revision_id` itself, or its newest same-body revision with minutes.
pub fn effective_summary(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<(SummaryAnalysis, ItemRevision)>> {
    let Some((root, connection)) = open_summary_status_database(library.as_ref())? else {
        return Ok(None);
    };
    let Some(requested) = crate::lineage::job_source_revision(&root, item_id, revision_id)? else {
        return Ok(None);
    };
    let source_id = minutes_revision(&root, &connection, item_id, revision_id)?;
    let source = if source_id == requested.revision_id {
        requested
    } else {
        match memex_store::same_body_revision(&root, item_id, revision_id, &source_id)? {
            Some(source) => source,
            None => return Ok(None),
        }
    };
    Ok(summary_at(&root, &connection, item_id, &source)?.map(|summary| (summary, source)))
}

/// Validates the succeeded summary of exactly `revision` (current or same-body historical).
fn summary_at(
    root: &Path,
    connection: &Connection,
    item_id: &str,
    revision: &ItemRevision,
) -> Result<Option<SummaryAnalysis>> {
    let root = root.to_path_buf();
    let revision_id = revision.revision_id.as_str();
    // A database not yet migrated by a writer only holds version-1 rows.
    let prompt_version_column = if has_column(&connection, "summary_jobs", "prompt_version")? {
        "prompt_version"
    } else {
        "'1'"
    };
    let binding = connection
        .query_row(
            &format!(
                "SELECT job_id,item_id,revision_id,source_language,target_language,state,
                    reason_code,attempt_count,created_at,updated_at,revision_sha256,
                    analysis_id,model_id,model_sha256,adapter_id,adapter_version,
                    request_timeout_ms,{prompt_version_column}
               FROM summary_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3 AND state='succeeded'"
            ),
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| {
                Ok((
                    job_view_columns(row)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    SummaryModelDescriptor {
                        model_id: row.get(12)?,
                        model_sha256: row.get(13)?,
                        adapter_id: row.get(14)?,
                        adapter_version: row.get(15)?,
                        request_timeout: Duration::from_millis(
                            u64::try_from(row.get::<_, i64>(16)?)
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        ),
                    },
                    row.get::<_, String>(17)?,
                ))
            },
        )
        .optional()?;
    let Some((view, revision_sha256, analysis_id, model, prompt_version)) = binding else {
        return Ok(None);
    };
    if !super::valid_analysis_id(&analysis_id)
        || !valid_sha256(&revision_sha256)
        || !model.is_valid()
    {
        bail!("invalid succeeded summary binding")
    }
    if revision_ref(item_id, revision)?.revision_sha256 != revision_sha256 {
        return Ok(None);
    }
    let record = super::read_record(&root, &analysis_id)?;
    let profile = SummaryProfile::of(&prompt_version)?;
    if !valid_summary_output(
        &record.body_markdown,
        &revision.content_markdown,
        profile.maximum,
    ) {
        return Ok(None);
    }
    let job = SummaryJobRecord {
        view,
        prompt_version,
        revision_sha256,
        lease_owner: String::new(),
        generated_summary: Some(record.body_markdown.clone()),
        model: Some(model.clone()),
    };
    let expected = summary_record(&root, &job, &model, &record.body_markdown)?;
    if record != expected || competing_summary_records(&root, &job, &record)? {
        return Ok(None);
    }
    Ok(Some(SummaryAnalysis { analysis_id }))
}

fn open_summary_status_database(library: &Path) -> Result<Option<(PathBuf, Connection)>> {
    let Some((root, connection)) = open_status_database(library)? else {
        return Ok(None);
    };
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='summary_jobs'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists.then_some((root, connection)))
}

fn valid_summary_output(output: &str, source: &str, maximum: u32) -> bool {
    let trimmed = output.trim();
    let characters = trimmed.chars().count();
    if trimmed.is_empty()
        || output.len() > MAX_SUMMARY_OUTPUT_BYTES
        || characters > maximum as usize
        || output.contains('\0')
        || output
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        || trimmed == source.trim()
        || (source.chars().count() >= 64 && trimmed.contains(source.trim()))
        || (trimmed.starts_with("```") && trimmed.ends_with("```"))
        || (trimmed.starts_with("~~~") && trimmed.ends_with("~~~"))
    {
        return false;
    }
    let han = trimmed
        .chars()
        .filter(|character| is_han(*character))
        .count();
    let letters = trimmed
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    han >= 4 && han.saturating_mul(2) >= letters
}

fn summary_record(
    library: &Path,
    job: &SummaryJobRecord,
    model: &SummaryModelDescriptor,
    body: &str,
) -> Result<super::AnalysisRecord> {
    let profile = SummaryProfile::of(&job.prompt_version)?;
    let (prompt_id, prompt_sha256) = (profile.prompt_id, profile.prompt_sha256.clone());
    let config = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        profile.config_tag,
        job.view.item_id,
        job.view.revision_id,
        job.revision_sha256,
        job.view.source_language,
        job.view.target_language,
        profile.maximum,
        model.model_id,
        model.model_sha256,
        prompt_sha256
    );
    let mut record = prepare_record(
        library,
        &AnalysisRequest {
            schema_version: SCHEMA_VERSION,
            analysis_kind: AnalysisKind::Summary,
            title: profile.record_title.into(),
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
                purpose: GeneratedTextPurpose::Summary,
                source_language: job.view.source_language.clone(),
                target_language: job.view.target_language.clone(),
                output_limit: Some(TextOutputLimit {
                    unit: TextLengthUnit::Characters,
                    maximum: profile.maximum,
                }),
                summary_job_id: Some(job.view.job_id.clone()),
                translation_job_id: None,
                model_id: model.model_id.clone(),
                model_sha256: Some(model.model_sha256.clone()),
                prompt_id: prompt_id.into(),
                prompt_version: profile.prompt_version.into(),
                prompt_sha256,
            }),
        },
    )?;
    // Bound to the job's revision: the current one, or a same-body revision that only
    // metadata edits have replaced since (see `translation_record`).
    if let Some(source) = crate::lineage::job_source(
        library,
        &job.view.item_id,
        &job.view.revision_id,
        &job.revision_sha256,
    )? {
        if crate::lineage::historical(library, &source)? {
            record.inputs = vec![revision_ref(&job.view.item_id, &source)?];
            record.sensitivity = source.sensitivity;
            record.analysis_id = super::analysis_id(&record)?;
            super::validate_record(&record)?;
        }
    }
    if record.inputs.len() != 1
        || record.inputs[0].item_id != job.view.item_id
        || record.inputs[0].revision_id != job.view.revision_id
        || record.inputs[0].revision_sha256 != job.revision_sha256
    {
        bail!("summary record revision changed")
    }
    Ok(record)
}

fn competing_summary_records(
    library: &Path,
    job: &SummaryJobRecord,
    expected: &super::AnalysisRecord,
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
        // Records of another summary prompt (a version-1 summary beside AI minutes) are a
        // different output, not a competitor.
        if generated.purpose == GeneratedTextPurpose::Summary
            && expected
                .generated_text
                .as_ref()
                .is_some_and(|own| own.prompt_id == generated.prompt_id)
            && same_language_family(&generated.source_language, &job.view.source_language)
            && same_language_family(&generated.target_language, &job.view.target_language)
            && record.analysis_id != expected.analysis_id
        {
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

fn same_model(left: &SummaryModelDescriptor, right: &SummaryModelDescriptor) -> bool {
    left.model_id == right.model_id
        && left.model_sha256 == right.model_sha256
        && left.adapter_id == right.adapter_id
        && left.adapter_version == right.adapter_version
        && left.request_timeout == right.request_timeout
}

/// Prompt binding of one summary job row (column `prompt_version`).
struct SummaryProfile {
    prompt_id: &'static str,
    prompt: &'static str,
    prompt_sha256: String,
    prompt_version: &'static str,
    maximum: u32,
    config_tag: &'static str,
    record_title: &'static str,
}

impl SummaryProfile {
    fn of(prompt_version: &str) -> Result<Self> {
        if prompt_version == AI_MINUTES_PROMPT_VERSION {
            let (prompt_id, prompt, prompt_sha256) = ai_minutes_prompt();
            return Ok(Self {
                prompt_id,
                prompt,
                prompt_sha256,
                prompt_version: AI_MINUTES_PROMPT_VERSION,
                maximum: MAX_SUMMARY_CHARACTERS,
                config_tag: "summary-v3-ai-minutes",
                record_title: "AI minutes",
            });
        }
        if prompt_version == crate::AI_MINUTES_V2_PROMPT_VERSION {
            let (prompt_id, prompt, prompt_sha256) = crate::ai_minutes_v2_prompt();
            return Ok(Self {
                prompt_id,
                prompt,
                prompt_sha256,
                prompt_version: crate::AI_MINUTES_V2_PROMPT_VERSION,
                maximum: MAX_SUMMARY_CHARACTERS,
                config_tag: "summary-v2-ai-minutes",
                record_title: "AI minutes",
            });
        }
        if prompt_version == LEGACY_PROMPT_VERSION {
            let (prompt_id, prompt, prompt_sha256) =
                generated_text_prompt(GeneratedTextPurpose::Summary);
            return Ok(Self {
                prompt_id,
                prompt,
                prompt_sha256,
                prompt_version: GENERATED_TEXT_PROMPT_VERSION,
                maximum: LEGACY_SUMMARY_CHARACTERS,
                config_tag: "summary-v1",
                record_title: "Validated source summary",
            });
        }
        bail!("unsupported summary prompt version")
    }
}

fn minutes_job_id(item_id: &str, revision_id: &str, source_language: &str) -> String {
    let key = format!(
        "{item_id}\0{revision_id}\0{source_language}\0{TARGET_LANGUAGE}\0{MAX_SUMMARY_CHARACTERS}\0ai-minutes-{AI_MINUTES_PROMPT_VERSION}"
    );
    format!("summary_{:x}", Sha256::digest(key.as_bytes()))
}

/// The prompt version of this revision's summary job: `"3"`/`"2"` are AI minutes, `"1"` a legacy
/// faithful summary. `None` when no job exists.
pub fn summary_prompt_version(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<String>> {
    let Some((root, connection)) = open_summary_status_database(library.as_ref())? else {
        return Ok(None);
    };
    if !has_column(&connection, "summary_jobs", "prompt_version")? {
        return Ok(Some(LEGACY_PROMPT_VERSION.into()));
    }
    let revision_id = &minutes_revision(&root, &connection, item_id, revision_id)?;
    connection
        .query_row(
            "SELECT prompt_version FROM summary_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language=?3",
            params![item_id, revision_id, TARGET_LANGUAGE],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn skipped(revision: &ItemRevision, reason: SummarySkipReason) -> SummaryEnqueueOutcome {
    SummaryEnqueueOutcome::Skipped {
        item_id: revision.item_id.clone(),
        revision_id: revision.revision_id.clone(),
        reason,
    }
}

fn parse_state(value: &str) -> rusqlite::Result<SummaryJobState> {
    match value {
        "queued" => Ok(SummaryJobState::Queued),
        "running" => Ok(SummaryJobState::Running),
        "succeeded" => Ok(SummaryJobState::Succeeded),
        "failed" => Ok(SummaryJobState::Failed),
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

fn job_view_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryJobView> {
    job_view_columns(row)
}

fn job_view_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryJobView> {
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
        SummaryJobState::Queued => matches!(
            reason_code.as_deref(),
            None | Some("automatic_retry" | "manual_retry" | "worker_recovered")
        ),
        SummaryJobState::Running | SummaryJobState::Succeeded => reason_code.is_none(),
        SummaryJobState::Failed => reason_code.is_some(),
    };
    if !valid_prefixed_digest(&job_id, "summary_")
        || !valid_component(&item_id, "item_")
        || !valid_component(&revision_id, "revision_")
        || !(source_language == "zh-CN" || valid_source_language(&source_language))
        || target_language != TARGET_LANGUAGE
        || !state_reason_valid
        || chrono::DateTime::parse_from_rfc3339(&created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&updated_at).is_err()
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(SummaryJobView {
        job_id,
        item_id,
        revision_id,
        source_language,
        target_language,
        state,
        retryable: state == SummaryJobState::Failed
            && reason_retryable(reason_code.as_deref(), attempt_count),
        reason_code,
        attempt_count,
        created_at,
        updated_at,
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

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_han(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ItemDraft, Sensitivity, SourceProfile, VerifiedBlob,
        VerifiedCapture,
    };

    fn ingest_source(root: &Path, marker: &str, item_type: &str) -> IngestReceipt {
        let body = format!(
            "# A bounded report about {marker}\n\nThis is the synthetic source article, and the report was written for the test. The article has more detail about the result, which was checked with a bounded fixture and not with private content. This report has the facts from the source, and it was prepared for deterministic verification."
        );
        let descriptor = BlobDescriptor {
            schema_version: SCHEMA_VERSION,
            locator: format!("memory:{marker}"),
            media_type: "text/plain".into(),
            byte_size: 4,
            sha256: "sha256:body".into(),
            source_url: None,
        };
        memex_store::ingest(
            root,
            &VerifiedCapture {
                source: SourceProfile {
                    schema_version: SCHEMA_VERSION,
                    source_id: "summary-test-source".into(),
                    name: "Synthetic summary test source".into(),
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
                    external_capture_id: format!("summary-{marker}"),
                    source_id: "summary-test-source".into(),
                    captured_at: "2026-08-14T00:00:00Z".into(),
                    capture_method: "synthetic".into(),
                    sensitivity: Sensitivity::Internal,
                    route_summary: None,
                    title: None,
                    source_url: format!("https://example.invalid/{marker}"),
                    canonical_url: None,
                    artifacts: vec![descriptor.clone()],
                },
                blobs: vec![VerifiedBlob {
                    descriptor,
                    blob_id: format!("blob-{marker}"),
                    source_backend_ref: Some("synthetic:test".into()),
                    bytes: b"body".to_vec(),
                }],
            },
            ItemDraft {
                item_type: item_type.into(),
                title: "A careful synthetic report about the test".into(),
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

    struct SyntheticSummaryModel {
        fail_once: bool,
    }

    impl SummaryModel for SyntheticSummaryModel {
        fn descriptor(&self) -> SummaryModelDescriptor {
            SummaryModelDescriptor {
                model_id: "synthetic-summary-v1".into(),
                model_sha256: format!("sha256:{}", "6".repeat(64)),
                adapter_id: "memex.synthetic-summary".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn summarize(
            &mut self,
            input: SummaryModelInput<'_>,
        ) -> std::result::Result<String, SummaryModelError> {
            assert_eq!(input.source_language(), "en");
            assert_eq!(input.target_language(), "zh-CN");
            assert_eq!(input.output_limit().maximum, MAX_SUMMARY_CHARACTERS);
            assert!(input.source_text().contains("synthetic source article"));
            if self.fail_once {
                self.fail_once = false;
                Err(SummaryModelError::Timeout)
            } else {
                Ok("这篇合成报告说明，测试使用有界且不含私密内容的固定资料验证确定性结果。".into())
            }
        }
    }

    struct ProfileModel {
        prompts: Vec<&'static str>,
    }

    impl SummaryModel for ProfileModel {
        fn descriptor(&self) -> SummaryModelDescriptor {
            SyntheticSummaryModel { fail_once: false }.descriptor()
        }

        fn summarize(
            &mut self,
            input: SummaryModelInput<'_>,
        ) -> std::result::Result<String, SummaryModelError> {
            self.prompts.push(input.prompt());
            assert!(input.document_title().contains("synthetic"));
            Ok("这篇合成报告说明，测试使用有界且不含私密内容的固定资料验证确定性结果。".into())
        }
    }

    #[test]
    fn legacy_summary_is_replaced_by_minutes_only_on_an_explicit_request() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "legacy", "webpage");
        let SummaryEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_summaries(root.path(), &receipt).unwrap()[0].clone()
        else {
            panic!("summary was not queued")
        };
        let (item, revision) = (job.item_id.clone(), job.revision_id.clone());
        assert_eq!(
            summary_prompt_version(root.path(), &item, &revision)
                .unwrap()
                .as_deref(),
            Some(AI_MINUTES_PROMPT_VERSION)
        );
        // Simulate a row written before AI minutes existed, then let it succeed as a v1 summary.
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        queue
            .connection
            .execute(
                "UPDATE summary_jobs SET prompt_version='1' WHERE job_id=?1",
                [&job.job_id],
            )
            .unwrap();
        let mut model = ProfileModel { prompts: vec![] };
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(model.prompts, [crate::FAITHFUL_SUMMARY_PROMPT]);
        let legacy = validated_summary(root.path(), &item, &revision)
            .unwrap()
            .unwrap();
        let legacy_record = super::super::read_record(root.path(), &legacy.analysis_id).unwrap();
        assert_eq!(
            legacy_record.generated_text.unwrap().prompt_version,
            GENERATED_TEXT_PROMPT_VERSION
        );
        // Background enqueue keeps the legacy row (and its summary) untouched.
        enqueue_ingest_summaries(root.path(), &receipt).unwrap();
        assert_eq!(
            summary_prompt_version(root.path(), &item, &revision)
                .unwrap()
                .as_deref(),
            Some("1")
        );
        // An explicit request replaces it with an authorized minutes job.
        queue.request_minutes(&item, &revision).unwrap();
        assert_eq!(
            summary_prompt_version(root.path(), &item, &revision)
                .unwrap()
                .as_deref(),
            Some(AI_MINUTES_PROMPT_VERSION)
        );
        assert!(summary_authorized(root.path(), &item, &revision).unwrap());
        let status = summary_status(root.path(), &item, &revision)
            .unwrap()
            .unwrap();
        assert_eq!(status.state, SummaryJobState::Queued);
        assert!(validated_summary(root.path(), &item, &revision)
            .unwrap()
            .is_none());
        assert!(queue.process_next_authorized(&mut model).unwrap());
        assert_eq!(model.prompts[1], crate::AI_MINUTES_PROMPT);
        let minutes = validated_summary(root.path(), &item, &revision)
            .unwrap()
            .unwrap();
        assert_ne!(minutes.analysis_id, legacy.analysis_id);
        let record = super::super::read_record(root.path(), &minutes.analysis_id).unwrap();
        let generated = record.generated_text.unwrap();
        assert_eq!(generated.prompt_id, crate::AI_MINUTES_PROMPT_ID);
        assert_eq!(generated.prompt_version, AI_MINUTES_PROMPT_VERSION);
        assert_eq!(
            generated.output_limit.unwrap().maximum,
            MAX_SUMMARY_CHARACTERS
        );
    }

    #[test]
    fn minutes_survive_title_edits_and_body_edits_invalidate_them() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "retitled-minutes", "webpage");
        let (item, first) = (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone());
        enqueue_item_summary(root.path(), &item, &first).unwrap();
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        queue.request_minutes(&item, &first).unwrap();
        let mut model = ProfileModel { prompts: vec![] };
        assert!(queue.process_next_authorized(&mut model).unwrap());
        let minutes = validated_summary(root.path(), &item, &first).unwrap().unwrap();
        let mut revision = first.clone();
        for title in ["Renamed synthetic episode", "Renamed synthetic episode, twice"] {
            revision = memex_store::revise_item_metadata(
                root.path(),
                &item,
                memex_store::ItemMetadataUpdate {
                    expected_revision_id: revision.clone(),
                    title: title.into(),
                    manual_tags: vec!["episode".into()],
                },
            )
            .unwrap()
            .revision_id;
        }
        assert_eq!(
            validated_summary(root.path(), &item, &revision).unwrap(),
            Some(minutes.clone())
        );
        assert_eq!(
            summary_status(root.path(), &item, &revision).unwrap().unwrap().state,
            SummaryJobState::Succeeded
        );
        assert_eq!(
            summary_prompt_version(root.path(), &item, &revision).unwrap().as_deref(),
            Some(AI_MINUTES_PROMPT_VERSION)
        );
        // A re-request queues nothing; a legacy duplicate row is settled without a request.
        enqueue_item_summary(root.path(), &item, &revision).unwrap();
        queue.request_minutes(&item, &revision).unwrap();
        assert!(!queue.process_next_authorized(&mut model).unwrap());
        let (_, current) = memex_store::read_item(root.path(), &item).unwrap();
        let duplicate = queue.enqueue_verified(&item, &current, "en").unwrap();
        queue.authorize(&item, &revision).unwrap();
        assert!(queue.process_next_authorized(&mut model).unwrap());
        assert_eq!(model.prompts.len(), 1, "minutes are generated once");
        assert_eq!(
            queue.get(&duplicate.job_id).unwrap().reason_code.as_deref(),
            Some(crate::translation::INHERITED)
        );
        assert_eq!(
            validated_summary(root.path(), &item, &revision).unwrap(),
            Some(minutes)
        );
        let edited = memex_store::edit_markdown(
            root.path(),
            memex_store::MarkdownEdit {
                item_id: item.clone(),
                expected_revision_id: revision.clone(),
                request_id: "minutes-body-change".into(),
                markdown: format!("{} More text.", current.content_markdown),
            },
            Sensitivity::Internal,
        )
        .unwrap();
        assert!(validated_summary(root.path(), &item, &edited.revision_id)
            .unwrap()
            .is_none());
        assert!(summary_status(root.path(), &item, &edited.revision_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn summary_queue_is_idempotent_durable_and_revision_bound() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "durable", "document");
        let first = enqueue_ingest_summaries(root.path(), &receipt).unwrap();
        let second = enqueue_ingest_summaries(root.path(), &receipt).unwrap();
        assert_eq!(first, second);
        let SummaryEnqueueOutcome::Enqueued { job } = first[0].clone() else {
            panic!("summary was not queued")
        };
        assert_eq!(job.state, SummaryJobState::Queued);
        assert!(!summary_authorized(root.path(), &job.item_id, &job.revision_id).unwrap());
        assert_eq!(
            summary_status(root.path(), &job.item_id, &job.revision_id).unwrap(),
            Some(job)
        );
    }

    #[test]
    fn succeeded_summary_requires_exact_validated_analysis_record() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "validated", "webpage");
        let SummaryEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_summaries(root.path(), &receipt).unwrap()[0].clone()
        else {
            panic!("summary was not queued")
        };
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        assert!(queue
            .process_next(&mut SyntheticSummaryModel { fail_once: false })
            .unwrap());
        assert_eq!(
            queue.get(&job.job_id).unwrap().state,
            SummaryJobState::Succeeded
        );
        let validated = validated_summary(root.path(), &job.item_id, &job.revision_id)
            .unwrap()
            .unwrap();
        let record = super::super::read_record(root.path(), &validated.analysis_id).unwrap();
        assert_eq!(record.analysis_kind, AnalysisKind::Summary);
        assert_eq!(
            record
                .generated_text
                .as_ref()
                .and_then(|generated| generated.summary_job_id.as_deref()),
            Some(job.job_id.as_str())
        );
        assert!(
            validated_summary(root.path(), &job.item_id, "revision_stale")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn provider_timeout_retries_to_validated_success() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "retry", "webpage");
        let SummaryEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_summaries(root.path(), &receipt).unwrap()[0].clone()
        else {
            panic!("summary was not queued")
        };
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        let mut model = SyntheticSummaryModel { fail_once: true };
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(
            queue.get(&job.job_id).unwrap().state,
            SummaryJobState::Failed
        );
        assert!(queue.retry_one_eligible(Duration::ZERO).unwrap());
        assert!(queue.process_next(&mut model).unwrap());
        assert_eq!(
            queue.get(&job.job_id).unwrap().state,
            SummaryJobState::Succeeded
        );
        assert!(
            validated_summary(root.path(), &job.item_id, &job.revision_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn stale_revision_digest_fails_before_model_or_analysis() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "stale", "webpage");
        let SummaryEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_summaries(root.path(), &receipt).unwrap()[0].clone()
        else {
            panic!("summary was not queued")
        };
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        queue
            .connection
            .execute(
                "UPDATE summary_jobs SET revision_sha256=?2 WHERE job_id=?1",
                params![job.job_id, format!("sha256:{}", "0".repeat(64))],
            )
            .unwrap();
        struct MustNotRun;
        impl SummaryModel for MustNotRun {
            fn descriptor(&self) -> SummaryModelDescriptor {
                SyntheticSummaryModel { fail_once: false }.descriptor()
            }
            fn summarize(
                &mut self,
                _: SummaryModelInput<'_>,
            ) -> std::result::Result<String, SummaryModelError> {
                panic!("stale source reached model")
            }
        }
        assert!(queue.process_next(&mut MustNotRun).unwrap());
        let failed = queue.get(&job.job_id).unwrap();
        assert_eq!(failed.reason_code.as_deref(), Some("stale_revision"));
        assert!(!failed.retryable);
        assert!(list_records(root.path()).unwrap().is_empty());
    }

    #[test]
    fn malformed_summary_never_becomes_analysis() {
        let root = tempfile::tempdir().unwrap();
        let receipt = ingest_source(root.path(), "malformed", "webpage");
        let SummaryEnqueueOutcome::Enqueued { job } =
            enqueue_ingest_summaries(root.path(), &receipt).unwrap()[0].clone()
        else {
            panic!("summary was not queued")
        };
        struct EchoModel;
        impl SummaryModel for EchoModel {
            fn descriptor(&self) -> SummaryModelDescriptor {
                SyntheticSummaryModel { fail_once: false }.descriptor()
            }
            fn summarize(
                &mut self,
                input: SummaryModelInput<'_>,
            ) -> std::result::Result<String, SummaryModelError> {
                Ok(input.source_text().into())
            }
        }
        let mut queue = SummaryQueue::open(root.path()).unwrap();
        assert!(queue.process_next(&mut EchoModel).unwrap());
        assert_eq!(
            queue.get(&job.job_id).unwrap().state,
            SummaryJobState::Failed
        );
        assert!(
            validated_summary(root.path(), &job.item_id, &job.revision_id)
                .unwrap()
                .is_none()
        );
    }
}
