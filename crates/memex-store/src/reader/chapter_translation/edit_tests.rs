use super::*;
use super::super::super::annotations_ai as ai;
use memex_core::{BlobDescriptor, VerifiedBlob};

const ORIGINAL: &str = "# One\n\nAlpha text.\n\nMore alpha.\n\n# Two\n\nBeta text.\n";
const PRIV: Sensitivity = Sensitivity::Private;
const P: &str = "local_owner";

struct Fx {
    _dir: tempfile::TempDir,
    lib: PathBuf,
    item: String,
    rev: String,
    ch: Vec<String>,
}

fn provenance() -> Provenance {
    Provenance {
        route: Route { provider_id: "synthetic".into(), model_id: "fixture".into(), reasoning_effort: None },
        prompt_version: "test-v1".into(),
        prompt_sha256: reader_sha256(b"prompt"),
        plan_sha256: reader_sha256(b"plan"),
    }
}

/// Mapped target with one generated part per planned block (separator on the left part).
fn mapped(source: &str, tag: &str) -> (String, Vec<TranslationBlock>) {
    let planned = paragraphs::blocks(source);
    let (mut md, mut blocks) = (String::new(), vec![]);
    for (i, (range, kind)) in planned.iter().enumerate() {
        let start = md.len();
        md.push_str(&format!("{tag}段{i}🌹译文"));
        if i + 1 < planned.len() {
            md.push_str("\n\n");
        }
        blocks.push(TranslationBlock { id: format!("block-{i}"), source_range: *range, target_range: [start, md.len()], kind: *kind });
    }
    (md, blocks)
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
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
    let manifest = BookManifest { schema_version: 1, alignment: "declared_chapter_order_not_paragraph_alignment".into(), editions: vec![original, translation] };
    capture.blobs = [
        ("original.md", "text/markdown", ORIGINAL.as_bytes().to_vec()),
        ("translation.md", "text/markdown", draft.content_markdown.as_bytes().to_vec()),
        ("manifest.json", BOOK_MANIFEST, serde_json::to_vec(&manifest).unwrap()),
    ]
    .into_iter()
    .map(|(name, media, bytes)| {
        let hash = reader_sha256(&bytes);
        VerifiedBlob {
            descriptor: BlobDescriptor { schema_version: 1, locator: name.into(), media_type: media.into(), byte_size: bytes.len() as u64, sha256: hash.clone(), source_url: None },
            blob_id: format!("sha256:{hash}"),
            source_backend_ref: None,
            bytes,
        }
    })
    .collect();
    capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
    let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
    let (item, rev) = (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone());
    let doc = reader_document(dir.path(), &item, &rev, PRIV).unwrap();
    let ch: Vec<String> = doc.editions[0].chapters.iter().map(|c| c.key.clone()).collect();
    assert_eq!(ch.len(), 2);
    let lib = dir.path().to_path_buf();
    for (i, c) in ch.iter().enumerate() {
        let s = super::super::source(&lib, &item, &rev, c, PRIV).unwrap();
        let (md, blocks) = mapped(&s.markdown, &format!("章{i}"));
        publish_mapped(&lib, s.identity, provenance(), md, blocks, PRIV).unwrap();
    }
    Fx { _dir: dir, lib, item, rev, ch }
}

impl Fx {
    fn meta(&self, rev: &str, ch: usize) -> Metadata {
        metadata(&self.lib, &self.item, rev, &self.ch[ch], PRIV).unwrap().unwrap()
    }
    fn body(&self, rev: &str, ch: usize) -> String {
        let m = self.meta(rev, ch);
        super::super::chunk(&self.lib, &self.item, rev, &self.ch[ch], &m.version, 0, READER_CHUNK_BYTES, PRIV).unwrap().markdown
    }
    fn begin(&self, rev: &str, ch: usize, base: &Metadata, body: &str, request: &str, principal: &str) -> Result<Upload> {
        begin(&self.lib, Begin {
            item_id: self.item.clone(), revision_id: rev.into(), chapter: self.ch[ch].clone(),
            expected_translation_version: base.version.clone(), expected_translation_sha256: base.sha256.clone(),
            request_id: request.into(), bytes: body.len(), sha256: reader_sha256(body.as_bytes()),
        }, principal, PRIV)
    }
    fn upload(&self, edit: &str, body: &str, principal: &str) -> Result<()> {
        let mut off = 0;
        while off < body.len() {
            let mut end = (off + MAX_CHUNK).min(body.len());
            while !body.is_char_boundary(end) { end -= 1; }
            chunk(&self.lib, &self.item, edit, off, &body[off..end], principal, PRIV)?;
            off = end;
        }
        Ok(())
    }
    fn save(&self, rev: &str, ch: usize, base: &Metadata, body: &str, request: &str) -> Result<Receipt> {
        let up = self.begin(rev, ch, base, body, request, P)?;
        if let Some(r) = up.receipt { return Ok(r) }
        self.upload(&up.edit_id, body, P)?;
        commit(&self.lib, &self.item, &up.edit_id, P, PRIV)
    }
    fn page(&self, rev: &str, ch: usize) -> AlignmentPage {
        let m = self.meta(rev, ch);
        alignment(&self.lib, &self.item, rev, &self.ch[ch], &m.version, 0, 256, PRIV).unwrap()
    }
    fn files(&self, sub: &str) -> usize {
        fn walk(p: &Path) -> usize {
            fs::read_dir(p).map(|d| d.flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { 1 }).sum()).unwrap_or(0)
        }
        walk(&self.lib.join("catalog").join(sub))
    }
    fn ai_seg(&self, rev: &str, ch: usize, start: usize, end: usize) -> ai::Segment {
        let m = self.meta(rev, ch);
        ai::Segment { chapter: self.ch[ch].clone(), translation_version: m.version, sha256: m.sha256, start, end }
    }
    fn mark(&self, id: &str, rev: &str, segments: Vec<ai::Segment>) {
        let v = ai::list(&self.lib, &self.item, None, 0, 100, None, None, PRIV).unwrap().version;
        ai::edit(&self.lib, &self.item, &v, ai::Change::Put { annotation: ai::Annotation {
            id: id.into(), style: "yellow".into(),
            anchor: ai::Anchor { revision_id: rev.into(), side: ai::SPACE.into(), segments },
        } }, "a".repeat(64).as_str(), PRIV).unwrap();
    }
}

#[test]
fn owner_edit_is_immutable_attributed_and_noop_writes_nothing() {
    let fx = fixture();
    let ai0 = fx.meta(&fx.rev, 0);
    let ai_file = path(&fx.lib, &source_locked(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap().identity);
    let ai_bytes = fs::read(&ai_file).unwrap();
    let old = fx.body(&fx.rev, 0);
    // Identical bytes: no version, no head, unchanged marks.
    let same = fx.save(&fx.rev, 0, &ai0, &old, "noop").unwrap();
    assert!(!same.changed && same.translation_version == ai0.version);
    assert_eq!(fx.files("chapter-translation-edits"), 0);
    assert_eq!(fx.meta(&fx.rev, 0), ai0);
    let next = old.replace("章0段1", "人工改写");
    let r = fx.save(&fx.rev, 0, &ai0, &next, "edit-1").unwrap();
    assert!(r.changed && r.previous_version == ai0.version && r.sha256 == reader_sha256(next.as_bytes()));
    let m = fx.meta(&fx.rev, 0);
    assert_eq!(m.version, r.translation_version);
    assert_eq!(m.provenance, "owner_edit");
    assert_eq!(m.route, ai0.route); // generation chain kept, not claimed as model output
    let o = m.owner_edit.as_ref().unwrap();
    assert_eq!((o.edit_of_version.as_str(), o.edit_of_kind.as_str(), o.depth), (ai0.version.as_str(), "machine_translation", 1));
    assert_eq!(o.machine_origin_sha256, ai0.version);
    assert_eq!(fx.body(&fx.rev, 0), next);
    // AI base never overwritten; other chapter, original body and edition unchanged.
    assert_eq!(fs::read(&ai_file).unwrap(), ai_bytes);
    assert_eq!(fx.meta(&fx.rev, 1).provenance, "machine_translation");
    assert_eq!(reader_document(&fx.lib, &fx.item, &fx.rev, PRIV).unwrap().revision_id, fx.rev);
    let json = serde_json::to_value(&ai0).unwrap();
    assert!(json.get("owner_edit").is_none(), "AI metadata wire unchanged");
    // A body hash is never a CAS substitute; the old opaque AI version is stale now.
    let mut fake = ai0.clone();
    fake.version = ai0.sha256.clone();
    assert_eq!(fx.begin(&fx.rev, 0, &fake, "x", "bad", P).unwrap_err().to_string(), "stale_reader_translation");
    assert_eq!(fx.begin(&fx.rev, 0, &ai0, "x", "late", P).unwrap_err().to_string(), "stale_reader_translation");
}

#[test]
fn two_device_cas_and_same_hash_different_version() {
    let fx = fixture();
    let ai0 = fx.meta(&fx.rev, 0);
    let old = fx.body(&fx.rev, 0);
    let (a, b) = (old.replace("章0段0", "甲设备"), old.replace("章0段0", "乙设备"));
    let ua = fx.begin(&fx.rev, 0, &ai0, &a, "dev-a", P).unwrap();
    let ub = fx.begin(&fx.rev, 0, &ai0, &b, "dev-b", P).unwrap();
    fx.upload(&ua.edit_id, &a, P).unwrap();
    fx.upload(&ub.edit_id, &b, P).unwrap();
    let ra = commit(&fx.lib, &fx.item, &ua.edit_id, P, PRIV).unwrap();
    assert_eq!(commit(&fx.lib, &fx.item, &ub.edit_id, P, PRIV).unwrap_err().to_string(), "stale_reader_translation");
    assert_eq!(fx.body(&fx.rev, 0), a);
    // Revert to the AI bytes: same hash as AI, a NEW version (no ABA).
    let back = fx.save(&fx.rev, 0, &fx.meta(&fx.rev, 0), &old, "revert").unwrap();
    assert!(back.changed && back.sha256 == ai0.sha256 && back.translation_version != ai0.version);
    assert_eq!(fx.meta(&fx.rev, 0).owner_edit.unwrap().depth, 2);
    assert_eq!(fx.begin(&fx.rev, 0, &ai0, &a, "aba", P).unwrap_err().to_string(), "stale_reader_translation");
    // Exact replay of device A still returns its own receipt after later edits.
    assert_eq!(commit(&fx.lib, &fx.item, &ua.edit_id, P, PRIV).unwrap(), ra);
    assert_eq!(fx.begin(&fx.rev, 0, &ai0, &a, "dev-a", P).unwrap().receipt.unwrap(), ra);
    // Same request id, different payload: conflict, not a new edit.
    assert_eq!(fx.begin(&fx.rev, 0, &ai0, &b, "dev-a", P).unwrap_err().to_string(), "request_id_conflict");
    // Another principal cannot use or see this stage.
    assert_eq!(commit(&fx.lib, &fx.item, &ua.edit_id, &"b".repeat(64), PRIV).unwrap_err().to_string(), "reader_translation_edit_not_found");
}

#[test]
fn chained_history_is_read_only_and_authorized() {
    let fx = fixture();
    let ai0 = fx.meta(&fx.rev, 0);
    let v1 = fx.body(&fx.rev, 0).replace("章0段0", "第一次");
    fx.save(&fx.rev, 0, &ai0, &v1, "h1").unwrap();
    let m1 = fx.meta(&fx.rev, 0);
    let v2 = v1.replace("第一次", "第二次");
    fx.save(&fx.rev, 0, &m1, &v2, "h2").unwrap();
    let m2 = fx.meta(&fx.rev, 0);
    let h = history(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m2.version, PRIV).unwrap();
    let kinds: Vec<_> = h.entries.iter().map(|e| (e.kind.as_str(), e.depth)).collect();
    assert_eq!(kinds, [("owner_edit", 2), ("owner_edit", 1), ("machine_translation", 0)]);
    assert!(!h.truncated && h.entries[0].artifact_sha256 == m2.version && h.entries[2].artifact_sha256 == ai0.version);
    let c = version_chunk(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m2.version, &ai0.version, 0, 4096, PRIV).unwrap();
    assert_eq!((c.kind.as_str(), c.sha256.as_str()), ("machine_translation", ai0.sha256.as_str()));
    assert!(history(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m1.version, PRIV).is_err(), "stale view rejected");
    assert!(history(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m2.version, Sensitivity::Public).is_err());
    assert_eq!(version_chunk(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m2.version, &"0".repeat(64), 0, 64, PRIV).unwrap_err().to_string(), "reader_translation_edit_not_found");
}

#[test]
fn inherited_version_edit_late_worker_and_source_change() {
    let fx = fixture();
    let renamed = revise_item_metadata(&fx.lib, &fx.item, ItemMetadataUpdate {
        expected_revision_id: fx.rev.clone(), title: "Renamed".into(), manual_tags: vec![] }).unwrap().revision_id;
    let inherited = fx.meta(&renamed, 0);
    assert_eq!(inherited.provenance, "machine_translation");
    let next = fx.body(&renamed, 0).replace("章0段0", "继承后编辑");
    let r = fx.save(&renamed, 0, &inherited, &next, "inh").unwrap();
    let m = fx.meta(&renamed, 0);
    assert_eq!((m.version.as_str(), m.provenance.as_str()), (r.translation_version.as_str(), "owner_edit"));
    assert_eq!(m.owner_edit.as_ref().unwrap().edit_of_version, inherited.version);
    // A late AI worker at the new revision cannot shadow the human version.
    let s = source_locked(&fx.lib, &fx.item, &renamed, &fx.ch[0], PRIV).unwrap();
    let (md, blocks) = mapped(&s.markdown, "迟到");
    assert_eq!(publish_mapped(&fx.lib, s.identity.clone(), provenance(), md, blocks, PRIV).unwrap_err().to_string(), "stale_reader_translation");
    assert!(!path(&fx.lib, &s.identity).exists());
    assert_eq!(fx.meta(&renamed, 0), m);
    // A later metadata-only revision inherits the owner edit with a bound view version.
    let again = revise_item_metadata(&fx.lib, &fx.item, ItemMetadataUpdate {
        expected_revision_id: renamed.clone(), title: "Again".into(), manual_tags: vec![] }).unwrap().revision_id;
    let view = fx.meta(&again, 0);
    assert!(view.provenance == "owner_edit" && view.version != m.version && view.sha256 == m.sha256);
    // Original changes concurrently (owner chapter edit of the source): commit is stale.
    let pending = next.replace("继承后编辑", "并发");
    let up = fx.begin(&again, 0, &view, &pending, "race", P).unwrap();
    fx.upload(&up.edit_id, &pending, P).unwrap();
    let src = source_locked(&fx.lib, &fx.item, &again, &fx.ch[0], PRIV).unwrap().markdown;
    let changed = src.replace("Alpha", "Gamma");
    let e = chapter_edit::begin(&fx.lib, chapter_edit::Begin { item_id: fx.item.clone(), revision_id: again.clone(),
        side: "original".into(), chapter: fx.ch[0].clone(), expected_chapter_sha256: reader_sha256(src.as_bytes()),
        request_id: "src".into(), bytes: changed.len(), sha256: reader_sha256(changed.as_bytes()) }, P, PRIV).unwrap();
    chapter_edit::chunk(&fx.lib, &fx.item, &e.edit_id, 0, &changed, P, PRIV).unwrap();
    let after = chapter_edit::commit(&fx.lib, &fx.item, &e.edit_id, P, PRIV).unwrap().revision_id;
    assert_eq!(commit(&fx.lib, &fx.item, &up.edit_id, P, PRIV).unwrap_err().to_string(), "stale_item_revision");
    // Changed source loses the translation; unchanged chapter two still inherits AI.
    assert!(metadata(&fx.lib, &fx.item, &after, &fx.ch[0], PRIV).unwrap().is_none());
    assert_eq!(fx.meta(&after, 1).provenance, "machine_translation");
}

#[test]
fn proven_block_bindings_only_and_no_fabricated_rows() {
    let fx = fixture();
    let before = fx.page(&fx.rev, 0);
    assert_eq!((before.status.as_str(), before.total), ("aligned", 3));
    let old = fx.body(&fx.rev, 0);
    let next = old.replace("章0段2🌹译文", "最后一段被人工改写，长度不同");
    fx.save(&fx.rev, 0, &fx.meta(&fx.rev, 0), &next, "map").unwrap();
    let p = fx.page(&fx.rev, 0);
    assert_eq!((p.status.as_str(), p.total), ("partial", 3));
    assert_eq!(p.pairs[..2].iter().map(|r| r.t).collect::<Vec<_>>(), before.pairs[..2].iter().map(|r| r.t).collect::<Vec<_>>());
    assert_eq!(p.pairs[0].relation, paragraphs::Relation::Matched);
    assert_eq!((p.pairs[2].t, p.pairs[2].relation), (None, paragraphs::Relation::Unresolved));
    assert_eq!(p.pairs[2].o, before.pairs[2].o, "source side stays addressable");
    assert_eq!(p.translation_sha256, reader_sha256(next.as_bytes()));
    assert_eq!(fx.meta(&fx.rev, 0).owner_edit.unwrap().alignment_status, "partial");
    // Rewriting everything (same block count) keeps no row: counts never prove binding.
    let all = next.replace("章0段0", "全改").replace("章0段1", "全改二").replace("最后", "末");
    fx.save(&fx.rev, 0, &fx.meta(&fx.rev, 0), &all, "map2").unwrap();
    let p = fx.page(&fx.rev, 0);
    assert_eq!(p.status, "none");
    assert!(p.pairs.iter().all(|r| r.t.is_none()));
    // A retrospective map job can never bind an owner-edited body.
    let m = fx.meta(&fx.rev, 0);
    assert_eq!(chapter_translation_map::source(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], &m.version, PRIV).unwrap().recorded_map, true);
}

#[test]
fn edit_preview_counts_visible_current_marks_and_edit_stales_them() {
    let fx = fixture();
    fx.mark("m-one", &fx.rev, vec![fx.ai_seg(&fx.rev, 0, 0, 4)]);
    fx.mark("m-cross", &fx.rev, vec![fx.ai_seg(&fx.rev, 0, 4, 8), fx.ai_seg(&fx.rev, 1, 0, 4)]);
    fx.mark("m-other", &fx.rev, vec![fx.ai_seg(&fx.rev, 1, 4, 8)]);
    let pv = preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap();
    assert_eq!(pv.stale_marks, 2);
    assert_eq!(pv.translation, fx.meta(&fx.rev, 0));
    assert_eq!(preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[1], PRIV).unwrap().stale_marks, 2);
    assert!(preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], Sensitivity::Public).is_err(), "hidden item reveals no count");
    let next = fx.body(&fx.rev, 0).replace("章0段1", "改");
    fx.save(&fx.rev, 0, &fx.meta(&fx.rev, 0), &next, "marks").unwrap();
    let listed = ai::list(&fx.lib, &fx.item, None, 0, 100, None, None, PRIV).unwrap();
    let status: BTreeMap<_, _> = listed.annotations.iter().map(|a| (a.id.as_str(), a.status.as_str())).collect();
    // Whole marks touching chapter one are stale (no migration); chapter two only mark stays.
    assert_eq!(status, BTreeMap::from([("m-cross", "stale"), ("m-one", "stale"), ("m-other", "current")]));
    assert_eq!(preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[1], PRIV).unwrap().stale_marks, 1);
    assert_eq!(preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap().stale_marks, 0);
}

#[test]
fn crash_windows_lost_ack_recovery_and_corruption_fail_closed() {
    let fx = fixture();
    let ai0 = fx.meta(&fx.rev, 0);
    let next = fx.body(&fx.rev, 0).replace("章0段0", "断电");
    let up = fx.begin(&fx.rev, 0, &ai0, &next, "crash", P).unwrap();
    fx.upload(&up.edit_id, &next, P).unwrap();
    // Crash after the immutable artifact, before the pointer: nothing visible.
    let e = commit_with_gate(&fx.lib, &fx.item, &up.edit_id, P, PRIV, |_| anyhow::bail!("power_loss")).unwrap_err();
    assert_eq!(e.to_string(), "power_loss");
    assert_eq!(fx.meta(&fx.rev, 0), ai0);
    assert_eq!(fx.files("chapter-translation-edits"), 1);
    // Pointer installed, then directory sync "fails": unknown to the client.
    assert!(commit_with_gate(&fx.lib, &fx.item, &up.edit_id, P, PRIV, |publish| { publish()?; anyhow::bail!("sync_failed") }).is_err());
    let visible = fx.meta(&fx.rev, 0);
    assert_eq!(visible.provenance, "owner_edit");
    // Retry reconciles to the same receipt; never a second version.
    let r = commit(&fx.lib, &fx.item, &up.edit_id, P, PRIV).unwrap();
    assert_eq!(r.translation_version, visible.version);
    assert_eq!(committed(&fx.lib, &fx.item, &up.edit_id, P, PRIV).unwrap().unwrap(), r);
    assert_eq!(fx.files("chapter-translation-edits"), 2);
    // Writer-stopped backup/restore keeps the version, receipt and body.
    let restored = tempfile::tempdir().unwrap();
    copy_tree(&fx.lib, restored.path());
    let m = metadata(restored.path(), &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap().unwrap();
    assert_eq!(m, visible);
    assert_eq!(commit(restored.path(), &fx.item, &up.edit_id, P, PRIV).unwrap(), r);
    // Corrupt pointer or artifact: unavailable, never a silent AI fallback.
    fx.mark("on-edit", &fx.rev, vec![fx.ai_seg(&fx.rev, 0, 0, 3)]);
    let id = source_locked(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap().identity;
    let art = artifact_path(&fx.lib, &id, &visible.version);
    let good = fs::read(&art).unwrap();
    let mut bad = good.clone();
    let n = bad.len() - 3;
    bad[n] ^= 1;
    fs::write(&art, &bad).unwrap();
    assert!(metadata(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).is_err());
    assert!(ai::list(&fx.lib, &fx.item, None, 0, 10, None, None, PRIV).is_err());
    fs::write(&art, &good).unwrap();
    let head = head_path(&fx.lib, &id);
    let good_head = fs::read(&head).unwrap();
    fs::write(&head, b"{}").unwrap();
    assert!(metadata(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).is_err());
    fs::write(&head, good_head).unwrap();
    assert_eq!(fx.meta(&fx.rev, 0), visible);
    assert!(any_owner_edits(&fx.lib).unwrap());
}

#[test]
fn bounds_permission_and_trash_reject_each_step() {
    let fx = fixture();
    let ai0 = fx.meta(&fx.rev, 0);
    let huge = Begin { item_id: fx.item.clone(), revision_id: fx.rev.clone(), chapter: fx.ch[0].clone(),
        expected_translation_version: ai0.version.clone(), expected_translation_sha256: ai0.sha256.clone(),
        request_id: "huge".into(), bytes: MAX_TEXT + 1, sha256: "0".repeat(64) };
    assert_eq!(begin(&fx.lib, huge, P, PRIV).unwrap_err().to_string(), "reader_translation_edit_too_large");
    let up = fx.begin(&fx.rev, 0, &ai0, "新内容", "steps", P).unwrap();
    assert_eq!(chunk(&fx.lib, &fx.item, &up.edit_id, 0, &"a".repeat(MAX_CHUNK + 1), P, PRIV).unwrap_err().to_string(), "invalid_reader_translation_edit");
    assert!(chunk(&fx.lib, &fx.item, &up.edit_id, 0, "新内容", P, Sensitivity::Public).is_err(), "ceiling enforced per step");
    chunk(&fx.lib, &fx.item, &up.edit_id, 0, "新内容", P, PRIV).unwrap();
    assert_eq!(chunk(&fx.lib, &fx.item, &up.edit_id, 0, "旧内容", P, PRIV).unwrap_err().to_string(), "reader_translation_edit_upload_conflict");
    let rest = "新内容".len();
    assert_eq!(rest, up_len("新内容"));
    crate::trash_item(&fx.lib, &fx.item, &fx.rev, "user_request", None, None).unwrap();
    assert!(read_item_at_sensitivity(&fx.lib, &fx.item, PRIV).unwrap().is_none());
    assert_eq!(commit(&fx.lib, &fx.item, &up.edit_id, P, PRIV).unwrap_err().to_string(), "library_resource_not_found");
    assert_eq!(preview(&fx.lib, &fx.item, &fx.rev, &fx.ch[0], PRIV).unwrap_err().to_string(), "library_resource_not_found");
}

fn up_len(s: &str) -> usize { s.len() }

fn copy_tree(from: &Path, to: &Path) {
    for e in fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type().unwrap();
        if t.is_dir() {
            fs::create_dir_all(&dst).unwrap();
            copy_tree(&src, &dst);
        } else if t.is_file() {
            fs::copy(&src, &dst).unwrap();
        }
    }
}
