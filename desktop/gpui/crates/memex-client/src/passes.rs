//! `reader_passes_v1` (阅读记录): reading passes with manual dates and method,
//! retrospective entries (补记), and opt-in active-time reports. Every op is a
//! `reader` command op; nothing is sent unless the agent advertises the feature.
//! Also the pure display rules (dates, durations, the three rates) both the
//! reader card and the statistics panel use.

use crate::book::ReaderRequest;
use crate::i18n::{language, tx, txf, Lang};
use crate::{Client, ClientError};
use serde::{Deserialize, Serialize};

pub const PASSES_FEATURE: &str = "reader_passes_v1";
/// Largest `passes_list` page.
pub const PASSES_PAGE: usize = 100;
/// Largest `passes_stats` page (days).
pub const STATS_PAGE: usize = 400;
pub const MAX_ESTIMATE_MINUTES: u32 = 525_600;
pub const MAX_SEGMENT_MS: u64 = 300_000;
pub const MAX_REPORT_SEGMENTS: usize = 64;
const MAX_PAGES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Full,
    Sampled,
    Structural,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::Full, Method::Sampled, Method::Structural];
    pub fn label(self) -> &'static str {
        match self {
            Method::Full => tx("全文通读"),
            Method::Sampled => tx("抽样阅读"),
            Method::Structural => tx("结构阅读"),
        }
    }
}

pub fn method_label(m: Option<Method>) -> &'static str {
    m.map_or(tx("未选方式"), Method::label)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Year,
    Month,
    Day,
    /// Anything else read back (never written).
    #[serde(other)]
    Unknown,
}

/// PartialDate `{value, precision, raw?}`: `value` is YYYY / YYYY-MM / YYYY-MM-DD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassDate {
    pub value: Option<String>,
    pub precision: Precision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

impl PassDate {
    /// A valid Gregorian date at the precision given by which parts are present
    /// (a day needs its month); `None` otherwise. Nothing is filled in.
    pub fn new(y: i32, m: Option<u32>, d: Option<u32>) -> Option<PassDate> {
        if !(1..=9999).contains(&y) { return None; }
        let (value, precision) = match (m, d) {
            (None, None) => (format!("{y:04}"), Precision::Year),
            (Some(m), None) if (1..=12).contains(&m) => (format!("{y:04}-{m:02}"), Precision::Month),
            (Some(m), Some(d)) if (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m) => (format!("{y:04}-{m:02}-{d:02}"), Precision::Day),
            _ => return None,
        };
        Some(PassDate { value: Some(value), precision, raw: None })
    }

    pub fn day(y: i32, m: u32, d: u32) -> Option<PassDate> {
        Self::new(y, Some(m), Some(d))
    }

    /// (year, month, day) as stored at this precision, when it parses.
    pub fn parts(&self) -> Option<(i32, Option<u32>, Option<u32>)> {
        let v = self.value.as_deref()?;
        let num = |s: Option<&str>| s.and_then(|s| s.parse::<u32>().ok());
        let mut it = v.split('-');
        let y: i32 = it.next()?.parse().ok()?;
        let parts = match self.precision {
            Precision::Year if v.len() == 4 => (y, None, None),
            Precision::Month if v.len() == 7 => (y, Some(num(it.next())?), None),
            Precision::Day if v.len() == 10 => (y, Some(num(it.next())?), Some(num(it.next())?)),
            _ => return None,
        };
        Self::new(parts.0, parts.1, parts.2).map(|_| parts)
    }

    /// Earliest and latest possible calendar day of this partial date.
    pub fn bounds(&self) -> Option<((i32, u32, u32), (i32, u32, u32))> {
        let (y, m, d) = self.parts()?;
        Some(match (m, d) {
            (None, _) => ((y, 1, 1), (y, 12, 31)),
            (Some(m), None) => ((y, m, 1), (y, m, days_in_month(y, m))),
            (Some(m), Some(d)) => ((y, m, d), (y, m, d)),
        })
    }
}

/// Rejects only an end whose latest possible day is before the start's
/// earliest; unknown or overlapping ranges are allowed (order stays uncertain).
pub fn dates_possible(start: Option<&PassDate>, end: Option<&PassDate>) -> bool {
    match (start.and_then(PassDate::bounds), end.and_then(PassDate::bounds)) {
        (Some((earliest, _)), Some((_, latest))) => latest >= earliest,
        _ => true,
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A stored date at its own precision: the current year is left out of month
/// and day dates, a year-only date of this year is 今年, other years stay.
pub fn date_label(date: Option<&PassDate>, this_year: i32) -> String {
    date_label_in(language(), date, this_year)
}

pub fn date_label_in(lang: Lang, date: Option<&PassDate>, this_year: i32) -> String {
    let Some((y, m, d)) = date.and_then(PassDate::parts) else {
        return if lang == Lang::En { "Unknown".into() } else { "未知".into() };
    };
    let same = y == this_year;
    match lang {
        Lang::Zh => match (m, d) {
            (None, _) if same => "今年".into(),
            (None, _) => format!("{y}年"),
            (Some(m), None) if same => format!("{m}月"),
            (Some(m), None) => format!("{y}年{m}月"),
            (Some(m), Some(d)) if same => format!("{m}月{d}日"),
            (Some(m), Some(d)) => format!("{y}年{m}月{d}日"),
        },
        Lang::En => {
            let mon = |m: u32| MONTHS[(m as usize - 1) % 12];
            match (m, d) {
                (None, _) if same => "This year".into(),
                (None, _) => format!("{y}"),
                (Some(m), None) if same => mon(m).into(),
                (Some(m), None) => format!("{} {y}", mon(m)),
                (Some(m), Some(d)) if same => format!("{} {d}", mon(m)),
                (Some(m), Some(d)) => format!("{} {d}, {y}", mon(m)),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationUnit {
    Minutes,
    Hours,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationError {
    Empty,
    NotNumber,
    Zero,
    TooLong,
    NotWholeMinutes,
}

impl DurationError {
    pub fn message(self) -> &'static str {
        match self {
            DurationError::Empty => tx("请填写大约的时长"),
            DurationError::NotNumber => tx("时长只能填数字"),
            DurationError::Zero => tx("时长要大于 0"),
            DurationError::TooLong => tx("时长不能超过 525600 分钟（一年）"),
            DurationError::NotWholeMinutes => tx("换算后不是整分钟，请改填分钟"),
        }
    }
}

/// Approximate minutes, or decimal hours converted exactly: a value that is not
/// a whole number of minutes, zero, negative or over a year is refused, never
/// rounded or clipped.
pub fn parse_duration(text: &str, unit: DurationUnit) -> Result<u32, DurationError> {
    let t = text.trim();
    if t.is_empty() { return Err(DurationError::Empty); }
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) || frac.len() > 9 || int.len() > 9 { return Err(DurationError::NotNumber); }
    let scale = 10u128.pow(frac.len() as u32);
    let value = format!("{int}{frac}").parse::<u128>().map_err(|_| DurationError::NotNumber)?;
    let factor = if unit == DurationUnit::Hours { 60 } else { 1 };
    if value == 0 { return Err(DurationError::Zero); }
    let scaled = value * factor;
    if scaled % scale != 0 { return Err(DurationError::NotWholeMinutes); }
    let minutes = scaled / scale;
    if minutes > MAX_ESTIMATE_MINUTES as u128 { return Err(DurationError::TooLong); }
    Ok(minutes as u32)
}

/// Whole minutes as 「1 小时 46 分」 / 「22 分」.
pub fn minutes_label(m: u64) -> String {
    let en = language() == Lang::En;
    match (m / 60, m % 60) {
        (0, r) => if en { format!("{r} min") } else { format!("{r} 分") },
        (h, 0) => if en { format!("{h} h") } else { format!("{h} 小时") },
        (h, r) => if en { format!("{h} h {r} min") } else { format!("{h} 小时 {r} 分") },
    }
}

/// Milliseconds rounded to minutes only for display; a nonzero sliver under half
/// a minute says so instead of showing zero.
pub fn ms_label(ms: u64) -> String {
    if ms > 0 && ms < 30_000 { return tx("不到 1 分").into(); }
    minutes_label((ms + 30_000) / 60_000)
}

/// 12,345.
pub fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassKind {
    Tracked,
    Retrospective,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassStatus {
    InProgress,
    Completed,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LengthSide {
    pub side: String,
    pub sha256: String,
    pub words: Option<u64>,
    pub characters: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lengths {
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub source: Option<LengthSide>,
    #[serde(default)]
    pub translation: Option<LengthSide>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub timing_version: Option<String>,
    pub accepted_ms: u64,
    pub unique_ms: u64,
    pub overlap_excluded_ms: u64,
    pub source_ms: u64,
    pub bilingual_ms: u64,
    pub translation_ms: u64,
    pub segment_count: u64,
    pub segment_limit: u64,
    pub recording_paused: bool,
}

/// One view's rate, per unit; never one mixed scalar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnitRate {
    #[serde(default)]
    pub words_per_minute: Option<f64>,
    #[serde(default)]
    pub characters_per_minute: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub basis: String,
    pub minutes: f64,
    pub estimate: bool,
    pub equivalent_full_text: bool,
    #[serde(default)]
    pub source: Option<UnitRate>,
    #[serde(default)]
    pub translation: Option<UnitRate>,
    #[serde(default)]
    pub combined: Option<UnitRate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pass {
    pub pass_id: String,
    pub ordinal: u64,
    pub display_rank: u64,
    pub kind: PassKind,
    pub status: PassStatus,
    pub method: Option<Method>,
    pub start_date: Option<PassDate>,
    pub end_date: Option<PassDate>,
    pub estimated_duration_minutes: Option<u32>,
    pub duration_display_unit: Option<DurationUnit>,
    pub timezone: String,
    pub revision_basis: Option<String>,
    pub revision_id: Option<String>,
    pub canonical_sha256: Option<String>,
    pub source_status: Option<String>,
    #[serde(default)]
    pub lengths: Lengths,
    pub created_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    #[serde(default)]
    pub timing: Option<Timing>,
    #[serde(default)]
    pub rates: Option<Rates>,
}

impl Pass {
    pub fn is_tracked(&self) -> bool { self.kind == PassKind::Tracked }
    pub fn in_progress(&self) -> bool { self.status == PassStatus::InProgress }
    pub fn unique_ms(&self) -> u64 { self.timing.as_ref().map_or(0, |t| t.unique_ms) }
    pub fn timing_version(&self) -> Option<String> { self.timing.as_ref().and_then(|t| t.timing_version.clone()) }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PassList {
    pub item_id: String,
    pub version: String,
    pub current_revision_id: Option<String>,
    pub passes: Vec<Pass>,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub pass_limit: usize,
}

impl PassList {
    pub fn get(&self, pass_id: &str) -> Option<&Pass> {
        self.passes.iter().find(|p| p.pass_id == pass_id)
    }

    /// Newest first by display rank (按填写日期排序).
    pub fn history(&self) -> Vec<&Pass> {
        let mut v: Vec<_> = self.passes.iter().collect();
        v.sort_by(|a, b| b.display_rank.cmp(&a.display_rank).then(b.ordinal.cmp(&a.ordinal)));
        v
    }

    /// The unfinished tracked pass a timer continues on this revision (the newest).
    pub fn open_tracked(&self, revision_id: &str) -> Option<&Pass> {
        self.passes.iter().filter(|p| p.is_tracked() && p.in_progress() && p.revision_id.as_deref() == Some(revision_id)).max_by_key(|p| p.ordinal)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsDay {
    pub date: String,
    pub ms: u64,
    pub source_ms: u64,
    pub bilingual_ms: u64,
    pub translation_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PassStats {
    pub item_id: String,
    pub pass_id: String,
    pub timezone: String,
    pub timing_version: Option<String>,
    pub days: Vec<StatsDay>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

/// One pass write; `pass_id` is client-generated and starts with `pass_`.
/// Optional values are sent as `null`, never left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PassChange {
    CreateTracked { pass_id: String, revision_id: String, method: Option<Method>, start_date: Option<PassDate>, timezone: String },
    CreateRetrospective {
        pass_id: String,
        method: Option<Method>,
        start_date: Option<PassDate>,
        end_date: Option<PassDate>,
        estimated_duration_minutes: Option<u32>,
        duration_display_unit: Option<DurationUnit>,
        timezone: String,
        reference_revision_id: Option<String>,
    },
    Update { pass_id: String, method: Option<Method>, start_date: Option<PassDate>, end_date: Option<PassDate>, estimated_duration_minutes: Option<u32>, duration_display_unit: Option<DurationUnit> },
    Complete { pass_id: String, end_date: Option<PassDate>, expected_timing_version: Option<String> },
    Reopen { pass_id: String },
    Delete { pass_id: String, expected_timing_version: Option<String> },
}

impl PassChange {
    pub fn pass_id(&self) -> &str {
        match self {
            PassChange::CreateTracked { pass_id, .. }
            | PassChange::CreateRetrospective { pass_id, .. }
            | PassChange::Update { pass_id, .. }
            | PassChange::Complete { pass_id, .. }
            | PassChange::Reopen { pass_id }
            | PassChange::Delete { pass_id, .. } => pass_id,
        }
    }

    /// Whether `list` already shows this change applied (read back after an unknown reply).
    pub fn visible_in(&self, list: &PassList) -> bool {
        let p = list.get(self.pass_id());
        match self {
            PassChange::CreateTracked { revision_id, .. } => p.is_some_and(|p| p.is_tracked() && p.revision_id.as_deref() == Some(revision_id)),
            PassChange::CreateRetrospective { .. } => p.is_some_and(|p| p.kind == PassKind::Retrospective),
            PassChange::Update { method, start_date, end_date, estimated_duration_minutes, duration_display_unit, .. } => p.is_some_and(|p| {
                p.method == *method && p.start_date == *start_date && p.end_date == *end_date && p.estimated_duration_minutes == *estimated_duration_minutes && p.duration_display_unit == *duration_display_unit
            }),
            PassChange::Complete { end_date, .. } => p.is_some_and(|p| p.status == PassStatus::Completed && p.end_date == *end_date),
            PassChange::Reopen { .. } => p.is_some_and(Pass::in_progress),
            PassChange::Delete { .. } => p.is_none(),
        }
    }
}

/// A fresh client pass id.
pub fn new_pass_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("pass_mac_{nanos:x}")
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PassReceipt {
    pub item_id: String,
    pub version: String,
    pub pass_id: String,
    pub ordinal: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TimingMode {
    Source,
    Bilingual,
    Translation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingSegment {
    pub segment_id: String,
    pub mode: TimingMode,
    pub start_offset_ms: u64,
    pub end_offset_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TimingSession {
    pub session_id: String,
    pub item_id: String,
    pub pass_id: String,
    pub revision_id: String,
    pub origin_utc_ms: u64,
    pub window_ms: u64,
    pub max_segment_ms: u64,
    pub report_deadline_utc_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SegmentOutcome {
    pub segment_id: String,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TimingReport {
    pub item_id: String,
    pub pass_id: String,
    pub session_id: String,
    pub outcomes: Vec<SegmentOutcome>,
    pub totals: Timing,
}

/// How one write ended.
#[derive(Debug, Clone, PartialEq)]
pub enum PassWrite<T> {
    Done(T),
    /// `stale_reader_passes`: read again (the caller may replay its intent once).
    Stale,
    /// A refusal made before anything was written.
    Rejected(String),
    /// May have been committed: keep the attempt, read back, resend only on request.
    Unknown(String),
}

/// Refusals the daemon makes before anything is written.
pub const DEFINITE_REFUSALS: &[&str] = &[
    "invalid_reader_pass", "invalid_reader_pass_dates", "invalid_reader_pass_timing", "stale_reader_passes", "stale_reader_pass_timing",
    "reader_pass_conflict", "reader_pass_not_found", "reader_pass_limit_exceeded", "reader_pass_closed", "reader_pass_not_timeable",
    "reader_pass_session_expired", "reader_pass_clock_discontinuity", "reader_pass_segment_conflict", "library_resource_not_found",
    "stale_item_revision", "permission_denied", "invalid_request",
];

/// A timing session the daemon no longer admits: start a new one, never move old segments into it.
pub fn session_gone(code: &str) -> bool {
    code == "reader_pass_session_expired" || code == "reader_pass_clock_discontinuity"
}

fn classify<T: serde::de::DeserializeOwned>(reply: Result<serde_json::Value, ClientError>) -> PassWrite<T> {
    match reply {
        Ok(v) => match serde_json::from_value::<T>(v) {
            Ok(r) => PassWrite::Done(r),
            Err(_) => PassWrite::Unknown(ClientError::InvalidFrame.message()),
        },
        Err(ClientError::Daemon(code)) if code == "stale_reader_passes" => PassWrite::Stale,
        Err(ClientError::Daemon(code)) if DEFINITE_REFUSALS.contains(&code.as_str()) => PassWrite::Rejected(code),
        // internal_error, outcome_unknown, reader_passes_unavailable after dispatch,
        // a lost reply and anything unrecognised may have been committed.
        Err(e) => PassWrite::Unknown(e.message()),
    }
}

/// One write the caller keeps exactly as sent while its outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassAttempt {
    pub item_id: String,
    pub expected_version: String,
    pub change: PassChange,
}

/// The result of [`Client::passes_write`].
#[derive(Debug, Clone, PartialEq)]
pub enum WriteResult {
    Saved { receipt: Option<PassReceipt>, list: Option<PassList> },
    Refused { code: String, list: Option<PassList> },
    Unknown { attempt: PassAttempt, message: String, list: Option<PassList> },
}

/// What one refusal tells the person (the code otherwise).
pub fn refusal_message(code: &str) -> String {
    match code {
        "invalid_reader_pass" => tx("这一遍的内容不能保存").into(),
        "invalid_reader_pass_dates" => tx("结束日期早于开始日期，或日期无效").into(),
        "invalid_reader_pass_timing" => tx("计时数据没有被接受").into(),
        "stale_reader_passes" => tx("阅读记录已在别处更新，请重新载入").into(),
        "stale_reader_pass_timing" => tx("这一遍的计时刚有更新，请再试一次").into(),
        "reader_pass_conflict" => tx("这一遍已存在或已被删除").into(),
        "reader_pass_not_found" => tx("找不到这一遍，可能已在别处删除").into(),
        "reader_pass_limit_exceeded" => tx("这篇的阅读记录已达上限").into(),
        "reader_pass_closed" => tx("这一遍已读完，不再计时").into(),
        "reader_pass_not_timeable" => tx("补记的遍不能计时").into(),
        "permission_denied" => tx("当前权限不能修改阅读记录").into(),
        other => ClientError::Daemon(other.into()).message(),
    }
}

impl Client {
    fn require_passes(&self) -> Result<(), ClientError> {
        if self.capabilities()?.features.iter().any(|f| f == PASSES_FEATURE) { Ok(()) } else { Err(ClientError::Unsupported) }
    }

    pub fn passes_supported(&self) -> bool {
        self.require_passes().is_ok()
    }

    /// Every pass of an item at one version; a page from another version restarts (three times at most).
    pub fn passes_list(&self, item_id: &str) -> Result<PassList, ClientError> {
        self.require_passes()?;
        'read: for _ in 0..3 {
            let (mut all, mut version) = (Vec::new(), None::<String>);
            for _ in 0..MAX_PAGES {
                let request = ReaderRequest::PassesList { item_id: item_id.into(), offset: all.len(), limit: PASSES_PAGE, expected_version: version.clone() };
                let page: PassList = match self.reader(request) {
                    Ok(v) => serde_json::from_value(v).map_err(|_| ClientError::InvalidFrame)?,
                    Err(ClientError::Daemon(code)) if code == "stale_reader_passes" && version.is_some() => continue 'read,
                    Err(e) => return Err(e),
                };
                if page.item_id != item_id { return Err(ClientError::InvalidFrame); }
                if version.as_ref().is_some_and(|v| *v != page.version) { continue 'read; }
                version = Some(page.version.clone());
                let got = page.passes.len();
                all.extend(page.passes.iter().cloned());
                match page.next_offset {
                    Some(next) if next == all.len() && got > 0 => continue,
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => return Ok(PassList { passes: all, next_offset: None, ..page }),
                }
            }
            return Err(ClientError::InvalidFrame);
        }
        Err(ClientError::Daemon("stale_reader_passes".into()))
    }

    /// Every day of one pass at one timing version.
    pub fn passes_stats(&self, item_id: &str, pass_id: &str) -> Result<PassStats, ClientError> {
        self.require_passes()?;
        'read: for _ in 0..3 {
            let (mut days, mut version) = (Vec::new(), None::<Option<String>>);
            for _ in 0..MAX_PAGES {
                let request = ReaderRequest::PassesStats { item_id: item_id.into(), pass_id: pass_id.into(), offset: days.len(), limit: STATS_PAGE, expected_timing_version: version.clone().flatten() };
                let page: PassStats = match self.reader(request) {
                    Ok(v) => serde_json::from_value(v).map_err(|_| ClientError::InvalidFrame)?,
                    Err(ClientError::Daemon(code)) if code == "stale_reader_pass_timing" && version.is_some() => continue 'read,
                    Err(e) => return Err(e),
                };
                if page.item_id != item_id || page.pass_id != pass_id { return Err(ClientError::InvalidFrame); }
                if version.as_ref().is_some_and(|v| *v != page.timing_version) { continue 'read; }
                version = Some(page.timing_version.clone());
                let got = page.days.len();
                days.extend(page.days.iter().cloned());
                match page.next_offset {
                    Some(next) if next == days.len() && got > 0 => continue,
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => return Ok(PassStats { days, next_offset: None, ..page }),
                }
            }
            return Err(ClientError::InvalidFrame);
        }
        Err(ClientError::Daemon("stale_reader_pass_timing".into()))
    }

    /// One edit against `expected_version`. Never retried here.
    pub fn passes_edit(&self, item_id: &str, expected_version: &str, change: &PassChange) -> PassWrite<PassReceipt> {
        if let Err(e) = self.require_passes() { return PassWrite::Rejected(e.message()); }
        classify(self.reader(ReaderRequest::PassesEdit { item_id: item_id.into(), expected_version: expected_version.into(), change: change.clone() }))
    }

    pub fn passes_timing_start(&self, item_id: &str, pass_id: &str) -> PassWrite<TimingSession> {
        if let Err(e) = self.require_passes() { return PassWrite::Rejected(e.message()); }
        classify(self.reader(ReaderRequest::PassesTimingStart { item_id: item_id.into(), pass_id: pass_id.into() }))
    }

    pub fn passes_timing_report(&self, item_id: &str, pass_id: &str, session_id: &str, segments: &[TimingSegment]) -> PassWrite<TimingReport> {
        if let Err(e) = self.require_passes() { return PassWrite::Rejected(e.message()); }
        let request = ReaderRequest::PassesTimingReport { item_id: item_id.into(), pass_id: pass_id.into(), session_id: session_id.into(), segments: segments.to_vec() };
        classify(self.reader(request))
    }

    /// One user intent: a stale version reads again and replays the same change
    /// once against the new version; an unknown reply reads back and is saved
    /// only when the change is visible, otherwise kept as the attempt.
    pub fn passes_write(&self, item_id: &str, expected_version: &str, change: &PassChange) -> WriteResult {
        let mut expected = expected_version.to_string();
        for replay in 0..2 {
            match self.passes_edit(item_id, &expected, change) {
                PassWrite::Done(receipt) => return WriteResult::Saved { receipt: Some(receipt), list: self.passes_list(item_id).ok() },
                PassWrite::Stale if replay == 0 => match self.passes_list(item_id) {
                    Ok(list) => expected = list.version,
                    Err(_) => return WriteResult::Refused { code: "stale_reader_passes".into(), list: None },
                },
                PassWrite::Stale => return WriteResult::Refused { code: "stale_reader_passes".into(), list: self.passes_list(item_id).ok() },
                PassWrite::Rejected(code) => return WriteResult::Refused { code, list: self.passes_list(item_id).ok() },
                PassWrite::Unknown(message) => return self.passes_reconcile(PassAttempt { item_id: item_id.into(), expected_version: expected, change: change.clone() }, message),
            }
        }
        unreachable!()
    }

    /// Reads back after an unknown outcome; nothing is sent again.
    pub fn passes_reconcile(&self, attempt: PassAttempt, message: String) -> WriteResult {
        match self.passes_list(&attempt.item_id) {
            Ok(list) if attempt.change.visible_in(&list) => WriteResult::Saved { receipt: None, list: Some(list) },
            Ok(list) => WriteResult::Unknown { attempt, message, list: Some(list) },
            Err(_) => WriteResult::Unknown { attempt, message, list: None },
        }
    }

    /// The person asked to send the kept attempt again: exactly the same request.
    pub fn passes_resend(&self, attempt: &PassAttempt) -> WriteResult {
        match self.passes_edit(&attempt.item_id, &attempt.expected_version, &attempt.change) {
            PassWrite::Done(receipt) => WriteResult::Saved { receipt: Some(receipt), list: self.passes_list(&attempt.item_id).ok() },
            PassWrite::Stale => self.passes_reconcile(attempt.clone(), refusal_message("stale_reader_passes")),
            PassWrite::Rejected(code) => match self.passes_list(&attempt.item_id) {
                Ok(list) if attempt.change.visible_in(&list) => WriteResult::Saved { receipt: None, list: Some(list) },
                list => WriteResult::Refused { code, list: list.ok() },
            },
            PassWrite::Unknown(message) => self.passes_reconcile(attempt.clone(), message),
        }
    }
}

// ----- the three rates -----------------------------------------------------

/// One row of 「同一时长的三种速率」.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateRow {
    pub label: &'static str,
    pub length: String,
    pub rate: String,
}

fn count_label(side: Option<&LengthSide>) -> String {
    let Some(s) = side else { return "—".into() };
    let mut parts = vec![];
    if let Some(w) = s.words.filter(|w| *w > 0) { parts.push(txf("{0} 词", &[&grouped(w)])); }
    if let Some(c) = s.characters.filter(|c| *c > 0) { parts.push(txf("{0} 字", &[&grouped(c)])); }
    if parts.is_empty() { "—".into() } else { parts.join(" ＋ ") }
}

/// A rate for display: one decimal below 10, whole numbers above, never a bare 0.0.
pub fn rate_number(x: f64) -> String {
    if x < 0.05 { "<0.1".into() } else if x < 10. { format!("{x:.1}") } else { grouped(x.round() as u64) }
}

fn rate_label(rate: Option<&UnitRate>) -> String {
    let Some(r) = rate else { return "—".into() };
    let mut parts = vec![];
    if let Some(w) = r.words_per_minute.filter(|w| w.is_finite() && *w > 0.) { parts.push(txf("{0} 词/分", &[&rate_number(w)])); }
    if let Some(c) = r.characters_per_minute.filter(|c| c.is_finite() && *c > 0.) { parts.push(txf("{0} 字/分", &[&rate_number(c)])); }
    if parts.is_empty() { "—".into() } else { parts.join(" ＋ ") }
}

/// 按原文 / 按译文 / 按对照, from the server's rates; a missing length or time is 「—」.
pub fn rate_rows(pass: &Pass) -> [RateRow; 3] {
    let r = pass.rates.as_ref();
    let both = pass.lengths.source.is_some() && pass.lengths.translation.is_some();
    [
        RateRow { label: tx("按原文"), length: count_label(pass.lengths.source.as_ref()), rate: rate_label(r.and_then(|r| r.source.as_ref())) },
        RateRow { label: tx("按译文"), length: count_label(pass.lengths.translation.as_ref()), rate: rate_label(r.and_then(|r| r.translation.as_ref())) },
        RateRow { label: tx("按对照"), length: if both { tx("原文＋译文").into() } else { "—".into() }, rate: rate_label(r.and_then(|r| r.combined.as_ref())) },
    ]
}

/// The label rates carry: estimated (补记), and/or equivalent full-text.
pub fn rate_caption(pass: &Pass) -> Option<String> {
    let r = pass.rates.as_ref()?;
    if r.basis == "retrospective_estimate" {
        let mut s = tx("约 · 按补记参考版本的全文长度估算").to_string();
        if r.equivalent_full_text { s.push_str(" · "); s.push_str(tx("等效全文速率")); }
        return Some(s);
    }
    (r.equivalent_full_text || r.estimate).then(|| tx("等效全文速率，不是实际读速").into())
}

// ----- local calendar ------------------------------------------------------

/// Today's (year, month, day) in the user's local time.
pub fn local_today() -> (i32, u32, u32) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (y, m, d) = crate::biblio::local_civil(now);
    (y as i32, m as u32, d as u32)
}

/// The user's configured IANA zone (`TZ`, else the `/etc/localtime` link); `UTC` when neither names one.
pub fn local_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') { return tz.to_string(); }
    }
    std::fs::read_link("/etc/localtime").ok()
        .and_then(|p| p.to_str().and_then(|s| s.split("zoneinfo/").nth(1)).map(str::to_string))
        .unwrap_or_else(|| "UTC".into())
}

#[cfg(test)]
#[path = "passes_tests.rs"]
mod tests;
