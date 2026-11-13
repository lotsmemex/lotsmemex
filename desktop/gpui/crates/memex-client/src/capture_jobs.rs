//! URL 采集进度（`capture_jobs_recent`）：Chrome、手机或命令行提交的网页在后台
//! 逐条抓取，这里只读最近的任务和按状态的计数，给条目列表底部的「采集」卡片用。
//!
//! 合同：B 树 `interfaces/memex.local_capture_agent.md`「Local command and status
//! contract」里的 `capture_jobs_recent {max_sensitivity, limit}`，能力名
//! `capture_jobs_recent_v1`。只读：不重试、不取消、不批准。回复里没有 producer
//! 原值、选项、正文或本地路径；结果条目在当前上限下不可见时 `url` 和
//! `result_item_id` 为 null。

use crate::{Client, ClientError, Command, Sensitivity};
use serde::{Deserialize, Serialize};

/// UDS `capabilities.features` 里门控 `capture_jobs_recent` 的项。
pub const CAPTURE_JOBS_FEATURE: &str = "capture_jobs_recent_v1";

/// 本机主人自己的界面。
pub const CAPTURE_JOBS_CEILING: Sensitivity = Sensitivity::Restricted;

/// 卡片一次读多少条（代理上限 100）。
pub const CAPTURE_JOBS_LIMIT: u32 = 50;

pub const CAPTURE_JOBS_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Queued,
    Leased,
    ArtifactReady,
    NeedsUser,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
    /// 更新的代理才有的状态。
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStage {
    Extract,
    Resolve,
    Fetch,
    Validate,
    Translate,
    Commit,
    #[serde(other)]
    Unknown,
}

/// 只是类别，不是 producer 原值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProducer {
    Chrome,
    Mobile,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureJob {
    pub job_id: String,
    #[serde(default)]
    pub url: Option<String>,
    pub state: CaptureState,
    pub stage: CaptureStage,
    #[serde(default)]
    pub reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default)]
    pub result_item_id: Option<String>,
    /// Unix 秒；null 表示随时可以开始。
    #[serde(default)]
    pub next_attempt_at: Option<i64>,
    pub producer: CaptureProducer,
}

/// 整个队列按状态的计数（不只是 `jobs` 里的那些）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureCounts {
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub leased: u64,
    #[serde(default)]
    pub artifact_ready: u64,
    #[serde(default)]
    pub needs_user: u64,
    #[serde(default)]
    pub retry_wait: u64,
    #[serde(default)]
    pub succeeded: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub cancelled: u64,
}

impl CaptureCounts {
    /// 正在抓取或入库。
    pub fn running(&self) -> u64 {
        self.leased + self.artifact_ready
    }

    /// 等着轮到它（含稍后重试）。
    pub fn waiting(&self) -> u64 {
        self.queued + self.retry_wait
    }

    pub fn of(jobs: &[CaptureJob]) -> Self {
        let mut c = Self::default();
        for j in jobs {
            match j.state {
                CaptureState::Queued => c.queued += 1,
                CaptureState::Leased => c.leased += 1,
                CaptureState::ArtifactReady => c.artifact_ready += 1,
                CaptureState::NeedsUser => c.needs_user += 1,
                CaptureState::RetryWait => c.retry_wait += 1,
                CaptureState::Succeeded => c.succeeded += 1,
                CaptureState::Failed => c.failed += 1,
                CaptureState::Cancelled => c.cancelled += 1,
                CaptureState::Unknown => {}
            }
        }
        c
    }
}

/// 回复里的 `capture_jobs`：运行中的在前，然后按处理顺序排队的，其余最新的在前。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureJobsRecent {
    pub schema_version: u32,
    pub jobs: Vec<CaptureJob>,
    pub counts: CaptureCounts,
}

impl Client {
    /// 代理宣告了这项能力；能力探测失败也算没有，什么都不发。
    pub fn capture_jobs_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == CAPTURE_JOBS_FEATURE))
    }

    /// 一次 `capture_jobs_recent`（阻塞；在 UI 线程外调用）。没有能力时不发。
    pub fn capture_jobs_recent(&self) -> Result<CaptureJobsRecent, ClientError> {
        if !self.capture_jobs_supported() {
            return Err(ClientError::Unsupported);
        }
        let command = Command::CaptureJobsRecent { max_sensitivity: CAPTURE_JOBS_CEILING, limit: CAPTURE_JOBS_LIMIT };
        let recent = self.backend.send(&command)?.capture_jobs.ok_or(ClientError::InvalidFrame)?;
        if recent.schema_version != CAPTURE_JOBS_SCHEMA {
            return Err(ClientError::InvalidFrame);
        }
        Ok(recent)
    }
}

/// 合成采集任务（fixture 与开发截图）：同一网站的一串播客，一条正在抓取，
/// 几条排队，一条需处理，一条失败，两条刚完成。`now`：Unix 秒。
pub fn synthetic_capture_jobs(now: i64) -> Vec<CaptureJob> {
    let at = |ago: i64| format_rfc3339(now - ago);
    let job = |n: u32, state: CaptureState, stage: CaptureStage, ago: i64| CaptureJob {
        job_id: format!("job_synthetic_{n:02}"),
        url: Some(format!("https://podcast.example.test/episodes/episode-{n}-synthetic-show")),
        state,
        stage,
        reason_code: None,
        created_at: at(600 - i64::from(n) * 10),
        updated_at: at(ago),
        attempt_count: u32::from(state != CaptureState::Queued),
        result_item_id: None,
        next_attempt_at: None,
        producer: CaptureProducer::Chrome,
    };
    let mut jobs = vec![job(7, CaptureState::Leased, CaptureStage::Fetch, 8)];
    for n in 8..12 {
        jobs.push(job(n, CaptureState::Queued, CaptureStage::Extract, 300));
    }
    let mut retry = job(12, CaptureState::RetryWait, CaptureStage::Fetch, 40);
    retry.reason_code = Some("rate_limited".into());
    retry.next_attempt_at = Some(now + 35);
    jobs.push(retry);
    let mut needs = job(6, CaptureState::NeedsUser, CaptureStage::Validate, 90);
    needs.reason_code = Some("needs_preview".into());
    needs.producer = CaptureProducer::Mobile;
    jobs.push(needs);
    let mut failed = job(5, CaptureState::Failed, CaptureStage::Fetch, 150);
    failed.reason_code = Some("unsupported_content".into());
    jobs.push(failed);
    for (n, ago) in [(4, 60), (3, 100)] {
        let mut done = job(n, CaptureState::Succeeded, CaptureStage::Commit, ago);
        done.result_item_id = Some(format!("item_synthetic_capture_{n}"));
        jobs.push(done);
    }
    jobs
}

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`。
pub fn format_rfc3339(seconds: i64) -> String {
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureBackend, FixtureControls};

    #[test]
    fn wire_shape_and_lenient_reply() {
        let command = Command::CaptureJobsRecent { max_sensitivity: CAPTURE_JOBS_CEILING, limit: 50 };
        assert_eq!(serde_json::to_value(&command).unwrap(), serde_json::json!({"command": "capture_jobs_recent", "max_sensitivity": "restricted", "limit": 50}));
        assert!(command.is_capability_gated());
        // The reply exactly as B serializes it, plus a state this client does not know yet.
        let reply: crate::Response = serde_json::from_value(serde_json::json!({"ok": true, "jobs": null, "capture_jobs": {
            "schema_version": 1,
            "jobs": [
                {"job_id": "j1", "url": "https://a.test/x", "state": "leased", "stage": "fetch", "reason_code": null, "created_at": "2026-11-09T10:00:00+00:00", "updated_at": "2026-11-09T10:00:05.123+00:00", "attempt_count": 1, "result_item_id": null, "next_attempt_at": null, "producer": "chrome"},
                {"job_id": "j2", "url": null, "state": "paused_for_later", "stage": "polish", "reason_code": null, "created_at": "t", "updated_at": "t", "attempt_count": 0, "result_item_id": null, "next_attempt_at": 1900000000, "producer": "watch"}
            ],
            "counts": {"queued": 2, "leased": 1, "artifact_ready": 0, "needs_user": 0, "retry_wait": 1, "succeeded": 9, "failed": 0, "cancelled": 0}
        }})).unwrap();
        let recent = reply.capture_jobs.unwrap();
        assert_eq!(recent.jobs[0].state, CaptureState::Leased);
        assert_eq!(recent.jobs[1].state, CaptureState::Unknown);
        assert_eq!(recent.jobs[1].stage, CaptureStage::Unknown);
        assert_eq!(recent.jobs[1].producer, CaptureProducer::Other);
        assert_eq!((recent.counts.running(), recent.counts.waiting()), (1, 3));
    }

    #[test]
    fn reads_the_fixture_queue_and_sends_nothing_without_the_feature() {
        let fixture = FixtureBackend::new(4);
        let client = Client::new(fixture.clone());
        assert!(client.capture_jobs_supported());
        assert!(client.capture_jobs_recent().unwrap().jobs.is_empty(), "an idle synthetic queue");
        fixture.set_capture_jobs(synthetic_capture_jobs(1_800_000_000));
        let recent = client.capture_jobs_recent().unwrap();
        assert_eq!(recent.jobs.len(), 10);
        assert_eq!(recent.counts, CaptureCounts { queued: 4, leased: 1, retry_wait: 1, needs_user: 1, failed: 1, succeeded: 2, ..Default::default() });
        assert_eq!(fixture.capture_job_calls(), 2);

        fixture.set_controls(FixtureControls { no_capture_jobs: true, ..Default::default() });
        client.forget_capabilities();
        assert!(!client.capture_jobs_supported());
        assert_eq!(client.capture_jobs_recent(), Err(ClientError::Unsupported));
        fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
        client.forget_capabilities();
        assert_eq!(client.capture_jobs_recent(), Err(ClientError::Unsupported));
        assert_eq!(fixture.capture_job_calls(), 2, "nothing reached the agent");
    }

    #[test]
    fn timestamps_round_trip() {
        for t in [0, 1_800_000_000, 951_782_400, 1_709_164_799] {
            assert_eq!(crate::biblio::parse_rfc3339(&format_rfc3339(t)), Some(t));
        }
        assert_eq!(format_rfc3339(1_800_000_000), "2027-01-15T08:00:00Z");
    }
}
