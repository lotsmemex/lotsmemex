use super::*;
use super::super::annotations as edition;
use super::super::annotations_ai as chapter;
use std::cell::{Cell, RefCell};

const PRIV: Sensitivity = Sensitivity::Private;
// Spans two 16 KiB translation pages; every 3- and 4-byte char repeats.
fn long_body() -> String {
    "甲🌹乙。\n\n".repeat(2500)
}

/// (analysis id, record JSON, body) per revision.
type Doc = (String, Vec<u8>, String);

/// Stands in for the Analysis validator: serves validated records per revision.
#[derive(Default)]
struct Fake {
    docs: RefCell<BTreeMap<String, Doc>>,
    known: RefCell<BTreeSet<String>>,
    hidden: RefCell<BTreeSet<String>>,
    corrupt: Cell<bool>,
    calls: Cell<usize>,
}

impl Resolver for Fake {
    fn current(&self, _: &Path, _: &str, revision: &str, _: Sensitivity) -> Result<Option<VerifiedDocument>> {
        self.calls.set(self.calls.get() + 1);
        anyhow::ensure!(!self.corrupt.get(), "analysis index entry mismatch");
        Ok(self.docs.borrow().get(revision).filter(|d| !self.hidden.borrow().contains(&d.0)).map(|(id, json, body)| {
            VerifiedDocument { analysis_id: id.clone(), record_json: json.clone(), body_markdown: body.clone() }
        }))
    }
    fn record_visible(&self, _: &Path, id: &str, _: Sensitivity) -> Result<bool> {
        anyhow::ensure!(!self.corrupt.get(), "analysis index entry mismatch");
        Ok(self.known.borrow().contains(id) && !self.hidden.borrow().contains(id))
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    lib: PathBuf,
    item: String,
    rev: String,
    r: Fake,
}

fn analysis_id(seed: &str) -> String {
    format!("analysis_{}", reader_sha256(seed.as_bytes()))
}

fn record(revision: &ItemRevision, id: &str, body: &str, purpose: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "analysis_id": id, "analysis_kind": "translation", "title": "合成",
        "body_markdown": body,
        "inputs": [{"item_id": revision.item_id, "revision_id": revision.revision_id,
            "revision_sha256": format!("sha256:{}", reader_sha256(&serde_json::to_vec(revision).unwrap())),
            "evidence_ids": revision.evidence_ids}],
        "topics": [], "sensitivity": "private", "created_at": "2026-11-08T00:00:00Z",
        "provenance": {"actor_kind": "tool", "actor_id": "synthetic", "tool_id": "synthetic", "tool_version": "1", "config_digest": "x"},
        "generated_text": {"purpose": purpose},
    }))
    .unwrap()
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().to_path_buf();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    capture.capture.sensitivity = PRIV;
    draft.sensitivity = PRIV;
    draft.content_markdown = "# Synthetic article\n\nPlain English body.\n".into();
    let receipt = ingest(&lib, &capture, draft).unwrap();
    let (item, rev) = (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone());
    let f = Fx { _dir: dir, lib, item, rev, r: Fake::default() };
    f.publish("doc-1", &long_body(), "document_translation");
    f
}

impl Fx {
    fn revision(&self) -> ItemRevision {
        read_item(&self.lib, &self.item).unwrap().1
    }
    /// A completed DocumentTranslation record for the current revision.
    fn publish(&self, seed: &str, body: &str, purpose: &str) -> String {
        let revision = self.revision();
        let id = analysis_id(seed);
        self.r.known.borrow_mut().insert(id.clone());
        self.r.docs.borrow_mut().insert(revision.revision_id.clone(), (id.clone(), record(&revision, &id, body, purpose), body.into()));
        id
    }
    fn ident(&self) -> Identity {
        identity(&self.lib, &self.item, PRIV, &self.r).unwrap().unwrap()
    }
    fn anchor(&self, id: &Identity, segs: &[(usize, usize)]) -> Anchor {
        Anchor { revision_id: self.revision().revision_id, side: SPACE.into(), projection_kind: KIND.into(),
            projection_id: id.projection_id.clone(), translation_version: id.translation_version.clone(), sha256: id.sha256.clone(),
            segments: segs.iter().map(|(s, e)| Segment { start: *s, end: *e }).collect() }
    }
    fn mark(&self, mid: &str, style: &str, id: &Identity, segs: &[(usize, usize)]) -> Change {
        Change::Put { annotation: Annotation { id: mid.into(), style: style.into(), anchor: self.anchor(id, segs) } }
    }
    fn page(&self) -> AnnotationPage {
        list(&self.lib, &self.item, None, 0, 100, None, None, PRIV, &self.r).unwrap()
    }
    fn put(&self, version: &str, change: Change) -> Result<EditResult> {
        edit(&self.lib, &self.item, version, change, "owner", PRIV, &self.r)
    }
    fn ledger_file(&self) -> PathBuf {
        self.lib.join(format!("catalog/reader-ai-document-annotations/{}.json", self.item))
    }
}

fn err<T: std::fmt::Debug>(r: Result<T>) -> String {
    r.unwrap_err().to_string()
}

/// The existing 16 KiB translation page split (analysis_translation_utf8_v1).
fn page_start(body: &str) -> usize {
    let mut end = (16 * 1024).min(body.len());
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[test]
fn multi_segment_utf8_marks_use_complete_body_coordinates_across_16k_pages() {
    let f = fixture();
    let body = long_body();
    assert!(body.len() > 2 * 16 * 1024);
    let id = f.ident();
    assert_eq!(id.sha256, reader_sha256(body.as_bytes()));
    let v0 = f.page().version;
    // Second page: start_byte plus the page-local offset of the visible "🌹".
    let p2 = page_start(&body);
    let local = body[p2..].find('🌹').unwrap();
    let rose = (p2 + local, p2 + local + 4);
    assert_eq!(&body[rose.0..rose.1], "🌹");
    let r = f.put(&v0, f.mark("x", "yellow", &id, &[(0, 3), rose])).unwrap();
    assert_eq!((r.outcome.as_str(), r.annotation_space.as_str(), r.projection_kind.as_str()), ("created", SPACE, KIND));
    let page = f.page();
    assert_eq!((page.total, page.annotations[0].status.as_str()), (1, "current"));
    assert_eq!(page.annotations[0].anchor.segments, vec![Segment { start: 0, end: 3 }, Segment { start: rose.0, end: rose.1 }]);
    // Page-local zero would be different text; the same range+style is a duplicate, never relocated.
    assert_ne!(body.get(local..local + 4), Some("🌹"));
    assert_eq!(err(f.put(&r.version, f.mark("y", "yellow", &id, &[(0, 3), rose]))), "reader_annotation_conflict");
    // Inside a multibyte char, past the body (title/cover/name overlays are not body bytes),
    // out of order or overlapping: the whole mark is rejected.
    for bad in [vec![(0, 3), (rose.0 + 1, rose.1)], vec![(0, body.len() + 1)], vec![(rose.0, rose.1), (0, 3)], vec![(0, 7), (3, 10)]] {
        assert_eq!(err(f.put(&r.version, f.mark("bad", "red", &id, &bad))), "invalid_reader_anchor");
    }
    assert_eq!(f.page().version, r.version);
}

#[test]
fn version_formula_and_same_hash_different_analysis_is_stale() {
    let f = fixture();
    let revision = f.revision();
    let id = f.ident();
    let (pid, json, body) = f.r.docs.borrow()[&f.rev].clone();
    let expected = reader_sha256(format!(r#"["memex.async-document-annotation.v1","{}","{}","{}","{}","{}"]"#,
        f.item, revision.revision_id, pid, reader_sha256(&json), reader_sha256(body.as_bytes())).as_bytes());
    assert_eq!(id.translation_version, expected);
    let v0 = f.page().version;
    let a = f.put(&v0, f.mark("m", "red", &id, &[(0, 3)])).unwrap();
    // A byte-identical document under another Analysis record is another identity.
    f.publish("doc-2", &long_body(), "document_translation");
    let id2 = f.ident();
    assert_eq!(id2.sha256, id.sha256);
    assert_ne!((&id2.projection_id, &id2.translation_version), (&id.projection_id, &id.translation_version));
    assert_eq!(f.page().annotations[0].status, "stale");
    assert_eq!(err(f.put(&a.version, f.mark("n", "red", &id, &[(3, 7)]))), "stale_reader_translation");
    assert_eq!(err(f.put(&a.version, f.mark("m", "blue", &id, &[(0, 3)]))), "stale_reader_translation");
    // Hash alone or a mixed identity never matches.
    let mut mixed = id2.clone();
    mixed.projection_id = id.projection_id.clone();
    assert_eq!(err(f.put(&a.version, f.mark("n", "red", &mixed, &[(3, 7)]))), "stale_reader_translation");
    let n = f.put(&a.version, f.mark("n", "red", &id2, &[(3, 7)])).unwrap();
    let only = |i: &Identity| -> Vec<String> {
        list(&f.lib, &f.item, Some(&ProjectionFilter { projection_id: i.projection_id.clone(),
            translation_version: i.translation_version.clone(), sha256: i.sha256.clone() }), 0, 100, None, None, PRIV, &f.r)
            .unwrap().annotations.into_iter().map(|m| m.id).collect()
    };
    assert_eq!((only(&id), only(&id2)), (vec!["m".to_string()], vec!["n".to_string()]));
    assert_eq!(f.put(&n.version, Change::Delete { id: "m".into() }).unwrap().outcome, "deleted");
}

#[test]
fn title_summary_and_wrong_kind_never_mark_and_corruption_fails_closed() {
    let f = fixture();
    let id = f.ident();
    let v0 = f.page().version;
    // A title (or any other) Analysis id is never the effective document.
    let mut title = id.clone();
    title.projection_id = analysis_id("title-1");
    assert_eq!(err(f.put(&v0, f.mark("t", "red", &title, &[(0, 3)]))), "stale_reader_translation");
    // Only async_document / ai_translation; no chapter key, no mixed kinds.
    for (kind, side) in [("chapter", SPACE), (KIND, "translation")] {
        let mut c = f.mark("k", "red", &id, &[(0, 3)]);
        if let Change::Put { annotation } = &mut c { annotation.anchor.projection_kind = kind.into(); annotation.anchor.side = side.into(); }
        assert_eq!(err(f.put(&v0, c)), "invalid_reader_annotation");
    }
    let chapter_shaped = serde_json::json!({"op":"put","annotation":{"id":"z","style":"red","anchor":{"revision_id": f.rev,
        "side": SPACE, "segments":[{"chapter":"section-0","translation_version": id.translation_version, "sha256": id.sha256, "start":0,"end":3}]}}});
    assert!(serde_json::from_value::<Change>(chapter_shaped).is_err());
    // A resolver result that is a summary/title record, or bound elsewhere, is corrupt, not stale.
    f.publish("summary-1", "摘要。", "summary");
    assert_eq!(err(f.put(&v0, f.mark("s", "red", &id, &[(0, 3)]))), "invalid_reader_translation_request");
    assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, PRIV, &f.r)), "invalid_reader_translation_request");
    let revision = f.revision();
    let mut other = revision.clone();
    other.revision_id = "revision_other".into();
    let pid = analysis_id("doc-x");
    f.r.docs.borrow_mut().insert(f.rev.clone(), (pid.clone(), record(&other, &pid, "正文", "document_translation"), "正文".into()));
    assert_eq!(err(f.put(&v0, f.mark("s", "red", &id, &[(0, 3)]))), "invalid_reader_translation_request");
    f.publish("doc-1", &long_body(), "document_translation");
    // Resolver corruption propagates under the writer lock; nothing is written.
    f.r.corrupt.set(true);
    assert_eq!(err(f.put(&v0, f.mark("c", "red", &id, &[(0, 3)]))), "analysis index entry mismatch");
    assert!(!f.ledger_file().exists());
}

#[test]
fn revision_change_and_missing_translation_are_stale_never_rebound() {
    let f = fixture();
    let id = f.ident();
    let a = f.put(&f.page().version, f.mark("old", "red", &id, &[(0, 3)])).unwrap();
    let r2 = revise_item_metadata(&f.lib, &f.item, ItemMetadataUpdate {
        expected_revision_id: f.rev.clone(), title: "Renamed synthetic".into(), manual_tags: vec![] }).unwrap();
    assert!(identity(&f.lib, &f.item, PRIV, &f.r).unwrap().is_none());
    assert_eq!(f.page().annotations[0].status, "stale");
    // Old-revision anchors are stale_item_revision; current revision without a document is stale translation.
    assert_eq!(err(f.put(&a.version, Change::Put { annotation: Annotation { id: "n".into(), style: "red".into(),
        anchor: Anchor { revision_id: f.rev.clone(), ..f.anchor(&id, &[(0, 3)]) } } })), "stale_item_revision");
    assert_eq!(err(f.put(&a.version, f.mark("n", "red", &id, &[(0, 3)]))), "stale_reader_translation");
    // Same body re-published for R2 is a new identity; the old mark stays stale.
    f.publish("doc-r2", &long_body(), "document_translation");
    let id2 = f.ident();
    assert_ne!(id2.translation_version, id.translation_version);
    let n = f.put(&a.version, f.mark("n", "red", &id2, &[(0, 3)])).unwrap();
    let page = f.page();
    assert_eq!(page.annotations.iter().map(|m| (m.id.as_str(), m.status.as_str())).collect::<Vec<_>>(), [("n", "current"), ("old", "stale")]);
    assert_eq!(page.annotations[0].anchor.revision_id, r2.revision_id);
    f.put(&n.version, Change::Delete { id: "old".into() }).unwrap();
}

#[test]
fn two_device_cas_exact_replay_unknown_and_aba() {
    let f = fixture();
    let id = f.ident();
    let v0 = f.page().version;
    let a = f.mark("a", "red", &id, &[(0, 3)]);
    let b = f.mark("b", "bold", &id, &[(3, 7)]);
    let ra = edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV, &f.r).unwrap();
    assert_eq!(err(edit(&f.lib, &f.item, &v0, b.clone(), "device-b", PRIV, &f.r)), "stale_reader_annotations");
    // Lost reply / internal_error after commit: the exact retry returns the stored result, no write.
    let bytes = fs::read(f.ledger_file()).unwrap();
    assert_eq!(edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV, &f.r).unwrap(), ra);
    assert_eq!(fs::read(f.ledger_file()).unwrap(), bytes);
    assert_eq!(err(edit(&f.lib, &f.item, &v0, a.clone(), "device-b", PRIV, &f.r)), "stale_reader_annotations");
    let page = f.page();
    assert!(page.version == ra.version && page.annotations.iter().any(|m| m.id == "a"));
    let rb = edit(&f.lib, &f.item, &ra.version, b, "device-b", PRIV, &f.r).unwrap();
    assert_eq!(err(edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV, &f.r)), "stale_reader_annotations");
    let d = f.put(&rb.version, Change::Delete { id: "a".into() }).unwrap();
    let back = f.put(&d.version, a).unwrap();
    assert_ne!(back.version, rb.version);
    // Restoring an older ledger never lets a later version pass CAS.
    let backup = fs::read(f.ledger_file()).unwrap();
    let later = f.put(&back.version, f.mark("l", "blue", &id, &[(7, 10)])).unwrap();
    fs::write(f.ledger_file(), &backup).unwrap();
    let other = f.put(&back.version, f.mark("o", "yellow", &id, &[(7, 10)])).unwrap();
    assert_ne!(other.version, later.version);
    assert_eq!(err(f.put(&later.version, Change::Delete { id: "l".into() })), "stale_reader_annotations");
}

#[test]
fn receipt_replay_rechecks_bound_history_and_analysis_access() {
    let f = fixture();
    let id = f.ident();
    let created = f.put(&f.page().version, f.mark("h", "red", &id, &[(0, 3)])).unwrap();
    revise_item_metadata(&f.lib, &f.item, ItemMetadataUpdate {
        expected_revision_id: f.rev.clone(), title: "Renamed synthetic".into(), manual_tags: vec![] }).unwrap();
    let delete = Change::Delete { id: "h".into() };
    let deleted = f.put(&created.version, delete.clone()).unwrap();
    let bytes = fs::read(f.ledger_file()).unwrap();
    // Control: bindings readable, the lost delete reply replays exactly.
    assert_eq!(f.put(&created.version, delete.clone()).unwrap(), deleted);
    // The bound Analysis record becomes unreadable: no receipt disclosure.
    f.r.hidden.borrow_mut().insert(id.projection_id.clone());
    assert_eq!(err(f.put(&created.version, delete.clone())), "stale_reader_annotations");
    f.r.hidden.borrow_mut().clear();
    assert_eq!(f.put(&created.version, delete.clone()).unwrap(), deleted);
    // The bound source history becomes unreadable: same.
    chapter::tests::hide_history(&f.rev);
    assert_eq!(err(f.put(&created.version, delete)), "stale_reader_annotations");
    assert_eq!(fs::read(f.ledger_file()).unwrap(), bytes, "no second write");
}

#[test]
fn hidden_analysis_records_hide_marks_counts_and_stale_old_pages() {
    let f = fixture();
    let id = f.ident();
    let mut v = f.page().version;
    for (m, s, e) in [("a1", 0, 3), ("a2", 3, 7)] {
        v = f.put(&v, f.mark(m, "red", &id, &[(s, e)])).unwrap().version;
    }
    f.publish("doc-2", &long_body(), "document_translation");
    let id2 = f.ident();
    for (m, s, e) in [("b1", 0, 3), ("b2", 3, 7)] {
        v = f.put(&v, f.mark(m, "bold", &id2, &[(s, e)])).unwrap().version;
    }
    let page = |offset, ev: Option<&str>, vv: Option<&str>| list(&f.lib, &f.item, None, offset, 2, ev, vv, PRIV, &f.r);
    let ids = |p: &AnnotationPage| p.annotations.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    let first = page(0, None, None).unwrap();
    assert_eq!((ids(&first), first.total, first.next_offset), (vec!["a1".to_string(), "a2".to_string()], 4, Some(2)));
    // Control: unchanged visibility continues the same view.
    assert_eq!(ids(&page(2, Some(&first.version), Some(&first.view_version)).unwrap()), ["b1", "b2"]);
    assert_eq!(err(page(2, Some(&first.version), None)), "invalid_reader_annotation");
    // The old (non-current) Analysis record becomes unreadable; ledger and current identity unchanged.
    f.r.hidden.borrow_mut().insert(id.projection_id.clone());
    let fresh = page(0, None, None).unwrap();
    assert_eq!(fresh.version, first.version);
    assert_eq!(err(page(2, Some(&first.version), Some(&first.view_version))), "stale_reader_annotations");
    assert_eq!((ids(&fresh), fresh.total, fresh.next_offset), (vec!["b1".to_string(), "b2".to_string()], 2, None));
    assert_eq!(err(f.put(&fresh.version, Change::Delete { id: "a1".into() })), "library_resource_not_found");
    f.put(&fresh.version, Change::Delete { id: "b1".into() }).unwrap();
}

#[test]
fn chapter_and_edition_ledgers_are_isolated_byte_for_byte() {
    let f = fixture();
    let id = f.ident();
    let doc = reader_document(&f.lib, &f.item, &f.rev, PRIV).unwrap();
    let e = &doc.editions[0];
    let ev0 = edition::list(&f.lib, &f.item, 0, 100, None, PRIV).unwrap().version;
    let ed = edition::edit(&f.lib, &f.item, &ev0, edition::Change::Put { annotation: edition::Annotation {
        id: "m".into(), style: "red".into(), anchor: edition::AnnotationAnchor { revision_id: f.rev.clone(),
            side: e.side.clone(), sha256: e.sha256.clone(),
            segments: vec![edition::Segment { chapter: e.chapters[0].key.clone(), start: 0, end: 3 }] } } }, "owner", PRIV).unwrap();
    let edition_file = f.lib.join(format!("catalog/reader-annotations/{}.json", f.item));
    let edition_bytes = fs::read(&edition_file).unwrap();
    let cv0 = chapter::list(&f.lib, &f.item, None, 0, 100, None, None, PRIV).unwrap().version;
    let dv0 = f.page().version;
    assert!(dv0 != cv0 && dv0 != ev0 && dv0 != ed.version);
    assert_eq!(err(f.put(&cv0, f.mark("m", "red", &id, &[(0, 3)]))), "stale_reader_annotations");
    let dm = f.put(&dv0, f.mark("m", "red", &id, &[(0, 3)])).unwrap();
    assert_eq!(err(chapter::edit(&f.lib, &f.item, &dm.version, chapter::Change::Delete { id: "m".into() }, "owner", PRIV)), "stale_reader_annotations");
    let doc_shaped = serde_json::to_value(f.mark("z", "red", &id, &[(0, 3)])).unwrap();
    assert!(serde_json::from_value::<chapter::Change>(doc_shaped.clone()).is_err());
    assert!(serde_json::from_value::<edition::Change>(doc_shaped).is_err());
    assert_eq!(fs::read(&edition_file).unwrap(), edition_bytes);
    assert!(!f.lib.join(format!("catalog/reader-ai-annotations/{}.json", f.item)).exists());
    // Edition writes leave the document ledger byte-identical; same id survives in both.
    let doc_bytes = fs::read(f.ledger_file()).unwrap();
    edition::edit(&f.lib, &f.item, &ed.version, edition::Change::Delete { id: "m".into() }, "owner", PRIV).unwrap();
    assert_eq!(fs::read(f.ledger_file()).unwrap(), doc_bytes);
    assert_eq!(f.page().annotations[0].id, "m");
}

#[test]
fn quota_full_still_deletes_and_corrupt_ledger_fails_closed() {
    let f = fixture();
    let id = f.ident();
    let anchor = f.anchor(&id, &[(0, 3)]);
    let marks = (0..MAX_MARKS).map(|i| Stored { id: format!("q{i:05}"), style: STYLES[i % 5].into(), anchor: anchor.clone(),
        order: i as u64 + 1 }).collect();
    let full = Ledger { sequence: MAX_MARKS as u64, marks, ..Ledger::empty(&f.item) };
    fs::create_dir_all(f.ledger_file().parent().unwrap()).unwrap();
    fs::write(f.ledger_file(), serde_json::to_vec(&full).unwrap()).unwrap();
    let page = f.page();
    assert_eq!(page.total, MAX_MARKS);
    assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_BYTES && page.next_offset.is_some());
    assert_eq!(err(f.put(&page.version, f.mark("over", "red", &id, &[(3, 7)]))), "reader_annotation_limit_exceeded");
    let d = f.put(&page.version, Change::Delete { id: "q00000".into() }).unwrap();
    // Oversized requests reject before any write.
    let many: Vec<_> = (0..MAX_SEGMENTS).map(|i| (i * 11, i * 11 + 3)).collect();
    let mut huge = f.mark(&"i".repeat(256), "red", &id, &many);
    if let Change::Put { annotation } = &mut huge { annotation.anchor.segments.push(Segment { start: 9000, end: 9003 }); }
    assert_eq!(err(f.put(&d.version, huge)), "invalid_reader_annotation");
    for bad in [b"{not json".to_vec(),
        serde_json::to_vec(&Ledger { projection_kind: "chapter".into(), ..Ledger::empty(&f.item) }).unwrap()] {
        fs::write(f.ledger_file(), bad).unwrap();
        assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, PRIV, &f.r)), "reader_annotations_unavailable");
        assert_eq!(err(f.put(&d.version, Change::Delete { id: "q00001".into() })), "reader_annotations_unavailable");
    }
}

#[test]
fn ceiling_and_trash_disclose_nothing() {
    let f = fixture();
    let id = f.ident();
    let v0 = f.page().version;
    let a = f.put(&v0, f.mark("a", "red", &id, &[(0, 3)])).unwrap();
    let low = Sensitivity::Public;
    assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, low, &f.r)), "library_resource_not_found");
    assert_eq!(err(edit(&f.lib, &f.item, &v0, f.mark("a", "red", &id, &[(0, 3)]), "owner", low, &f.r)), "library_resource_not_found");
    assert!(identity(&f.lib, &f.item, low, &f.r).is_err());
    trash_item(&f.lib, &f.item, &f.rev, "owner", None, None).unwrap();
    assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, PRIV, &f.r)), "library_resource_not_found");
    assert_eq!(err(f.put(&v0, f.mark("a", "red", &id, &[(0, 3)]))), "library_resource_not_found");
    assert_eq!(err(f.put(&a.version, Change::Delete { id: "a".into() })), "library_resource_not_found");
}
