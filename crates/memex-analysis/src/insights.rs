//! AI insights ("ai_insights_v1"): highlights, golden quotes and a model-knowledge fact check
//! of one revision. Rows exist only for an explicit owner `insight_request` (the background
//! reconciler never creates them), so every queued row is provider-authorized.
//!
//! This module owns durable state only. The provider adapter (memex-agent) splits the source,
//! calls the model and validates every anchor before returning `InsightOutput`; the stored items
//! are bound to the exact revision digest and the kind's prompt version. Highlights (v2) are
//! also bound to the translation document they anchor into (`translation_analysis_id`).

use super::revision_ref;
use crate::translation::{
    model_error_reason, open_status_database, secure_database_family, validate_database_family,
    TranslationModelDescriptor, TranslationModelError, TranslationQueue, DATABASE_NAME,
};
use anyhow::{bail, Result};
use chrono::Utc;
use memex_core::ItemRevision;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Attempts of one request (first + automatic retries).
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BASE_MS: i64 = 30_000;

/// SQL condition: a requeued attempt waits `RETRY_BASE_MS * 2^(attempts-1)` since its failure.
fn retry_due(now: chrono::DateTime<Utc>) -> String {
    let now = now.to_rfc3339();
    format!(
        "(insight_jobs.attempt_count=0 OR insight_jobs.reason_code IS NULL
          OR (julianday('{now}')-julianday(insight_jobs.updated_at))*86400000.0
             >= {RETRY_BASE_MS}.0*(1 << (insight_jobs.attempt_count-1)))"
    )
}

/// Prompt (job contract) version of quotes and fact check.
pub const INSIGHT_PROMPT_VERSION: &str = "1";
/// Highlights v2: key terms (not sentences), on the original and on the Chinese translation.
/// A v1 row never satisfies a v2 read or request.
pub const HIGHLIGHTS_PROMPT_VERSION: &str = "2";
pub const HIGHLIGHTS_PROMPT: &str = include_str!("../prompts/insights-highlights-v2.txt");
pub const QUOTES_PROMPT: &str = include_str!("../prompts/insights-quotes-v1.txt");
pub const FACT_CHECK_PROMPT: &str = include_str!("../prompts/insights-fact-check-v1.txt");
/// Several sectioned requests of up to 120 s each may run under one lease.
const LEASE_SECONDS: i64 = 30 * 60;
const MAX_ITEMS_JSON_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightKind {
    Highlights,
    Quotes,
    FactCheck,
}

impl InsightKind {
    pub const ALL: [Self; 3] = [Self::Highlights, Self::Quotes, Self::FactCheck];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highlights => "highlights",
            Self::Quotes => "quotes",
            Self::FactCheck => "fact_check",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Highlights => HIGHLIGHTS_PROMPT,
            Self::Quotes => QUOTES_PROMPT,
            Self::FactCheck => FACT_CHECK_PROMPT,
        }
    }

    /// The job contract version stored with (and required of) every row of this kind.
    pub fn prompt_version(self) -> &'static str {
        match self {
            Self::Highlights => HIGHLIGHTS_PROMPT_VERSION,
            Self::Quotes | Self::FactCheck => INSIGHT_PROMPT_VERSION,
        }
    }

    pub fn prompt_id(self) -> &'static str {
        match self {
            Self::Highlights => "memex.insights-highlights",
            Self::Quotes => "memex.insights-quotes",
            Self::FactCheck => "memex.insights-fact-check",
        }
    }
}

/// Source text is not `Debug`/`Serialize`; only the queue constructs it.
pub struct InsightModelInput<'a> {
    kind: InsightKind,
    title: &'a str,
    markdown: &'a str,
    source_language: &'static str,
    translation: Option<&'a str>,
}

impl<'a> InsightModelInput<'a> {
    pub fn kind(&self) -> InsightKind {
        self.kind
    }
    pub fn prompt(&self) -> &'static str {
        self.kind.prompt()
    }
    pub fn title(&self) -> &'a str {
        self.title
    }
    pub fn markdown(&self) -> &'a str {
        self.markdown
    }
    /// Best-effort BCP 47 tag (`zh-CN` for Chinese sources).
    pub fn source_language(&self) -> &'static str {
        self.source_language
    }
    /// Highlights only: the revision's validated Chinese translation document (same block count
    /// and order as `markdown`), when one exists.
    pub fn translation(&self) -> Option<&'a str> {
        self.translation
    }
}

/// Validated items (a JSON array) and the number of model items the validator rejected.
pub struct InsightOutput {
    pub items: serde_json::Value,
    pub dropped: u32,
    /// Highlights: the items carry translation spans (the given translation was usable).
    pub translation_used: bool,
}

pub trait InsightModel {
    fn descriptor(&self) -> TranslationModelDescriptor;
    fn generate(
        &mut self,
        input: InsightModelInput<'_>,
    ) -> std::result::Result<InsightOutput, TranslationModelError>;
}

/// Current-revision projection of one kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightView {
    /// not_requested | queued | running | succeeded | failed | unavailable
    pub state: String,
    pub reason: Option<String>,
    pub items: Option<serde_json::Value>,
    pub dropped: u32,
    /// Succeeded highlights only: `original_and_translation` | `original_only` (no usable
    /// Chinese translation existed when they were generated).
    pub sides: Option<String>,
}

impl InsightView {
    pub fn of(state: &str, reason: Option<&str>) -> Self {
        Self {
            state: state.into(),
            reason: reason.map(Into::into),
            items: None,
            dropped: 0,
            sides: None,
        }
    }
}

/// Document analysis id of the revision's validated translation (never for other kinds).
fn current_translation(root: &Path, revision: &ItemRevision) -> Option<String> {
    crate::translation::validated_translation_pair(root, &revision.item_id, &revision.revision_id)
        .ok()
        .flatten()
        .map(|pair| pair.document_analysis_id)
}

pub struct InsightQueue {
    connection: Connection,
    root: PathBuf,
}

/// A leased job.
pub struct InsightJob {
    pub item_id: String,
    pub revision_id: String,
    pub kind: InsightKind,
    revision_sha256: String,
    lease_owner: String,
}

impl InsightQueue {
    pub fn open(library: impl AsRef<Path>) -> Result<Self> {
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
             CREATE TABLE IF NOT EXISTS insight_jobs(
               item_id TEXT NOT NULL,
               revision_id TEXT NOT NULL,
               kind TEXT NOT NULL CHECK(kind IN ('highlights','quotes','fact_check')),
               revision_sha256 TEXT NOT NULL,
               prompt_version TEXT NOT NULL,
               state TEXT NOT NULL CHECK(state IN ('queued','running','succeeded','failed')),
               reason_code TEXT,
               items_json TEXT,
               dropped INTEGER NOT NULL DEFAULT 0,
               model_id TEXT,
               adapter_version TEXT,
               lease_until INTEGER,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(item_id,revision_id,kind)
             );",
        )?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        // Additive: the translation document a succeeded highlights row anchors into.
        if !crate::translation::has_column(&connection, "insight_jobs", "translation_analysis_id")?
        {
            connection.execute_batch(
                "ALTER TABLE insight_jobs ADD COLUMN translation_analysis_id TEXT",
            )?;
        }
        if !crate::translation::has_column(&connection, "insight_jobs", "lease_owner")? {
            connection.execute_batch("ALTER TABLE insight_jobs ADD COLUMN lease_owner TEXT")?;
        }
        // Additive: attempts of the current request, for the bounded automatic retry.
        if !crate::translation::has_column(&connection, "insight_jobs", "attempt_count")? {
            connection.execute_batch(
                "ALTER TABLE insight_jobs ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        connection.execute_batch("COMMIT")?;
        validate_database_family(&database)?;
        secure_database_family(&database)?;
        Ok(Self { connection, root })
    }

    /// Explicit owner request for one kind of this revision. A current succeeded or in-flight
    /// job is kept; a failed or out-of-date (digest or prompt version) job restarts, and so do
    /// succeeded highlights whose translation side no longer matches the revision's current
    /// translation (for example one that succeeded after them).
    pub fn request(&mut self, revision: &ItemRevision, kind: InsightKind) -> Result<()> {
        let digest = revision_ref(&revision.item_id, revision)?.revision_sha256;
        let now = Utc::now().to_rfc3339();
        let translation = (kind == InsightKind::Highlights)
            .then(|| current_translation(&self.root, revision))
            .flatten();
        let view = insight_view(&self.root, revision, kind)?;
        if view.state == "succeeded"
            && (kind != InsightKind::Highlights
                || translation.is_none()
                || view.sides.as_deref() == Some("original_and_translation"))
        {
            return Ok(());
        }
        self.connection.execute(
            "INSERT INTO insight_jobs(item_id,revision_id,kind,revision_sha256,prompt_version,
                                      state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,'queued',?6,?6)
             ON CONFLICT(item_id,revision_id,kind) DO UPDATE
               SET state='queued',reason_code=NULL,items_json=NULL,dropped=0,model_id=NULL,
                   adapter_version=NULL,lease_until=NULL,lease_owner=NULL,attempt_count=0,
                   revision_sha256=excluded.revision_sha256,
                   prompt_version=excluded.prompt_version,updated_at=excluded.updated_at
             WHERE insight_jobs.state='failed'
                -- An explicit request skips the backoff of a pending automatic retry.
                OR (insight_jobs.state='queued' AND insight_jobs.reason_code IS NOT NULL)
                OR insight_jobs.revision_sha256<>excluded.revision_sha256
                OR (insight_jobs.prompt_version<>excluded.prompt_version
                    AND insight_jobs.state<>'running')
                OR (insight_jobs.kind='highlights' AND insight_jobs.state='succeeded'
                    AND insight_jobs.translation_analysis_id IS NOT ?7)",
            params![
                revision.item_id,
                revision.revision_id,
                kind.as_str(),
                digest,
                kind.prompt_version(),
                now,
                translation
            ],
        )?;
        Ok(())
    }

    /// Kinds with queued work (expired leases are failed first).
    pub fn queued_kinds(&mut self) -> Result<Vec<InsightKind>> {
        self.expire()?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT DISTINCT kind FROM insight_jobs WHERE state='queued'
               AND prompt_version=CASE kind WHEN 'highlights' THEN ?1 ELSE ?2 END
               AND {} AND {} ORDER BY kind",
            crate::translation::metadata_gate("insight_jobs", Utc::now()),
            retry_due(Utc::now())
        ))?;
        let kinds = statement
            .query_map(
                params![HIGHLIGHTS_PROMPT_VERSION, INSIGHT_PROMPT_VERSION],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(kinds
            .iter()
            .filter_map(|kind| InsightKind::parse(kind))
            .collect())
    }

    /// A lease that expired has an uncertain outcome: retried automatically within the bounded
    /// attempts (see `retry_or_fail`), then failed.
    fn expire(&mut self) -> Result<()> {
        let now = Utc::now();
        self.connection.execute(
            "UPDATE insight_jobs
                SET state=CASE WHEN attempt_count<?3 THEN 'queued' ELSE 'failed' END,
                    reason_code='provider_outcome_unknown',
                    lease_until=NULL,lease_owner=NULL,updated_at=?2
              WHERE state='running' AND lease_until<?1",
            params![now.timestamp(), now.to_rfc3339(), MAX_ATTEMPTS],
        )?;
        Ok(())
    }

    /// A failed, uncertain or malformed model step: queued again (after a backoff, see
    /// `retry_due`) while attempts remain, else failed with the reason.
    fn retry_or_fail(&mut self, job: &InsightJob, reason: &str) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE insight_jobs
                SET state=CASE WHEN attempt_count<?5 THEN 'queued' ELSE 'failed' END,
                    reason_code=?4,lease_until=NULL,lease_owner=NULL,updated_at=?6
              WHERE item_id=?1 AND lease_owner=?2 AND kind=?3 AND state='running'",
            params![
                job.item_id,
                job.lease_owner,
                job.kind.as_str(),
                reason,
                MAX_ATTEMPTS,
                Utc::now().to_rfc3339()
            ],
        )?;
        if changed == 0 {
            bail!("insight_lease_lost")
        }
        Ok(())
    }

    fn lease(&mut self, kind: InsightKind) -> Result<Option<InsightJob>> {
        let now = Utc::now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = transaction
            .query_row(
                &format!(
                    "SELECT item_id,revision_id,revision_sha256 FROM insight_jobs
                      WHERE state='queued' AND kind=?1 AND prompt_version=?2 AND {} AND {}
                        AND NOT EXISTS(SELECT 1 FROM insight_jobs sibling
                              WHERE sibling.item_id=insight_jobs.item_id
                                AND sibling.kind=insight_jobs.kind AND sibling.state='running')
                      ORDER BY created_at,item_id LIMIT 1",
                    crate::translation::metadata_gate("insight_jobs", now),
                    retry_due(now)
                ),
                params![kind.as_str(), kind.prompt_version()],
                |row| {
                    Ok(InsightJob {
                        item_id: row.get(0)?,
                        revision_id: row.get(1)?,
                        kind,
                        revision_sha256: row.get(2)?,
                        lease_owner: memex_core::new_id("insight_lease"),
                    })
                },
            )
            .optional()?;
        if let Some(job) = &job {
            transaction.execute(
                "UPDATE insight_jobs SET state='running',lease_until=?4,updated_at=?5,lease_owner=?6,
                        attempt_count=attempt_count+1
                  WHERE item_id=?1 AND revision_id=?2 AND kind=?3 AND state='queued'",
                params![
                    job.item_id,
                    job.revision_id,
                    kind.as_str(),
                    now.timestamp() + LEASE_SECONDS,
                    now.to_rfc3339(),
                    job.lease_owner
                ],
            )?;
        }
        transaction.commit()?;
        Ok(job)
    }

    fn finish(
        &mut self,
        job: &InsightJob,
        state: &str,
        reason: Option<&str>,
        output: Option<(&str, u32, &TranslationModelDescriptor, Option<&str>)>,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE insight_jobs
                SET state=CASE WHEN ?4='failed' AND ?5='stale_revision' AND revision_id<>?12
                               THEN 'queued' ELSE ?4 END,
                    reason_code=?5,items_json=?6,dropped=?7,model_id=?8,
                    adapter_version=?9,lease_until=NULL,lease_owner=NULL,updated_at=?10,
                    translation_analysis_id=?11
              WHERE item_id=?1 AND lease_owner=?2 AND kind=?3 AND state='running'",
            params![
                job.item_id,
                job.lease_owner,
                job.kind.as_str(),
                state,
                reason,
                output.map(|(items, _, _, _)| items),
                output.map_or(0, |(_, dropped, _, _)| dropped),
                output.map(|(_, _, model, _)| model.model_id.as_str()),
                output.map(|(_, _, model, _)| model.adapter_version.as_str()),
                Utc::now().to_rfc3339(),
                output.and_then(|(_, _, _, translation)| translation),
                job.revision_id
            ],
        )?;
        if changed == 0 {
            bail!("insight_lease_lost")
        }
        Ok(())
    }

    /// The job's revision while its body is still the current body (metadata-only edits since
    /// neither stop nor supersede it; its result stays bound to the job's revision).
    fn current(&self, job: &InsightJob) -> Result<Option<ItemRevision>> {
        crate::lineage::job_source(&self.root, &job.item_id, &job.revision_id, &job.revision_sha256)
    }

    /// Processes the oldest queued job of `kind`. No database lock is held across model calls.
    pub fn process_next<M: InsightModel>(
        &mut self,
        kind: InsightKind,
        model: &mut M,
    ) -> Result<bool> {
        let Some(job) = self.lease(kind)? else {
            return Ok(false);
        };
        let descriptor = model.descriptor();
        if !descriptor.is_valid() {
            self.finish(&job, "failed", Some("invalid_provider_configuration"), None)?;
            return Ok(true);
        }
        let Some(revision) = self.current(&job)? else {
            self.finish(&job, "failed", Some("stale_revision"), None)?;
            return Ok(true);
        };
        // A duplicate of a metadata-only revision: a same-body revision's result is inherited.
        let own = InsightView::of("not_requested", None);
        let view = insight_view(&self.root, &revision, kind).unwrap_or(own);
        if view.state == "succeeded" {
            self.finish(&job, "failed", Some(crate::translation::INHERITED), None)?;
            return Ok(true);
        }
        let source_language = crate::source_language(&revision.title, &revision.content_markdown);
        // Highlights also mark the Chinese translation when a validated one exists now.
        let translation = match kind {
            InsightKind::Highlights => current_translation(&self.root, &revision).and_then(|id| {
                super::read_record(&self.root, &id)
                    .ok()
                    .map(|record| (id, record.body_markdown))
            }),
            InsightKind::Quotes | InsightKind::FactCheck => None,
        };
        let output = model.generate(InsightModelInput {
            kind,
            title: &revision.title,
            markdown: &revision.content_markdown,
            source_language,
            translation: translation.as_ref().map(|(_, text)| text.as_str()),
        });
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let (reason, retryable) = model_error_reason(error);
                if retryable {
                    self.retry_or_fail(&job, reason)?;
                } else {
                    self.finish(&job, "failed", Some(reason), None)?;
                }
                return Ok(true);
            }
        };
        let items = serde_json::to_string(&output.items)?;
        let translation_id = translation
            .as_ref()
            .filter(|_| output.translation_used)
            .map(|(id, _)| id.as_str());
        if !output.items.is_array() || items.len() > MAX_ITEMS_JSON_BYTES {
            self.retry_or_fail(&job, "malformed_model_output")?;
            return Ok(true);
        }
        if self.current(&job)?.is_none() {
            // Keep the paid result on its original row before transfer. Carry may have run
            // already or may run after this completion; neither ordering loses the result.
            let (_, current) = memex_store::read_item(&self.root, &job.item_id)?;
            if current.title == revision.title
                && current.content_markdown == revision.content_markdown
            {
                self.finish(
                    &job,
                    "succeeded",
                    None,
                    Some((&items, output.dropped, &descriptor, translation_id)),
                )?;
                self.carry(&job.revision_id, &current)?;
            } else {
                self.finish(&job, "failed", Some(crate::translation::SUPERSEDED), None)?;
            }
            return Ok(true);
        }
        self.finish(
            &job,
            "succeeded",
            None,
            Some((&items, output.dropped, &descriptor, translation_id)),
        )?;
        Ok(true)
    }

    /// Copies a metadata-only revision's state atomically. Running aliases share the exact
    /// attempt token and deadline: completion/expiry settles all aliases, never a second call.
    pub fn carry(&mut self, previous_revision_id: &str, current: &ItemRevision) -> Result<()> {
        self.connection.execute(
            "INSERT INTO insight_jobs(item_id,revision_id,kind,revision_sha256,prompt_version,
                                      state,reason_code,items_json,dropped,model_id,
                                      adapter_version,lease_until,created_at,updated_at,
                                      translation_analysis_id,lease_owner,attempt_count)
             SELECT item_id,?3,kind,?4,prompt_version,
                    CASE WHEN state='failed' AND reason_code='stale_revision' THEN 'queued' ELSE state END,
                    reason_code,items_json,dropped,
                    model_id,adapter_version,lease_until,created_at,?5,translation_analysis_id,lease_owner,
                    attempt_count
               FROM insight_jobs
              WHERE item_id=?1 AND revision_id=?2
                AND prompt_version=(CASE kind WHEN 'highlights' THEN ?7 ELSE ?6 END)
             ON CONFLICT(item_id,revision_id,kind) DO UPDATE
                SET state=excluded.state,reason_code=excluded.reason_code,items_json=excluded.items_json,
                    dropped=excluded.dropped,model_id=excluded.model_id,adapter_version=excluded.adapter_version,
                    lease_until=excluded.lease_until,lease_owner=excluded.lease_owner,
                    translation_analysis_id=excluded.translation_analysis_id,updated_at=excluded.updated_at,
                    attempt_count=excluded.attempt_count
              WHERE insight_jobs.state='queued' AND insight_jobs.prompt_version=excluded.prompt_version
                AND insight_jobs.revision_sha256=excluded.revision_sha256
                AND excluded.state IN ('running','succeeded')",
            params![
                current.item_id,
                previous_revision_id,
                current.revision_id,
                revision_ref(&current.item_id, current)?.revision_sha256,
                Utc::now().to_rfc3339(),
                INSIGHT_PROMPT_VERSION,
                HIGHLIGHTS_PROMPT_VERSION
            ],
        )?;
        Ok(())
    }
}

type InsightRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

/// Projection of one kind for `revision` (the caller's current revision).
pub fn insight_view(
    library: impl AsRef<Path>,
    revision: &ItemRevision,
    kind: InsightKind,
) -> Result<InsightView> {
    let Some((root, connection)) = open_status_database(library.as_ref())? else {
        return Ok(InsightView::of("not_requested", None));
    };
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='insight_jobs'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Ok(InsightView::of("not_requested", None));
    }
    let row_of = |revision: &ItemRevision| -> Result<Option<InsightRow>> {
        Ok(connection
        .query_row(
            &format!(
                "SELECT revision_sha256,prompt_version,state,reason_code,items_json,dropped,{}
                   FROM insight_jobs WHERE item_id=?1 AND revision_id=?2 AND kind=?3",
                // A database not yet opened by this version's queue has no such column.
                if crate::translation::has_column(
                    &connection,
                    "insight_jobs",
                    "translation_analysis_id"
                )? {
                    "translation_analysis_id"
                } else {
                    "NULL"
                }
            ),
            params![revision.item_id, revision.revision_id, kind.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?)
    };
    let own = row_of(revision)?;
    // A metadata-only revision shows the succeeded result of its newest same-body revision,
    // validated against that revision's digest below.
    let mut source = revision.clone();
    let mut row = own;
    if !row.as_ref().is_some_and(|row| row.2 == "succeeded") {
        for inherited in crate::lineage::inherited_revisions(
            &connection,
            &root,
            "insight_jobs",
            "AND kind=?3",
            kind.as_str(),
            &revision.item_id,
            &revision.revision_id,
        )? {
            if let Some(found) = row_of(&inherited)?.filter(|found| {
                found.2 == "succeeded" && found.1 == kind.prompt_version()
            }) {
                row = Some(found);
                source = inherited;
                break;
            }
        }
    }
    let revision = &source;
    let Some((digest, prompt_version, state, reason, items, dropped, translation)) = row else {
        return Ok(InsightView::of("not_requested", None));
    };
    if digest != revision_ref(&revision.item_id, revision)?.revision_sha256
        || prompt_version != kind.prompt_version()
    {
        return Ok(InsightView::of("not_requested", None));
    }
    if kind == InsightKind::Highlights && state == "succeeded" {
        if let Some(previous) = translation.as_deref() {
            let current = current_translation(library.as_ref(), revision);
            let unchanged = current.as_deref().is_some_and(|current| {
                current == previous
                    || match (
                        super::read_record(library.as_ref(), previous),
                        super::read_record(library.as_ref(), current),
                    ) {
                        // Metadata-only carry may change the analysis id without changing text.
                        (Ok(old), Ok(new)) => old.body_markdown == new.body_markdown,
                        _ => false,
                    }
            });
            if !unchanged {
                return Ok(InsightView::of(
                    "unavailable",
                    Some("insight_translation_changed"),
                ));
            }
        }
    }
    let dropped = u32::try_from(dropped).unwrap_or(0);
    Ok(match state.as_str() {
        "queued" | "running" => InsightView::of(&state, None),
        "failed" => InsightView::of("failed", reason.as_deref()),
        "succeeded" => match items
            .as_deref()
            .and_then(|items| serde_json::from_str::<serde_json::Value>(items).ok())
            .filter(serde_json::Value::is_array)
        {
            Some(items) => InsightView {
                state: "succeeded".into(),
                reason: None,
                items: Some(items),
                dropped,
                sides: (kind == InsightKind::Highlights).then(|| {
                    if translation.is_some() {
                        "original_and_translation".into()
                    } else {
                        "original_only".into()
                    }
                }),
            },
            None => InsightView::of("unavailable", Some("insights_unavailable")),
        },
        _ => InsightView::of("unavailable", Some("insights_unavailable")),
    })
}
