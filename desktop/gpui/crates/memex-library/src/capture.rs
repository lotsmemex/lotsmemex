//! 「采集」卡片里网页那一半的纯逻辑：哪些任务要显示、摘要怎么数、每行怎么说、
//! 多久读一次。读取由 `import_view` 驱动（`memex_client::capture_jobs`）。
//!
//! 代理会一直留着历史任务，所以卡片只显示：还在进行或排队的、需处理的，以及
//! 最近 `RECENT_WINDOW` 秒内结束的；「清除已结束的项」之后，更早结束的不再显示。

use memex_client::biblio::parse_rfc3339;
use memex_client::capture_jobs::{CaptureCounts, CaptureJob, CaptureJobsRecent, CaptureStage, CaptureState};
use memex_client::{tx, txf};
use std::collections::HashSet;
use std::time::Duration;

/// 有任务在进行或排队时多久读一次。
pub const ACTIVE_POLL: Duration = Duration::from_secs(4);
/// 空闲时（只为发现新提交的任务）多久读一次。
pub const IDLE_POLL: Duration = Duration::from_secs(30);
/// 结束的任务显示多久（秒）。
pub const RECENT_WINDOW: i64 = 30 * 60;

/// 行和摘要的语气，决定颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Running,
    Waiting,
    Attention,
    Failed,
    Done,
    Muted,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureFeed {
    /// 代理是否宣告了 `capture_jobs_recent_v1`（None：还没问到）。
    pub supported: Option<bool>,
    jobs: Vec<CaptureJob>,
    counts: CaptureCounts,
    /// 已见过、还没完成的任务。
    unfinished: HashSet<String>,
    /// 「清除已结束的项」的时刻（Unix 秒）：更早结束的不再显示。
    dismissed_before: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureSummary {
    pub running: u64,
    pub waiting: u64,
    pub needs_user: u64,
    pub failed: u64,
    pub done: u64,
}

impl CaptureSummary {
    /// 非零的部分：「进行中 2」「排队 9」「需处理 1」……
    pub fn parts(&self) -> Vec<(Tone, String)> {
        [(Tone::Running, self.running, "进行中 {0}"), (Tone::Waiting, self.waiting, "排队 {0}"), (Tone::Attention, self.needs_user, "需处理 {0}"), (Tone::Failed, self.failed, "失败 {0}"), (Tone::Done, self.done, "完成 {0}")]
            .into_iter()
            .filter(|(_, n, _)| *n > 0)
            .map(|(tone, n, key)| (tone, txf(key, &[&n])))
            .collect()
    }
}

impl CaptureFeed {
    /// 用一次读取的结果替换；返回是否有之前见过的任务刚完成（列表该刷新了）。
    pub fn apply(&mut self, recent: CaptureJobsRecent) -> bool {
        self.supported = Some(true);
        let finished = recent.jobs.iter().any(|j| j.state == CaptureState::Succeeded && self.unfinished.contains(&j.job_id));
        self.unfinished = recent.jobs.iter().filter(|j| is_unfinished(j.state)).map(|j| j.job_id.clone()).collect();
        self.jobs = recent.jobs;
        self.counts = recent.counts;
        finished
    }

    /// 代理没有这项能力（或不再有）：什么都不显示。
    pub fn unsupported(&mut self) {
        *self = Self { supported: Some(false), ..Self::default() };
    }

    /// 队列里还有在进行或排队的任务。
    pub fn busy(&self) -> bool {
        self.counts.running() + self.counts.waiting() > 0
    }

    pub fn running(&self) -> bool {
        self.counts.running() > 0
    }

    pub fn poll_interval(&self) -> Duration {
        if self.busy() { ACTIVE_POLL } else { IDLE_POLL }
    }

    fn shown(&self, job: &CaptureJob, now: i64) -> bool {
        let updated = parse_rfc3339(&job.updated_at);
        match job.state {
            CaptureState::Queued | CaptureState::Leased | CaptureState::ArtifactReady | CaptureState::RetryWait => true,
            CaptureState::NeedsUser => updated.is_none_or(|t| t >= self.dismissed_before),
            CaptureState::Unknown => false,
            CaptureState::Succeeded | CaptureState::Failed | CaptureState::Cancelled => updated.is_some_and(|t| t >= self.dismissed_before && now - t <= RECENT_WINDOW),
        }
    }

    /// 要显示的任务，按代理给的顺序（进行中、排队、其余最新的在前）。
    pub fn rows(&self, now: i64) -> Vec<&CaptureJob> {
        self.jobs.iter().filter(|j| self.shown(j, now)).collect()
    }

    pub fn visible(&self, now: i64) -> bool {
        self.supported == Some(true) && (self.busy() || !self.rows(now).is_empty())
    }

    /// 进行中和排队数整个队列；需处理、失败、完成只数显示着的。
    pub fn summary(&self, now: i64) -> CaptureSummary {
        let rows = self.rows(now);
        let count = |s: CaptureState| rows.iter().filter(|j| j.state == s).count() as u64;
        CaptureSummary { running: self.counts.running(), waiting: self.counts.waiting(), needs_user: count(CaptureState::NeedsUser), failed: count(CaptureState::Failed), done: count(CaptureState::Succeeded) }
    }

    /// 「清除已结束的项」：此刻之前结束（和需处理）的不再显示。
    pub fn dismiss_finished(&mut self, now: i64) {
        self.dismissed_before = now + 1;
    }
}

fn is_unfinished(state: CaptureState) -> bool {
    matches!(state, CaptureState::Queued | CaptureState::Leased | CaptureState::ArtifactReady | CaptureState::RetryWait | CaptureState::NeedsUser)
}

pub fn tone(job: &CaptureJob) -> Tone {
    match job.state {
        CaptureState::Leased | CaptureState::ArtifactReady => Tone::Running,
        CaptureState::Queued | CaptureState::RetryWait => Tone::Waiting,
        CaptureState::NeedsUser => Tone::Attention,
        CaptureState::Failed => Tone::Failed,
        CaptureState::Succeeded => Tone::Done,
        CaptureState::Cancelled | CaptureState::Unknown => Tone::Muted,
    }
}

/// 网址的最后一段（解码后），没有路径时是主机名；看不到网址时是占位。
pub fn job_title(job: &CaptureJob) -> String {
    let Some(url) = job.url.as_deref() else { return tx("已收录的条目").into() };
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    match rest.split('/').skip(1).filter(|s| !s.is_empty()).last() {
        Some(segment) => percent_decode(segment),
        None => job_host(job).unwrap_or_default(),
    }
}

/// 网址的主机名（去掉 `www.`）。
pub fn job_host(job: &CaptureJob) -> Option<String> {
    let url = job.url.as_deref()?;
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    Some(host.strip_prefix("www.").unwrap_or(host).to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 行右侧的状态。
pub fn state_label(job: &CaptureJob, now: i64) -> String {
    match job.state {
        CaptureState::Leased => match job.stage {
            CaptureStage::Extract | CaptureStage::Resolve => tx("解析中"),
            CaptureStage::Fetch => tx("抓取中"),
            CaptureStage::Validate => tx("校验中"),
            CaptureStage::Translate => tx("翻译中"),
            CaptureStage::Commit => tx("入库中"),
            CaptureStage::Unknown => tx("处理中"),
        }
        .into(),
        CaptureState::ArtifactReady => tx("入库中").into(),
        CaptureState::Queued => tx("排队").into(),
        CaptureState::RetryWait => match job.next_attempt_at.map(|t| t - now).filter(|s| *s > 0) {
            Some(s) if s < 60 => txf("{0} 秒后重试", &[&s]),
            Some(s) => txf("{0} 分钟后重试", &[&((s + 59) / 60)]),
            None => tx("等待重试").into(),
        },
        CaptureState::NeedsUser => tx("需处理").into(),
        CaptureState::Failed => tx("失败").into(),
        CaptureState::Succeeded => tx("完成").into(),
        CaptureState::Cancelled => tx("已取消").into(),
        CaptureState::Unknown => tx("未知").into(),
    }
}

/// 失败或需处理的原因（只认得的代码；其余不显示）。
pub fn reason_label(job: &CaptureJob) -> Option<&'static str> {
    if !matches!(job.state, CaptureState::Failed | CaptureState::NeedsUser | CaptureState::RetryWait) {
        return None;
    }
    Some(match job.reason_code.as_deref()? {
        "needs_preview" => tx("需要先确认这个网址"),
        "interactive_login_required" => tx("网站需要登录"),
        "challenge_detected" => tx("网站要求人机验证"),
        "rate_limited" => tx("网站限速，稍后自动重试"),
        "fetch_retry" | "podcast_retry" | "commit_retry" | "lease_recovered" => tx("上次没成功，稍后自动重试"),
        "unsupported_content" => tx("不支持这种网页内容"),
        "network_policy_rejected" | "address_not_public" => tx("这个地址不允许抓取"),
        "retry_exhausted" => tx("多次重试仍未成功"),
        "translation_failed" => tx("翻译失败"),
        "artifact_verification_failed" => tx("抓到的内容校验没通过"),
        "podcast_conflict" => tx("和已有的单集冲突"),
        _ => return None,
    })
}

/// 最后变化距今多久。
pub fn when_label(job: &CaptureJob, now: i64) -> String {
    let Some(t) = parse_rfc3339(&job.updated_at) else { return String::new() };
    match (now - t).max(0) {
        s if s < 60 => tx("刚刚").into(),
        s if s < 3600 => txf("{0} 分钟前", &[&(s / 60)]),
        s if s < 86_400 => txf("{0} 小时前", &[&(s / 3600)]),
        s => txf("{0} 天前", &[&(s / 86_400)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::capture_jobs::{format_rfc3339, synthetic_capture_jobs, CaptureProducer, CAPTURE_JOBS_SCHEMA};

    const NOW: i64 = 1_800_000_000;

    fn recent(jobs: Vec<CaptureJob>) -> CaptureJobsRecent {
        CaptureJobsRecent { schema_version: CAPTURE_JOBS_SCHEMA, counts: CaptureCounts::of(&jobs), jobs }
    }

    fn job(id: &str, state: CaptureState, ago: i64) -> CaptureJob {
        CaptureJob { job_id: id.into(), url: Some(format!("https://www.site.test/a/{id}/")), state, stage: CaptureStage::Fetch, reason_code: None, created_at: format_rfc3339(NOW - ago), updated_at: format_rfc3339(NOW - ago), attempt_count: 0, result_item_id: None, next_attempt_at: None, producer: CaptureProducer::Chrome }
    }

    #[test]
    fn summary_counts_the_queue_and_shows_only_recent_finished_jobs() {
        let mut feed = CaptureFeed::default();
        assert!(!feed.visible(NOW), "unknown support shows nothing");
        feed.apply(recent(synthetic_capture_jobs(NOW)));
        assert!(feed.visible(NOW) && feed.busy() && feed.running());
        assert_eq!(feed.summary(NOW), CaptureSummary { running: 1, waiting: 5, needs_user: 1, failed: 1, done: 2 });
        let line: Vec<String> = feed.summary(NOW).parts().into_iter().map(|(_, s)| s).collect();
        assert_eq!(line, ["进行中 1", "排队 5", "需处理 1", "失败 1", "完成 2"]);
        assert_eq!(feed.poll_interval(), ACTIVE_POLL);

        // Old history only: hidden, polled slowly.
        feed.apply(recent(vec![job("old", CaptureState::Succeeded, RECENT_WINDOW + 5), job("x", CaptureState::Unknown, 1)]));
        assert!(!feed.visible(NOW));
        assert_eq!(feed.poll_interval(), IDLE_POLL);

        // Clearing hides what finished before, never what still runs.
        feed.apply(recent(vec![job("run", CaptureState::Leased, 1), job("done", CaptureState::Succeeded, 10), job("ask", CaptureState::NeedsUser, 20)]));
        assert_eq!(feed.rows(NOW).len(), 3);
        feed.dismiss_finished(NOW);
        assert_eq!(feed.rows(NOW).iter().map(|j| j.job_id.as_str()).collect::<Vec<_>>(), ["run"]);
        let mut later = job("ask", CaptureState::NeedsUser, 0);
        later.updated_at = format_rfc3339(NOW + 5);
        feed.apply(recent(vec![later]));
        assert_eq!(feed.rows(NOW + 5).len(), 1, "a new change shows again");

        feed.unsupported();
        assert!(!feed.visible(NOW));
        assert_eq!(feed.supported, Some(false));
    }

    #[test]
    fn a_seen_job_that_finishes_refreshes_the_list_once() {
        let mut feed = CaptureFeed::default();
        assert!(!feed.apply(recent(vec![job("a", CaptureState::Succeeded, 5)])), "already done when first seen");
        assert!(!feed.apply(recent(vec![job("b", CaptureState::Leased, 5)])));
        assert!(feed.apply(recent(vec![job("b", CaptureState::Succeeded, 1)])));
        assert!(!feed.apply(recent(vec![job("b", CaptureState::Succeeded, 1)])));
    }

    #[test]
    fn row_wording() {
        let mut j = job("Episode%20%E4%B8%80", CaptureState::Leased, 30);
        assert_eq!(job_title(&j), "Episode 一");
        assert_eq!(job_host(&j).as_deref(), Some("site.test"));
        assert_eq!(state_label(&j, NOW), "抓取中");
        assert_eq!(when_label(&j, NOW), "刚刚");
        j.url = Some("https://user@example.test?x=1".into());
        assert_eq!(job_title(&j), "example.test");
        assert_eq!(job_host(&j).as_deref(), Some("example.test"));
        j.url = Some("https://a.test/show/ep-5?utm=1#t".into());
        assert_eq!(job_title(&j), "ep-5");
        j.url = None;
        assert_eq!(job_title(&j), "已收录的条目");
        assert_eq!(job_host(&j), None);
        j.state = CaptureState::RetryWait;
        j.next_attempt_at = Some(NOW + 35);
        j.reason_code = Some("rate_limited".into());
        assert_eq!(state_label(&j, NOW), "35 秒后重试");
        assert_eq!(reason_label(&j), Some("网站限速，稍后自动重试"));
        j.next_attempt_at = Some(NOW + 61);
        assert_eq!(state_label(&j, NOW), "2 分钟后重试");
        j.next_attempt_at = None;
        assert_eq!(state_label(&j, NOW), "等待重试");
        j.state = CaptureState::Failed;
        j.reason_code = Some("something_new".into());
        assert_eq!(reason_label(&j), None);
        assert_eq!(tone(&j), Tone::Failed);
        assert_eq!(when_label(&job("h", CaptureState::Failed, 7200), NOW), "2 小时前");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
