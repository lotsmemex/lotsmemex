//! `capture_jobs_recent`: a bounded, read-only progress projection of the capture queue.
//! Exposes no producer identity, options, payloads, artifact paths or content.
use super::{parse_stage, parse_state, valid_result_item_id, Queue, Stage, State};
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const CAPTURE_JOBS_RECENT_FEATURE: &str = "capture_jobs_recent_v1";
pub const CAPTURE_JOBS_RECENT_MAX: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerKind {
    Chrome,
    Mobile,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureJobSummary {
    pub job_id: String,
    /// Null when the result item is not readable at the caller's ceiling (or trashed).
    pub url: Option<String>,
    pub state: State,
    pub stage: Stage,
    pub reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub attempt_count: u32,
    pub result_item_id: Option<String>,
    /// Unix seconds; null when the job may run immediately.
    pub next_attempt_at: Option<i64>,
    pub producer: ProducerKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureJobCounts {
    pub queued: u64,
    pub leased: u64,
    pub artifact_ready: u64,
    pub needs_user: u64,
    pub retry_wait: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureJobsRecent {
    pub schema_version: u32,
    /// Running first, then waiting jobs in processing order, then the rest newest first.
    pub jobs: Vec<CaptureJobSummary>,
    /// Over the whole queue, not only `jobs`.
    pub counts: CaptureJobCounts,
}

pub(crate) fn producer_kind(producer: &str) -> ProducerKind {
    if producer.starts_with("chrome_") {
        ProducerKind::Chrome
    } else if producer.starts_with("mobile_") || producer == "remote_iphone" {
        ProducerKind::Mobile
    } else {
        ProducerKind::Other
    }
}

fn machine_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl Queue {
    /// `visible(item_id)` decides whether a succeeded job's URL and item id may be shown.
    pub fn capture_jobs_recent(
        &self,
        limit: u32,
        visible: impl Fn(&str) -> bool,
    ) -> Result<CaptureJobsRecent> {
        let limit = limit.clamp(1, CAPTURE_JOBS_RECENT_MAX);
        let mut statement = self.conn.prepare(
            "SELECT job_id,url,state,stage,reason_code,created_at,updated_at,attempt_count,result_item_id,next_attempt_at,producer,
                CASE WHEN state IN ('leased','artifact_ready') THEN 0 WHEN state IN ('queued','retry_wait') THEN 1 ELSE 2 END AS grp
             FROM jobs
             WHERE state IN ('queued','leased','artifact_ready','needs_user','retry_wait','succeeded','failed','cancelled')
             ORDER BY grp, CASE WHEN grp=1 THEN created_at END ASC, updated_at DESC, job_id DESC
             LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, u32>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                    r.get::<_, String>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut jobs = Vec::with_capacity(rows.len());
        for (job_id, url, state, stage, reason, created_at, updated_at, attempts, item, next, producer) in rows {
            // A malformed row is skipped rather than failing the whole progress view.
            let (Ok(state), Ok(stage)) = (parse_state(&state), parse_stage(&stage)) else {
                continue;
            };
            let item = item.filter(|id| valid_result_item_id(id));
            let shown = item.as_deref().is_none_or(&visible);
            jobs.push(CaptureJobSummary {
                job_id,
                url: shown.then_some(url),
                state,
                stage,
                reason_code: reason.filter(|code| machine_code(code)),
                created_at,
                updated_at,
                attempt_count: attempts,
                result_item_id: item.filter(|_| shown),
                next_attempt_at: next.filter(|seconds| *seconds > 0),
                producer: producer_kind(&producer),
            });
        }
        let mut counts = CaptureJobCounts::default();
        let mut statement = self
            .conn
            .prepare("SELECT state,COUNT(*) FROM jobs GROUP BY state")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
            let (state, count) = row?;
            let slot = match parse_state(&state) {
                Ok(State::Queued) => &mut counts.queued,
                Ok(State::Leased) => &mut counts.leased,
                Ok(State::ArtifactReady) => &mut counts.artifact_ready,
                Ok(State::NeedsUser) => &mut counts.needs_user,
                Ok(State::RetryWait) => &mut counts.retry_wait,
                Ok(State::Succeeded) => &mut counts.succeeded,
                Ok(State::Failed) => &mut counts.failed,
                Ok(State::Cancelled) => &mut counts.cancelled,
                Err(_) => continue,
            };
            *slot += count;
        }
        Ok(CaptureJobsRecent { schema_version: 1, jobs, counts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[allow(clippy::too_many_arguments)]
    fn insert(queue: &Queue, id: &str, producer: &str, state: &str, created: &str, updated: &str, item: Option<&str>, next: i64) {
        queue.conn.execute(
            "INSERT INTO jobs(job_id,producer,producer_job_id,request_digest,url,state,stage,reason_code,created_at,updated_at,next_attempt_at,attempt_count,result_item_id,chrome_options,artifact_path)
             VALUES(?1,?2,?1,'digest',?3,?4,'fetch',?5,?6,?7,?8,2,?9,'{\"secret_option\":true}','/private/synthetic/artifact')",
            params![id, producer, format!("https://synthetic.test/show/{id}"), state,
                if state == "failed" { Some("Raw Body: not a code") } else { Some("fetch_retry") },
                created, updated, next, item],
        ).unwrap();
    }

    #[test]
    fn recent_jobs_order_bound_count_and_redact() {
        let root = tempfile::tempdir().unwrap();
        let queue = Queue::open(&root.path().canonicalize().unwrap().join("agent.db")).unwrap();
        let empty = queue.capture_jobs_recent(10, |_| true).unwrap();
        assert!(empty.jobs.is_empty());
        assert_eq!(empty.counts, CaptureJobCounts::default());

        insert(&queue, "j_done_old", "local_cli", "succeeded", "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z", Some("item_visible"), 0);
        insert(&queue, "j_done_new", "mobile_abc", "succeeded", "2026-01-02T00:00:00Z", "2026-01-02T00:01:00Z", Some("item_hidden"), 0);
        insert(&queue, "j_wait_b", "chrome_reg", "queued", "2026-01-03T00:00:02Z", "2026-01-03T00:00:02Z", None, 1_900_000_000);
        insert(&queue, "j_wait_a", "chrome_reg", "retry_wait", "2026-01-03T00:00:01Z", "2026-01-03T00:00:09Z", None, 0);
        insert(&queue, "j_run", "remote_iphone", "leased", "2026-01-03T00:00:03Z", "2026-01-03T00:00:04Z", None, 0);
        insert(&queue, "j_fail", "chrome_reg", "failed", "2026-01-01T12:00:00Z", "2026-01-03T00:00:00Z", None, 0);
        insert(&queue, "j_bad", "chrome_reg", "mystery", "2026-01-04T00:00:00Z", "2026-01-04T00:00:00Z", None, 0);

        let recent = queue.capture_jobs_recent(100, |id| id == "item_visible").unwrap();
        let ids: Vec<_> = recent.jobs.iter().map(|j| j.job_id.as_str()).collect();
        assert_eq!(ids, ["j_run", "j_wait_a", "j_wait_b", "j_fail", "j_done_new", "j_done_old"]);
        assert_eq!(recent.counts, CaptureJobCounts { queued: 1, leased: 1, retry_wait: 1, succeeded: 2, failed: 1, ..Default::default() });

        let by = |id: &str| recent.jobs.iter().find(|j| j.job_id == id).unwrap();
        assert_eq!(by("j_run").producer, ProducerKind::Mobile);
        assert_eq!(by("j_wait_b").producer, ProducerKind::Chrome);
        assert_eq!(by("j_done_old").producer, ProducerKind::Other);
        assert_eq!(by("j_done_new").producer, ProducerKind::Mobile);
        assert_eq!(by("j_wait_b").next_attempt_at, Some(1_900_000_000));
        assert_eq!(by("j_wait_a").next_attempt_at, None);
        assert_eq!(by("j_wait_a").reason_code.as_deref(), Some("fetch_retry"));
        assert_eq!(by("j_fail").reason_code, None, "free text is never echoed");
        assert_eq!(by("j_done_old").result_item_id.as_deref(), Some("item_visible"));
        assert_eq!(by("j_done_old").url.as_deref(), Some("https://synthetic.test/show/j_done_old"));
        assert_eq!(by("j_done_new").url, None);
        assert_eq!(by("j_done_new").result_item_id, None);
        assert_eq!(by("j_run").attempt_count, 2);

        let text = serde_json::to_string(&recent).unwrap();
        for leaked in ["secret_option", "/private/synthetic", "chrome_reg", "mobile_abc", "remote_iphone", "local_cli", "digest", "Raw Body"] {
            assert!(!text.contains(leaked), "{leaked}");
        }
        let job = serde_json::to_value(by("j_run")).unwrap();
        assert_eq!(job.as_object().unwrap().len(), 11);

        assert_eq!(queue.capture_jobs_recent(2, |_| true).unwrap().jobs.len(), 2);
        assert_eq!(queue.capture_jobs_recent(0, |_| true).unwrap().jobs.len(), 1);
        for index in 0..120 {
            insert(&queue, &format!("j_bulk_{index:03}"), "chrome_reg", "cancelled", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z", None, 0);
        }
        let capped = queue.capture_jobs_recent(u32::MAX, |_| true).unwrap();
        assert_eq!(capped.jobs.len(), CAPTURE_JOBS_RECENT_MAX as usize);
        assert_eq!(capped.counts.cancelled, 120);
    }
}
