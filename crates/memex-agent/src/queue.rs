use crate::config::{ensure_owner_directory, sync_owner_directory};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

mod active_group;
mod chrome;
pub mod content_format;
mod lookup;
pub mod reader_authority;
pub mod reader_repair;
pub mod reader_translation;
pub mod reader_translation_map;
pub mod recent;

/// Automatic retries of a failed or uncertain model step (after the first attempt): bounded,
/// with exponential backoff, resuming from validated outputs. The owner uses a subscription
/// route, so an uncertain (possibly billed) request is retried instead of waiting for a click.
pub(crate) const AUTO_RETRIES: u32 = 2;
pub(crate) fn auto_retry_delay_ms(retry: u32) -> i64 {
    #[cfg(test)]
    if let Some(ms) = TEST_RETRY_DELAY_MS.with(|d| d.get()) {
        return ms;
    }
    30_000i64 << retry.saturating_sub(1).min(4)
}
/// A definite provider refusal (rejection, invalid request, scope or model mismatch) repeats
/// deterministically, so it is never retried automatically. Format errors (invalid or
/// incomplete response, unparsable output) and uncertain outcomes are.
pub(crate) fn auto_retryable(error: &anyhow::Error) -> bool {
    use memex_provider::generation::GenerationError as E;
    !matches!(
        error.downcast_ref::<E>(),
        Some(E::Rejected | E::InvalidRequest | E::CredentialScopeMismatch | E::ModelMismatch)
    )
}
#[cfg(test)]
thread_local! {
    /// Test-only: makes automatic retries due immediately on this thread.
    pub(crate) static TEST_RETRY_DELAY_MS: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
}
pub use chrome::{ChromeIntake, ChromeOptions};

fn reader_item_busy(conn: &Connection, library: &Path, item: &str) -> Result<bool> {
    let busy: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM reader_repair_jobs WHERE library=?1 AND item_id=?2 AND state IN ('queued','running') UNION ALL SELECT 1 FROM reader_translation_jobs WHERE library=?1 AND item_id=?2 AND state IN ('queued','running') UNION ALL SELECT 1 FROM content_format_active WHERE library=?1 AND item_id=?2)",
        params![library.to_string_lossy(), item], |r| r.get(0))?;
    if busy { return Ok(true) }
    // New synthetic contracts do not migrate a production queue just by opening
    // it. If their tables exist, reservations still fence all older writers.
    for table in ["reader_repair_batches", "reader_translation_map_jobs"] {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table], |r| r.get(0))?;
        if exists && conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE library=?1 AND item_id=?2 AND state IN ('queued','running','stopping','outcome_unknown'))"),
            params![library.to_string_lossy(),item], |r|r.get::<_,bool>(0))? { return Ok(true) }
    }
    Ok(false)
}

pub const LIST_PAGE_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Leased,
    ArtifactReady,
    NeedsUser,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Extract,
    Resolve,
    Fetch,
    Validate,
    Translate,
    Commit,
}
impl State {
    fn text(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::ArtifactReady => "artifact_ready",
            Self::NeedsUser => "needs_user",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
impl Stage {
    fn text(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Resolve => "resolve",
            Self::Fetch => "fetch",
            Self::Validate => "validate",
            Self::Translate => "translate",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewMetadata {
    pub host: String,
    pub path_class: String,
    pub query_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobView {
    pub job_id: String,
    pub state: State,
    pub stage: Stage,
    pub reason_code: Option<String>,
    pub item_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub attempt_count: u32,
    pub preview: Option<PreviewMetadata>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct JobRecord {
    pub view: JobView,
    pub url: String,
    pub external_capture_id: Option<String>,
    pub lease_owner: Option<String>,
    pub artifact_digest: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub artifact_media: Option<String>,
    pub byte_size: Option<u64>,
    pub captured_at: Option<String>,
    pub final_url: Option<String>,
    pub http_status: Option<u16>,
    pub admission_class: Option<String>,
    pub admission_reason: Option<String>,
    pub policy_version: Option<String>,
    pub preview_host: Option<String>,
    pub preview_path_class: Option<String>,
    pub preview_query: Option<bool>,
    pub approval_digest: Option<String>,
    pub approval_policy: Option<String>,
    pub result_revision_id: Option<String>,
    pub translation_hook_completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranslationIntent {
    pub job_id: String,
    pub item_id: String,
    pub revision_id: String,
}
#[derive(Debug)]
pub struct IdentityConflict;
impl std::fmt::Display for IdentityConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request_identity_conflict")
    }
}
impl std::error::Error for IdentityConflict {}

pub struct Queue {
    conn: Connection,
    path: PathBuf,
}
impl Queue {
    pub(crate) fn database_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn model_settings(&self) -> Result<memex_models::Snapshot> {
        memex_models::read(&self.conn)
    }

    pub(crate) fn replace_model_settings(
        &mut self,
        expected_revision: u64,
        configuration: memex_models::Configuration,
    ) -> Result<memex_models::Snapshot> {
        memex_models::replace(&mut self.conn, expected_revision, configuration)
    }

    // Serialize Keychain CAS with settings replacement, including another daemon
    // using this database. No secret or Keychain payload is written to SQLite.
    pub(crate) fn with_model_settings<T>(
        &mut self,
        expected_revision: u64,
        operation: impl FnOnce(&Path, memex_models::Snapshot) -> Result<T>,
    ) -> Result<T> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = memex_models::read(&transaction)?;
        if snapshot.revision != expected_revision {
            bail!("stale_model_settings");
        }
        operation(&self.path, snapshot)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let parent = path.parent().context("unsafe_database")?;
        ensure_owner_directory(parent)?;
        let file_name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .context("unsafe_database")?;
        let parent = parent.canonicalize()?;
        let path = parent.join(file_name);
        validate_database_family(&path)?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS reader_repair_jobs(job_id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, library TEXT NOT NULL, item_id TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS reader_repair_active_item ON reader_repair_jobs(library,item_id) WHERE state IN ('queued','running');")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS reader_translation_quotes(quote_id TEXT PRIMARY KEY, payload TEXT NOT NULL);")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS reader_translation_jobs(job_id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, library TEXT NOT NULL, item_id TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS reader_translation_active_item ON reader_translation_jobs(library,item_id) WHERE state IN ('queued','running');")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS content_format_active(library TEXT NOT NULL,item_id TEXT NOT NULL,batch_id TEXT NOT NULL,PRIMARY KEY(library,item_id));")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
CREATE TABLE IF NOT EXISTS jobs(job_id TEXT PRIMARY KEY,producer TEXT NOT NULL,producer_job_id TEXT NOT NULL,request_digest TEXT NOT NULL,url TEXT NOT NULL,external_capture_id TEXT,state TEXT NOT NULL,stage TEXT NOT NULL,reason_code TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,lease_owner TEXT,lease_until INTEGER,next_attempt_at INTEGER,artifact_digest TEXT,artifact_path TEXT,artifact_media TEXT,byte_size INTEGER,captured_at TEXT,final_url TEXT,http_status INTEGER,admission_class TEXT,admission_reason TEXT,policy_version TEXT,preview_host TEXT,preview_path_class TEXT,preview_query INTEGER,approval_digest TEXT,approval_policy TEXT,attempt_count INTEGER NOT NULL DEFAULT 0,result_item_id TEXT,result_revision_id TEXT,translation_hook_completed INTEGER NOT NULL DEFAULT 0,UNIQUE(producer,producer_job_id));
CREATE TABLE IF NOT EXISTS attempts(id INTEGER PRIMARY KEY,job_id TEXT NOT NULL REFERENCES jobs(job_id),stage TEXT NOT NULL,reason_code TEXT,started_at TEXT NOT NULL,finished_at TEXT);
CREATE TABLE IF NOT EXISTS conflicts(id INTEGER PRIMARY KEY,producer TEXT NOT NULL,producer_job_id TEXT NOT NULL,request_digest TEXT,reason_code TEXT NOT NULL,created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS capture_origins(origin TEXT PRIMARY KEY,next_fetch_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS capture_origin_leases(origin TEXT PRIMARY KEY,job_id TEXT NOT NULL REFERENCES jobs(job_id));
CREATE TABLE IF NOT EXISTS chrome_episode_requests(producer TEXT NOT NULL,request_key TEXT NOT NULL,request_digest TEXT NOT NULL,job_id TEXT NOT NULL REFERENCES jobs(job_id),PRIMARY KEY(producer,request_key));
CREATE TABLE IF NOT EXISTS chrome_episode_jobs(producer TEXT NOT NULL,url TEXT NOT NULL,request_digest TEXT NOT NULL,job_id TEXT NOT NULL REFERENCES jobs(job_id),PRIMARY KEY(producer,url));
CREATE TABLE IF NOT EXISTS chrome_episode_work(job_id TEXT PRIMARY KEY REFERENCES jobs(job_id),payload TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS jobs_list_order ON jobs(created_at,job_id);
CREATE TABLE IF NOT EXISTS dedup_plans(plan_id TEXT PRIMARY KEY,created_at TEXT NOT NULL,plan_json TEXT NOT NULL,progress_json TEXT,result_json TEXT);")?;
        // Additive migration for the scaffold schema.
        for (name, ty) in [
            ("external_capture_id", "TEXT"),
            ("lease_owner", "TEXT"),
            ("next_attempt_at", "INTEGER"),
            ("artifact_media", "TEXT"),
            ("byte_size", "INTEGER"),
            ("captured_at", "TEXT"),
            ("final_url", "TEXT"),
            ("http_status", "INTEGER"),
            ("admission_class", "TEXT"),
            ("admission_reason", "TEXT"),
            ("preview_host", "TEXT"),
            ("preview_path_class", "TEXT"),
            ("preview_query", "INTEGER"),
            ("approval_digest", "TEXT"),
            ("approval_policy", "TEXT"),
            ("result_item_id", "TEXT"),
            ("result_revision_id", "TEXT"),
            ("translation_hook_completed", "INTEGER NOT NULL DEFAULT 0"),
            ("chrome_options", "TEXT"),
            ("chrome_result", "TEXT"),
            ("fetch_origin", "TEXT"),
        ] {
            if !has_column(&conn, name)? {
                conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {name} {ty}"), [])?;
            }
        }
        if !has_table_column(&conn, "conflicts", "request_digest")? {
            conn.execute("ALTER TABLE conflicts ADD COLUMN request_digest TEXT", [])?;
        }
        conn.execute_batch(
            "UPDATE jobs SET state='artifact_ready',stage='translate',reason_code='commit_retry',
                    result_item_id=NULL,result_revision_id=NULL,translation_hook_completed=0,
                    next_attempt_at=0,lease_owner=NULL,lease_until=NULL
             WHERE state='succeeded' AND result_revision_id IS NULL
             AND external_capture_id IS NOT NULL AND artifact_digest IS NOT NULL AND artifact_path IS NOT NULL
             AND artifact_media IS NOT NULL AND byte_size IS NOT NULL AND captured_at IS NOT NULL
             AND final_url IS NOT NULL AND http_status IS NOT NULL AND admission_class IS NOT NULL
             AND admission_reason IS NOT NULL AND policy_version IS NOT NULL;
             UPDATE jobs SET state='failed',stage='validate',reason_code='artifact_verification_failed',
                    result_item_id=NULL,result_revision_id=NULL,translation_hook_completed=0,
                    next_attempt_at=0,lease_owner=NULL,lease_until=NULL
             WHERE state='succeeded' AND result_revision_id IS NULL;",
        )?;
        let q = Self { conn, path };
        q.secure()?;
        sync_owner_directory(&parent)?;
        Ok(q)
    }
    fn secure(&self) -> Result<()> {
        chmod_db(&self.path)
    }
    pub fn enqueue(&mut self, producer: &str, producer_job_id: &str, url: &str) -> Result<JobView> {
        self.enqueue_options(producer, producer_job_id, url, None, || Ok(()))
    }
    fn enqueue_options(
        &mut self,
        producer: &str,
        producer_job_id: &str,
        url: &str,
        options: Option<&ChromeOptions>,
        admit_new_request: impl FnOnce() -> Result<()>,
    ) -> Result<JobView> {
        if !valid_identity(producer) || !valid_identity(producer_job_id) {
            bail!("invalid_request")
        }
        let (canonical_url, _) = crate::url_input::extract_url(url)?;
        if canonical_url != url {
            bail!("invalid_request")
        }
        let options_json = options.map(serde_json::to_string).transpose()?;
        let digest = if let Some(options) = options {
            hex::encode(Sha256::digest(serde_json::to_vec(&(
                canonical_url.as_str(),
                options,
            ))?))
        } else {
            hex::encode(Sha256::digest(canonical_url.as_bytes()))
        };
        let now = Utc::now().to_rfc3339();
        let mut admission_check = Some(admit_new_request);
        let mut admission_result = None;
        loop {
        // Serialize replay lookup and insertion across owner connections/restarts.
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((old, id, owner)) = transaction.query_row(
            "SELECT request_digest,job_id,producer FROM chrome_episode_requests WHERE request_key=?1",
            [producer_job_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        ).optional()? {
            if owner != producer { bail!("remote_resource_not_found"); }
            if old != digest { bail!(IdentityConflict); }
            transaction.commit()?;
            return self.get(&id);
        }
        if let Some((old, id, owner)) = transaction
            .query_row(
                "SELECT request_digest,job_id,producer FROM jobs WHERE producer_job_id=?2 AND (producer=?1 OR (?3 AND producer GLOB 'mobile_*') OR (?4 AND producer GLOB 'chrome_*'))",
                params![producer, producer_job_id, producer.starts_with("mobile_"), producer.starts_with("chrome_")],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?
        {
            // An unknown old request replayed after re-pairing cannot become a
            // second capture or disclose the original owner's receipt.
            if owner != producer { bail!("remote_resource_not_found"); }
            if old == digest {
                transaction.commit()?;
                return self.get(&id);
            }
            transaction.execute("INSERT INTO conflicts(producer,producer_job_id,request_digest,reason_code,created_at) VALUES(?1,?2,?3,'request_identity_conflict',?4)",params![producer,producer_job_id,digest,now])?;
            transaction.commit()?;
            bail!(IdentityConflict)
        }
        // Store admission may wait for a snapshot or scan a large library.
        // Never hold the queue writer lock during that work. Reconcile identity
        // again in the final transaction before applying even a rejection, so
        // an acceptance while admission was running still returns its receipt.
        if let Some(check) = admission_check.take() {
            transaction.commit()?;
            admission_result = Some(check());
            continue;
        }
        admission_result.take().expect("admission checked")?;
        let episode = options.is_some_and(|o| o.is_podcast(&canonical_url));
        // A changed request for an episode whose job already ended without success
        // (failed or cancelled) starts a new job and takes over the episode slot;
        // the old job and its keys stay as they were. While the job is live or after
        // it succeeded, a changed request is still a conflict.
        let mut replaces_ended = false;
        if episode {
            if let Some((old, id, state)) = transaction.query_row(
                "SELECT e.request_digest,e.job_id,j.state FROM chrome_episode_jobs e JOIN jobs j ON j.job_id=e.job_id WHERE e.producer=?1 AND e.url=?2",
                params![producer,canonical_url], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            ).optional()? {
                if old != digest {
                    if !matches!(state.as_str(), "failed" | "cancelled") { bail!(IdentityConflict); }
                    replaces_ended = true;
                } else {
                    transaction.execute("INSERT INTO chrome_episode_requests VALUES(?1,?2,?3,?4)",
                        params![producer,producer_job_id,digest,id])?;
                    transaction.commit()?;
                    return self.get(&id);
                }
            }
        }
        let id = format!("job_{}", uuid::Uuid::now_v7().simple());
        let origin = options
            .map(|_| url::Url::parse(&canonical_url).map(|url| url.origin().ascii_serialization()))
            .transpose()?;
        transaction.execute("INSERT INTO jobs(job_id,producer,producer_job_id,request_digest,url,state,stage,created_at,updated_at,next_attempt_at,chrome_options,fetch_origin) VALUES(?1,?2,?3,?4,?5,'queued','extract',?6,?6,COALESCE((SELECT next_fetch_at FROM capture_origins WHERE origin=?8),0),?7,?8)",params![id,producer,producer_job_id,digest,canonical_url,now,options_json,origin])?;
        if episode {
            if replaces_ended {
                transaction.execute("UPDATE chrome_episode_jobs SET request_digest=?3,job_id=?4 WHERE producer=?1 AND url=?2", params![producer,canonical_url,digest,id])?;
            } else {
                transaction.execute("INSERT INTO chrome_episode_jobs VALUES(?1,?2,?3,?4)", params![producer,canonical_url,digest,id])?;
            }
            transaction.execute("INSERT INTO chrome_episode_requests VALUES(?1,?2,?3,?4)", params![producer,producer_job_id,digest,id])?;
        }
        transaction.commit()?;
        self.secure()?;
        return self.get(&id);
        }
    }
    pub fn scoped_enqueue(
        &mut self,
        principal: &str,
        request_key: &str,
        url: &str,
    ) -> Result<JobView> {
        let producer = scoped_producer(principal)?;
        let key = request_key
            .strip_prefix("remote_mobile_")
            .context("invalid_request")?;
        if key.len() != 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !url.starts_with("https://")
        {
            bail!("invalid_request");
        }
        self.enqueue(&producer, request_key, url)
    }
    pub fn scoped_get(&self, principal: &str, id: &str) -> Result<JobView> {
        let producer = scoped_producer(principal)?;
        self.conn.query_row(
            "SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,admission_class,preview_host,preview_path_class,preview_query,result_item_id FROM jobs WHERE job_id=?1 AND producer=?2",
            params![id, producer], view_row,
        ).optional()?.context("remote_resource_not_found")
    }
    pub fn get(&self, id: &str) -> Result<JobView> {
        self.conn.query_row("SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,admission_class,preview_host,preview_path_class,preview_query,result_item_id FROM jobs WHERE job_id=?1",[id],view_row).context("job_not_found")
    }
    pub fn remote_get(&self, id: &str) -> Result<JobView> {
        self.conn
            .query_row("SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,admission_class,preview_host,preview_path_class,preview_query,result_item_id FROM jobs WHERE job_id=?1 AND producer='remote_iphone'", [id], view_row)
            .optional()?
            .context("remote_resource_not_found")
    }
    pub fn authorize_remote_item(&self, job_id: &str, item_id: &str) -> Result<()> {
        self.conn
            .query_row(
                "SELECT 1 FROM jobs WHERE job_id=?1 AND producer='remote_iphone' AND state='succeeded' AND stage='commit' AND result_item_id=?2",
                params![job_id, item_id],
                |_| Ok(()),
            )
            .optional()?
            .context("remote_resource_not_found")
    }
    pub fn list(&self, cursor: Option<&str>) -> Result<(Vec<JobView>, Option<String>)> {
        let (has_cursor, created_at, cursor_id) = match cursor {
            Some(id) => {
                let created_at = self
                    .conn
                    .query_row("SELECT created_at FROM jobs WHERE job_id=?1", [id], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()?
                    .context("invalid_cursor")?;
                (1, created_at, id)
            }
            None => (0, String::new(), ""),
        };
        let mut statement = self.conn.prepare("SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,admission_class,preview_host,preview_path_class,preview_query,result_item_id FROM jobs WHERE ?1=0 OR created_at>?2 OR (created_at=?2 AND job_id>?3) ORDER BY created_at,job_id LIMIT ?4")?;
        let mut rows = statement
            .query_map(
                params![has_cursor, created_at, cursor_id, LIST_PAGE_SIZE + 1],
                view_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_cursor = if rows.len() > LIST_PAGE_SIZE {
            rows.truncate(LIST_PAGE_SIZE);
            rows.last().map(|job| job.job_id.clone())
        } else {
            None
        };
        Ok((rows, next_cursor))
    }
    pub fn recover_leases(&mut self) -> Result<usize> {
        let timestamp = Utc::now().timestamp();
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE attempts SET reason_code='lease_recovered',finished_at=?2 WHERE finished_at IS NULL AND job_id IN (SELECT job_id FROM jobs WHERE state='leased' AND lease_until<?1)",params![timestamp,now])?;
        let n=tx.execute("UPDATE jobs SET state='retry_wait',reason_code='lease_recovered',lease_owner=NULL,lease_until=NULL,next_attempt_at=0,updated_at=?2 WHERE state='leased' AND lease_until<?1",params![timestamp,now])?;
        tx.commit()?;
        self.secure()?;
        Ok(n)
    }
    pub(crate) fn lease(&mut self, seconds: i64) -> Result<Option<JobRecord>> {
        self.recover_leases()?;
        self.lease_at(seconds, Utc::now().timestamp())
    }
    fn lease_at(&mut self, seconds: i64, now: i64) -> Result<Option<JobRecord>> {
        let owner = uuid::Uuid::now_v7().simple().to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE jobs SET next_attempt_at=MAX(COALESCE(next_attempt_at,0),COALESCE((SELECT next_fetch_at FROM capture_origins WHERE origin=jobs.fetch_origin),0)) WHERE chrome_options IS NOT NULL AND artifact_digest IS NULL AND state IN ('queued','retry_wait')", [])?;
        let id:Option<String>=tx.query_row("SELECT job_id FROM jobs j WHERE state IN ('queued','retry_wait','artifact_ready') AND lease_owner IS NULL AND COALESCE(next_attempt_at,0)<=?1 AND (chrome_options IS NULL OR ((SELECT COUNT(*) FROM jobs WHERE chrome_options IS NOT NULL AND state='leased')<2 AND (artifact_digest IS NOT NULL OR (NOT EXISTS(SELECT 1 FROM jobs other WHERE other.state='leased' AND other.fetch_origin=j.fetch_origin) AND NOT EXISTS(SELECT 1 FROM capture_origin_leases l JOIN jobs other ON other.job_id=l.job_id WHERE l.origin=j.fetch_origin AND other.state='leased' AND other.lease_until>?1))))) ORDER BY created_at,job_id LIMIT 1",[now],|r|r.get(0)).optional()?;
        if let Some(id) = id {
            let origin: Option<String> = tx.query_row("SELECT CASE WHEN artifact_digest IS NULL THEN fetch_origin END FROM jobs WHERE job_id=?1", [&id], |row| row.get(0))?;
            let seconds = if origin.is_some() {
                seconds.max(240)
            } else {
                seconds
            };
            if let Some(origin) = origin {
                let delay = 8 + i64::from(uuid::Uuid::now_v7().as_bytes()[15] % 18);
                tx.execute("INSERT INTO capture_origins(origin,next_fetch_at) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET next_fetch_at=MAX(next_fetch_at,excluded.next_fetch_at)", params![origin, now.saturating_add(delay)])?;
                tx.execute("INSERT INTO capture_origin_leases(origin,job_id) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET job_id=excluded.job_id", params![origin,id])?;
                tx.execute("UPDATE jobs SET next_attempt_at=MAX(COALESCE(next_attempt_at,0),?2) WHERE fetch_origin=?1 AND artifact_digest IS NULL AND state IN ('queued','retry_wait') AND job_id<>?3", params![origin, now.saturating_add(delay), id])?;
            }
            let started = Utc::now().to_rfc3339();
            tx.execute("UPDATE jobs SET state='leased',lease_owner=?2,lease_until=?3,attempt_count=attempt_count+1,updated_at=?4 WHERE job_id=?1",params![id,owner,now+seconds,started])?;
            tx.execute("INSERT INTO attempts(job_id,stage,started_at) SELECT job_id,stage,?2 FROM jobs WHERE job_id=?1",params![id,started])?;
            tx.commit()?;
            self.secure()?;
            return Ok(Some(self.record(&id)?));
        }
        tx.commit()?;
        Ok(None)
    }
    pub(crate) fn record(&self, id: &str) -> Result<JobRecord> {
        self.conn.query_row("SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,url,external_capture_id,lease_owner,artifact_digest,artifact_path,artifact_media,byte_size,captured_at,final_url,http_status,admission_class,admission_reason,policy_version,preview_host,preview_path_class,preview_query,approval_digest,approval_policy,result_item_id,result_revision_id,translation_hook_completed FROM jobs WHERE job_id=?1",[id],record_row).context("job_not_found")
    }
    pub(crate) fn stage(&mut self, j: &JobRecord, stage: Stage) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE jobs SET stage=?2,updated_at=?3 WHERE job_id=?1 AND state='leased' AND lease_owner=?4",
            params![j.view.job_id, stage.text(), now, j.lease_owner],
        )?;
        if changed != 1 {
            bail!("lease_lost")
        }
        tx.execute(
            "UPDATE attempts SET finished_at=?2 WHERE id=(SELECT id FROM attempts WHERE job_id=?1 AND finished_at IS NULL ORDER BY id DESC LIMIT 1)",
            params![j.view.job_id, now],
        )?;
        tx.execute(
            "INSERT INTO attempts(job_id,stage,started_at) VALUES(?1,?2,?3)",
            params![j.view.job_id, stage.text(), now],
        )?;
        tx.commit()?;
        self.secure()
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn freeze(
        &mut self,
        j: &JobRecord,
        external_capture_id: &str,
        digest: &str,
        path: &Path,
        media: &str,
        size: u64,
        captured: &str,
        final_url: &str,
        status: u16,
        class: &str,
        reason: &str,
        policy: &str,
        preview: Option<(&str, &str, bool)>,
    ) -> Result<()> {
        if !matches!(class, "article" | "needs_preview") {
            bail!("invalid_admission_transition")
        }
        let (h, p, q) = preview.map_or((None, None, None), |(h, p, q)| (Some(h), Some(p), Some(q)));
        let needs_preview = class == "needs_preview";
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE jobs SET state=?16,stage=?17,reason_code=?18,external_capture_id=COALESCE(external_capture_id,?2),artifact_digest=?3,artifact_path=?4,artifact_media=?5,byte_size=?6,captured_at=?7,final_url=?8,http_status=?9,admission_class=?10,admission_reason=?11,policy_version=?12,preview_host=?13,preview_path_class=?14,preview_query=?15,next_attempt_at=0,lease_owner=CASE WHEN ?19 THEN NULL ELSE lease_owner END,lease_until=CASE WHEN ?19 THEN NULL ELSE lease_until END,updated_at=?20 WHERE job_id=?1 AND state='leased' AND lease_owner=?21 AND (external_capture_id IS NULL OR external_capture_id=?2)",params![j.view.job_id,external_capture_id,digest,path.to_string_lossy(),media,size as i64,captured,final_url,status,class,reason,policy,h,p,q,if needs_preview { State::NeedsUser.text() } else { State::Leased.text() },if needs_preview { Stage::Validate.text() } else { Stage::Translate.text() },if needs_preview { Some("needs_preview") } else { None },needs_preview,now,j.lease_owner])?;
        if changed != 1 {
            bail!("lease_lost")
        }
        if needs_preview {
            tx.execute(
                "UPDATE attempts SET reason_code='needs_preview',finished_at=?2 WHERE id=(SELECT id FROM attempts WHERE job_id=?1 AND finished_at IS NULL ORDER BY id DESC LIMIT 1)",
                params![j.view.job_id, now],
            )?;
        }
        tx.commit()?;
        self.secure()
    }
    pub(crate) fn finish(
        &mut self,
        j: &JobRecord,
        state: State,
        stage: Stage,
        reason: Option<&str>,
        delay: i64,
    ) -> Result<()> {
        if state == State::Succeeded {
            bail!("invalid_item_receipt")
        }
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE jobs SET state=?2,stage=?3,reason_code=?4,next_attempt_at=?5,lease_owner=NULL,lease_until=NULL,updated_at=?6 WHERE job_id=?1 AND state='leased' AND lease_owner=?7",params![j.view.job_id,state.text(),stage.text(),reason,Utc::now().timestamp().saturating_add(delay),now,j.lease_owner])?;
        if changed != 1 {
            bail!("lease_lost")
        }
        if reason == Some("polite_wait") {
            tx.execute(
                "UPDATE jobs SET attempt_count=MAX(0,attempt_count-1) WHERE job_id=?1",
                [&j.view.job_id],
            )?;
        }
        tx.execute(
            "UPDATE attempts SET reason_code=?2,finished_at=?3 WHERE id=(SELECT id FROM attempts WHERE job_id=?1 AND finished_at IS NULL ORDER BY id DESC LIMIT 1)",
            params![j.view.job_id, reason, now],
        )?;
        tx.commit()?;
        self.secure()
    }
    pub(crate) fn finish_stored(
        &mut self,
        j: &JobRecord,
        item_id: &str,
        revision_id: &str,
    ) -> Result<()> {
        if !valid_result_item_id(item_id) || !valid_result_revision_id(revision_id) {
            bail!("invalid_item_receipt")
        }
        // Options are immutable request identity, loaded from durable state on
        // every commit/recovery. Close the hook in the SAME success transaction;
        // a later acknowledgement leaves a race for the provider worker.
        let suppress_generation = self.chrome_options(&j.view.job_id)?.is_some_and(|options|
            options.generation_policy == Some(memex_intake::url_jobs::GenerationPolicy::None));
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE jobs SET state='succeeded',stage='commit',reason_code=NULL,result_item_id=?2,result_revision_id=?3,translation_hook_completed=?6,next_attempt_at=0,lease_owner=NULL,lease_until=NULL,updated_at=?4 WHERE job_id=?1 AND state='leased' AND lease_owner=?5",
            params![j.view.job_id, item_id, revision_id, now, j.lease_owner, suppress_generation],
        )?;
        if changed != 1 {
            bail!("lease_lost")
        }
        tx.execute(
            "UPDATE attempts SET reason_code=NULL,finished_at=?2 WHERE id=(SELECT id FROM attempts WHERE job_id=?1 AND finished_at IS NULL ORDER BY id DESC LIMIT 1)",
            params![j.view.job_id, now],
        )?;
        tx.commit()?;
        self.secure()
    }
    pub(crate) fn translation_intent_pending(&self, item: &str, revision: &str) -> Result<bool> {
        // Include deferred retries: elapsed time cannot turn durable intent into absence.
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE state='succeeded' AND stage='commit'
             AND translation_hook_completed=0 AND result_item_id=?1 AND result_revision_id=?2)",
            params![item, revision],
            |row| row.get(0),
        )?)
    }
    pub(crate) fn pending_translation_intent(&self) -> Result<Option<TranslationIntent>> {
        self.conn
            .query_row(
                "SELECT job_id,result_item_id,result_revision_id FROM jobs
                  WHERE state='succeeded' AND stage='commit' AND translation_hook_completed=0
                    AND result_item_id IS NOT NULL AND result_revision_id IS NOT NULL
                    AND next_attempt_at<=?1
                  ORDER BY next_attempt_at,updated_at,job_id LIMIT 1",
                [Utc::now().timestamp()],
                translation_intent_row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub(crate) fn defer_translation_intent(&mut self, intent: &TranslationIntent) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE jobs SET next_attempt_at=?4
              WHERE job_id=?1 AND state='succeeded' AND stage='commit'
                AND result_item_id=?2 AND result_revision_id=?3 AND translation_hook_completed=0",
            params![
                intent.job_id,
                intent.item_id,
                intent.revision_id,
                Utc::now().timestamp() + 2
            ],
        )?;
        if changed != 1 {
            bail!("translation_intent_changed")
        }
        self.secure()
    }
    pub(crate) fn complete_translation_intent(&mut self, intent: &TranslationIntent) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE jobs SET translation_hook_completed=1,next_attempt_at=0
              WHERE job_id=?1 AND state='succeeded' AND stage='commit'
                AND result_item_id=?2 AND result_revision_id=?3 AND translation_hook_completed=0",
            params![intent.job_id, intent.item_id, intent.revision_id],
        )?;
        if changed != 1 {
            let completed = self
                .conn
                .query_row(
                    "SELECT 1 FROM jobs WHERE job_id=?1 AND state='succeeded' AND stage='commit'
                       AND result_item_id=?2 AND result_revision_id=?3 AND translation_hook_completed=1",
                    params![intent.job_id, intent.item_id, intent.revision_id],
                    |_| Ok(()),
                )
                .optional()?;
            if completed.is_none() {
                bail!("translation_intent_changed")
            }
        }
        self.secure()
    }
    pub fn retry(&mut self, id: &str) -> Result<JobView> {
        let changed = self.conn.execute("UPDATE jobs SET state=CASE WHEN artifact_digest IS NULL THEN 'queued' ELSE 'artifact_ready' END,reason_code='manual_retry',next_attempt_at=CASE WHEN chrome_options IS NOT NULL AND artifact_digest IS NULL THEN COALESCE((SELECT next_fetch_at FROM capture_origins WHERE origin=jobs.fetch_origin),0) ELSE 0 END,lease_owner=NULL,lease_until=NULL,attempt_count=0,updated_at=?2 WHERE job_id=?1 AND (state IN ('failed','retry_wait') OR (state='needs_user' AND artifact_digest IS NULL)) AND lease_owner IS NULL",params![id,Utc::now().to_rfc3339()])?;
        if changed != 1 {
            self.get(id)?;
            bail!("job_not_retryable")
        }
        self.secure()?;
        self.get(id)
    }
    pub fn cancel(&mut self, id: &str) -> Result<JobView> {
        let changed = self.conn.execute("UPDATE jobs SET state='cancelled',reason_code='cancelled_by_user',lease_owner=NULL,lease_until=NULL,updated_at=?2 WHERE job_id=?1 AND state IN ('queued','artifact_ready','needs_user','retry_wait','failed') AND lease_owner IS NULL",params![id,Utc::now().to_rfc3339()])?;
        if changed != 1 {
            self.get(id)?;
            bail!("job_not_cancellable")
        }
        self.secure()?;
        self.get(id)
    }
    pub fn approve(&mut self, id: &str) -> Result<JobView> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ok:Option<i64>=tx.query_row("SELECT 1 FROM jobs WHERE job_id=?1 AND state='needs_user' AND admission_class='needs_preview' AND artifact_digest IS NOT NULL AND artifact_path IS NOT NULL AND policy_version IS NOT NULL",[id],|r|r.get(0)).optional()?;
        if ok.is_none() {
            bail!("job_not_approvable")
        }
        tx.execute("UPDATE jobs SET approval_digest=artifact_digest,approval_policy=policy_version,state='artifact_ready',stage='translate',reason_code='preview_approved',next_attempt_at=0,updated_at=?2 WHERE job_id=?1",params![id,Utc::now().to_rfc3339()])?;
        tx.commit()?;
        self.secure()?;
        self.get(id)
    }
    #[cfg(test)]
    pub(crate) fn make_retry_due(&self, id: &str) -> Result<()> {
        if self.conn.execute(
            "UPDATE jobs SET next_attempt_at=0 WHERE job_id=?1 AND state='retry_wait'",
            [id],
        )? != 1
        {
            bail!("job_not_retryable")
        }
        Ok(())
    }
}
fn has_column(c: &Connection, n: &str) -> Result<bool> {
    has_table_column(c, "jobs", n)
}
fn has_table_column(c: &Connection, table: &str, n: &str) -> Result<bool> {
    if !matches!(table, "jobs" | "conflicts") {
        bail!("invalid_database_table")
    }
    let mut s = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = s
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|x| x == n))
}
fn scoped_producer(principal: &str) -> Result<String> {
    if principal.len() != 64
        || !principal
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("invalid_request");
    }
    Ok(format!("mobile_{principal}"))
}
fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
fn parse_state(s: &str) -> rusqlite::Result<State> {
    match s {
        "queued" => Ok(State::Queued),
        "leased" => Ok(State::Leased),
        "artifact_ready" => Ok(State::ArtifactReady),
        "needs_user" => Ok(State::NeedsUser),
        "retry_wait" => Ok(State::RetryWait),
        "succeeded" => Ok(State::Succeeded),
        "failed" => Ok(State::Failed),
        "cancelled" => Ok(State::Cancelled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn parse_stage(s: &str) -> rusqlite::Result<Stage> {
    match s {
        "extract" => Ok(Stage::Extract),
        "resolve" => Ok(Stage::Resolve),
        "fetch" => Ok(Stage::Fetch),
        "validate" => Ok(Stage::Validate),
        "translate" => Ok(Stage::Translate),
        "commit" => Ok(Stage::Commit),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn view_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobView> {
    let state = parse_state(&r.get::<_, String>(1)?)?;
    let reason_code = r.get::<_, Option<String>>(3)?;
    if reason_code.as_deref().is_some_and(|reason| {
        !matches!(
            reason,
            "artifact_verification_failed"
                | "address_not_public"
                | "cancelled_by_user"
                | "challenge_detected"
                | "chrome_unavailable"
                | "commit_retry"
                | "podcast_retry"
                | "podcast_conflict"
                | "fetch_retry"
                | "image_too_large"
                | "rate_limited"
                | "interactive_login_required"
                | "lease_recovered"
                | "manual_retry"
                | "needs_preview"
                | "network_policy_rejected"
                | "page_changed"
                | "preview_approved"
                | "polite_wait"
                | "retry_exhausted"
                | "translation_failed"
                | "unsupported_content"
                | "weixin_source_conflict"
                | "weixin_content_invalid"
                | "weixin_commit_retry"
        )
    }) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let admission_class = r.get::<_, Option<String>>(7)?;
    let preview_host = r.get::<_, Option<String>>(8)?;
    let preview_path_class = r.get::<_, Option<String>>(9)?;
    let preview_query = r.get::<_, Option<i64>>(10)?;
    let item_id = r.get::<_, Option<String>>(11)?;
    if item_id
        .as_deref()
        .is_some_and(|value| !valid_result_item_id(value))
        || (state == State::Succeeded) != item_id.is_some()
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let preview = if admission_class.as_deref() == Some("needs_preview") {
        let host = preview_host.ok_or(rusqlite::Error::InvalidQuery)?;
        let path_class = preview_path_class.ok_or(rusqlite::Error::InvalidQuery)?;
        if host.is_empty()
            || host.len() > 253
            || host.chars().any(char::is_control)
            || !matches!(path_class.as_str(), "root" | "single_segment" | "nested")
            || !matches!(preview_query, Some(0 | 1))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Some(PreviewMetadata {
            host,
            path_class,
            query_present: preview_query == Some(1),
        })
    } else {
        None
    };
    Ok(JobView {
        job_id: r.get(0)?,
        state,
        stage: parse_stage(&r.get::<_, String>(2)?)?,
        reason_code,
        item_id,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        attempt_count: r.get::<_, i64>(6)? as u32,
        preview,
    })
}
fn record_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    let state = parse_state(&r.get::<_, String>(1)?)?;
    let item_id = r.get::<_, Option<String>>(25)?;
    let revision_id = r.get::<_, Option<String>>(26)?;
    let translation_hook_completed = r.get::<_, i64>(27)?;
    let byte_size = r.get::<_, Option<i64>>(13)?;
    let http_status = r.get::<_, Option<i64>>(16)?;
    if byte_size.is_some_and(|value| value < 0)
        || http_status.is_some_and(|value| !(100..=599).contains(&value))
        || item_id
            .as_deref()
            .is_some_and(|value| !valid_result_item_id(value))
        || revision_id
            .as_deref()
            .is_some_and(|value| !valid_result_revision_id(value))
        || (state == State::Succeeded) != item_id.is_some()
        || (state == State::Succeeded) != revision_id.is_some()
        || !matches!(translation_hook_completed, 0 | 1)
        || translation_hook_completed == 1 && (state != State::Succeeded || revision_id.is_none())
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(JobRecord {
        view: JobView {
            job_id: r.get(0)?,
            state,
            stage: parse_stage(&r.get::<_, String>(2)?)?,
            reason_code: r.get(3)?,
            item_id,
            created_at: r.get(4)?,
            updated_at: r.get(5)?,
            attempt_count: r.get::<_, i64>(6)? as u32,
            preview: None,
        },
        url: r.get(7)?,
        external_capture_id: r.get(8)?,
        lease_owner: r.get(9)?,
        artifact_digest: r.get(10)?,
        artifact_path: r.get::<_, Option<String>>(11)?.map(Into::into),
        artifact_media: r.get(12)?,
        byte_size: byte_size.map(|x| x as u64),
        captured_at: r.get(14)?,
        final_url: r.get(15)?,
        http_status: http_status.map(|x| x as u16),
        admission_class: r.get(17)?,
        admission_reason: r.get(18)?,
        policy_version: r.get(19)?,
        preview_host: r.get(20)?,
        preview_path_class: r.get(21)?,
        preview_query: r.get::<_, Option<i64>>(22)?.map(|x| x != 0),
        approval_digest: r.get(23)?,
        approval_policy: r.get(24)?,
        result_revision_id: revision_id,
        translation_hook_completed: translation_hook_completed == 1,
    })
}
fn translation_intent_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationIntent> {
    let intent = TranslationIntent {
        job_id: r.get(0)?,
        item_id: r.get(1)?,
        revision_id: r.get(2)?,
    };
    if !valid_identity(&intent.job_id)
        || !valid_result_item_id(&intent.item_id)
        || !valid_result_revision_id(&intent.revision_id)
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(intent)
}
fn valid_result_item_id(value: &str) -> bool {
    value.starts_with("item_")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
fn valid_result_revision_id(value: &str) -> bool {
    value.starts_with("revision_")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
fn database_family(path: &Path) -> [PathBuf; 3] {
    [
        path.to_owned(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
}
fn validate_database_family(path: &Path) -> Result<()> {
    for member in database_family(path) {
        match fs::symlink_metadata(member) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.permissions().mode() & 0o077 != 0 =>
            {
                bail!("unsafe_database")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn chmod_db(path: &Path) -> Result<()> {
    for member in database_family(path) {
        match fs::symlink_metadata(&member) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() } =>
            {
                bail!("unsafe_database")
            }
            Ok(_) => fs::set_permissions(member, fs::Permissions::from_mode(0o600))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn scoped_replay_is_atomic_durable_and_not_transferable() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().canonicalize().unwrap().join("agent.db");
        let owner = "a".repeat(64);
        let key = format!("remote_mobile_{}", "b".repeat(32));
        // Independent owner connections racing the same durable request.
        let first = Queue::open(&database).unwrap();
        let second = Queue::open(&database).unwrap();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let joins: Vec<_> = [first, second]
            .into_iter()
            .map(|mut queue| {
                let (owner, key, gate) = (owner.clone(), key.clone(), gate.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    queue
                        .scoped_enqueue(&owner, &key, "https://capture.example.invalid/article")
                        .unwrap()
                        .job_id
                })
            })
            .collect();
        let ids: Vec<_> = joins.into_iter().map(|j| j.join().unwrap()).collect();
        assert_eq!(ids[0], ids[1]);
        let mut reopened = Queue::open(&database).unwrap();
        assert_eq!(
            reopened
                .scoped_enqueue(&owner, &key, "https://capture.example.invalid/article")
                .unwrap()
                .job_id,
            ids[0]
        );
        assert_eq!(
            reopened
                .scoped_enqueue(&owner, &key, "https://capture.example.invalid/different")
                .unwrap_err()
                .to_string(),
            "request_identity_conflict"
        );
        let foreign = "c".repeat(64);
        assert_eq!(
            reopened
                .scoped_get(&foreign, &ids[0])
                .unwrap_err()
                .to_string(),
            "remote_resource_not_found"
        );
        assert_eq!(
            reopened
                .scoped_enqueue(&foreign, &key, "https://capture.example.invalid/article")
                .unwrap_err()
                .to_string(),
            "remote_resource_not_found"
        );
        assert_eq!(
            reopened.remote_get(&ids[0]).unwrap_err().to_string(),
            "remote_resource_not_found"
        );
        assert_eq!(reopened.list(None).unwrap().0.len(), 1);
    }

    #[test]
    fn remote_queries_require_producer_and_exact_succeeded_item() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut queue = Queue::open(&root_path.join("agent.db")).unwrap();
        let local = queue
            .enqueue("local_surface", "local", "https://local.test/one")
            .unwrap();
        assert_eq!(
            queue.remote_get(&local.job_id).unwrap_err().to_string(),
            "remote_resource_not_found"
        );
        assert_eq!(
            queue
                .authorize_remote_item(&local.job_id, "item_guessed")
                .unwrap_err()
                .to_string(),
            "remote_resource_not_found"
        );
        let remote = queue
            .enqueue("remote_iphone", "remote", "https://remote.test/one")
            .unwrap();
        assert_eq!(
            queue.remote_get(&remote.job_id).unwrap().job_id,
            remote.job_id
        );
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish(&leased, State::Cancelled, Stage::Extract, None, 0)
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&leased, "item_remote_exact", "revision_remote_exact")
            .unwrap();
        assert_eq!(
            queue
                .authorize_remote_item(&remote.job_id, "item_wrong")
                .unwrap_err()
                .to_string(),
            "remote_resource_not_found"
        );
        queue
            .authorize_remote_item(&remote.job_id, "item_remote_exact")
            .unwrap();
    }

    #[test]
    fn replay_conflict_lease_restart_and_attempts_are_durable() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let first = queue
            .enqueue("test", "stable", "https://queue.test/one")
            .unwrap();
        assert_eq!(
            first.job_id,
            queue
                .enqueue("test", "stable", "https://queue.test/one")
                .unwrap()
                .job_id
        );
        assert_eq!(
            queue
                .enqueue("test", "stable", "https://queue.test/two")
                .unwrap_err()
                .to_string(),
            "request_identity_conflict"
        );
        let conflict_count: i64 = queue
            .conn
            .query_row(
                "SELECT count(*) FROM conflicts WHERE request_digest IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(conflict_count, 1);

        let leased = queue.lease(-1).unwrap().unwrap();
        let external_capture_id = leased.external_capture_id.clone();
        queue.stage(&leased, Stage::Resolve).unwrap();
        drop(queue);

        let mut reopened = Queue::open(&database).unwrap();
        assert_eq!(reopened.recover_leases().unwrap(), 1);
        let unfinished: i64 = reopened
            .conn
            .query_row(
                "SELECT count(*) FROM attempts WHERE finished_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unfinished, 0);
        let recovered = reopened.lease(60).unwrap().unwrap();
        assert_eq!(recovered.view.job_id, first.job_id);
        assert_eq!(recovered.external_capture_id, external_capture_id);
        reopened.stage(&recovered, Stage::Fetch).unwrap();
        reopened
            .finish(
                &recovered,
                State::RetryWait,
                Stage::Fetch,
                Some("fetch_retry"),
                30,
            )
            .unwrap();
        let unfinished: i64 = reopened
            .conn
            .query_row(
                "SELECT count(*) FROM attempts WHERE finished_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unfinished, 0);

        let mode = fs::metadata(&database).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        let view = reopened.retry(&first.job_id).unwrap();
        assert_eq!(view.state, State::Queued);
        reopened.cancel(&first.job_id).unwrap();
        assert!(reopened.approve(&first.job_id).is_err());
    }

    #[test]
    fn job_listing_is_sql_bounded_and_cursor_paginated_below_the_frame_limit() {
        let root = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(&root.path().join("agent.db")).unwrap();
        let transaction = queue.conn.transaction().unwrap();
        for index in 0..256 {
            transaction
                .execute(
                    "INSERT INTO jobs(job_id,producer,producer_job_id,request_digest,url,state,stage,reason_code,created_at,updated_at,next_attempt_at,admission_class,preview_host,preview_path_class,preview_query) VALUES(?1,'test',?2,?3,'https://page.test/one','needs_user','validate','needs_preview',?4,?4,0,'needs_preview',?5,'nested',1)",
                    params![
                        format!("job_{index:032x}"),
                        format!("producer_{index:04}"),
                        format!("{index:064x}"),
                        format!("2026-08-11T00:00:{:02}Z", index / 16),
                        format!("{}.test", "a".repeat(248)),
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let mut statement = queue.conn.prepare("SELECT job_id,state,stage,reason_code,created_at,updated_at,attempt_count,admission_class,preview_host,preview_path_class,preview_query,result_item_id FROM jobs ORDER BY created_at,job_id").unwrap();
        let all = statement
            .query_map([], view_row)
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(serde_json::to_vec(&all).unwrap().len() > crate::ipc::MAX_FRAME);
        drop(statement);

        let mut cursor = None;
        let mut listed_ids = Vec::new();
        let mut pages = 0;
        loop {
            let (jobs, next_cursor) = queue.list(cursor.as_deref()).unwrap();
            assert!(!jobs.is_empty());
            assert!(jobs.len() <= LIST_PAGE_SIZE);
            let response = crate::ipc::Response {
                ok: true,
                job: None,
                jobs: Some(jobs.clone()),
                next_cursor: next_cursor.clone(),
                reason_code: None,
                item: None,
                library_items: None,
                library_item: None,
                item_update: None,
                context: None,
                library_query: None,
                library_entry: None,
                model_settings: None,
                provider_credential: None,
                provider_models: None,
                translation: None,
                bibliographic_metadata: None,
                identifier_duplicates: None,
                lookup_preview: None,
                lookup_value: None,
                group_tree: None,
                generation_test: None,
                capabilities: None,
                group_edit: None,
                ..crate::ipc::Response::empty_ok()
            };
            assert!(serde_json::to_vec(&response).unwrap().len() <= crate::ipc::MAX_FRAME);
            listed_ids.extend(jobs.into_iter().map(|job| job.job_id));
            pages += 1;
            cursor = next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(pages, 4);
        assert_eq!(listed_ids.len(), 256);
        listed_ids.sort();
        listed_ids.dedup();
        assert_eq!(listed_ids.len(), 256);
        assert_eq!(
            queue.list(Some("job_missing")).unwrap_err().to_string(),
            "invalid_cursor"
        );
    }

    #[test]
    fn preview_freeze_is_atomic_across_restart_and_remains_approvable() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let job = queue
            .enqueue("test", "preview-freeze", "https://preview.test/one")
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        let digest = "a".repeat(64);
        queue
            .freeze(
                &leased,
                "agent_synthetic",
                &digest,
                &root.path().join("spool/cas/aa").join(&digest),
                "text/html",
                32,
                "2026-08-11T00:00:00Z",
                "https://preview.test/one",
                200,
                "needs_preview",
                "generic_page_requires_preview",
                "capture-admission-v1",
                Some(("preview.test", "single_segment", false)),
            )
            .unwrap();
        assert_eq!(queue.get(&job.job_id).unwrap().state, State::NeedsUser);
        let unfinished: i64 = queue
            .conn
            .query_row(
                "SELECT count(*) FROM attempts WHERE job_id=?1 AND finished_at IS NULL",
                [&job.job_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unfinished, 0);
        drop(queue);

        let mut reopened = Queue::open(&database).unwrap();
        let approved = reopened.approve(&job.job_id).unwrap();
        assert_eq!(approved.state, State::ArtifactReady);
        let record = reopened.record(&job.job_id).unwrap();
        assert_eq!(record.approval_digest.as_deref(), Some(digest.as_str()));
        assert_eq!(
            record.approval_policy.as_deref(),
            Some("capture-admission-v1")
        );
    }

    #[test]
    fn frozen_article_keeps_its_active_lease_and_attempt_transitions_are_atomic() {
        let root = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(&root.path().join("agent.db")).unwrap();
        let job = queue
            .enqueue("test", "article-freeze", "https://article.test/one")
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        let digest = "b".repeat(64);
        queue
            .freeze(
                &leased,
                "agent_synthetic",
                &digest,
                &root.path().join("spool/cas/bb").join(&digest),
                "text/html",
                32,
                "2026-08-11T00:00:00Z",
                "https://article.test/one",
                200,
                "article",
                "supported_article",
                "capture-admission-v1",
                Some(("article.test", "single_segment", false)),
            )
            .unwrap();
        let frozen = queue.record(&job.job_id).unwrap();
        assert_eq!(frozen.view.state, State::Leased);
        assert_eq!(frozen.lease_owner, leased.lease_owner);
        assert!(queue.lease(60).unwrap().is_none());
        queue.stage(&frozen, Stage::Commit).unwrap();
        queue
            .finish_stored(&frozen, "item_durable_result", "revision_durable_result")
            .unwrap();
        let intent = queue.pending_translation_intent().unwrap().unwrap();
        assert_eq!(intent.job_id, job.job_id);
        assert_eq!(intent.item_id, "item_durable_result");
        assert_eq!(intent.revision_id, "revision_durable_result");
        assert_eq!(
            queue.get(&job.job_id).unwrap().item_id.as_deref(),
            Some("item_durable_result")
        );
        let unfinished: i64 = queue
            .conn
            .query_row(
                "SELECT count(*) FROM attempts WHERE job_id=?1 AND finished_at IS NULL",
                [&job.job_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unfinished, 0);
        drop(queue);
        let mut reopened = Queue::open(&root.path().join("agent.db")).unwrap();
        assert_eq!(
            reopened.get(&job.job_id).unwrap().item_id.as_deref(),
            Some("item_durable_result")
        );
        assert_eq!(
            reopened.pending_translation_intent().unwrap(),
            Some(intent.clone())
        );
        reopened.complete_translation_intent(&intent).unwrap();
        reopened.complete_translation_intent(&intent).unwrap();
        assert!(reopened.pending_translation_intent().unwrap().is_none());
    }

    #[test]
    fn legacy_success_without_item_receipt_is_repaired_without_refetch() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let complete = queue
            .enqueue("test", "complete-legacy", "https://article.test/one")
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        let digest = "c".repeat(64);
        queue
            .freeze(
                &leased,
                "agent_legacy",
                &digest,
                &root.path().join("spool/cas/cc").join(&digest),
                "text/html",
                32,
                "2026-08-11T00:00:00Z",
                "https://article.test/one",
                200,
                "article",
                "semantic_article_container",
                "capture-admission-v1",
                Some(("article.test", "single_segment", false)),
            )
            .unwrap();
        queue
            .conn
            .execute(
                "UPDATE jobs SET state='succeeded',stage='commit',reason_code=NULL,lease_owner=NULL,lease_until=NULL WHERE job_id=?1",
                [&complete.job_id],
            )
            .unwrap();
        let incomplete = queue
            .enqueue("test", "incomplete-legacy", "https://missing.test/one")
            .unwrap();
        queue
            .conn
            .execute(
                "UPDATE jobs SET state='succeeded',stage='commit',reason_code=NULL WHERE job_id=?1",
                [&incomplete.job_id],
            )
            .unwrap();
        let partial = queue
            .enqueue("test", "partial-receipt", "https://article.test/partial")
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        let digest = "d".repeat(64);
        queue
            .freeze(
                &leased,
                "agent_partial",
                &digest,
                &root.path().join("spool/cas/dd").join(&digest),
                "text/html",
                32,
                "2026-08-11T00:00:00Z",
                "https://article.test/partial",
                200,
                "article",
                "semantic_article_container",
                "capture-admission-v1",
                Some(("article.test", "single_segment", false)),
            )
            .unwrap();
        queue
            .conn
            .execute(
                "UPDATE jobs SET state='succeeded',stage='commit',reason_code=NULL,
                        result_item_id='item_legacy_without_revision',lease_owner=NULL,lease_until=NULL
                  WHERE job_id=?1",
                [&partial.job_id],
            )
            .unwrap();
        drop(queue);

        let repaired = Queue::open(&database).unwrap();
        let complete = repaired.get(&complete.job_id).unwrap();
        assert_eq!(complete.state, State::ArtifactReady);
        assert_eq!(complete.stage, Stage::Translate);
        assert_eq!(complete.reason_code.as_deref(), Some("commit_retry"));
        let incomplete = repaired.get(&incomplete.job_id).unwrap();
        assert_eq!(incomplete.state, State::Failed);
        assert_eq!(
            incomplete.reason_code.as_deref(),
            Some("artifact_verification_failed")
        );
        let partial = repaired.get(&partial.job_id).unwrap();
        assert_eq!(partial.state, State::ArtifactReady);
        assert_eq!(partial.stage, Stage::Translate);
        assert_eq!(partial.reason_code.as_deref(), Some("commit_retry"));
        assert!(partial.item_id.is_none());
    }

    #[test]
    fn database_family_is_private_and_symlinks_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("state/agent.db");
        let queue = Queue::open(&database).unwrap();
        let journal_mode: String = queue
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = queue
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = queue
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 2);
        for member in database_family(&database) {
            if let Ok(metadata) = fs::metadata(member) {
                assert_eq!(metadata.permissions().mode() & 0o077, 0);
            }
        }
        assert_eq!(
            fs::metadata(database.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(queue);

        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        let database = state.join("agent.db");
        symlink(&outside, &database).unwrap();
        assert_eq!(
            Queue::open(&database).err().unwrap().to_string(),
            "unsafe_database"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");

        fs::remove_file(&database).unwrap();
        symlink(&outside, format!("{}-wal", database.display())).unwrap();
        assert_eq!(
            Queue::open(&database).err().unwrap().to_string(),
            "unsafe_database"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
    }
}

/// Persisted dedup plans (`dedup_plan` / `dedup_apply`): item and revision ids, titles and URLs
/// of the library itself, never document text.
impl Queue {
    pub(crate) fn save_dedup_plan(
        &mut self,
        plan_id: &str,
        created_at: &str,
        plan_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dedup_plans(plan_id,created_at,plan_json) VALUES(?1,?2,?3)",
            params![plan_id, created_at, plan_json],
        )?;
        Ok(())
    }

    /// `(plan_json, progress_json, result_json)`.
    pub(crate) fn dedup_plan(
        &self,
        plan_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT plan_json,progress_json,result_json FROM dedup_plans WHERE plan_id=?1",
                [plan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    pub(crate) fn save_dedup_progress(
        &mut self,
        plan_id: &str,
        progress_json: &str,
        result_json: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE dedup_plans SET progress_json=?2,result_json=COALESCE(?3,result_json)
              WHERE plan_id=?1",
            params![plan_id, progress_json, result_json],
        )?;
        Ok(())
    }
}
