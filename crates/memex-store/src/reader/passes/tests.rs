use super::*;
use memex_core::{BlobDescriptor, VerifiedBlob};

const P: Sensitivity = Sensitivity::Public;
const NY: &str = "America/New_York";
const T0: i64 = 1_780_000_000_000; // 2026-05-28T20:26:40Z

fn doc(markdown: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut draft = crate::tests::draft();
    draft.content_markdown = markdown.into();
    let receipt = ingest(dir.path(), &crate::tests::capture(), draft).unwrap();
    (dir, receipt.item_ids[0].clone(), receipt.revision_ids[0].clone())
}

/// Paired original/translation book (same shape as the reader module's fixture).
fn book() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    draft.content_markdown = "甲🌹乙\n".into();
    let mut original = heading_edition("First chapter.\n");
    original.side = "original".into();
    let mut translated = heading_edition(&draft.content_markdown);
    translated.side = "translation".into();
    let manifest = BookManifest {
        schema_version: 1,
        alignment: "declared_chapter_order_not_paragraph_alignment".into(),
        editions: vec![original, translated],
    };
    capture.blobs = [
        ("original.md", "text/markdown", b"First chapter.\n".to_vec()),
        ("translation.md", "text/markdown", draft.content_markdown.as_bytes().to_vec()),
        ("manifest.json", BOOK_MANIFEST, serde_json::to_vec(&manifest).unwrap()),
    ]
    .into_iter()
    .map(|(name, media, bytes)| {
        let sha = reader_sha256(&bytes);
        VerifiedBlob {
            descriptor: BlobDescriptor { schema_version: 1, locator: name.into(), media_type: media.into(),
                byte_size: bytes.len() as u64, sha256: sha.clone(), source_url: None },
            blob_id: format!("sha256:{sha}"),
            source_backend_ref: None,
            bytes,
        }
    })
    .collect();
    capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
    let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
    (dir, receipt.item_ids[0].clone(), receipt.revision_ids[0].clone())
}

fn clock(boot: &str, utc: i64) -> TimingClock {
    TimingClock { boot_id: boot.into(), utc_ms: utc, mono_ms: utc - T0 }
}
fn day(v: &str) -> Option<PassDate> {
    let precision = match v.len() { 4 => Precision::Year, 7 => Precision::Month, _ => Precision::Day };
    Some(PassDate { value: v.into(), precision, raw: None })
}
fn version(lib: &Path, id: &str) -> String {
    list(lib, id, 0, 100, None, P).unwrap().version
}
fn ed(lib: &Path, id: &str, change: Change) -> Result<EditResult> {
    edit(lib, id, &version(lib, id), change, "owner", P, T0)
}
fn err<T: std::fmt::Debug>(r: Result<T>) -> String {
    r.unwrap_err().to_string()
}
fn tracked(pass: &str, rev: &str) -> Change {
    Change::CreateTracked { pass_id: pass.into(), revision_id: rev.into(), method: Some(Method::Full), start_date: None, timezone: NY.into() }
}
fn retro(pass: &str, start: Option<PassDate>, end: Option<PassDate>, minutes: Option<u32>) -> Change {
    Change::CreateRetrospective {
        pass_id: pass.into(), method: None, start_date: start, end_date: end, estimated_duration_minutes: minutes,
        duration_display_unit: None, timezone: NY.into(), reference_revision_id: None,
    }
}
fn update(pass: &str, start: Option<PassDate>, end: Option<PassDate>, minutes: Option<u32>, unit: Option<DisplayUnit>) -> Change {
    Change::Update { pass_id: pass.into(), method: None, start_date: start, end_date: end, estimated_duration_minutes: minutes, duration_display_unit: unit }
}
fn seg(id: &str, mode: Mode, start: i64, end: i64) -> SegmentReport {
    SegmentReport { segment_id: id.into(), mode, start_offset_ms: start, end_offset_ms: end }
}
fn pass(lib: &Path, id: &str, pass_id: &str) -> ListedPass {
    list(lib, id, 0, 100, None, P).unwrap().passes.into_iter().find(|p| p.pass_id == pass_id).unwrap()
}

#[test]
fn lifecycle_cas_exact_replay_tombstone_and_nonreused_ordinals() {
    let (dir, id, rev) = doc("# 一\nHello world\n");
    let lib = dir.path();
    let v0 = version(lib, &id);
    let a = edit(lib, &id, &v0, tracked("pass_a", &rev), "owner", P, T0).unwrap();
    assert_eq!((a.ordinal, a.outcome.as_str()), (1, "created"));
    // Exact retry returns the stored result; another principal's identical attempt is stale.
    assert_eq!(edit(lib, &id, &v0, tracked("pass_a", &rev), "owner", P, T0).unwrap(), a);
    assert_eq!(err(edit(lib, &id, &v0, tracked("pass_a", &rev), "phone", P, T0)), "stale_reader_passes");
    // Duplicate ID and stale revision binding reject.
    assert_eq!(err(ed(lib, &id, tracked("pass_a", &rev))), "reader_pass_conflict");
    assert_eq!(err(ed(lib, &id, tracked("pass_x", "revision_other"))), "stale_item_revision");
    let b = ed(lib, &id, tracked("pass_b", &rev)).unwrap();
    assert_eq!(b.ordinal, 2);
    // Tracked completion needs an end date and explicit action; reopen clears it, not time.
    let complete = |end| Change::Complete { pass_id: "pass_a".into(), end_date: end, expected_timing_version: None };
    assert_eq!(err(ed(lib, &id, complete(None))), "invalid_reader_pass");
    assert_eq!(ed(lib, &id, complete(day("2026-05-28"))).unwrap().outcome, "completed");
    assert_eq!(err(ed(lib, &id, complete(day("2026-05-28")))), "reader_pass_conflict");
    let listed = pass(lib, &id, "pass_a");
    assert_eq!((listed.status, listed.completed_at_ms), (Status::Completed, Some(T0)));
    assert_eq!(ed(lib, &id, Change::Reopen { pass_id: "pass_a".into() }).unwrap().outcome, "reopened");
    let listed = pass(lib, &id, "pass_a");
    assert_eq!((listed.status, listed.end_date.clone()), (Status::InProgress, None));
    // Delete leaves a tombstone: no recreation, no ordinal reuse, delayed packets refused.
    let before = version(lib, &id);
    let d = ed(lib, &id, Change::Delete { pass_id: "pass_b".into(), expected_timing_version: None }).unwrap();
    assert_eq!(d.outcome, "deleted");
    assert_eq!(err(edit(lib, &id, &before, Change::Reopen { pass_id: "pass_a".into() }, "owner", P, T0)), "stale_reader_passes");
    assert_eq!(err(ed(lib, &id, tracked("pass_b", &rev))), "reader_pass_conflict");
    assert_eq!(err(ed(lib, &id, Change::Reopen { pass_id: "pass_b".into() })), "reader_pass_not_found");
    assert_eq!(ed(lib, &id, tracked("pass_c", &rev)).unwrap().ordinal, 3);
    let c = clock("boot-life", T0);
    assert_eq!(err(timing_start(lib, &id, "pass_b", "owner", P, &c)), "reader_pass_not_found");
    assert_eq!(err(timing_report(lib, &id, "pass_b", "tsess_00", &[seg("s1", Mode::Source, 0, 1)], "owner", P, &c)), "reader_pass_closed");
    assert!(!lib.join(format!("catalog/reader-passes/{id}.timing/pass_b.json")).exists());
    let page = list(lib, &id, 0, 100, None, P).unwrap();
    assert_eq!(page.passes.iter().map(|p| p.ordinal).collect::<Vec<_>>(), vec![1, 3]);
    // Reader state and annotation ledgers are untouched.
    assert!(!lib.join("catalog/reader-state.json").exists());
    assert!(!lib.join("catalog/reader-annotations").exists());
}

#[test]
fn partial_dates_keep_precision_and_reject_only_impossible_order() {
    let (dir, id, _) = doc("正文\n");
    let lib = dir.path();
    // Overlapping/unknown ranges are accepted, not resolved.
    for (n, (start, end)) in [("2024-03", "2024-03-01"), ("2024", "2024-01"), ("2024-03-31", "2024-03"), ("2023-12-31", "2024")].iter().enumerate() {
        ed(lib, &id, retro(&format!("pass_ok{n}"), day(start), day(end), None)).unwrap();
    }
    ed(lib, &id, retro("pass_nulls", None, None, None)).unwrap();
    ed(lib, &id, retro("pass_end_only", None, day("2020"), None)).unwrap();
    // End's latest possible day before start's earliest possible day.
    for (start, end) in [("2024", "2023-12"), ("2024-03", "2024-02-29"), ("2024-03-02", "2024-03-01"), ("2025", "2024")] {
        assert_eq!(err(ed(lib, &id, retro("pass_bad", day(start), day(end), None))), "invalid_reader_pass_dates", "{start}..{end}");
    }
    // Invalid Gregorian values or precision mismatches reject without fabrication.
    for bad in [
        PassDate { value: "2023-02-29".into(), precision: Precision::Day, raw: None },
        PassDate { value: "2024-03".into(), precision: Precision::Day, raw: None },
        PassDate { value: "2024".into(), precision: Precision::Month, raw: None },
        PassDate { value: "0000".into(), precision: Precision::Year, raw: None },
        PassDate { value: "2024-13".into(), precision: Precision::Month, raw: None },
        PassDate { value: "2024".into(), precision: Precision::Year, raw: Some("x\u{7}".into()) },
    ] {
        assert_eq!(err(ed(lib, &id, retro("pass_bad", Some(bad), None, None))), "invalid_reader_pass");
    }
    assert!(serde_json::from_value::<PassDate>(serde_json::json!({"value": null, "precision": "unparsed", "raw": "x"})).is_err());
    assert!(serde_json::from_value::<PassDate>(serde_json::json!({"value": "2024", "precision": "year", "raw": null, "x": 1})).is_err());
    // Stored precision is returned verbatim.
    let p = pass(lib, &id, "pass_ok1");
    assert_eq!(p.start_date, day("2024"));
    assert_eq!(p.end_date.unwrap().precision, Precision::Month);
    // In-progress passes cannot carry an end date.
    let (dir2, id2, rev2) = doc("x\n");
    ed(dir2.path(), &id2, tracked("pass_t", &rev2)).unwrap();
    assert_eq!(err(ed(dir2.path(), &id2, update("pass_t", day("2026"), day("2026"), None, None))), "invalid_reader_pass");
    ed(dir2.path(), &id2, update("pass_t", day("2026-05"), None, None, None)).unwrap();
}

#[test]
fn retrospective_estimates_are_bounded_separate_and_never_timed() {
    let (dir, id, rev) = book();
    let lib = dir.path();
    for (n, minutes) in [0u32, 525_601].iter().enumerate() {
        assert_eq!(err(ed(lib, &id, retro(&format!("pass_x{n}"), None, None, Some(*minutes)))), "invalid_reader_pass");
    }
    ed(lib, &id, retro("pass_one", None, None, Some(1))).unwrap();
    ed(lib, &id, retro("pass_year", None, None, Some(525_600))).unwrap();
    // 1.5 hours entered by the owner is stored as exactly 90 whole minutes plus the unit preference.
    let mut with_ref = retro("pass_ref", day("2019-07"), None, Some(90));
    if let Change::CreateRetrospective { reference_revision_id, duration_display_unit, .. } = &mut with_ref {
        *reference_revision_id = Some(rev.clone());
        *duration_display_unit = Some(DisplayUnit::Hours);
    }
    ed(lib, &id, with_ref).unwrap();
    // A unit without an estimate, or any estimate on a tracked pass, rejects.
    assert_eq!(err(ed(lib, &id, update("pass_one", None, None, None, Some(DisplayUnit::Hours)))), "invalid_reader_pass");
    ed(lib, &id, tracked("pass_live", &rev)).unwrap();
    assert_eq!(err(ed(lib, &id, update("pass_live", None, None, Some(30), None))), "invalid_reader_pass");
    // Decimal or negative minutes never reach the Store as integers.
    assert!(serde_json::from_value::<Change>(serde_json::json!({"op":"update","pass_id":"pass_one","method":null,
        "start_date":null,"end_date":null,"estimated_duration_minutes":1.5,"duration_display_unit":null})).is_err());
    // Kind/timing isolation.
    let c = clock("boot-retro", T0);
    assert_eq!(err(timing_start(lib, &id, "pass_ref", "owner", P, &c)), "reader_pass_not_timeable");
    assert_eq!(err(timing_report(lib, &id, "pass_ref", "tsess_00", &[seg("s", Mode::Source, 0, 1)], "owner", P, &c)), "reader_pass_not_timeable");
    let r = pass(lib, &id, "pass_ref");
    assert_eq!((r.kind, r.status, r.revision_basis), (Kind::Retrospective, Status::Completed, Some(RevisionBasis::ReferenceAtEntry)));
    assert_eq!((r.estimated_duration_minutes, r.duration_display_unit), (Some(90), Some(DisplayUnit::Hours)));
    assert_eq!(r.timing.unique_ms, 0);
    assert_eq!(r.rates.basis, "retrospective_estimate");
    assert!(r.rates.estimate && r.rates.equivalent_full_text);
    assert_eq!(r.rates.minutes, Some(90.0));
    assert_eq!(r.rates.source.words_per_minute, Some(2.0 / 90.0));
    assert_eq!(r.rates.translation.characters_per_minute, Some(2.0 / 90.0));
    // No reference: lengths and rates unavailable even with an estimate.
    let one = pass(lib, &id, "pass_one");
    assert_eq!((one.lengths.clone(), one.revision_id.clone(), one.source_status.clone()), (None, None, None));
    assert_eq!(one.rates.source.words_per_minute, None);
    // Editing the estimate changes only estimated rates, never measured timing.
    ed(lib, &id, update("pass_ref", day("2019-07"), None, Some(45), None)).unwrap();
    let r = pass(lib, &id, "pass_ref");
    assert_eq!((r.rates.minutes, r.timing.accepted_ms, r.timing.segment_count), (Some(45.0), 0, 0));
    // A reference is only the then-current revision, never a guessed historical one.
    let mut old = retro("pass_old", None, None, None);
    if let Change::CreateRetrospective { reference_revision_id, .. } = &mut old {
        *reference_revision_id = Some("revision_older".into());
    }
    assert_eq!(err(ed(lib, &id, old)), "stale_item_revision");
}

#[test]
fn display_rank_sorts_known_starts_by_prefix_then_ordinal_with_unknown_last() {
    let (dir, id, _) = doc("x\n");
    let lib = dir.path();
    for (pass_id, start) in [("pass_u1", None), ("pass_y", day("2024")), ("pass_d", day("2024-03-05")),
        ("pass_m", day("2024-03")), ("pass_old", day("2023-12-31")), ("pass_m2", day("2024-03")), ("pass_u2", None),
        ("pass_apr", day("2024-04"))] {
        ed(lib, &id, retro(pass_id, start, None, None)).unwrap();
    }
    let order = |lib: &Path| list(lib, &id, 0, 100, None, P).unwrap().passes.iter()
        .map(|p| (p.display_rank, p.pass_id.clone(), p.ordinal)).collect::<Vec<_>>();
    let names = |v: &[(usize, String, u64)]| v.iter().map(|(_, n, _)| n.as_str().to_owned()).collect::<Vec<_>>();
    let got = order(lib);
    assert_eq!(names(&got), ["pass_old", "pass_y", "pass_m", "pass_m2", "pass_d", "pass_apr", "pass_u1", "pass_u2"]);
    assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), (1..=8).collect::<Vec<_>>());
    // Editing a date or inserting/deleting another pass reranks without identity changes.
    ed(lib, &id, update("pass_u1", day("2001"), None, None, None)).unwrap();
    ed(lib, &id, retro("pass_new", day("2024-03-01"), None, None)).unwrap();
    ed(lib, &id, Change::Delete { pass_id: "pass_y".into(), expected_timing_version: None }).unwrap();
    let got = order(lib);
    assert_eq!(names(&got), ["pass_u1", "pass_old", "pass_m", "pass_m2", "pass_new", "pass_d", "pass_apr", "pass_u2"]);
    assert_eq!(got[0], (1, "pass_u1".into(), 1));
    // Pages are cut in rank order and bound to one ledger version.
    let first = list(lib, &id, 0, 3, None, P).unwrap();
    assert_eq!((first.next_offset, first.total), (Some(3), 8));
    let second = list(lib, &id, 3, 3, Some(&first.version), P).unwrap();
    assert_eq!(second.passes[0].display_rank, 4);
    assert_eq!(err(list(lib, &id, 3, 3, None, P)), "invalid_reader_pass");
    ed(lib, &id, update("pass_u2", None, None, None, None)).unwrap();
    assert_eq!(err(list(lib, &id, 3, 3, Some(&first.version), P)), "stale_reader_passes");
}

#[test]
fn two_devices_union_by_first_acceptance_with_exact_replay_and_conflicts() {
    let (dir, id, rev) = doc("Hello world\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let start = clock("boot-union", T0);
    let mac = timing_start(lib, &id, "pass_a", "mac", P, &start).unwrap();
    let phone = timing_start(lib, &id, "pass_a", "phone", P, &start).unwrap();
    assert_eq!((mac.window_ms, mac.max_segment_ms, mac.origin_utc_ms), (300_000, 300_000, T0));
    let at = clock("boot-union", T0 + 290_000);
    // Phone's LATER wall-clock interval is accepted first, so it owns the overlap.
    let r = timing_report(lib, &id, "pass_a", &phone.session_id, &[seg("p2", Mode::Translation, 100_000, 200_000)], "phone", P, &at).unwrap();
    assert_eq!(r.outcomes[0].outcome, "accepted");
    let r = timing_report(lib, &id, "pass_a", &mac.session_id,
        &[seg("m1", Mode::Source, 0, 150_000), seg("m2", Mode::Bilingual, 150_000, 250_000)], "mac", P, &at).unwrap();
    let t = &r.totals;
    assert_eq!((t.accepted_ms, t.unique_ms, t.overlap_excluded_ms), (350_000, 250_000, 100_000));
    assert_eq!((t.source_ms, t.bilingual_ms, t.translation_ms), (100_000, 50_000, 100_000));
    assert_eq!(t.source_ms + t.bilingual_ms + t.translation_ms, t.unique_ms);
    // Out-of-order earlier segment from the phone adds only uncovered time.
    let r = timing_report(lib, &id, "pass_a", &phone.session_id, &[seg("p1", Mode::Source, 0, 120_000)], "phone", P, &at).unwrap();
    assert_eq!((r.totals.unique_ms, r.totals.source_ms), (250_000, 100_000));
    // Exact replay is a no-op; same ID with changed payload conflicts; IDs are session+principal scoped.
    let before = r.totals.timing_version.clone();
    let replay = timing_report(lib, &id, "pass_a", &mac.session_id, &[seg("m1", Mode::Source, 0, 150_000)], "mac", P, &at).unwrap();
    assert_eq!((replay.outcomes[0].outcome.as_str(), replay.totals.timing_version.as_str()), ("duplicate", before.as_str()));
    assert_eq!(err(timing_report(lib, &id, "pass_a", &mac.session_id, &[seg("m1", Mode::Translation, 0, 150_000)], "mac", P, &at)), "reader_pass_segment_conflict");
    assert_eq!(err(timing_report(lib, &id, "pass_a", &mac.session_id,
        &[seg("m9", Mode::Source, 260_000, 270_000), seg("m1", Mode::Source, 0, 1)], "mac", P, &at)), "reader_pass_segment_conflict");
    assert_eq!(pass(lib, &id, "pass_a").timing.timing_version, before, "a rejected report adds nothing");
    // Another principal cannot use the mac's session.
    assert_eq!(err(timing_report(lib, &id, "pass_a", &mac.session_id, &[seg("m1", Mode::Source, 0, 150_000)], "phone", P, &at)), "reader_pass_session_expired");
    // Duplicate IDs inside one report are malformed.
    assert_eq!(err(timing_report(lib, &id, "pass_a", &mac.session_id,
        &[seg("z", Mode::Source, 0, 1), seg("z", Mode::Source, 1, 2)], "mac", P, &at)), "invalid_reader_pass_timing");
    // Tracked measured rates use one total time and separate units.
    let p = pass(lib, &id, "pass_a");
    assert_eq!(p.rates.basis, "measured");
    assert_eq!(p.rates.source.words_per_minute, Some(2.0 / (250_000.0 / 60_000.0)));
    assert_eq!(p.rates.source.characters_per_minute, None);
    assert_eq!((p.rates.translation.words_per_minute, p.rates.combined.words_per_minute), (None, None));
    assert!(p.rates.estimate, "in-progress rates are estimates");
}

#[test]
fn session_and_segment_bounds_reject_instead_of_clamping() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let boot = "boot-bounds";
    let s = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0)).unwrap();
    let report = |segs: &[SegmentReport], at: &TimingClock| timing_report(lib, &id, "pass_a", &s.session_id, segs, "owner", P, at);
    let mid = clock(boot, T0 + 100_000);
    for bad in [seg("neg", Mode::Source, -1, 10), seg("rev", Mode::Source, 50, 40), seg("empty", Mode::Source, 50, 50),
        seg("future", Mode::Source, 0, 100_001)] {
        assert_eq!(err(report(&[bad], &mid)), "invalid_reader_pass_timing");
    }
    let end = clock(boot, T0 + 300_000 + 59_000);
    assert_eq!(err(report(&[seg("long", Mode::Source, 0, 300_001)], &end)), "invalid_reader_pass_timing");
    assert_eq!(err(report(&[seg("outside", Mode::Source, 299_000, 300_001)], &end)), "invalid_reader_pass_timing");
    assert_eq!(report(&[seg("full", Mode::Source, 0, 300_000)], &end).unwrap().totals.unique_ms, 300_000);
    let segs: Vec<_> = (0..65).map(|i| seg(&format!("x{i}"), Mode::Source, i, i + 1)).collect();
    assert_eq!(err(report(&segs, &end)), "invalid_reader_pass_timing");
    // After window + grace the session no longer admits new time, but replays reconcile.
    // The report deadline is origin + 360000 ms of server monotonic time, inclusive.
    let deadline = clock(boot, T0 + 360_000);
    assert_eq!(report(&[seg("edge", Mode::Source, 299_999, 300_000)], &deadline).unwrap().outcomes[0].outcome, "accepted");
    let late = clock(boot, T0 + 300_000 + 60_001);
    assert_eq!(err(report(&[seg("late", Mode::Source, 0, 1)], &late)), "reader_pass_session_expired");
    assert_eq!(report(&[seg("full", Mode::Source, 0, 300_000)], &late).unwrap().outcomes[0].outcome, "duplicate");
    // Daemon restart (new boot) invalidates live sessions, not accepted records.
    let s2 = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0 + 400_000)).unwrap();
    let restarted = TimingClock { boot_id: "boot-bounds-2".into(), utc_ms: T0 + 410_000, mono_ms: 5_000 };
    assert_eq!(err(timing_report(lib, &id, "pass_a", &s2.session_id, &[seg("r", Mode::Source, 0, 1000)], "owner", P, &restarted)), "reader_pass_session_expired");
    assert_eq!(report(&[seg("full", Mode::Source, 0, 300_000)], &restarted).unwrap().totals.unique_ms, 300_000);
    // Server wall clock stepped backward: admission suspends; the session is dropped.
    let s3 = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0 + 500_000)).unwrap();
    let back = TimingClock { boot_id: boot.into(), utc_ms: T0 + 500_000 + 60_000 - 10_000, mono_ms: 500_000 + 60_000 };
    let r3 = |segs: &[SegmentReport], at: &TimingClock| timing_report(lib, &id, "pass_a", &s3.session_id, segs, "owner", P, at);
    assert_eq!(err(r3(&[seg("b", Mode::Source, 0, 30_000)], &back)), "reader_pass_clock_discontinuity");
    assert_eq!(err(r3(&[seg("b", Mode::Source, 0, 30_000)], &clock(boot, T0 + 560_000))), "reader_pass_session_expired");
    // Sleep: wall clock advanced but monotonic time did not; the gap is not billable.
    let s4 = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0 + 600_000)).unwrap();
    let slept = TimingClock { boot_id: boot.into(), utc_ms: T0 + 600_000 + 200_000, mono_ms: 600_000 + 20_000 };
    assert_eq!(err(timing_report(lib, &id, "pass_a", &s4.session_id, &[seg("z", Mode::Source, 0, 10_000)], "owner", P, &slept)), "reader_pass_clock_discontinuity");
    // Small drift within tolerance is accepted.
    let s5 = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0 + 700_000)).unwrap();
    let drift = TimingClock { boot_id: boot.into(), utc_ms: T0 + 700_000 + 30_000 + 2_000, mono_ms: 700_000 + 30_000 };
    let ok = timing_report(lib, &id, "pass_a", &s5.session_id, &[seg("d", Mode::Source, 0, 30_000)], "owner", P, &drift).unwrap();
    assert_eq!(ok.totals.unique_ms, 330_000);
    // 2001 ms of divergence invalidates the session; accepted intervals stay.
    let over = TimingClock { boot_id: boot.into(), utc_ms: T0 + 700_000 + 40_000 + 2_001, mono_ms: 700_000 + 40_000 };
    assert_eq!(err(timing_report(lib, &id, "pass_a", &s5.session_id, &[seg("e", Mode::Source, 30_000, 40_000)], "owner", P, &over)), "reader_pass_clock_discontinuity");
    assert_eq!(pass(lib, &id, "pass_a").timing.unique_ms, 330_000);
}

#[test]
fn completion_and_deletion_fence_delayed_packets_while_heartbeats_spare_date_cas() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let boot = "boot-fence";
    let s = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0)).unwrap();
    let at = clock(boot, T0 + 200_000);
    let report = |segs: &[SegmentReport]| timing_report(lib, &id, "pass_a", &s.session_id, segs, "owner", P, &at);
    let meta = version(lib, &id);
    let first = report(&[seg("a", Mode::Source, 0, 60_000)]).unwrap();
    // A heartbeat does not invalidate an unrelated manual-date CAS.
    assert_eq!(version(lib, &id), meta);
    let dated = edit(lib, &id, &meta, update("pass_a", day("2026-05"), None, None, None), "owner", P, T0).unwrap();
    report(&[seg("b", Mode::Bilingual, 60_000, 120_000)]).unwrap();
    // Completion with a stale timing version is refused; reconcile and retry.
    let complete = |v: Option<String>| Change::Complete { pass_id: "pass_a".into(), end_date: day("2026-05-28"), expected_timing_version: v };
    assert_eq!(err(edit(lib, &id, &dated.version, complete(Some(first.totals.timing_version.clone())), "owner", P, T0)), "stale_reader_pass_timing");
    let current = pass(lib, &id, "pass_a").timing.timing_version;
    edit(lib, &id, &dated.version, complete(Some(current.clone())), "owner", P, T0).unwrap();
    // Delayed new time cannot be added to a completed pass; replays still reconcile.
    assert_eq!(err(report(&[seg("c", Mode::Source, 120_000, 180_000)])), "reader_pass_closed");
    assert_eq!(report(&[seg("b", Mode::Bilingual, 60_000, 120_000)]).unwrap().totals.timing_version, current);
    assert_eq!(err(timing_start(lib, &id, "pass_a", "owner", P, &at)), "reader_pass_closed");
    // Reopen keeps recorded time; delete removes it from statistics with no replay disclosure.
    ed(lib, &id, Change::Reopen { pass_id: "pass_a".into() }).unwrap();
    assert_eq!(pass(lib, &id, "pass_a").timing.unique_ms, 120_000);
    assert_eq!(err(ed(lib, &id, Change::Delete { pass_id: "pass_a".into(), expected_timing_version: Some(first.totals.timing_version) })), "stale_reader_pass_timing");
    ed(lib, &id, Change::Delete { pass_id: "pass_a".into(), expected_timing_version: Some(current) }).unwrap();
    assert!(!lib.join(format!("catalog/reader-passes/{id}.timing/pass_a.json")).exists());
    assert_eq!(err(report(&[seg("b", Mode::Bilingual, 60_000, 120_000)])), "reader_pass_closed");
    assert_eq!(err(stats(lib, &id, "pass_a", 0, 10, None, P)), "reader_pass_not_found");
    assert!(!lib.join(format!("catalog/reader-passes/{id}.timing/pass_a.json")).exists());
}

/// Covers [from, to) with back-to-back ≤300000 ms sessions/segments.
fn cover(lib: &Path, id: &str, pass_id: &str, boot: &str, from: i64, to: i64, mode: Mode) {
    let mut at = from;
    while at < to {
        let len = (to - at).min(300_000);
        let s = timing_start(lib, id, pass_id, "owner", P, &clock(boot, at)).unwrap();
        timing_report(lib, id, pass_id, &s.session_id, &[seg(&format!("c{at}"), mode, 0, len)], "owner", P, &clock(boot, at + len)).unwrap();
        at += len;
    }
}

#[test]
fn days_split_at_local_midnight_including_23_and_25_hour_dst_days() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    // 2026-03-08 00:00 EST = 05:00Z; 2026-03-09 00:00 EDT = 04:00Z (23 h).
    let spring = 1_772_946_000_000;
    let spring_next = spring + 23 * 3_600_000;
    cover(lib, &id, "pass_a", "boot-dst", spring - 120_000, spring_next + 60_000, Mode::Source);
    // 2026-11-01 00:00 EDT = 04:00Z; 2026-11-02 00:00 EST = 05:00Z (25 h).
    let fall = 1_793_505_600_000;
    cover(lib, &id, "pass_a", "boot-dst", fall, fall + 25 * 3_600_000, Mode::Translation);
    // Another device double-covering one hour does not inflate the day.
    cover(lib, &id, "pass_a", "boot-dst", fall + 3_600_000, fall + 7_200_000, Mode::Bilingual);
    let page = stats(lib, &id, "pass_a", 0, 400, None, P).unwrap();
    let get = |d: &str| page.days.iter().find(|x| x.date == d).cloned().unwrap();
    assert_eq!(get("2026-03-07").ms, 120_000);
    assert_eq!(get("2026-03-08").ms, 23 * 3_600_000);
    assert_eq!(get("2026-03-09").ms, 60_000);
    let fall_day = get("2026-11-01");
    assert_eq!((fall_day.ms, fall_day.translation_ms, fall_day.bilingual_ms), (25 * 3_600_000, 25 * 3_600_000, 0));
    assert_eq!(page.total, 4);
    assert_eq!(page.timezone, NY);
    // Editing reading dates never moves measured minutes.
    ed(lib, &id, update("pass_a", day("2025"), None, None, None)).unwrap();
    assert_eq!(stats(lib, &id, "pass_a", 0, 400, None, P).unwrap().days, page.days);
    // Day pages are bound to the timing version.
    let first = stats(lib, &id, "pass_a", 0, 1, None, P).unwrap();
    assert_eq!(first.next_offset, Some(1));
    assert_eq!(err(stats(lib, &id, "pass_a", 1, 1, Some(&"0".repeat(64)), P)), "stale_reader_pass_timing");
    stats(lib, &id, "pass_a", 1, 1, Some(&first.timing_version), P).unwrap();
    // An invalid or non-IANA zone is rejected at creation.
    for zone in ["Mars/Base", "EST5EDT,M3.2.0,M11.1.0", ""] {
        let c = Change::CreateTracked { pass_id: "pass_tz".into(), revision_id: rev.clone(), method: None, start_date: None, timezone: zone.into() };
        assert_eq!(err(ed(lib, &id, c)), "invalid_reader_pass");
    }
}

#[test]
fn source_change_hidden_items_and_ceiling_rules() {
    let (dir, id, rev) = doc("# 一\nold words here\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_old", &rev)).unwrap();
    let boot = "boot-rev";
    let s = timing_start(lib, &id, "pass_old", "owner", P, &clock(boot, T0)).unwrap();
    let at = clock(boot, T0 + 100_000);
    timing_report(lib, &id, "pass_old", &s.session_id, &[seg("a", Mode::Source, 0, 60_000)], "owner", P, &at).unwrap();
    let newer = edit_markdown(lib, MarkdownEdit { item_id: id.clone(), expected_revision_id: rev.clone(),
        request_id: "pass-edit".into(), markdown: "# 新\n正文\n".into() }, P).unwrap();
    // The old pass is retained, flagged changed, keeps its frozen basis and lengths.
    let old = pass(lib, &id, "pass_old");
    assert_eq!((old.source_status.as_deref(), old.revision_id.as_deref()), (Some("changed"), Some(rev.as_str())));
    assert_eq!(old.lengths.as_ref().unwrap().source.as_ref().unwrap().words, 3);
    assert_eq!(old.timing.unique_ms, 60_000);
    // New time on the old pass needs a deliberately started pass for the new revision.
    assert_eq!(err(timing_start(lib, &id, "pass_old", "owner", P, &at)), "stale_item_revision");
    assert_eq!(err(timing_report(lib, &id, "pass_old", &s.session_id, &[seg("b", Mode::Source, 60_000, 90_000)], "owner", P, &at)), "stale_item_revision");
    assert_eq!(timing_report(lib, &id, "pass_old", &s.session_id, &[seg("a", Mode::Source, 0, 60_000)], "owner", P, &at).unwrap().outcomes[0].outcome, "duplicate");
    ed(lib, &id, tracked("pass_new", &newer.revision_id)).unwrap();
    ed(lib, &id, update("pass_old", day("2026"), None, None, None)).unwrap();
    let new = pass(lib, &id, "pass_new");
    assert_eq!((new.source_status.as_deref(), new.lengths.unwrap().source.unwrap().characters), (Some("current"), 3));
    // Trash hides passes, stats, sessions and reports.
    trash_item(lib, &id, &newer.revision_id, "owner", None, None).unwrap();
    assert_eq!(err(list(lib, &id, 0, 10, None, P)), "library_resource_not_found");
    assert_eq!(err(stats(lib, &id, "pass_old", 0, 10, None, P)), "library_resource_not_found");
    assert_eq!(err(timing_start(lib, &id, "pass_new", "owner", P, &at)), "library_resource_not_found");
    assert_eq!(err(timing_report(lib, &id, "pass_old", &s.session_id, &[seg("a", Mode::Source, 0, 60_000)], "owner", P, &at)), "library_resource_not_found");
    assert_eq!(err(edit(lib, &id, &"0".repeat(64), Change::Reopen { pass_id: "pass_old".into() }, "owner", P, T0)), "library_resource_not_found");
}

#[test]
fn private_items_are_invisible_below_their_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    capture.capture.sensitivity = Sensitivity::Private;
    let mut draft = crate::tests::draft();
    draft.sensitivity = Sensitivity::Private;
    draft.content_markdown = "secret\n".into();
    let receipt = ingest(dir.path(), &capture, draft).unwrap();
    let (lib, id, rev) = (dir.path(), &receipt.item_ids[0], &receipt.revision_ids[0]);
    let v = list(lib, id, 0, 10, None, Sensitivity::Private).unwrap().version;
    edit(lib, id, &v, tracked("pass_a", rev), "owner", Sensitivity::Private, T0).unwrap();
    assert_eq!(err(list(lib, id, 0, 10, None, P)), "library_resource_not_found");
    assert_eq!(err(stats(lib, id, "pass_a", 0, 10, None, P)), "library_resource_not_found");
    assert_eq!(err(timing_start(lib, id, "pass_a", "owner", P, &clock("boot-priv", T0))), "library_resource_not_found");
    let v = list(lib, id, 0, 10, None, Sensitivity::Private).unwrap().version;
    assert_eq!(err(edit(lib, id, &v, Change::Reopen { pass_id: "pass_a".into() }, "owner", P, T0)), "library_resource_not_found");
}

#[test]
fn counts_use_visible_text_separate_units_and_bound_translation_edition() {
    assert_eq!(count_text("Hello, world! [link text](https://example.com/very/long/path) `code`\n"), (5, 0));
    assert_eq!(count_text("# 标题\n\n甲🌹乙 abc ![alt words](x.png) <span>html</span>\n"), (2, 4));
    assert_eq!(count_text("| 列 | col |\n|---|---|\n| 中 | two words |\n"), (3, 2));
    assert_eq!(count_text("- item *one*two\n\n```\nlet x = 1;\n```\n"), (5, 0));
    assert_eq!(count_text("カタカナ 한국어 ｶﾅ\n"), (0, 9));
    let (dir, id, rev) = book();
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let p = pass(lib, &id, "pass_a");
    let l = p.lengths.clone().unwrap();
    assert_eq!(l.policy, COUNT_POLICY);
    let (src, tr) = (l.source.unwrap(), l.translation.unwrap());
    assert_eq!((src.side.as_str(), src.words, src.characters), ("original", 2, 0));
    assert_eq!((tr.side.as_str(), tr.words, tr.characters), ("translation", 0, 2));
    assert_ne!(src.sha256, tr.sha256);
    // Zero measured time yields unavailable rates, not infinity.
    assert_eq!((p.rates.minutes, p.rates.source.words_per_minute, p.rates.combined.words_per_minute), (None, None, None));
    let boot = "boot-count";
    let s = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0)).unwrap();
    timing_report(lib, &id, "pass_a", &s.session_id, &[seg("a", Mode::Bilingual, 0, 120_000)], "owner", P, &clock(boot, T0 + 130_000)).unwrap();
    let p = pass(lib, &id, "pass_a");
    assert_eq!(p.rates.minutes, Some(2.0));
    assert_eq!(p.rates.source.words_per_minute, Some(1.0));
    assert_eq!(p.rates.translation.characters_per_minute, Some(1.0));
    assert_eq!((p.rates.combined.words_per_minute, p.rates.combined.characters_per_minute), (Some(1.0), Some(1.0)));
    // Ordinary documents have no translation edition: target/combined unavailable.
    let (dir2, id2, rev2) = doc("one two three four\n");
    ed(dir2.path(), &id2, tracked("pass_b", &rev2)).unwrap();
    let p = pass(dir2.path(), &id2, "pass_b");
    assert_eq!(p.lengths.unwrap().translation, None);
    assert_eq!(p.rates.combined, UnitRates { words_per_minute: None, characters_per_minute: None });
}

fn synthetic_pass(n: usize, start: Option<PassDate>) -> Pass {
    Pass {
        pass_id: format!("pass_{n:05}"), ordinal: n as u64 + 1, kind: Kind::Retrospective, status: Status::Completed,
        method: None, start_date: start, end_date: None, estimated_duration_minutes: None, duration_display_unit: None,
        timezone: NY.into(), revision_basis: None, revision_id: None, canonical_sha256: None, lengths: None,
        created_at_ms: T0, completed_at_ms: Some(T0),
    }
}

#[test]
fn quotas_pause_creation_and_recording_but_allow_edit_complete_delete() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_live", &rev)).unwrap();
    let mut l = ledger(&existing_canonical_root(lib).unwrap(), &id).unwrap();
    for n in 1..MAX_PASSES {
        l.passes.push(synthetic_pass(n, None));
    }
    l.last_ordinal = MAX_PASSES as u64;
    fs::write(ledger_path(lib, &id), serde_json::to_vec(&l).unwrap()).unwrap();
    assert_eq!(err(ed(lib, &id, retro("pass_more", None, None, None))), "reader_pass_limit_exceeded");
    // At the live-pass cap, editing and completion remain available.
    ed(lib, &id, update("pass_00007", day("2020"), None, Some(5), None)).unwrap();
    ed(lib, &id, Change::Complete { pass_id: "pass_live".into(), end_date: day("2026"), expected_timing_version: None }).unwrap();
    ed(lib, &id, Change::Reopen { pass_id: "pass_live".into() }).unwrap();
    ed(lib, &id, Change::Delete { pass_id: "pass_00001".into(), expected_timing_version: None }).unwrap();
    assert_eq!(ed(lib, &id, retro("pass_more", None, None, None)).unwrap().ordinal, MAX_PASSES as u64 + 1);
    // Pages respect the 48 KiB budget without truncating a record.
    let page = list(lib, &id, 0, 100, None, P).unwrap();
    assert!(page.passes.len() < 100 && page.next_offset == Some(page.passes.len()));
    assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_BYTES);
    // Identity cap includes tombstones.
    let mut l = ledger(&existing_canonical_root(lib).unwrap(), &id).unwrap();
    l.passes.truncate(10);
    l.tombstones = (0..MAX_PASS_IDENTITIES - 10).map(|n| Tombstone { pass_id: format!("pass_t{n}"), ordinal: 1 }).collect();
    fs::write(ledger_path(lib, &id), serde_json::to_vec(&l).unwrap()).unwrap();
    assert_eq!(err(ed(lib, &id, retro("pass_again", None, None, None))), "reader_pass_limit_exceeded");
    ed(lib, &id, Change::Delete { pass_id: "pass_00002".into(), expected_timing_version: None }).unwrap();
    // Segment identity cap pauses recording, keeps data and still permits completion.
    let mut t = timing(lib, &id, "pass_live").unwrap();
    t.segments = (0..MAX_SEGMENTS as i64).map(|n| Accepted { session_id: "tsess_old".into(), principal: "owner".into(),
        segment_id: format!("s{n}"), payload_sha256: "0".repeat(64), mode: Mode::Source,
        start_ms: T0 + n * 10, end_ms: T0 + n * 10 + 5, seq: n as u64 + 1 }).collect();
    t.sequence = MAX_SEGMENTS as u64;
    fs::create_dir_all(lib.join(timing_dir(&id))).unwrap();
    fs::write(timing_path(lib, &id, "pass_live"), serde_json::to_vec(&t).unwrap()).unwrap();
    let c = clock("boot-quota", T0);
    assert_eq!(err(timing_start(lib, &id, "pass_live", "owner", P, &c)), "reader_pass_limit_exceeded");
    let live = pass(lib, &id, "pass_live");
    assert!(live.timing.recording_paused);
    assert_eq!(live.timing.unique_ms, MAX_SEGMENTS as i64 * 5);
    ed(lib, &id, Change::Complete { pass_id: "pass_live".into(), end_date: day("2026"), expected_timing_version: Some(live.timing.timing_version) }).unwrap();
}

#[test]
fn segment_cap_rejects_a_new_report_on_a_live_session() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let mut t = timing(lib, &id, "pass_a").unwrap();
    t.segments = (0..MAX_SEGMENTS as i64 - 1).map(|n| Accepted { session_id: "tsess_old".into(), principal: "owner".into(),
        segment_id: format!("s{n}"), payload_sha256: "0".repeat(64), mode: Mode::Source,
        start_ms: n * 10, end_ms: n * 10 + 5, seq: n as u64 + 1 }).collect();
    t.sequence = MAX_SEGMENTS as u64 - 1;
    fs::create_dir_all(lib.join(timing_dir(&id))).unwrap();
    fs::write(timing_path(lib, &id, "pass_a"), serde_json::to_vec(&t).unwrap()).unwrap();
    let boot = "boot-cap";
    let s = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0)).unwrap();
    let at = clock(boot, T0 + 10_000);
    assert_eq!(err(timing_report(lib, &id, "pass_a", &s.session_id, &[seg("a", Mode::Source, 0, 1), seg("b", Mode::Source, 1, 2)], "owner", P, &at)), "reader_pass_limit_exceeded");
    let r = timing_report(lib, &id, "pass_a", &s.session_id, &[seg("a", Mode::Source, 0, 1)], "owner", P, &at).unwrap();
    assert!(r.totals.recording_paused);
}

#[test]
fn corrupt_or_foreign_files_fail_closed_not_empty() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    fs::create_dir_all(lib.join(timing_dir(&id))).unwrap();
    fs::write(timing_path(lib, &id, "pass_a"), b"{not json").unwrap();
    assert_eq!(err(list(lib, &id, 0, 10, None, P)), "reader_passes_unavailable");
    assert_eq!(err(stats(lib, &id, "pass_a", 0, 10, None, P)), "reader_passes_unavailable");
    let mut t = Timing { schema_version: SCHEMA, item_id: "item_other".into(), pass_id: "pass_a".into(), ..Default::default() };
    fs::write(timing_path(lib, &id, "pass_a"), serde_json::to_vec(&t).unwrap()).unwrap();
    assert_eq!(err(list(lib, &id, 0, 10, None, P)), "reader_passes_unavailable");
    t.item_id = id.clone();
    t.schema_version = 2;
    fs::write(timing_path(lib, &id, "pass_a"), serde_json::to_vec(&t).unwrap()).unwrap();
    assert_eq!(err(list(lib, &id, 0, 10, None, P)), "reader_passes_unavailable");
    fs::write(ledger_path(lib, &id), b"[]").unwrap();
    assert_eq!(err(list(lib, &id, 0, 10, None, P)), "reader_passes_unavailable");
    assert_eq!(err(edit(lib, &id, &"0".repeat(64), retro("pass_b", None, None, None), "owner", P, T0)), "reader_passes_unavailable");
}

#[test]
fn restoring_an_older_backup_never_lets_a_later_version_pass_cas() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    let a = ed(lib, &id, tracked("pass_a", &rev)).unwrap();
    let boot = "boot-restore";
    let s = timing_start(lib, &id, "pass_a", "owner", P, &clock(boot, T0)).unwrap();
    let at = clock(boot, T0 + 200_000);
    let report = |segs: &[SegmentReport]| timing_report(lib, &id, "pass_a", &s.session_id, segs, "owner", P, &at);
    let t1 = report(&[seg("a", Mode::Source, 0, 10_000)]).unwrap().totals.timing_version;
    let (ledger_file, timing_file) = (ledger_path(lib, &id), timing_path(lib, &id, "pass_a"));
    let backup = (fs::read(&ledger_file).unwrap(), fs::read(&timing_file).unwrap());
    // After the backup another device edits and reports; it holds those versions.
    let b = ed(lib, &id, update("pass_a", day("2026"), None, None, None)).unwrap();
    let t2 = report(&[seg("b", Mode::Source, 10_000, 20_000)]).unwrap().totals.timing_version;
    fs::write(&ledger_file, &backup.0).unwrap();
    fs::write(&timing_file, &backup.1).unwrap();
    assert_eq!((version(lib, &id), pass(lib, &id, "pass_a").timing.timing_version), (a.version.clone(), t1.clone()));
    // A different write takes the same sequence numbers yet yields different versions.
    let c = edit(lib, &id, &a.version, update("pass_a", day("2025"), None, None, None), "owner", P, T0).unwrap();
    assert_ne!(c.version, b.version);
    let t3 = report(&[seg("c", Mode::Translation, 30_000, 40_000)]).unwrap().totals.timing_version;
    assert_ne!(t3, t2);
    assert_eq!(err(edit(lib, &id, &b.version, Change::Reopen { pass_id: "pass_a".into() }, "owner", P, T0)), "stale_reader_passes");
    assert_eq!(err(ed(lib, &id, Change::Complete { pass_id: "pass_a".into(), end_date: day("2026"), expected_timing_version: Some(t2) })), "stale_reader_pass_timing");
    // The restored ordinal counter continues from the backup's state.
    assert_eq!(ed(lib, &id, tracked("pass_z", &rev)).unwrap().ordinal, 2);
}

#[test]
fn counts_exclude_cjk_punctuation_but_keep_marks_that_are_part_of_words() {
    // The previous whole-block ranges counted ・ ゠ ゛ as characters (18) and took the
    // iteration mark 〻 for a word (1). Only CJK letters and in-word marks count.
    let text = "東京・大阪、ラーメン゠つけ麺。ゝ か゛ ｶﾅ･ 〻\n";
    assert_eq!(count_text(text), (0, 16));
    for c in ['・', '゠', '゛', '゜', '･', '、', '。', '「', '」', '（', '，', '！', '〜', '〇'] {
        assert_eq!(is_cjk(c), c == '〇', "{c}");
    }
    for c in ['ー', 'ｰ', '々', '〆', '〻', 'ゝ', 'ゞ', 'ヽ', 'ヾ', 'ヿ', 'ゟ', 'ㇰ', 'ㄅ', '가', 'ㄱ', '𠀀'] {
        assert!(is_cjk(c), "{c}");
    }
}

#[test]
fn metadata_storage_limit_blocks_only_creation() {
    let (dir, id, rev) = doc("x\n");
    let lib = dir.path();
    ed(lib, &id, tracked("pass_live", &rev)).unwrap();
    let raw = Some("r".repeat(256));
    let mut l = ledger(&existing_canonical_root(lib).unwrap(), &id).unwrap();
    for n in 1..MAX_PASSES {
        let mut p = synthetic_pass(n, Some(PassDate { value: "2024-01-02".into(), precision: Precision::Day, raw: raw.clone() }));
        p.pass_id = format!("pass_{n:0>122}");
        p.end_date = p.start_date.clone();
        l.passes.push(p);
    }
    let base = MAX_PASSES as u64;
    l.tombstones = (0..MAX_PASS_IDENTITIES - MAX_PASSES - 1)
        .map(|n| Tombstone { pass_id: format!("pass_t{n:0>121}"), ordinal: base }).collect();
    l.last_ordinal = base;
    let bytes = serde_json::to_vec(&l).unwrap();
    assert!(bytes.len() > MAX_LEDGER_BYTES && bytes.len() <= MAX_LEDGER_FILE_BYTES, "{}", bytes.len());
    fs::write(ledger_path(lib, &id), bytes).unwrap();
    // One identity slot remains, but the storage bound refuses a new record.
    let p = l.passes.iter().map(|p| p.pass_id.clone()).nth(5).unwrap();
    l.passes.retain(|x| x.pass_id != p);
    drop(l);
    assert_eq!(err(ed(lib, &id, retro("pass_more", None, None, None))), "reader_pass_limit_exceeded");
    // Edit, complete and delete remain available above the bound.
    ed(lib, &id, update(&p, day("2024"), day("2025"), Some(30), None)).unwrap();
    ed(lib, &id, Change::Complete { pass_id: "pass_live".into(), end_date: day("2026"), expected_timing_version: None }).unwrap();
    ed(lib, &id, Change::Reopen { pass_id: "pass_live".into() }).unwrap();
    ed(lib, &id, Change::Delete { pass_id: p.clone(), expected_timing_version: None }).unwrap();
}
