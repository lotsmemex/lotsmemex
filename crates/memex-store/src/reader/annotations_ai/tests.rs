use super::*;
use super::super::annotations as edition;
use super::super::chapter_translation as ct;
use memex_core::{BlobDescriptor, VerifiedBlob};

const ORIGINAL: &str = "# One\n\nAlpha text.\n\n# Two\n\nBeta text.\n";
// Repeated "甲🌹乙" and multibyte text in chapter one.
const T1: &str = "# 一\n\n甲🌹乙甲🌹乙。\n\n第二段。\n";
const T2: &str = "# 二\n\n丙丁。\n";
const PRIV: Sensitivity = Sensitivity::Private;

thread_local! {
    static HIDDEN_HISTORY: std::cell::RefCell<BTreeSet<String>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}
/// Revisions whose history read is denied in this test thread (narrowed grant).
pub(super) fn hidden_history(revision: &str) -> bool {
    HIDDEN_HISTORY.with(|h| h.borrow().contains(revision))
}
pub(crate) fn hide_history(revision: &str) {
    HIDDEN_HISTORY.with(|h| h.borrow_mut().insert(revision.into()));
}

struct Fx {
    _dir: tempfile::TempDir,
    lib: PathBuf,
    item: String,
    rev: String,
    ch: [String; 2],
    meta: [Metadata; 2],
}

fn provenance() -> ct::Provenance {
    ct::Provenance {
        route: ct::Route { provider_id: "synthetic".into(), model_id: "fixture".into(), reasoning_effort: None },
        prompt_version: "test-v1".into(),
        prompt_sha256: reader_sha256(b"prompt"),
        plan_sha256: reader_sha256(b"plan"),
    }
}

fn blob(name: &str, media: &str, bytes: Vec<u8>) -> VerifiedBlob {
    let hash = reader_sha256(&bytes);
    VerifiedBlob {
        descriptor: BlobDescriptor {
            schema_version: 1,
            locator: name.into(),
            media_type: media.into(),
            byte_size: bytes.len() as u64,
            sha256: hash.clone(),
            source_url: None,
        },
        blob_id: format!("sha256:{hash}"),
        source_backend_ref: None,
        bytes,
    }
}

/// A private paired book whose two English original chapters each carry a
/// published AI translation with a different version and hash.
fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().to_path_buf();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    capture.capture.sensitivity = PRIV;
    draft.sensitivity = PRIV;
    draft.content_markdown = "另一个版本。".into();
    let mut original = heading_edition(ORIGINAL);
    original.side = "original".into();
    original.language = "en".into();
    let mut translation = heading_edition(&draft.content_markdown);
    translation.side = "translation".into();
    let manifest = BookManifest {
        schema_version: 1,
        alignment: "declared_chapter_order_not_paragraph_alignment".into(),
        editions: vec![original, translation],
    };
    capture.blobs = vec![
        blob("original.md", "text/markdown", ORIGINAL.as_bytes().to_vec()),
        blob("translation.md", "text/markdown", draft.content_markdown.as_bytes().to_vec()),
        blob("manifest.json", BOOK_MANIFEST, serde_json::to_vec(&manifest).unwrap()),
        blob("original.pdf", "application/pdf", b"%PDF-fixture".to_vec()),
        blob("translation.pdf", "application/pdf", b"%PDF-other-fixture".to_vec()),
    ];
    capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
    let receipt = ingest_reader_book(&lib, capture, draft).unwrap();
    let (item, rev) = (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone());
    let doc = reader_document(&lib, &item, &rev, PRIV).unwrap();
    let ch = [doc.editions[0].chapters[0].key.clone(), doc.editions[0].chapters[1].key.clone()];
    let meta = [(0, T1), (1, T2)].map(|(i, t)| {
        let s = ct::source(&lib, &item, &rev, &ch[i], PRIV).unwrap();
        ct::publish(&lib, s.identity, provenance(), t.into(), PRIV).unwrap()
    });
    Fx { _dir: dir, lib, item, rev, ch, meta }
}

impl Fx {
    fn seg(&self, i: usize, start: usize, end: usize) -> Segment {
        Segment {
            chapter: self.ch[i].clone(),
            translation_version: self.meta[i].version.clone(),
            sha256: self.meta[i].sha256.clone(),
            start,
            end,
        }
    }
    fn mark(&self, id: &str, style: &str, segs: Vec<Segment>) -> Change {
        Change::Put { annotation: Annotation { id: id.into(), style: style.into(),
            anchor: Anchor { revision_id: self.rev.clone(), side: SPACE.into(), segments: segs } } }
    }
    fn page(&self) -> AnnotationPage {
        list(&self.lib, &self.item, None, 0, 100, None, None, PRIV).unwrap()
    }
    fn put(&self, version: &str, change: Change) -> Result<EditResult> {
        edit(&self.lib, &self.item, version, change, "owner", PRIV)
    }
    fn projection_file(&self, i: usize) -> PathBuf {
        self.lib.join("catalog/chapter-translations").join(&self.item).join(&self.rev)
            .join(format!("{}.json", reader_sha256(self.ch[i].as_bytes())))
    }
    /// Simulates an effective projection change in place (fixture-only; the
    /// product never replaces a published translation).
    fn rewrite(&self, i: usize, markdown: Option<&str>, prompt_version: &str) {
        let file = self.projection_file(i);
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        if let Some(md) = markdown {
            v["markdown"] = md.into();
            v["sha256"] = reader_sha256(md.as_bytes()).into();
        }
        v["provenance"]["prompt_version"] = prompt_version.into();
        fs::write(file, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    fn ledger_file(&self) -> PathBuf {
        self.lib.join(format!("catalog/reader-ai-annotations/{}.json", self.item))
    }
    fn edition_file(&self) -> PathBuf {
        self.lib.join(format!("catalog/reader-annotations/{}.json", self.item))
    }
}

fn err<T: std::fmt::Debug>(r: Result<T>) -> String {
    r.unwrap_err().to_string()
}

/// "# 一" / "# 二": both translations start with a 5-byte heading run.
const H: usize = 5;

#[test]
fn two_chapters_with_different_versions_round_trip_as_one_atomic_mark() {
    let f = fixture();
    assert_ne!(f.meta[0].version, f.meta[1].version);
    assert_ne!(f.meta[0].sha256, f.meta[1].sha256);
    let v0 = f.page().version;
    let cross = f.mark("x", "yellow", vec![f.seg(0, T1.len() - 4, T1.len()), f.seg(1, 0, T2.len())]);
    let r = f.put(&v0, cross).unwrap();
    assert_eq!((r.outcome.as_str(), r.annotation_space.as_str()), ("created", SPACE));
    let page = f.page();
    assert_eq!((page.total, page.annotation_space.as_str()), (1, SPACE));
    let m = &page.annotations[0];
    assert_eq!(m.status, "current");
    assert_eq!(m.projections.len(), 2);
    for (p, i) in m.projections.iter().zip([0, 1]) {
        assert_eq!((&p.chapter, &p.translation_version, &p.sha256, p.status.as_str()),
            (&f.ch[i], &f.meta[i].version, &f.meta[i].sha256, "current"));
    }
    // Out-of-toc order, overlap, or one failing segment rejects the whole mark.
    for bad in [
        vec![f.seg(1, 0, H), f.seg(0, 0, H)],
        vec![f.seg(0, 0, 10), f.seg(0, 7, 14)],
        vec![f.seg(0, 0, H), f.seg(1, 0, H), f.seg(1, H, T2.len() + 1)],
    ] {
        assert_eq!(err(f.put(&r.version, f.mark("bad", "red", bad))), "invalid_reader_anchor");
    }
    // A chapter that is not an AI translation surface is not a segment target.
    let mut unknown = f.seg(0, 0, H);
    unknown.chapter = "no-such-chapter".into();
    assert_eq!(err(f.put(&r.version, f.mark("bad", "red", vec![unknown]))), "invalid_reader_anchor");
    let after = f.page();
    assert_eq!((after.total, after.version), (1, r.version));
}

#[test]
fn utf8_boundaries_repeated_text_and_chunk_offsets_use_full_translation_coordinates() {
    let f = fixture();
    let v0 = f.page().version;
    let n = "甲🌹乙".len();
    let first = T1.find("甲🌹乙").unwrap();
    let second = first + n + T1[first + n..].find("甲🌹乙").unwrap();
    // A split inside 🌹 (4 bytes) is rejected, never clipped.
    let inside = first + "甲".len() + 1;
    assert_eq!(err(f.put(&v0, f.mark("u", "red", vec![f.seg(0, first, inside)]))), "invalid_reader_anchor");
    // A later chunk's local offset plus chunk.start is the full-translation offset.
    let chunk = ct::chunk(&f.lib, &f.item, &f.rev, &f.ch[0], &f.meta[0].version, second, 64, PRIV).unwrap();
    assert_eq!(chunk.metadata.version, f.meta[0].version);
    let local = chunk.markdown.find("甲🌹乙").unwrap();
    assert_eq!(local, 0);
    let (s, e) = (chunk.start + local, chunk.start + local + n);
    assert_eq!(s, second);
    let a = f.put(&v0, f.mark("a", "red", vec![f.seg(0, first, first + n)])).unwrap();
    let b = f.put(&a.version, f.mark("b", "red", vec![f.seg(0, s, e)])).unwrap();
    // A chunk-local zero would address different text; the first occurrence's range is
    // a duplicate of mark "a", never relocated to the second by substring search.
    assert_ne!(&T1[local..local + n], "甲🌹乙");
    assert_eq!(err(f.put(&b.version, f.mark("c", "red", vec![f.seg(0, first, first + n)]))), "reader_annotation_conflict");
    let ranges: Vec<_> = f.page().annotations.iter().map(|m| (m.anchor.segments[0].start, m.anchor.segments[0].end)).collect();
    assert_eq!(ranges, vec![(first, first + n), (second, second + n)]);
    assert_eq!(&T1[second..second + n], "甲🌹乙");
    // Ranges past the translation (e.g. original-text coordinates) are rejected.
    assert_eq!(err(f.put(&b.version, f.mark("o", "bold", vec![f.seg(1, 0, T2.len() + 1)]))), "invalid_reader_anchor");
}

#[test]
fn one_changed_segment_makes_the_whole_mark_stale_and_delete_only() {
    let f = fixture();
    let v0 = f.page().version;
    let a = f.put(&v0, f.mark("x", "red", vec![f.seg(0, 0, H), f.seg(1, 0, H)])).unwrap();
    let solo = f.put(&a.version, f.mark("solo", "bold", vec![f.seg(0, 0, H)])).unwrap();
    // Chapter two changes hash and version; chapter one is untouched.
    f.rewrite(1, Some("# 二\n\n新的丙丁。\n"), "test-v2");
    let page = f.page();
    let x = page.annotations.iter().find(|m| m.id == "x").unwrap();
    assert_eq!(x.status, "stale");
    assert_eq!(x.projections.iter().map(|p| p.status.as_str()).collect::<Vec<_>>(), vec!["current", "stale"]);
    // The unchanged chapter's mark stays current.
    assert_eq!(page.annotations.iter().find(|m| m.id == "solo").unwrap().status, "current");
    // Stale marks cannot be restyled; current ones can.
    assert_eq!(err(f.put(&solo.version, f.mark("x", "blue", vec![f.seg(0, 0, H), f.seg(1, 0, H)]))), "stale_reader_translation");
    let s = f.put(&solo.version, f.mark("solo", "yellow", vec![f.seg(0, 0, H)])).unwrap();
    assert_eq!(s.outcome, "style_changed");
    // Writing the old identity for the changed chapter is the determinate stale code.
    assert_eq!(err(f.put(&s.version, f.mark("y", "red", vec![f.seg(1, 0, H)]))), "stale_reader_translation");
    assert_eq!(f.put(&s.version, Change::Delete { id: "x".into() }).unwrap().outcome, "deleted");
    assert_eq!(f.page().total, 1);
}

#[test]
fn same_hash_different_version_is_stale_and_hash_alone_never_matches() {
    let f = fixture();
    let v0 = f.page().version;
    let a = f.put(&v0, f.mark("m", "red", vec![f.seg(0, 0, H)])).unwrap();
    f.rewrite(0, None, "test-v2");
    let now = ct::metadata(&f.lib, &f.item, &f.rev, &f.ch[0], PRIV).unwrap().unwrap();
    assert_eq!(now.sha256, f.meta[0].sha256);
    assert_ne!(now.version, f.meta[0].version);
    assert_eq!(f.page().annotations[0].status, "stale");
    // Version race: the client still holds the previous version.
    assert_eq!(err(f.put(&a.version, f.mark("n", "red", vec![f.seg(0, H, H + 2)]))), "stale_reader_translation");
    let mut seg = f.seg(0, H, H + 2);
    seg.translation_version = now.version.clone();
    f.put(&a.version, f.mark("n", "red", vec![seg])).unwrap();
    // Filtering is by the complete chapter+version+sha identity.
    let only = |version: &str| -> Vec<String> {
        list(&f.lib, &f.item, Some(&ProjectionFilter { chapter: f.ch[0].clone(), translation_version: version.into(),
            sha256: now.sha256.clone() }), 0, 100, None, None, PRIV).unwrap().annotations.into_iter().map(|m| m.id).collect()
    };
    assert_eq!(only(&now.version), vec!["n"]);
    assert_eq!(only(&f.meta[0].version), vec!["m"]);
    assert!(only(&"0".repeat(64)).is_empty());
}

#[test]
fn metadata_view_inheritance_binds_the_effective_version_not_the_raw_projection() {
    let f = fixture();
    let v0 = f.page().version;
    let old = f.put(&v0, f.mark("old", "red", vec![f.seg(0, 0, H)])).unwrap();
    let renamed = revise_item_metadata(&f.lib, &f.item, ItemMetadataUpdate {
        expected_revision_id: f.rev.clone(), title: "Renamed synthetic".into(), manual_tags: vec![] }).unwrap();
    let eff = ct::metadata(&f.lib, &f.item, &renamed.revision_id, &f.ch[0], PRIV).unwrap().unwrap();
    assert_ne!(eff.version, f.meta[0].version);
    assert_eq!(eff.sha256, f.meta[0].sha256);
    // The mark keeps its bound identity and stays current after a metadata-only revision.
    assert_eq!(f.page().annotations[0].status, "current");
    let anchor = |version: &str| Change::Put { annotation: Annotation { id: "new".into(), style: "red".into(),
        anchor: Anchor { revision_id: renamed.revision_id.clone(), side: SPACE.into(), segments: vec![Segment {
            chapter: f.ch[0].clone(), translation_version: version.into(), sha256: eff.sha256.clone(), start: 0, end: H }] } } };
    // The raw inherited projection version is not the effective identity.
    assert_eq!(err(f.put(&old.version, anchor(&f.meta[0].version))), "stale_reader_translation");
    f.put(&old.version, anchor(&eff.version)).unwrap();
    let page = f.page();
    assert_eq!(page.annotations.iter().map(|m| (m.id.as_str(), m.status.as_str())).collect::<Vec<_>>(),
        vec![("new", "current"), ("old", "current")]);
    // Old-revision anchors are rejected for new marks; the inherited mark is still deletable.
    assert_eq!(err(f.put(&page.version, f.mark("again", "red", vec![f.seg(0, 0, H)]))), "stale_item_revision");
    f.put(&page.version, Change::Delete { id: "old".into() }).unwrap();
}

#[test]
fn two_device_cas_exact_replay_unknown_reconcile_and_aba() {
    let f = fixture();
    let v0 = f.page().version;
    let a = f.mark("a", "red", vec![f.seg(0, 0, H)]);
    let b = f.mark("b", "bold", vec![f.seg(1, 0, H)]);
    // Device A commits but its response is lost (e.g. internal_error after rename).
    let ra = edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV).unwrap();
    // Device B still holds v0: determinate stale, never a merge.
    assert_eq!(err(edit(&f.lib, &f.item, &v0, b.clone(), "device-b", PRIV)), "stale_reader_annotations");
    // Device A's exact retry returns the committed receipt without a new version.
    assert_eq!(edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV).unwrap(), ra);
    assert_eq!(f.page().version, ra.version);
    // The same change from another principal is not a replay.
    assert_eq!(err(edit(&f.lib, &f.item, &v0, a.clone(), "device-b", PRIV)), "stale_reader_annotations");
    // Reconcile by list: the attempted id is present at the receipt version.
    let page = f.page();
    assert!(page.version == ra.version && page.annotations.iter().any(|m| m.id == "a"));
    let rb = edit(&f.lib, &f.item, &ra.version, b, "device-b", PRIV).unwrap();
    // After an intervening edit the old attempt is stale, not replayed.
    assert_eq!(err(edit(&f.lib, &f.item, &v0, a.clone(), "device-a", PRIV)), "stale_reader_annotations");
    // put/delete/put ABA never repeats a version or resurrects through an old delete.
    let d = f.put(&rb.version, Change::Delete { id: "a".into() }).unwrap();
    let back = f.put(&d.version, a).unwrap();
    assert_ne!(back.version, rb.version);
    assert_eq!(err(f.put(&rb.version, Change::Delete { id: "a".into() })), "stale_reader_annotations");
    // Restoring an older ledger never lets a later version pass CAS.
    let backup = fs::read(f.ledger_file()).unwrap();
    let later = f.put(&back.version, f.mark("l", "blue", vec![f.seg(1, 0, H)])).unwrap();
    fs::write(f.ledger_file(), &backup).unwrap();
    let other = f.put(&back.version, f.mark("o", "yellow", vec![f.seg(1, 0, H)])).unwrap();
    assert_ne!(other.version, later.version);
    assert_eq!(err(f.put(&later.version, Change::Delete { id: "l".into() })), "stale_reader_annotations");
}

#[test]
fn same_id_is_isolated_across_namespaces_and_edition_ledger_bytes_never_change() {
    let f = fixture();
    let doc = reader_document(&f.lib, &f.item, &f.rev, PRIV).unwrap();
    let e = &doc.editions[0];
    let ev0 = edition::list(&f.lib, &f.item, 0, 100, None, PRIV).unwrap().version;
    let ed = edition::edit(&f.lib, &f.item, &ev0, edition::Change::Put { annotation: edition::Annotation {
        id: "m".into(), style: "red".into(), anchor: edition::AnnotationAnchor { revision_id: f.rev.clone(),
            side: e.side.clone(), sha256: e.sha256.clone(),
            segments: vec![edition::Segment { chapter: e.chapters[0].key.clone(), start: 0, end: 3 }] } } }, "owner", PRIV).unwrap();
    let edition_bytes = fs::read(f.edition_file()).unwrap();
    let av0 = f.page().version;
    assert_ne!(av0, ed.version);
    assert_ne!(av0, ev0);
    // An edition version never passes AI CAS and vice versa.
    assert_eq!(err(f.put(&ed.version, f.mark("m", "red", vec![f.seg(0, 0, H)]))), "stale_reader_annotations");
    let ai = f.put(&av0, f.mark("m", "red", vec![f.seg(0, 0, H)])).unwrap();
    assert_eq!(err(edition::edit(&f.lib, &f.item, &ai.version, edition::Change::Delete { id: "m".into() }, "owner", PRIV)),
        "stale_reader_annotations");
    // Each ledger accepts only its own anchor shape.
    let edition_shaped = serde_json::json!({"op":"put","annotation":{"id":"z","style":"red","anchor":{
        "revision_id": f.rev, "side": "original", "sha256": e.sha256, "segments":[{"chapter": e.chapters[0].key, "start":0, "end":3}]}}});
    assert!(serde_json::from_value::<Change>(edition_shaped).is_err());
    let ai_shaped = serde_json::to_value(f.mark("z", "red", vec![f.seg(0, 0, H)])).unwrap();
    assert!(serde_json::from_value::<edition::Change>(ai_shaped).is_err());
    let mut wrong_side = f.mark("z", "red", vec![f.seg(0, 0, H)]);
    if let Change::Put { annotation } = &mut wrong_side { annotation.anchor.side = "translation".into(); }
    assert_eq!(err(f.put(&ai.version, wrong_side)), "invalid_reader_annotation");
    // Deleting AI "m" leaves edition "m" and the edition ledger bytes untouched.
    f.put(&ai.version, Change::Delete { id: "m".into() }).unwrap();
    assert!(f.page().annotations.is_empty());
    assert_eq!(fs::read(f.edition_file()).unwrap(), edition_bytes);
    let old = edition::list(&f.lib, &f.item, 0, 100, None, PRIV).unwrap();
    assert_eq!((old.version.as_str(), old.annotations[0].id.as_str()), (ed.version.as_str(), "m"));
    // Edition writes leave the AI ledger byte-identical.
    let ai_bytes = fs::read(f.ledger_file()).unwrap();
    edition::edit(&f.lib, &f.item, &ed.version, edition::Change::Delete { id: "m".into() }, "owner", PRIV).unwrap();
    assert_eq!(fs::read(f.ledger_file()).unwrap(), ai_bytes);
}

#[test]
fn view_version_binds_pages_to_translation_and_visibility() {
    let f = fixture();
    let mut v = f.page().version;
    for (i, (s, e)) in [(0, H), (H, 7), (7, 10)].into_iter().enumerate() {
        v = f.put(&v, f.mark(&format!("p{i}"), "red", vec![f.seg(0, s, e)])).unwrap().version;
    }
    let first = list(&f.lib, &f.item, None, 0, 2, None, None, PRIV).unwrap();
    assert_eq!((first.total, first.next_offset), (3, Some(2)));
    // Later pages need both versions.
    assert_eq!(err(list(&f.lib, &f.item, None, 2, 2, Some(&first.version), None, PRIV)), "invalid_reader_annotation");
    let second = list(&f.lib, &f.item, None, 2, 2, Some(&first.version), Some(&first.view_version), PRIV).unwrap();
    assert_eq!((second.annotations.len(), second.next_offset), (1, None));
    // A translation change keeps the ledger version but moves the view version.
    f.rewrite(0, None, "test-v2");
    let again = list(&f.lib, &f.item, None, 0, 2, None, None, PRIV).unwrap();
    assert_eq!(again.version, first.version);
    assert_ne!(again.view_version, first.view_version);
    assert_eq!(err(list(&f.lib, &f.item, None, 2, 2, Some(&first.version), Some(&first.view_version), PRIV)),
        "stale_reader_annotations");
}

#[test]
fn quota_full_still_deletes_and_corrupt_ledger_fails_closed() {
    let f = fixture();
    let marks: Vec<Stored> = (0..MAX_MARKS).map(|i| Stored { id: format!("q{i:05}"), style: STYLES[i % 5].into(),
        anchor: Anchor { revision_id: f.rev.clone(), side: SPACE.into(), segments: vec![f.seg(0, 0, H)] },
        order: i as u64 + 1 }).collect();
    let full = Ledger { sequence: MAX_MARKS as u64, marks, ..Ledger::empty(&f.item) };
    fs::create_dir_all(f.ledger_file().parent().unwrap()).unwrap();
    fs::write(f.ledger_file(), serde_json::to_vec(&full).unwrap()).unwrap();
    let page = list(&f.lib, &f.item, None, 0, 100, None, None, PRIV).unwrap();
    assert_eq!(page.total, MAX_MARKS);
    assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_BYTES);
    assert!(page.next_offset.is_some());
    let v = page.version;
    assert_eq!(err(f.put(&v, f.mark("over", "red", vec![f.seg(1, 0, H)]))), "reader_annotation_limit_exceeded");
    let d = f.put(&v, Change::Delete { id: "q00000".into() }).unwrap();
    assert_eq!(f.page().total, MAX_MARKS - 1);
    // Corrupt or foreign-namespace ledgers fail closed, never as empty.
    for bad in [b"{not json".to_vec(),
        serde_json::to_vec(&Ledger { annotation_space: "original".into(), ..Ledger::empty(&f.item) }).unwrap()] {
        fs::write(f.ledger_file(), bad).unwrap();
        assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, PRIV)), "reader_annotations_unavailable");
        assert_eq!(err(f.put(&d.version, Change::Delete { id: "q00001".into() })), "reader_annotations_unavailable");
    }
}

#[test]
fn hidden_trashed_and_corrupt_projection_paths_disclose_nothing() {
    let f = fixture();
    let v0 = f.page().version;
    let a = f.put(&v0, f.mark("a", "red", vec![f.seg(0, 0, H)])).unwrap();
    // A lower ceiling sees neither marks, counts nor the replayable receipt.
    assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, Sensitivity::Public)), "library_resource_not_found");
    assert_eq!(err(edit(&f.lib, &f.item, &v0, f.mark("a", "red", vec![f.seg(0, 0, H)]), "owner", Sensitivity::Public)),
        "library_resource_not_found");
    // A corrupt projection fails closed, not as "no translation" or "stale".
    let file = f.projection_file(1);
    let good = fs::read(&file).unwrap();
    fs::write(&file, b"{}").unwrap();
    let e = err(f.put(&a.version, f.mark("b", "red", vec![f.seg(1, 0, H)])));
    assert!(!["stale_reader_translation", "reader_translation_unavailable", "invalid_reader_anchor"].contains(&e.as_str()), "{e}");
    fs::write(&file, &good).unwrap();
    let b = f.put(&a.version, f.mark("b", "red", vec![f.seg(1, 0, H)])).unwrap();
    // A missing (not corrupt) projection: the mark is stale, listed without excerpt and deletable.
    fs::remove_file(&file).unwrap();
    assert_eq!(f.page().annotations.iter().find(|m| m.id == "b").unwrap().status, "stale");
    let d = f.put(&b.version, Change::Delete { id: "b".into() }).unwrap();
    // Trash hides everything, including replay of the last receipt.
    trash_item(&f.lib, &f.item, &f.rev, "owner", None, None).unwrap();
    assert_eq!(err(list(&f.lib, &f.item, None, 0, 10, None, None, PRIV)), "library_resource_not_found");
    assert_eq!(err(f.put(&b.version, Change::Delete { id: "b".into() })), "library_resource_not_found");
    assert_eq!(err(f.put(&d.version, f.mark("c", "red", vec![f.seg(0, 0, H)]))), "library_resource_not_found");
}

/// Copies `items/<item>` so a test can restore the item to an earlier verified chain
/// while the AI ledger stays newer (a per-component restore): the current item stays
/// readable but revisions created after the snapshot leave its history.
fn snapshot_item(f: &Fx) -> tempfile::TempDir {
    let to = tempfile::tempdir().unwrap();
    copy_dir(&f.lib.join("items").join(&f.item), to.path());
    to
}
fn restore_item(f: &Fx, from: &Path) {
    let dir = f.lib.join("items").join(&f.item);
    fs::remove_dir_all(&dir).unwrap();
    copy_dir(from, &dir);
}
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() { copy_dir(&e.path(), &target) } else { fs::copy(e.path(), target).unwrap(); }
    }
}

#[test]
fn exact_replay_requires_the_bound_history_to_still_be_readable() {
    let f = fixture();
    let snapshot = snapshot_item(&f);
    let r2 = revise_item_metadata(&f.lib, &f.item, ItemMetadataUpdate {
        expected_revision_id: f.rev.clone(), title: "Renamed synthetic".into(), manual_tags: vec![] }).unwrap();
    let eff = ct::metadata(&f.lib, &f.item, &r2.revision_id, &f.ch[0], PRIV).unwrap().unwrap();
    let put = Change::Put { annotation: Annotation { id: "h".into(), style: "red".into(),
        anchor: Anchor { revision_id: r2.revision_id.clone(), side: SPACE.into(), segments: vec![Segment {
            chapter: f.ch[0].clone(), translation_version: eff.version.clone(), sha256: eff.sha256.clone(), start: 0, end: H }] } } };
    let created = f.put(&f.page().version, put).unwrap();
    let delete = Change::Delete { id: "h".into() };
    let deleted = f.put(&created.version, delete.clone()).unwrap();
    // Control: while the bound history is readable, the lost reply replays exactly, no new write.
    let bytes = fs::read(f.ledger_file()).unwrap();
    assert_eq!(f.put(&created.version, delete.clone()).unwrap(), deleted);
    assert_eq!(fs::read(f.ledger_file()).unwrap(), bytes);
    // The item is restored to its earlier chain: current R1 is readable, R2 is no longer history.
    restore_item(&f, snapshot.path());
    assert_eq!(read_item_at_sensitivity(&f.lib, &f.item, PRIV).unwrap().unwrap().1.revision_id, f.rev);
    assert!(revision_text::historical_revision(&f.lib, &f.item, &r2.revision_id, PRIV).is_err());
    // The same principal's exact replay must not disclose the deletion receipt (id/outcome/version).
    assert_eq!(err(f.put(&created.version, delete)), "stale_reader_annotations");
    assert_eq!(fs::read(f.ledger_file()).unwrap(), bytes, "no second write");
}

#[test]
fn narrowed_history_access_stales_old_pages_hides_marks_and_blocks_receipt_replay() {
    let f = fixture();
    // Two stale marks bound to R1 (and a third used for the receipt), then R2 is current.
    let mut v = f.page().version;
    for (id, s, e) in [("a1", 0, H), ("a2", H, 7), ("a3", 7, 10)] {
        v = f.put(&v, f.mark(id, "red", vec![f.seg(0, s, e)])).unwrap().version;
    }
    let r2 = revise_item_metadata(&f.lib, &f.item, ItemMetadataUpdate {
        expected_revision_id: f.rev.clone(), title: "Renamed synthetic".into(), manual_tags: vec![] }).unwrap();
    let eff = ct::metadata(&f.lib, &f.item, &r2.revision_id, &f.ch[0], PRIV).unwrap().unwrap();
    let current = |id: &str, s: usize, e: usize| Change::Put { annotation: Annotation { id: id.into(), style: "bold".into(),
        anchor: Anchor { revision_id: r2.revision_id.clone(), side: SPACE.into(), segments: vec![Segment {
            chapter: f.ch[0].clone(), translation_version: eff.version.clone(), sha256: eff.sha256.clone(), start: s, end: e }] } } };
    v = f.put(&v, current("b1", 0, H)).unwrap().version;
    v = f.put(&v, current("b2", H, 7)).unwrap().version;
    // Receipt: deleting the R1-bound "a3" is the ledger's last write.
    let delete = Change::Delete { id: "a3".into() };
    let deleted = f.put(&v, delete.clone()).unwrap();
    let page = |offset, ev: Option<&str>, vv: Option<&str>| list(&f.lib, &f.item, None, offset, 2, ev, vv, PRIV);
    let first = page(0, None, None).unwrap();
    assert_eq!((first.total, first.next_offset), (4, Some(2)));
    let ids = |p: &AnnotationPage| p.annotations.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&first), ["a1", "a2"]);
    // Control: access unchanged, the original view continues and the receipt replays exactly.
    assert_eq!(ids(&page(2, Some(&first.version), Some(&first.view_version)).unwrap()), ["b1", "b2"]);
    let bytes = fs::read(f.ledger_file()).unwrap();
    assert_eq!(f.put(&v, delete.clone()).unwrap(), deleted);
    // History access to R1 narrows; current revision, projections and ledger are unchanged.
    hide_history(&f.rev);
    assert_eq!(ct::metadata(&f.lib, &f.item, &r2.revision_id, &f.ch[0], PRIV).unwrap().unwrap(), eff);
    let fresh = page(0, None, None).unwrap();
    assert_eq!(fresh.version, first.version);
    // The old view's second page is stale, never a mixed page.
    assert_eq!(err(page(2, Some(&first.version), Some(&first.view_version))), "stale_reader_annotations");
    // A new first page discloses neither hidden marks nor their count.
    assert_eq!((ids(&fresh), fresh.total, fresh.next_offset), (vec!["b1".to_string(), "b2".to_string()], 2, None));
    assert_ne!(fresh.view_version, first.view_version);
    // The receipt of a mark bound to now-unreadable history is not disclosed by replay.
    assert_eq!(err(f.put(&v, delete)), "stale_reader_annotations");
    // Hidden marks cannot be deleted either; visible ones still can.
    assert_eq!(err(f.put(&fresh.version, Change::Delete { id: "a1".into() })), "library_resource_not_found");
    f.put(&fresh.version, Change::Delete { id: "b1".into() }).unwrap();
    assert_ne!(fs::read(f.ledger_file()).unwrap(), bytes);
}

#[test]
fn receipt_without_bound_history_still_loads_but_never_replays() {
    let f = fixture();
    let v0 = f.page().version;
    let put = f.mark("m", "red", vec![f.seg(0, 0, H)]);
    let created = f.put(&v0, put.clone()).unwrap();
    assert_eq!(f.put(&v0, put.clone()).unwrap(), created);
    // A pre-fix ledger: same receipt without `bound_revision_id`.
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(f.ledger_file()).unwrap()).unwrap();
    assert!(v["last"].as_object_mut().unwrap().remove("bound_revision_id").is_some());
    fs::write(f.ledger_file(), serde_json::to_vec(&v).unwrap()).unwrap();
    let page = f.page();
    assert_eq!((page.version, page.total), (created.version.clone(), 1));
    assert_eq!(err(f.put(&v0, put)), "stale_reader_annotations");
    // The next write records the binding again.
    f.put(&created.version, Change::Delete { id: "m".into() }).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&fs::read(f.ledger_file()).unwrap()).unwrap();
    assert_eq!(v["last"]["bound_revision_id"], f.rev.as_str());
}
