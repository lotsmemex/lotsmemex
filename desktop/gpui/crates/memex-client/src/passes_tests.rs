use super::*;
use crate::fixture::{FixtureBackend, FixtureControls, LONGREAD_ITEM, SHOWCASE_ITEM};
use crate::{Backend, BackendMode, Command, Response, Sensitivity};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Records every command; `drop_next_edit` loses one `passes_edit` before it reaches the agent.
struct Rec(FixtureBackend, Arc<Mutex<Vec<serde_json::Value>>>, Arc<Mutex<bool>>);
impl Backend for Rec {
    fn send(&self, c: &Command) -> Result<Response, ClientError> {
        let v = serde_json::to_value(c).unwrap();
        self.1.lock().unwrap().push(v.clone());
        if v["request"]["op"] == "passes_edit" && std::mem::take(&mut *self.2.lock().unwrap()) { return Err(ClientError::Timeout); }
        self.0.send(c)
    }
    fn mode(&self) -> BackendMode {
        BackendMode::SyntheticFixture
    }
}

fn setup() -> (FixtureBackend, Client, Arc<Mutex<Vec<serde_json::Value>>>, Arc<Mutex<bool>>) {
    let fixture = FixtureBackend::new(32);
    let sent = Arc::new(Mutex::new(vec![]));
    let drop = Arc::new(Mutex::new(false));
    let client = Client::new(Rec(fixture.clone(), sent.clone(), drop.clone()));
    (fixture, client, sent, drop)
}

fn ops(sent: &Arc<Mutex<Vec<serde_json::Value>>>, op: &str) -> usize {
    sent.lock().unwrap().iter().filter(|c| c["request"]["op"] == op).count()
}

fn rev(client: &Client, item: &str) -> String {
    client.passes_list(item).unwrap().current_revision_id.unwrap()
}

fn tracked(client: &Client, item: &str, id: &str, start: Option<PassDate>) -> PassList {
    let list = client.passes_list(item).unwrap();
    let change = PassChange::CreateTracked { pass_id: id.into(), revision_id: rev(client, item), method: None, start_date: start, timezone: "Asia/Shanghai".into() };
    let WriteResult::Saved { list: Some(list), .. } = client.passes_write(item, &list.version, &change) else { panic!("not saved") };
    list
}

#[test]
fn passes_ops_have_the_frozen_wire_shapes() {
    use crate::book::ReaderRequest;
    let wire = |r: ReaderRequest| serde_json::to_value(Command::Reader { max_sensitivity: Sensitivity::Restricted, request: r }).unwrap();
    let list = wire(ReaderRequest::PassesList { item_id: "i".into(), offset: 0, limit: 100, expected_version: None });
    assert_eq!(list["command"], "reader");
    assert_eq!(list["request"], json!({"op": "passes_list", "item_id": "i", "offset": 0, "limit": 100, "expected_version": null}));
    assert_eq!(wire(ReaderRequest::PassesStats { item_id: "i".into(), pass_id: "pass_a".into(), offset: 400, limit: 400, expected_timing_version: Some("t".into()) })["request"],
        json!({"op": "passes_stats", "item_id": "i", "pass_id": "pass_a", "offset": 400, "limit": 400, "expected_timing_version": "t"}));
    assert_eq!(wire(ReaderRequest::PassesTimingStart { item_id: "i".into(), pass_id: "pass_a".into() })["request"], json!({"op": "passes_timing_start", "item_id": "i", "pass_id": "pass_a"}));
    let seg = TimingSegment { segment_id: "s1".into(), mode: TimingMode::Bilingual, start_offset_ms: 0, end_offset_ms: 1200 };
    assert_eq!(wire(ReaderRequest::PassesTimingReport { item_id: "i".into(), pass_id: "pass_a".into(), session_id: "x".into(), segments: vec![seg] })["request"],
        json!({"op": "passes_timing_report", "item_id": "i", "pass_id": "pass_a", "session_id": "x", "segments": [{"segment_id": "s1", "mode": "bilingual", "start_offset_ms": 0, "end_offset_ms": 1200}]}));
    let edit = |change: PassChange| wire(ReaderRequest::PassesEdit { item_id: "i".into(), expected_version: "v".into(), change })["request"].clone();
    let day = PassDate::day(2026, 11, 6);
    assert_eq!(edit(PassChange::CreateTracked { pass_id: "pass_a".into(), revision_id: "r".into(), method: None, start_date: day.clone(), timezone: "Asia/Shanghai".into() }),
        json!({"op": "passes_edit", "item_id": "i", "expected_version": "v", "change": {"op": "create_tracked", "pass_id": "pass_a", "revision_id": "r", "method": null,
            "start_date": {"value": "2026-11-06", "precision": "day"}, "timezone": "Asia/Shanghai"}}));
    assert_eq!(edit(PassChange::CreateRetrospective { pass_id: "pass_b".into(), method: Some(Method::Structural), start_date: PassDate::new(2024, None, None), end_date: None,
        estimated_duration_minutes: Some(90), duration_display_unit: Some(DurationUnit::Hours), timezone: "UTC".into(), reference_revision_id: None })["change"],
        json!({"op": "create_retrospective", "pass_id": "pass_b", "method": "structural", "start_date": {"value": "2024", "precision": "year"}, "end_date": null,
            "estimated_duration_minutes": 90, "duration_display_unit": "hours", "timezone": "UTC", "reference_revision_id": null}));
    assert_eq!(edit(PassChange::Update { pass_id: "pass_a".into(), method: Some(Method::Full), start_date: None, end_date: None, estimated_duration_minutes: None, duration_display_unit: None })["change"],
        json!({"op": "update", "pass_id": "pass_a", "method": "full", "start_date": null, "end_date": null, "estimated_duration_minutes": null, "duration_display_unit": null}));
    assert_eq!(edit(PassChange::Complete { pass_id: "pass_a".into(), end_date: day, expected_timing_version: None })["change"],
        json!({"op": "complete", "pass_id": "pass_a", "end_date": {"value": "2026-11-06", "precision": "day"}, "expected_timing_version": null}));
    assert_eq!(edit(PassChange::Reopen { pass_id: "pass_a".into() })["change"], json!({"op": "reopen", "pass_id": "pass_a"}));
    assert_eq!(edit(PassChange::Delete { pass_id: "pass_a".into(), expected_timing_version: Some("h".into()) })["change"], json!({"op": "delete", "pass_id": "pass_a", "expected_timing_version": "h"}));
    assert!(new_pass_id().starts_with("pass_"));
}

#[test]
fn nothing_is_sent_without_the_reader_passes_capability() {
    let (fixture, client, sent, _) = setup();
    fixture.set_controls(FixtureControls { no_reader_passes: true, ..Default::default() });
    assert!(!client.passes_supported());
    assert_eq!(client.passes_list(SHOWCASE_ITEM), Err(ClientError::Unsupported));
    assert!(matches!(client.passes_edit(SHOWCASE_ITEM, "v", &PassChange::Reopen { pass_id: "pass_a".into() }), PassWrite::Rejected(_)));
    assert!(matches!(client.passes_timing_start(SHOWCASE_ITEM, "pass_a"), PassWrite::Rejected(_)));
    assert!(sent.lock().unwrap().iter().all(|c| !c["request"]["op"].as_str().unwrap_or("").starts_with("passes_")), "no passes op may be sent");
    // With the feature, the same read goes out.
    let (_, client, sent, _) = setup();
    assert!(client.passes_supported());
    assert!(client.passes_list(SHOWCASE_ITEM).unwrap().passes.is_empty());
    assert_eq!(ops(&sent, "passes_list"), 1);
}

#[test]
fn dates_keep_their_precision_and_drop_only_the_current_year() {
    use crate::i18n::Lang::{En, Zh};
    let l = |lang, d: Option<PassDate>| date_label_in(lang, d.as_ref(), 2026);
    assert_eq!(l(Zh, PassDate::day(2026, 11, 6)), "11月6日");
    assert_eq!(l(Zh, PassDate::day(2025, 10, 21)), "2025年10月21日");
    assert_eq!(l(Zh, PassDate::new(2026, Some(3), None)), "3月");
    assert_eq!(l(Zh, PassDate::new(2024, Some(3), None)), "2024年3月");
    assert_eq!(l(Zh, PassDate::new(2026, None, None)), "今年");
    assert_eq!(l(Zh, PassDate::new(2024, None, None)), "2024年");
    assert_eq!(l(Zh, None), "未知");
    assert_eq!(l(En, PassDate::day(2026, 11, 6)), "Nov 6");
    assert_eq!(l(En, PassDate::day(2025, 10, 21)), "Oct 21, 2025");
    assert_eq!(l(En, PassDate::new(2026, None, None)), "This year");
    // An unparsed stored value is not guessed into a calendar position.
    assert_eq!(l(Zh, Some(PassDate { value: Some("2026-1".into()), precision: Precision::Month, raw: None })), "未知");
    assert!(PassDate::day(2025, 2, 29).is_none() && PassDate::day(2024, 2, 29).is_some() && PassDate::new(2024, Some(13), None).is_none() && PassDate::new(2024, None, Some(3)).is_none());
    // Only an end wholly before the start is impossible.
    assert!(dates_possible(PassDate::new(2024, None, None).as_ref(), PassDate::new(2024, Some(3), None).as_ref()));
    assert!(dates_possible(PassDate::day(2024, 4, 30).as_ref(), PassDate::new(2024, Some(4), None).as_ref()));
    assert!(!dates_possible(PassDate::day(2024, 5, 1).as_ref(), PassDate::new(2024, Some(4), None).as_ref()));
    assert!(dates_possible(None, PassDate::new(2020, None, None).as_ref()));
}

#[test]
fn hours_convert_to_exact_minutes_or_are_refused_never_rounded() {
    use DurationError::*;
    use DurationUnit::{Hours, Minutes};
    assert_eq!(parse_duration("1.5", Hours), Ok(90));
    assert_eq!(parse_duration(" 2 ", Hours), Ok(120));
    assert_eq!(parse_duration("0.25", Hours), Ok(15));
    assert_eq!(parse_duration("8760", Hours), Ok(525_600));
    assert_eq!(parse_duration("525600", Minutes), Ok(525_600));
    assert_eq!(parse_duration("45", Minutes), Ok(45));
    assert_eq!(parse_duration("0.01", Hours), Err(NotWholeMinutes));
    assert_eq!(parse_duration("1.1", Minutes), Err(NotWholeMinutes));
    assert_eq!(parse_duration("1.333", Hours), Err(NotWholeMinutes));
    assert_eq!(parse_duration("525601", Minutes), Err(TooLong));
    assert_eq!(parse_duration("8760.5", Hours), Err(TooLong));
    assert_eq!(parse_duration("0", Minutes), Err(Zero));
    assert_eq!(parse_duration("0.0", Hours), Err(Zero));
    assert_eq!(parse_duration("", Minutes), Err(Empty));
    for bad in ["-5", "1e3", "abc", "1/2", "NaN", "inf", ".", "1.2.3", "+3"] { assert_eq!(parse_duration(bad, Hours), Err(NotNumber), "{bad}"); }
    assert_eq!(minutes_label(106), "1 小时 46 分");
    assert_eq!(minutes_label(22), "22 分");
    assert_eq!(ms_label(10_000), "不到 1 分");
    assert_eq!(grouped(15841), "15,841");
}

#[test]
fn passes_rank_by_written_dates_keep_ordinals_and_replay_exactly() {
    let (_, client, _, _) = setup();
    let item = SHOWCASE_ITEM;
    let list = tracked(&client, item, "pass_t1", PassDate::day(2026, 11, 6));
    let version = list.version.clone();
    // A 补记 dated earlier ranks first; ordinals are creation identity and stay.
    let retro = PassChange::CreateRetrospective { pass_id: "pass_r1".into(), method: Some(Method::Full), start_date: PassDate::new(2024, Some(3), None), end_date: PassDate::new(2024, Some(4), None),
        estimated_duration_minutes: Some(120), duration_display_unit: Some(DurationUnit::Hours), timezone: "Asia/Shanghai".into(), reference_revision_id: Some(rev(&client, item)) };
    let PassWrite::Done(receipt) = client.passes_edit(item, &version, &retro) else { panic!() };
    assert_eq!((receipt.outcome.as_str(), receipt.ordinal), ("created", 2));
    // The exact retry answers the same receipt, though the version moved on.
    assert_eq!(client.passes_edit(item, &version, &retro), PassWrite::Done(receipt.clone()));
    let list = client.passes_list(item).unwrap();
    let rank = |id: &str| list.get(id).map(|p| (p.display_rank, p.ordinal));
    assert_eq!((rank("pass_r1"), rank("pass_t1")), (Some((1, 2)), Some((2, 1))));
    assert_eq!(list.history().iter().map(|p| p.pass_id.as_str()).collect::<Vec<_>>(), ["pass_t1", "pass_r1"]);
    let r = list.get("pass_r1").unwrap();
    assert!(r.kind == PassKind::Retrospective && r.status == PassStatus::Completed && r.estimated_duration_minutes == Some(120));
    assert_eq!(list.open_tracked(&list.current_revision_id.clone().unwrap()).map(|p| p.pass_id.as_str()), Some("pass_t1"));
    // A retrospective record takes no timing session.
    assert_eq!(client.passes_timing_start(item, "pass_r1"), PassWrite::Rejected("reader_pass_not_timeable".into()));
    // An end wholly before the start is refused, with nothing written.
    let bad = PassChange::CreateRetrospective { pass_id: "pass_r2".into(), method: None, start_date: PassDate::day(2024, 5, 1), end_date: PassDate::new(2024, Some(4), None),
        estimated_duration_minutes: None, duration_display_unit: None, timezone: "UTC".into(), reference_revision_id: None };
    assert_eq!(client.passes_edit(item, &list.version, &bad), PassWrite::Rejected("invalid_reader_pass_dates".into()));
    // Deleted ids are tombstoned: the same id cannot come back.
    let del = PassChange::Delete { pass_id: "pass_r1".into(), expected_timing_version: None };
    assert!(matches!(client.passes_edit(item, &list.version, &del), PassWrite::Done(_)));
    let list = client.passes_list(item).unwrap();
    assert!(list.get("pass_r1").is_none());
    assert_eq!(list.get("pass_t1").unwrap().display_rank, 1);
    assert_eq!(client.passes_edit(item, &list.version, &PassChange::CreateRetrospective { pass_id: "pass_r1".into(), method: None, start_date: None, end_date: None,
        estimated_duration_minutes: None, duration_display_unit: None, timezone: "UTC".into(), reference_revision_id: None }), PassWrite::Rejected("reader_pass_conflict".into()));
}

#[test]
fn an_unknown_reply_is_read_back_and_resent_only_as_the_same_request() {
    let (fixture, client, sent, drop) = setup();
    let item = SHOWCASE_ITEM;
    let list = tracked(&client, item, "pass_t1", PassDate::day(2026, 11, 6));
    let end = PassDate::day(2026, 11, 8);
    let complete = PassChange::Complete { pass_id: "pass_t1".into(), end_date: end.clone(), expected_timing_version: list.get("pass_t1").unwrap().timing_version() };
    // internal_error after the commit is unknown, never a refusal; the read back finds it saved.
    fixture.set_controls(FixtureControls { fail_after_next_pass_commit: true, ..Default::default() });
    assert!(matches!(client.passes_edit(item, &list.version, &complete), PassWrite::Unknown(_)));
    let after = client.passes_list(item).unwrap();
    assert!(complete.visible_in(&after) && after.get("pass_t1").unwrap().status == PassStatus::Completed);
    // A lost reply of an applied write: passes_write reads back and reports it saved (no receipt).
    let reopen = PassChange::Reopen { pass_id: "pass_t1".into() };
    fixture.set_controls(FixtureControls { lose_next_pass_reply: true, ..Default::default() });
    let WriteResult::Saved { receipt: None, list: Some(l) } = client.passes_write(item, &after.version, &reopen) else { panic!("read back must show the reopen") };
    assert!(l.get("pass_t1").unwrap().in_progress());
    // A request lost before the agent: unknown, kept exactly; not resent by itself.
    *drop.lock().unwrap() = true;
    let before = ops(&sent, "passes_edit");
    let WriteResult::Unknown { attempt, list: Some(seen), .. } = client.passes_write(item, &l.version, &complete) else { panic!("must stay unknown") };
    assert_eq!(ops(&sent, "passes_edit"), before + 1);
    assert!(seen.get("pass_t1").unwrap().in_progress());
    assert_eq!(attempt, PassAttempt { item_id: item.into(), expected_version: l.version.clone(), change: complete.clone() });
    // The person resends: the very same version and change.
    let WriteResult::Saved { receipt: Some(r), .. } = client.passes_resend(&attempt) else { panic!() };
    assert_eq!(r.outcome, "completed");
    let last = sent.lock().unwrap().iter().rev().find(|c| c["request"]["op"] == "passes_edit").cloned().unwrap();
    assert_eq!(last["request"]["expected_version"], json!(l.version));
    assert_eq!(last["request"]["change"], serde_json::to_value(&complete).unwrap());
}

#[test]
fn a_stale_version_is_read_again_and_the_intent_replayed_once() {
    let (fixture, client, sent, _) = setup();
    let item = SHOWCASE_ITEM;
    let list = tracked(&client, item, "pass_t1", None);
    let update = PassChange::Update { pass_id: "pass_t1".into(), method: Some(Method::Sampled), start_date: PassDate::day(2026, 11, 1), end_date: None, estimated_duration_minutes: None, duration_display_unit: None };
    fixture.set_controls(FixtureControls { stale_next_pass_edit: true, ..Default::default() });
    let before = ops(&sent, "passes_edit");
    let WriteResult::Saved { receipt: Some(r), list: Some(l) } = client.passes_write(item, &list.version, &update) else { panic!() };
    assert_eq!(ops(&sent, "passes_edit"), before + 2, "one replay");
    assert_eq!(r.outcome, "updated");
    assert_ne!(l.version, list.version);
    assert_eq!(l.get("pass_t1").unwrap().method, Some(Method::Sampled));
    // A version that stays stale is reported after one replay, never looped.
    struct Stale(FixtureBackend, Arc<Mutex<usize>>);
    impl Backend for Stale {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            if let Command::Reader { request: crate::book::ReaderRequest::PassesEdit { item_id, .. }, .. } = c {
                *self.1.lock().unwrap() += 1;
                let _ = item_id;
                return Err(ClientError::Daemon("stale_reader_passes".into()));
            }
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode { BackendMode::SyntheticFixture }
    }
    let n = Arc::new(Mutex::new(0));
    let client = Client::new(Stale(fixture, n.clone()));
    assert!(matches!(client.passes_write(item, "old", &update), WriteResult::Refused { code, .. } if code == "stale_reader_passes"));
    assert_eq!(*n.lock().unwrap(), 2);
}

#[test]
fn timing_reports_dedupe_union_and_need_a_live_session() {
    let (fixture, client, _, _) = setup();
    let item = SHOWCASE_ITEM;
    tracked(&client, item, "pass_t1", None);
    let PassWrite::Done(s1) = client.passes_timing_start(item, "pass_t1") else { panic!() };
    assert_eq!((s1.window_ms, s1.max_segment_ms), (300_000, 300_000));
    let seg = |id: &str, mode, a, b| TimingSegment { segment_id: id.into(), mode, start_offset_ms: a, end_offset_ms: b };
    let PassWrite::Done(r) = client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("a", TimingMode::Source, 0, 800)]) else { panic!() };
    assert_eq!((r.outcomes[0].outcome.as_str(), r.totals.unique_ms, r.totals.source_ms), ("accepted", 800, 800));
    // Same id and payload: a harmless duplicate. Same id, other payload: a conflict.
    let PassWrite::Done(r) = client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("a", TimingMode::Source, 0, 800)]) else { panic!() };
    assert_eq!((r.outcomes[0].outcome.as_str(), r.totals.accepted_ms), ("duplicate", 800));
    assert_eq!(client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("a", TimingMode::Source, 0, 900)]), PassWrite::Rejected("reader_pass_segment_conflict".into()));
    // Oversized and future segments are refused whole, not clamped.
    assert_eq!(client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("b", TimingMode::Source, 0, 300_001)]), PassWrite::Rejected("invalid_reader_pass_timing".into()));
    assert_eq!(client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("c", TimingMode::Source, 0, 200_000)]), PassWrite::Rejected("invalid_reader_pass_timing".into()));
    // A second device's overlapping interval counts only its uncovered time.
    let PassWrite::Done(s2) = client.passes_timing_start(item, "pass_t1") else { panic!() };
    let PassWrite::Done(r) = client.passes_timing_report(item, "pass_t1", &s2.session_id, &[seg("a", TimingMode::Bilingual, 0, 900)]) else { panic!() };
    let gap = s2.origin_utc_ms - s1.origin_utc_ms;
    assert_eq!(r.totals.accepted_ms, 1700);
    assert!(gap < 800, "both sessions started within the test");
    // The first accepted interval owns the shared instants; only the uncovered tail is added.
    assert_eq!((r.totals.unique_ms, r.totals.source_ms, r.totals.bilingual_ms), (gap + 900, 800, gap + 100));
    assert_eq!(r.totals.source_ms + r.totals.bilingual_ms + r.totals.translation_ms, r.totals.unique_ms);
    assert_eq!(r.totals.overlap_excluded_ms, r.totals.accepted_ms - r.totals.unique_ms);
    // After a restart the session is gone: a new one must be started.
    fixture.set_controls(FixtureControls { expire_pass_sessions: true, ..Default::default() });
    let PassWrite::Rejected(code) = client.passes_timing_report(item, "pass_t1", &s2.session_id, &[seg("d", TimingMode::Source, 0, 10)]) else { panic!() };
    assert!(session_gone(&code));
    // An accepted receipt stays reconcilable without a live session.
    assert!(matches!(client.passes_timing_report(item, "pass_t1", &s1.session_id, &[seg("a", TimingMode::Source, 0, 800)]), PassWrite::Done(r) if r.outcomes[0].outcome == "duplicate"));
    // Completion fences timing: an old timing version is refused, the current one closes it.
    let list = client.passes_list(item).unwrap();
    let end = PassDate::day(2026, 11, 8);
    assert_eq!(client.passes_edit(item, &list.version, &PassChange::Complete { pass_id: "pass_t1".into(), end_date: end.clone(), expected_timing_version: None }), PassWrite::Rejected("stale_reader_pass_timing".into()));
    let tv = list.get("pass_t1").unwrap().timing_version();
    assert!(matches!(client.passes_edit(item, &list.version, &PassChange::Complete { pass_id: "pass_t1".into(), end_date: end, expected_timing_version: tv }), PassWrite::Done(_)));
    assert_eq!(client.passes_timing_start(item, "pass_t1"), PassWrite::Rejected("reader_pass_closed".into()));
}

#[test]
fn three_rates_from_one_time_with_dashes_where_a_length_or_time_is_missing() {
    let (fixture, client, _, _) = setup();
    fixture.seed_reading_passes(SHOWCASE_ITEM);
    fixture.seed_reading_passes(LONGREAD_ITEM);
    let list = client.passes_list(SHOWCASE_ITEM).unwrap();
    let current = list.passes.iter().find(|p| p.in_progress()).unwrap();
    let t = current.timing.as_ref().unwrap();
    assert_eq!((t.unique_ms, t.source_ms, t.bilingual_ms, t.translation_ms, t.overlap_excluded_ms), (102 * 60_000, 12 * 60_000, 64 * 60_000, 26 * 60_000, 6 * 60_000));
    let rows = rate_rows(current);
    let src = current.lengths.source.as_ref().unwrap();
    let words = src.words.unwrap();
    assert!(rows[0].rate.starts_with(&format!("{} 词/分", rate_number(words as f64 / 102.))), "{:?}", rows[0]);
    assert_eq!((rate_number(8.34), rate_number(412.4), rate_number(1234.6), rate_number(0.04)), ("8.3".into(), "412".into(), "1,235".into(), "<0.1".into()));
    let tr = current.lengths.translation.as_ref().unwrap();
    assert!(rows[1].rate.ends_with(&format!("{} 字/分", rate_number(tr.characters.unwrap() as f64 / 102.))), "{:?}", rows[1]);
    assert!(rows[1].length.ends_with(&format!("{} 字", grouped(tr.characters.unwrap()))), "{:?}", rows[1]);
    // 对照 keeps words and characters apart: two per-unit rates, never one sum.
    assert!(rows[2].rate.contains("词/分 ＋ ") && rows[2].rate.contains("字/分"), "{:?}", rows[2]);
    assert_eq!(rate_caption(current).as_deref(), Some("等效全文速率，不是实际读速"));
    let retro = list.passes.iter().find(|p| p.kind == PassKind::Retrospective).unwrap();
    assert_eq!(retro.rates.as_ref().unwrap().minutes, 120.);
    assert_eq!(rate_caption(retro).as_deref(), Some("约 · 按补记参考版本的全文长度估算"));
    // No translation: translation and combined rates are 「—」, the source rate remains.
    let list = client.passes_list(LONGREAD_ITEM).unwrap();
    let current = list.passes.iter().find(|p| p.in_progress()).unwrap();
    let rows = rate_rows(current);
    assert_ne!(rows[0].rate, "—");
    assert_eq!((rows[1].length.as_str(), rows[1].rate.as_str(), rows[2].length.as_str(), rows[2].rate.as_str()), ("—", "—", "—", "—"));
    // No time at all: every rate is 「—」, not infinity.
    let fresh = tracked(&client, LONGREAD_ITEM, "pass_new", None);
    let p = fresh.get("pass_new").unwrap();
    assert!(p.rates.is_none());
    assert!(rate_rows(p).iter().all(|r| r.rate == "—"));
    assert!(rate_caption(p).is_none());
    // Days: the seeded pass has three days, no day longer than its own minutes.
    let stats = client.passes_stats(SHOWCASE_ITEM, &client.passes_list(SHOWCASE_ITEM).unwrap().passes.iter().find(|p| p.in_progress()).unwrap().pass_id).unwrap();
    assert_eq!(stats.days.iter().map(|d| d.ms).sum::<u64>(), 102 * 60_000);
    assert!(stats.days.iter().all(|d| d.ms == d.source_ms + d.bilingual_ms + d.translation_ms));
}
