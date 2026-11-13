//! Synthetic `reader_passes_v1` with the frozen semantics, simplified where the
//! review screens do not need more: per-item CAS with a monotonic version, exact
//! replay of any earlier edit, tombstones for deleted ids, server timing
//! sessions with monotonic offsets, segment id dedup/conflict, a first-accepted
//! union across reports (modes never double-count), and days split at the
//! system's local midnight (the pass timezone is stored, not resolved).

use crate::book::ReaderRequest;
use crate::passes::{Method, PassChange, PassDate, TimingMode, TimingSegment, MAX_ESTIMATE_MINUTES, MAX_REPORT_SEGMENTS, MAX_SEGMENT_MS};
use crate::Response;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

const WINDOW_MS: u64 = 300_000;
const REPORT_GRACE_MS: u64 = 120_000;
const PASS_LIMIT: usize = 4096;
const SEGMENT_LIMIT: u64 = 65_536;
const POLICY: &str = "synthetic-length-v1";

fn sha(s: &[u8]) -> String {
    format!("{:x}", Sha256::digest(s))
}
fn err(code: &str) -> Response {
    Response { ok: false, reason_code: Some(code.into()), ..Default::default() }
}
fn ok(v: Value) -> Response {
    Response { ok: true, reader: Some(v), ..Default::default() }
}
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// What a pass needs of an item: its revision, source text and translation (if any).
pub(crate) struct ItemText {
    pub rev: String,
    pub source: String,
    pub translation: Option<String>,
}

#[derive(Clone, Copy)]
struct Count {
    words: u64,
    characters: u64,
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// Latin/digit word runs and Han characters, visible text only (Markdown marks and link targets skipped).
fn count(markdown: &str) -> Count {
    let link = regex::Regex::new(r"\]\([^)]*\)").unwrap();
    let text = link.replace_all(markdown, "]");
    let (mut words, mut characters, mut in_word) = (0, 0, false);
    for c in text.chars() {
        let w = c.is_ascii_alphanumeric() || (in_word && c == '\'');
        if w && !in_word { words += 1; }
        in_word = w;
        if is_han(c) { characters += 1; }
    }
    Count { words, characters }
}

#[derive(Clone)]
struct Side {
    side: &'static str,
    sha256: String,
    count: Count,
}

impl Side {
    fn json(&self) -> Value {
        json!({"side": self.side, "sha256": self.sha256, "words": self.count.words, "characters": self.count.characters})
    }
}

#[derive(Clone)]
struct Rec {
    pass_id: String,
    ordinal: u64,
    tracked: bool,
    completed: bool,
    method: Option<Method>,
    start: Option<PassDate>,
    end: Option<PassDate>,
    estimate: Option<u32>,
    unit: Option<crate::passes::DurationUnit>,
    timezone: String,
    revision_id: Option<String>,
    canonical: Option<String>,
    source: Option<Side>,
    translation: Option<Side>,
    created_at_ms: u64,
    completed_at_ms: Option<u64>,
    /// Accepted intervals in acceptance order (UTC ms, mode).
    accepted: Vec<(u64, u64, TimingMode)>,
    segments: HashMap<(String, String), (TimingMode, u64, u64)>,
    timing_counter: u64,
}

impl Rec {
    fn timing_version(&self) -> Option<String> {
        self.tracked.then(|| sha(format!("{}:{}", self.pass_id, self.timing_counter).as_bytes()))
    }

    /// The parts of every instant first accepted, with their mode.
    fn owned(&self) -> Vec<(u64, u64, TimingMode)> {
        let mut covered: Vec<(u64, u64)> = vec![];
        let mut out = vec![];
        for &(s, e, ref mode) in &self.accepted {
            let mut at = s;
            let mut pieces = vec![];
            for &(cs, ce) in &covered {
                if ce <= at || cs >= e { continue; }
                if cs > at { pieces.push((at, cs.min(e))); }
                at = at.max(ce);
                if at >= e { break; }
            }
            if at < e { pieces.push((at, e)); }
            for (ps, pe) in pieces { if ps < pe { out.push((ps, pe, mode.clone())); } }
            covered.push((s, e));
            covered.sort();
            let mut merged: Vec<(u64, u64)> = vec![];
            for (cs, ce) in covered.drain(..) {
                match merged.last_mut() { Some(l) if cs <= l.1 => l.1 = l.1.max(ce), _ => merged.push((cs, ce)) }
            }
            covered = merged;
        }
        out
    }

    fn timing(&self) -> Value {
        let owned = self.owned();
        let by = |m: TimingMode| owned.iter().filter(|o| o.2 == m).map(|o| o.1 - o.0).sum::<u64>();
        let accepted: u64 = self.accepted.iter().map(|a| a.1 - a.0).sum();
        let unique: u64 = owned.iter().map(|o| o.1 - o.0).sum();
        json!({"timing_version": self.timing_version(), "accepted_ms": accepted, "unique_ms": unique, "overlap_excluded_ms": accepted - unique,
            "source_ms": by(TimingMode::Source), "bilingual_ms": by(TimingMode::Bilingual), "translation_ms": by(TimingMode::Translation),
            "segment_count": self.segments.len(), "segment_limit": SEGMENT_LIMIT, "recording_paused": false})
    }

    fn rates(&self) -> Value {
        let unique: u64 = self.owned().iter().map(|o| o.1 - o.0).sum();
        let (basis, minutes) = if self.tracked { ("measured", unique as f64 / 60_000.) } else { ("retrospective_estimate", self.estimate.unwrap_or(0) as f64) };
        if minutes <= 0. || (self.source.is_none() && self.translation.is_none()) { return Value::Null; }
        let per = |n: u64| (n > 0).then(|| n as f64 / minutes);
        let unit = |w: u64, c: u64| if w == 0 && c == 0 { Value::Null } else { json!({"words_per_minute": per(w), "characters_per_minute": per(c)}) };
        let side = |s: &Option<Side>| s.as_ref().map_or(Value::Null, |s| unit(s.count.words, s.count.characters));
        let combined = match (&self.source, &self.translation) {
            (Some(a), Some(b)) => unit(a.count.words + b.count.words, a.count.characters + b.count.characters),
            _ => Value::Null,
        };
        let full_done = self.method == Some(Method::Full) && (self.completed || !self.tracked);
        json!({"basis": basis, "minutes": minutes, "estimate": !self.tracked || !full_done, "equivalent_full_text": !full_done,
            "source": side(&self.source), "translation": side(&self.translation), "combined": combined})
    }
}

#[derive(Default)]
struct ItemPasses {
    counter: u64,
    next_ordinal: u64,
    passes: Vec<Rec>,
    tombstones: HashSet<String>,
    /// (expected version, canonical change JSON) → result: an exact retry answers the same.
    receipts: HashMap<(String, String), Value>,
}

impl ItemPasses {
    fn version(&self) -> String {
        format!("passes-v{}", self.counter)
    }

    /// Known starts by value (shorter prefix first), then ordinal and id; unknown starts last.
    fn ranked(&self) -> Vec<&Rec> {
        let mut v: Vec<&Rec> = self.passes.iter().collect();
        v.sort_by(|a, b| {
            let key = |r: &Rec| r.start.as_ref().filter(|d| d.parts().is_some()).and_then(|d| d.value.clone());
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then(a.ordinal.cmp(&b.ordinal))
            .then(a.pass_id.cmp(&b.pass_id))
        });
        v
    }
}

struct Session {
    item_id: String,
    pass_id: String,
    origin_ms: u64,
    started: Instant,
}

#[derive(Default)]
pub(crate) struct PassesStore {
    items: HashMap<String, ItemPasses>,
    sessions: HashMap<String, Session>,
    next_session: u64,
}

fn date_ok(d: &Option<PassDate>) -> bool {
    d.as_ref().is_none_or(|d| d.parts().is_some())
}

fn pass_json(r: &Rec, rank: usize, current_rev: &str) -> Value {
    let source_status = r.revision_id.as_ref().map(|rev| if rev == current_rev { "current" } else { "changed" });
    json!({
        "pass_id": r.pass_id, "ordinal": r.ordinal, "display_rank": rank, "kind": if r.tracked { "tracked" } else { "retrospective" },
        "status": if r.completed { "completed" } else { "in_progress" }, "method": r.method, "start_date": r.start, "end_date": r.end,
        "estimated_duration_minutes": r.estimate, "duration_display_unit": r.unit, "timezone": r.timezone,
        "revision_basis": if r.revision_id.is_none() { Value::Null } else if r.tracked { json!("started_on_revision") } else { json!("reference_at_entry") },
        "revision_id": r.revision_id, "canonical_sha256": r.canonical, "source_status": source_status,
        "lengths": {"policy": r.source.as_ref().map(|_| POLICY), "source": r.source.as_ref().map(Side::json), "translation": r.translation.as_ref().map(Side::json)},
        "created_at_ms": r.created_at_ms, "completed_at_ms": r.completed_at_ms, "timing": r.timing(), "rates": r.rates(),
    })
}

fn sides(text: &ItemText) -> (Option<Side>, Option<Side>) {
    (
        Some(Side { side: "source", sha256: sha(text.source.as_bytes()), count: count(&text.source) }),
        text.translation.as_ref().map(|t| Side { side: "translation", sha256: sha(t.as_bytes()), count: count(t) }),
    )
}

/// Local midnight (UTC ms) after `at`.
fn next_midnight(at: u64) -> u64 {
    let secs = (at / 1000) as i64;
    let off = crate::biblio::local_offset(secs);
    let local = secs + off;
    ((local.div_euclid(86_400) + 1) * 86_400 - off) as u64 * 1000
}

fn local_date(at: u64) -> String {
    let (y, m, d) = crate::biblio::local_civil((at / 1000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

impl PassesStore {
    /// Every live timing session ends, as when the daemon restarts.
    pub(crate) fn expire_sessions(&mut self) {
        self.sessions.clear();
    }

    /// Another device edited the item's passes (the version moves; no pass changes).
    pub(crate) fn touch(&mut self, item_id: &str) {
        self.items.entry(item_id.into()).or_default().counter += 1;
    }

    /// Clearly synthetic history for review screens: a 补记 (2024年3月 → 4月, 约 2 小时),
    /// a completed 抽样阅读 pass on 2025-10-21 (22 minutes) and an unfinished pass
    /// started two days ago with time on three days in all three modes.
    pub(crate) fn seed(&mut self, item_id: &str, text: &ItemText) {
        let now = now_ms();
        let (source, translation) = sides(text);
        let base = |n: u64, tracked: bool| Rec {
            pass_id: format!("pass_fixture_seed_{n}"), ordinal: n, tracked, completed: true, method: None, start: None, end: None, estimate: None, unit: None,
            timezone: crate::passes::local_timezone(), revision_id: Some(text.rev.clone()), canonical: Some(sha(text.source.as_bytes())), source: source.clone(),
            translation: translation.clone(), created_at_ms: now, completed_at_ms: Some(now), accepted: vec![], segments: HashMap::new(), timing_counter: 0,
        };
        let day = 86_400_000;
        let mut retro = base(1, false);
        retro.method = Some(Method::Full);
        retro.start = PassDate::new(2024, Some(3), None);
        retro.end = PassDate::new(2024, Some(4), None);
        retro.estimate = Some(120);
        retro.unit = Some(crate::passes::DurationUnit::Hours);
        let mut sampled = base(2, true);
        sampled.method = Some(Method::Sampled);
        sampled.start = PassDate::day(2025, 10, 21);
        sampled.end = PassDate::day(2025, 10, 21);
        let oct = 1_761_040_800_000; // 2025-10-21 10:00 UTC
        sampled.accepted = vec![(oct, oct + 22 * 60_000, TimingMode::Source)];
        sampled.timing_counter = 1;
        let mut current = base(3, true);
        current.completed = false;
        current.completed_at_ms = None;
        let (y, m, d) = crate::biblio::local_civil(((now - 2 * day) / 1000) as i64);
        current.start = PassDate::day(y as i32, m as u32, d as u32);
        let at = |days_ago: u64, minute: u64| now - days_ago * day - 3 * 3_600_000 + minute * 60_000;
        let min = 60_000;
        current.accepted = vec![
            (at(2, 0), at(2, 0) + 12 * min, TimingMode::Source),
            (at(2, 12), at(2, 12) + 26 * min, TimingMode::Bilingual),
            (at(1, 0), at(1, 0) + 21 * min, TimingMode::Bilingual),
            (at(0, 0), at(0, 0) + 17 * min, TimingMode::Bilingual),
            (at(0, 17), at(0, 17) + 26 * min, TimingMode::Translation),
            // The phone's report of the same minutes: counted once.
            (at(0, 20), at(0, 20) + 6 * min, TimingMode::Source),
        ];
        for (k, a) in current.accepted.iter().enumerate() { current.segments.insert(("session_fixture_seed".into(), format!("seg_{k}")), (a.2.clone(), a.0, a.1)); }
        current.timing_counter = 6;
        let item = self.items.entry(item_id.into()).or_default();
        item.passes = vec![retro, sampled, current];
        item.next_ordinal = 3;
        item.counter += 1;
    }

    #[inline(never)]
    pub(crate) fn handle(&mut self, request: &ReaderRequest, item: &dyn Fn(&str) -> Option<ItemText>) -> Option<Response> {
        Some(match request {
            ReaderRequest::PassesList { item_id, offset, limit, expected_version } => {
                let Some(text) = item(item_id) else { return Some(err("library_resource_not_found")) };
                if !(1..=100).contains(limit) || (expected_version.is_none() && *offset > 0) { return Some(err("invalid_request")); }
                let entry = self.items.entry(item_id.clone()).or_default();
                let version = entry.version();
                if expected_version.as_ref().is_some_and(|v| *v != version) { return Some(err("stale_reader_passes")); }
                let ranked = entry.ranked();
                let page: Vec<_> = ranked.iter().enumerate().skip(*offset).take(*limit).map(|(k, r)| pass_json(r, k + 1, &text.rev)).collect();
                let next = (offset + page.len() < ranked.len()).then_some(offset + page.len());
                ok(json!({"item_id": item_id, "version": version, "current_revision_id": text.rev, "passes": page, "total": ranked.len(), "next_offset": next, "pass_limit": PASS_LIMIT}))
            }
            ReaderRequest::PassesStats { item_id, pass_id, offset, limit, expected_timing_version } => {
                if item(item_id).is_none() { return Some(err("library_resource_not_found")); }
                if !(1..=400).contains(limit) || (expected_timing_version.is_none() && *offset > 0) { return Some(err("invalid_request")); }
                let Some(rec) = self.items.get(item_id).and_then(|i| i.passes.iter().find(|p| p.pass_id == *pass_id)) else { return Some(err("reader_pass_not_found")) };
                if *offset > 0 && *expected_timing_version != rec.timing_version() { return Some(err("stale_reader_pass_timing")); }
                let mut days: Vec<(String, [u64; 3])> = vec![];
                for (s, e, mode) in rec.owned() {
                    let mut at = s;
                    while at < e {
                        let cut = next_midnight(at).min(e);
                        let date = local_date(at);
                        let k = match mode { TimingMode::Source => 0, TimingMode::Bilingual => 1, TimingMode::Translation => 2 };
                        match days.iter_mut().find(|d| d.0 == date) { Some(d) => d.1[k] += cut - at, None => { let mut v = [0; 3]; v[k] = cut - at; days.push((date, v)); } }
                        at = cut;
                    }
                }
                days.sort();
                let page: Vec<_> = days.iter().skip(*offset).take(*limit).map(|(date, v)| json!({"date": date, "ms": v[0] + v[1] + v[2], "source_ms": v[0], "bilingual_ms": v[1], "translation_ms": v[2]})).collect();
                let next = (offset + page.len() < days.len()).then_some(offset + page.len());
                ok(json!({"item_id": item_id, "pass_id": pass_id, "timezone": rec.timezone, "timing_version": rec.timing_version(), "days": page, "total": days.len(), "next_offset": next}))
            }
            ReaderRequest::PassesEdit { item_id, expected_version, change } => {
                let Some(text) = item(item_id) else { return Some(err("library_resource_not_found")) };
                let entry = self.items.entry(item_id.clone()).or_default();
                let key = (expected_version.clone(), serde_json::to_string(change).unwrap());
                if let Some(result) = entry.receipts.get(&key) { return Some(ok(result.clone())); }
                if *expected_version != entry.version() { return Some(err("stale_reader_passes")); }
                let pass_id = change.pass_id().to_string();
                if !pass_id.starts_with("pass_") || pass_id.len() > 128 { return Some(err("invalid_reader_pass")); }
                let now = now_ms();
                let find = |entry: &mut ItemPasses| entry.passes.iter().position(|p| p.pass_id == pass_id);
                let outcome = match change {
                    PassChange::CreateTracked { revision_id, method, start_date, timezone, .. } => {
                        if find(entry).is_some() || entry.tombstones.contains(&pass_id) { return Some(err("reader_pass_conflict")); }
                        if entry.passes.len() >= PASS_LIMIT { return Some(err("reader_pass_limit_exceeded")); }
                        if *revision_id != text.rev { return Some(err("stale_item_revision")); }
                        if !date_ok(start_date) { return Some(err("invalid_reader_pass_dates")); }
                        if timezone.is_empty() { return Some(err("invalid_reader_pass")); }
                        let (source, translation) = sides(&text);
                        entry.next_ordinal += 1;
                        entry.passes.push(Rec { pass_id: pass_id.clone(), ordinal: entry.next_ordinal, tracked: true, completed: false, method: *method, start: start_date.clone(), end: None,
                            estimate: None, unit: None, timezone: timezone.clone(), revision_id: Some(text.rev.clone()), canonical: Some(sha(text.source.as_bytes())), source, translation,
                            created_at_ms: now, completed_at_ms: None, accepted: vec![], segments: HashMap::new(), timing_counter: 0 });
                        "created"
                    }
                    PassChange::CreateRetrospective { method, start_date, end_date, estimated_duration_minutes, duration_display_unit, timezone, reference_revision_id, .. } => {
                        if find(entry).is_some() || entry.tombstones.contains(&pass_id) { return Some(err("reader_pass_conflict")); }
                        if entry.passes.len() >= PASS_LIMIT { return Some(err("reader_pass_limit_exceeded")); }
                        if !date_ok(start_date) || !date_ok(end_date) || !crate::passes::dates_possible(start_date.as_ref(), end_date.as_ref()) { return Some(err("invalid_reader_pass_dates")); }
                        if estimated_duration_minutes.is_some_and(|m| m == 0 || m > MAX_ESTIMATE_MINUTES) || timezone.is_empty() { return Some(err("invalid_reader_pass")); }
                        if reference_revision_id.as_ref().is_some_and(|r| *r != text.rev) { return Some(err("stale_item_revision")); }
                        let (source, translation) = if reference_revision_id.is_some() { sides(&text) } else { (None, None) };
                        entry.next_ordinal += 1;
                        entry.passes.push(Rec { pass_id: pass_id.clone(), ordinal: entry.next_ordinal, tracked: false, completed: true, method: *method, start: start_date.clone(), end: end_date.clone(),
                            estimate: *estimated_duration_minutes, unit: *duration_display_unit, timezone: timezone.clone(), revision_id: reference_revision_id.clone(),
                            canonical: reference_revision_id.as_ref().map(|_| sha(text.source.as_bytes())), source, translation, created_at_ms: now, completed_at_ms: Some(now),
                            accepted: vec![], segments: HashMap::new(), timing_counter: 0 });
                        "created"
                    }
                    PassChange::Update { method, start_date, end_date, estimated_duration_minutes, duration_display_unit, .. } => {
                        let Some(at) = find(entry) else { return Some(err("reader_pass_not_found")) };
                        let r = &mut entry.passes[at];
                        if !date_ok(start_date) || !date_ok(end_date) || !crate::passes::dates_possible(start_date.as_ref(), end_date.as_ref()) { return Some(err("invalid_reader_pass_dates")); }
                        if r.tracked && !r.completed && end_date.is_some() { return Some(err("invalid_reader_pass_dates")); }
                        if r.tracked && r.completed && end_date.is_none() { return Some(err("invalid_reader_pass_dates")); }
                        if (r.tracked && estimated_duration_minutes.is_some()) || estimated_duration_minutes.is_some_and(|m| m == 0 || m > MAX_ESTIMATE_MINUTES) { return Some(err("invalid_reader_pass")); }
                        (r.method, r.start, r.end, r.estimate, r.unit) = (*method, start_date.clone(), end_date.clone(), *estimated_duration_minutes, *duration_display_unit);
                        "updated"
                    }
                    PassChange::Complete { end_date, expected_timing_version, .. } => {
                        let Some(at) = find(entry) else { return Some(err("reader_pass_not_found")) };
                        let r = &mut entry.passes[at];
                        if !r.tracked { return Some(err("invalid_reader_pass")); }
                        if r.completed { return Some(err("reader_pass_closed")); }
                        if end_date.is_none() || !date_ok(end_date) || !crate::passes::dates_possible(r.start.as_ref(), end_date.as_ref()) { return Some(err("invalid_reader_pass_dates")); }
                        if *expected_timing_version != r.timing_version() { return Some(err("stale_reader_pass_timing")); }
                        (r.completed, r.end, r.completed_at_ms) = (true, end_date.clone(), Some(now));
                        "completed"
                    }
                    PassChange::Reopen { .. } => {
                        let Some(at) = find(entry) else { return Some(err("reader_pass_not_found")) };
                        let r = &mut entry.passes[at];
                        if !r.tracked || !r.completed { return Some(err("invalid_reader_pass")); }
                        (r.completed, r.end, r.completed_at_ms) = (false, None, None);
                        "reopened"
                    }
                    PassChange::Delete { expected_timing_version, .. } => {
                        let Some(at) = find(entry) else { return Some(err("reader_pass_not_found")) };
                        if *expected_timing_version != entry.passes[at].timing_version() { return Some(err("stale_reader_pass_timing")); }
                        entry.passes.remove(at);
                        entry.tombstones.insert(pass_id.clone());
                        "deleted"
                    }
                };
                entry.counter += 1;
                let ordinal = entry.passes.iter().find(|p| p.pass_id == pass_id).map_or(0, |p| p.ordinal);
                let result = json!({"item_id": item_id, "version": entry.version(), "pass_id": pass_id, "ordinal": ordinal, "outcome": outcome});
                entry.receipts.insert(key, result.clone());
                ok(result)
            }
            ReaderRequest::PassesTimingStart { item_id, pass_id } => {
                let Some(text) = item(item_id) else { return Some(err("library_resource_not_found")) };
                let Some(rec) = self.items.get(item_id).and_then(|i| i.passes.iter().find(|p| p.pass_id == *pass_id)) else { return Some(err("reader_pass_not_found")) };
                if !rec.tracked { return Some(err("reader_pass_not_timeable")); }
                if rec.completed { return Some(err("reader_pass_closed")); }
                if rec.revision_id.as_deref() != Some(text.rev.as_str()) { return Some(err("stale_item_revision")); }
                self.next_session += 1;
                let id = format!("session_fixture_{}", self.next_session);
                let origin = now_ms();
                self.sessions.insert(id.clone(), Session { item_id: item_id.clone(), pass_id: pass_id.clone(), origin_ms: origin, started: Instant::now() });
                ok(json!({"session_id": id, "item_id": item_id, "pass_id": pass_id, "revision_id": text.rev, "origin_utc_ms": origin, "window_ms": WINDOW_MS,
                    "max_segment_ms": MAX_SEGMENT_MS, "report_deadline_utc_ms": origin + WINDOW_MS + REPORT_GRACE_MS}))
            }
            ReaderRequest::PassesTimingReport { item_id, pass_id, session_id, segments } => {
                if item(item_id).is_none() { return Some(err("library_resource_not_found")); }
                if segments.is_empty() || segments.len() > MAX_REPORT_SEGMENTS { return Some(err("invalid_reader_pass_timing")); }
                let Some(rec) = self.items.get_mut(item_id).and_then(|i| i.passes.iter_mut().find(|p| p.pass_id == *pass_id)) else { return Some(err("reader_pass_not_found")) };
                let key = |s: &TimingSegment| (session_id.clone(), s.segment_id.clone());
                let payload = |s: &TimingSegment| (s.mode.clone(), s.start_offset_ms, s.end_offset_ms);
                // Previously accepted segments stay reconcilable after expiry or completion.
                let mut seen = HashSet::new();
                let mut fresh = vec![];
                for s in segments {
                    if !seen.insert(s.segment_id.clone()) { return Some(err("reader_pass_segment_conflict")); }
                    match rec.segments.get(&key(s)) {
                        Some(p) if *p == payload(s) => {}
                        Some(_) => return Some(err("reader_pass_segment_conflict")),
                        None => fresh.push(s),
                    }
                }
                if !fresh.is_empty() {
                    let Some(session) = self.sessions.get(session_id) else { return Some(err("reader_pass_session_expired")) };
                    if session.item_id != *item_id || session.pass_id != *pass_id { return Some(err("invalid_reader_pass_timing")); }
                    if now_ms() > session.origin_ms + WINDOW_MS + REPORT_GRACE_MS { return Some(err("reader_pass_session_expired")); }
                    if rec.completed { return Some(err("reader_pass_closed")); }
                    let elapsed = session.started.elapsed().as_millis() as u64 + 1_000;
                    for s in &fresh {
                        if s.segment_id.is_empty() || s.start_offset_ms >= s.end_offset_ms || s.end_offset_ms - s.start_offset_ms > MAX_SEGMENT_MS || s.end_offset_ms > WINDOW_MS || s.end_offset_ms > elapsed {
                            return Some(err("invalid_reader_pass_timing"));
                        }
                    }
                    if rec.segments.len() as u64 + fresh.len() as u64 > SEGMENT_LIMIT { return Some(err("reader_pass_limit_exceeded")); }
                    let origin = session.origin_ms;
                    for s in &fresh {
                        rec.segments.insert(key(s), payload(s));
                        rec.accepted.push((origin + s.start_offset_ms, origin + s.end_offset_ms, s.mode.clone()));
                    }
                    rec.timing_counter += 1;
                }
                let fresh_ids: HashSet<_> = fresh.iter().map(|s| s.segment_id.clone()).collect();
                let outcomes: Vec<_> = segments.iter().map(|s| json!({"segment_id": s.segment_id, "outcome": if fresh_ids.contains(&s.segment_id) { "accepted" } else { "duplicate" }})).collect();
                ok(json!({"item_id": item_id, "pass_id": pass_id, "session_id": session_id, "outcomes": outcomes, "totals": rec.timing()}))
            }
            _ => return None,
        })
    }
}
