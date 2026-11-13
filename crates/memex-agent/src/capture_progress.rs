//! Capture progress presentation: pure functions over a `capture_jobs_recent` reply.
//! No I/O, no GPUI, no HTTP, no clock: callers pass `now` (Unix seconds) and a language.
//! Shared definition of "what the 采集 card shows" for the CLI (`memex jobs recent`),
//! the Mac card (mirrored in `desktop/gpui/crates/memex-library/src/capture.rs`, which
//! cannot depend on this crate) and later Lots MemeX / Yuanfang clients.
use crate::queue::recent::{CaptureJobSummary, CaptureJobsRecent};
use crate::queue::{Stage, State};
use serde::{Deserialize, Serialize};

/// Finished jobs stay "recent" this long (seconds) in the card summary.
pub const RECENT_WINDOW: i64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Zh,
    En,
}

/// What a state means to the owner; decides color and grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Running,
    Waiting,
    Attention,
    Failed,
    Done,
    Muted,
}

pub fn category(state: State) -> Category {
    match state {
        State::Leased | State::ArtifactReady => Category::Running,
        State::Queued | State::RetryWait => Category::Waiting,
        State::NeedsUser => Category::Attention,
        State::Failed => Category::Failed,
        State::Succeeded => Category::Done,
        State::Cancelled => Category::Muted,
    }
}

/// Card summary: `running`/`waiting` over the whole queue; `needs_user`, `failed`,
/// `done` only among the returned jobs updated within `RECENT_WINDOW`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressSummary {
    pub running: u64,
    pub waiting: u64,
    pub needs_user: u64,
    pub failed: u64,
    pub done: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDisplay {
    pub job_id: String,
    pub category: Category,
    /// Last URL path segment (decoded) or host; a placeholder when the URL is hidden.
    pub title: String,
    pub host: Option<String>,
    pub state_label: String,
    pub reason_label: Option<String>,
    pub when_label: Option<String>,
    /// Unfinished, or finished within `RECENT_WINDOW`.
    pub recent: bool,
    pub result_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProgress {
    pub schema_version: u32,
    pub summary: ProgressSummary,
    /// e.g. 「进行中 2 · 排队 9 · 需处理 1」; empty when nothing to report.
    pub summary_line: String,
    /// Busy queue: poll again soon (the Mac card uses 4 s, else 30 s).
    pub busy: bool,
    pub jobs: Vec<JobDisplay>,
}

fn t(lang: Lang, zh: &str, en: &str) -> String {
    match lang {
        Lang::Zh => zh.into(),
        Lang::En => en.into(),
    }
}

fn parse_seconds(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|d| d.timestamp())
}

pub fn is_recent(job: &CaptureJobSummary, now: i64) -> bool {
    match category(job.state) {
        Category::Running | Category::Waiting | Category::Attention => true,
        _ => parse_seconds(&job.updated_at).is_some_and(|t| now - t <= RECENT_WINDOW),
    }
}

pub fn summarize(recent: &CaptureJobsRecent, now: i64) -> ProgressSummary {
    let shown = |s: State| recent.jobs.iter().filter(|j| j.state == s && is_recent(j, now)).count() as u64;
    ProgressSummary {
        running: recent.counts.leased + recent.counts.artifact_ready,
        waiting: recent.counts.queued + recent.counts.retry_wait,
        needs_user: shown(State::NeedsUser),
        failed: shown(State::Failed),
        done: shown(State::Succeeded),
    }
}

pub fn summary_line(summary: &ProgressSummary, lang: Lang) -> String {
    [
        (summary.running, "进行中", "in progress"),
        (summary.waiting, "排队", "queued"),
        (summary.needs_user, "需处理", "need attention"),
        (summary.failed, "失败", "failed"),
        (summary.done, "完成", "done"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, zh, en)| match lang {
        Lang::Zh => format!("{zh} {n}"),
        Lang::En => format!("{n} {en}"),
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

pub fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    Some(host.strip_prefix("www.").unwrap_or(host).to_string())
}

pub fn title(url: Option<&str>, lang: Lang) -> String {
    let Some(url) = url else { return t(lang, "已收录的条目", "Saved item") };
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    match rest.split('/').skip(1).filter(|s| !s.is_empty()).last() {
        Some(segment) => percent_decode(segment),
        None => host(url).unwrap_or_default(),
    }
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

pub fn state_label(job: &CaptureJobSummary, now: i64, lang: Lang) -> String {
    match job.state {
        State::Leased => match job.stage {
            Stage::Extract | Stage::Resolve => t(lang, "解析中", "Resolving"),
            Stage::Fetch => t(lang, "抓取中", "Fetching"),
            Stage::Validate => t(lang, "校验中", "Checking"),
            Stage::Translate => t(lang, "翻译中", "Translating"),
            Stage::Commit => t(lang, "入库中", "Saving"),
        },
        State::ArtifactReady => t(lang, "入库中", "Saving"),
        State::Queued => t(lang, "排队", "Queued"),
        State::RetryWait => match job.next_attempt_at.map(|at| at - now).filter(|s| *s > 0) {
            Some(s) if s < 60 => match lang {
                Lang::Zh => format!("{s} 秒后重试"),
                Lang::En => format!("Retry in {s} s"),
            },
            Some(s) => match lang {
                Lang::Zh => format!("{} 分钟后重试", (s + 59) / 60),
                Lang::En => format!("Retry in {} min", (s + 59) / 60),
            },
            None => t(lang, "等待重试", "Waiting to retry"),
        },
        State::NeedsUser => t(lang, "需处理", "Needs attention"),
        State::Failed => t(lang, "失败", "Failed"),
        State::Succeeded => t(lang, "完成", "Done"),
        State::Cancelled => t(lang, "已取消", "Cancelled"),
    }
}

/// Only known machine codes get words; anything else shows nothing.
pub fn reason_label(job: &CaptureJobSummary, lang: Lang) -> Option<String> {
    if !matches!(job.state, State::Failed | State::NeedsUser | State::RetryWait) {
        return None;
    }
    let (zh, en) = match job.reason_code.as_deref()? {
        "needs_preview" => ("需要先确认这个网址", "Confirm this address first"),
        "interactive_login_required" => ("网站需要登录", "The site requires signing in"),
        "challenge_detected" => ("网站要求人机验证", "The site asks for a human check"),
        "rate_limited" => ("网站限速，稍后自动重试", "Rate-limited by the site; retrying later"),
        "fetch_retry" | "podcast_retry" | "commit_retry" | "lease_recovered" => ("上次没成功，稍后自动重试", "Last try failed; retrying later"),
        "unsupported_content" => ("不支持这种网页内容", "This kind of page is not supported"),
        "network_policy_rejected" | "address_not_public" => ("这个地址不允许抓取", "This address may not be fetched"),
        "retry_exhausted" => ("多次重试仍未成功", "Still failing after several retries"),
        "translation_failed" => ("翻译失败", "Translation failed"),
        "artifact_verification_failed" => ("抓到的内容校验没通过", "The fetched content failed verification"),
        "chrome_unavailable" => ("本机没有找到 Chrome", "Chrome was not found on this Mac"),
        "image_too_large" => ("文章里有图片超过 16 MB", "An image in the article is larger than 16 MB"),
        "page_changed" => ("打开后跳到了别的页面", "The page redirected somewhere else"),
        "podcast_conflict" => ("和已有的单集冲突", "Conflicts with an existing episode"),
        _ => return None,
    };
    Some(t(lang, zh, en))
}

pub fn when_label(updated_at: &str, now: i64, lang: Lang) -> Option<String> {
    let s = (now - parse_seconds(updated_at)?).max(0);
    Some(match (s, lang) {
        (s, Lang::Zh) if s < 60 => "刚刚".into(),
        (s, Lang::En) if s < 60 => "Just now".into(),
        (s, Lang::Zh) if s < 3600 => format!("{} 分钟前", s / 60),
        (s, Lang::En) if s < 3600 => format!("{} min ago", s / 60),
        (s, Lang::Zh) if s < 86_400 => format!("{} 小时前", s / 3600),
        (s, Lang::En) if s < 86_400 => format!("{} h ago", s / 3600),
        (s, Lang::Zh) => format!("{} 天前", s / 86_400),
        (s, Lang::En) => format!("{} d ago", s / 86_400),
    })
}

pub fn display(job: &CaptureJobSummary, now: i64, lang: Lang) -> JobDisplay {
    JobDisplay {
        job_id: job.job_id.clone(),
        category: category(job.state),
        title: title(job.url.as_deref(), lang),
        host: job.url.as_deref().and_then(host),
        state_label: state_label(job, now, lang),
        reason_label: reason_label(job, lang),
        when_label: when_label(&job.updated_at, now, lang),
        recent: is_recent(job, now),
        result_item_id: job.result_item_id.clone(),
    }
}

pub fn project(recent: &CaptureJobsRecent, now: i64, lang: Lang) -> CaptureProgress {
    let summary = summarize(recent, now);
    CaptureProgress {
        schema_version: 1,
        summary,
        summary_line: summary_line(&summary, lang),
        busy: summary.running + summary.waiting > 0,
        jobs: recent.jobs.iter().map(|j| display(j, now, lang)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::recent::{CaptureJobCounts, ProducerKind};

    const NOW: i64 = 1_800_000_000;

    fn at(seconds: i64) -> String {
        chrono::DateTime::from_timestamp(seconds, 0).unwrap().to_rfc3339()
    }

    fn job(id: &str, state: State, stage: Stage, ago: i64) -> CaptureJobSummary {
        CaptureJobSummary {
            job_id: id.into(),
            url: Some(format!("https://www.show.test/episodes/{id}")),
            state,
            stage,
            reason_code: None,
            created_at: at(NOW - 900),
            updated_at: at(NOW - ago),
            attempt_count: 1,
            result_item_id: (state == State::Succeeded).then(|| format!("item_{id}")),
            next_attempt_at: None,
            producer: ProducerKind::Chrome,
        }
    }

    #[test]
    fn projection_classifies_counts_and_words_jobs() {
        let mut retry = job("ep-3", State::RetryWait, Stage::Fetch, 20);
        retry.reason_code = Some("rate_limited".into());
        retry.next_attempt_at = Some(NOW + 35);
        let mut failed = job("ep-2", State::Failed, Stage::Fetch, 60);
        failed.reason_code = Some("Raw text".into());
        let mut hidden = job("ep-0", State::Succeeded, Stage::Commit, 120);
        hidden.url = None;
        hidden.result_item_id = None;
        let jobs = vec![
            job("Episode%20%E4%B8%80", State::Leased, Stage::Fetch, 5),
            job("ep-4", State::Queued, Stage::Extract, 300),
            retry,
            job("ep-9", State::NeedsUser, Stage::Validate, 90),
            failed,
            job("ep-1", State::Succeeded, Stage::Commit, 100),
            hidden,
            job("ep-old", State::Succeeded, Stage::Commit, RECENT_WINDOW + 10),
        ];
        let recent = CaptureJobsRecent {
            schema_version: 1,
            jobs,
            counts: CaptureJobCounts { leased: 1, queued: 7, retry_wait: 1, needs_user: 1, failed: 1, succeeded: 40, ..Default::default() },
        };
        let zh = project(&recent, NOW, Lang::Zh);
        assert_eq!(zh.summary, ProgressSummary { running: 1, waiting: 8, needs_user: 1, failed: 1, done: 2 });
        assert_eq!(zh.summary_line, "进行中 1 · 排队 8 · 需处理 1 · 失败 1 · 完成 2");
        assert!(zh.busy);
        let j = &zh.jobs;
        assert_eq!((j[0].title.as_str(), j[0].host.as_deref(), j[0].state_label.as_str(), j[0].category), ("Episode 一", Some("show.test"), "抓取中", Category::Running));
        assert_eq!(j[0].when_label.as_deref(), Some("刚刚"));
        assert_eq!((j[2].state_label.as_str(), j[2].reason_label.as_deref()), ("35 秒后重试", Some("网站限速，稍后自动重试")));
        assert_eq!(j[3].category, Category::Attention);
        assert_eq!((j[4].category, j[4].reason_label.as_deref()), (Category::Failed, None), "free text is never worded");
        assert_eq!((j[6].title.as_str(), j[6].host.as_deref()), ("已收录的条目", None));
        assert!(!j[7].recent && j[5].recent);
        let en = project(&recent, NOW, Lang::En);
        assert_eq!(en.summary_line, "1 in progress · 8 queued · 1 need attention · 1 failed · 2 done");
        assert_eq!(en.jobs[0].state_label, "Fetching");
        let idle = CaptureJobsRecent { schema_version: 1, jobs: vec![], counts: CaptureJobCounts::default() };
        let idle = project(&idle, NOW, Lang::Zh);
        assert!(!idle.busy && idle.summary_line.is_empty());
        assert_eq!(when_label(&at(NOW - 7200), NOW, Lang::Zh).as_deref(), Some("2 小时前"));
        assert_eq!(title(Some("https://user@example.test?x=1"), Lang::Zh), "example.test");
    }
}
