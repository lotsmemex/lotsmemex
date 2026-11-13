use super::*;
use memex_core::{BlobDescriptor, VerifiedBlob};

const ORIGINAL: &str = "# Synthetic\n\nEnglish 🌹 text.\n\n[^1]: Note.\n";
const TARGET: &str = "# 合成\n\n译🌹文。\n\n[^1]: 注。\n";

fn provenance() -> Provenance {
    Provenance {
        route: tr::Route {
            provider_id: "synthetic".into(),
            model_id: "fixture".into(),
            reasoning_effort: None,
        },
        input_profile_sha256: reader_sha256(b"profile"),
        prompt_version: "fixture-v1".into(),
        prompt_sha256: reader_sha256(b"prompt"),
        algorithm: "fixture-matching-v1".into(),
    }
}

fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    capture.capture.sensitivity = Sensitivity::Private;
    draft.sensitivity = Sensitivity::Private;
    draft.content_markdown = "Other synthetic edition.".into();
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
    capture.blobs = [
        ("original.md", "text/markdown", ORIGINAL.as_bytes().to_vec()),
        (
            "translation.md",
            "text/markdown",
            draft.content_markdown.as_bytes().to_vec(),
        ),
        (
            "manifest.json",
            BOOK_MANIFEST,
            serde_json::to_vec(&manifest).unwrap(),
        ),
        ("original.pdf", "application/pdf", b"%PDF-fixture".to_vec()),
        (
            "translation.pdf",
            "application/pdf",
            b"%PDF-other-fixture".to_vec(),
        ),
    ]
    .into_iter()
    .map(|(name, media, bytes)| {
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
    })
    .collect();
    capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
    let r = ingest_reader_book(dir.path(), capture, draft).unwrap();
    let doc = reader_document(
        dir.path(),
        &r.item_ids[0],
        &r.revision_ids[0],
        Sensitivity::Private,
    )
    .unwrap();
    let chapter = &doc.editions[0].chapters[0].key;
    let s = tr::source(
        dir.path(),
        &r.item_ids[0],
        &r.revision_ids[0],
        chapter,
        Sensitivity::Private,
    )
    .unwrap();
    let p = provenance();
    let m = tr::publish(
        dir.path(),
        s.identity,
        tr::Provenance {
            route: p.route,
            prompt_version: p.prompt_version,
            prompt_sha256: p.prompt_sha256,
            plan_sha256: reader_sha256(b"plan"),
        },
        TARGET.into(),
        Sensitivity::Private,
    )
    .unwrap();
    let s = source(
        dir.path(),
        &r.item_ids[0],
        &r.revision_ids[0],
        chapter,
        &m.version,
        Sensitivity::Private,
    )
    .unwrap();
    (dir, s)
}

fn pairs() -> Vec<paragraphs::Pair> {
    paragraphs::blocks(ORIGINAL)
        .into_iter()
        .zip(paragraphs::blocks(TARGET))
        .map(|((o, kind), (t, _))| paragraphs::Pair {
            o: Some(o),
            t: Some(t),
            kind,
            relation: paragraphs::Relation::Matched,
        })
        .collect()
}

fn page(root: &Path, id: &Identity, version: &str, offset: usize) -> Result<Page> {
    read(
        root,
        &id.source.item_id,
        &id.source.revision_id,
        &id.source.original_chapter,
        &id.translation_version,
        version,
        offset,
        1,
        Sensitivity::Private,
    )
}

fn save(root: &Path, id: &Identity) -> Receipt {
    publish(
        root,
        id.clone(),
        provenance(),
        pairs(),
        "job-1",
        Sensitivity::Private,
    )
    .unwrap()
}

fn renamed(root: &Path, id: &Identity, title: &str) -> Identity {
    let r = revise_item_metadata(
        root,
        &id.source.item_id,
        ItemMetadataUpdate {
            expected_revision_id: id.source.revision_id.clone(),
            title: title.into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let m = tr::metadata(
        root,
        &id.source.item_id,
        &r.revision_id,
        &id.source.original_chapter,
        Sensitivity::Private,
    )
    .unwrap()
    .unwrap();
    source(
        root,
        &id.source.item_id,
        &r.revision_id,
        &id.source.original_chapter,
        &m.version,
        Sensitivity::Private,
    )
    .unwrap()
    .identity
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_owned(),
                    fs::read(p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn final_gate_denial_and_post_publish_error_recover_without_republication() {
    let (dir, s) = fixture();
    let root = fs::canonicalize(dir.path()).unwrap();
    let root = root.as_path();
    let id = s.identity;
    let before = tree(root);
    let error = publish_with_gate(root, id.clone(), provenance(), pairs(), "gated", Sensitivity::Private,
        |_| anyhow::bail!("revoked")).unwrap_err();
    assert_eq!(error.to_string(), "revoked");
    recover_pending_ingests(root).unwrap();
    assert!(!path(root, &id.source).exists());
    assert!(receipt(root, &id, "gated", Sensitivity::Private).unwrap().is_none());
    for (p, bytes) in before {
        if p != Path::new("catalog/generation") {
            assert_eq!(fs::read(root.join(p)).unwrap(), bytes);
        }
    }
    assert!(publish_with_gate(root, id.clone(), provenance(), pairs(), "gated", Sensitivity::Private,
        |_| Ok(())).unwrap_err().to_string().contains("publication_gate_did_not_publish"));
    let fence = std::sync::Mutex::new(());
    let error = publish_with_gate(root, id.clone(), provenance(), pairs(), "gated", Sensitivity::Private,
        |publish| {
            let _guard = fence.lock().unwrap();
            assert!(!path(root, &id.source).exists());
            publish()?;
            assert!(path(root, &id.source).exists());
            anyhow::bail!("lost_after_publication")
        }).unwrap_err();
    assert_eq!(error.to_string(), "lost_after_publication");
    recover_pending_ingests(root).unwrap();
    let saved = receipt(root, &id, "gated", Sensitivity::Private).unwrap().unwrap();
    let replay = publish_with_gate(root, id, provenance(), pairs(), "gated", Sensitivity::Private,
        |_| panic!("receipt replay must not publish")).unwrap();
    assert_eq!(saved, replay);
}

#[test]
fn repair_final_gate_has_no_recoverable_unpublished_intent() {
    for change_body in [false, true] {
        let (dir, s) = fixture();
        let root = fs::canonicalize(dir.path()).unwrap();
        let root = root.as_path();
        let renamed = renamed(root, &s.identity, "Existing current pointer");
        let id = &renamed.source;
        let mut plan = repair::plan(root, &id.item_id, &id.revision_id,
            &id.original_chapter, Sensitivity::Private).unwrap();
        let prior = repair::apply(root, plan.clone(), Sensitivity::Private).unwrap();
        plan.expected_alignment_version = prior.alignment_version;
        plan.input_profile_sha256 = reader_sha256(b"new synthetic profile");
        if change_body {
            plan.original_edits = vec![repair::Edit { range: [11, 13], replacement: "\n".into() }];
            let changed = ORIGINAL.replacen("\n\n", "\n", 1);
            plan.pairs = paragraphs::blocks(&changed).into_iter().map(|(range, kind)| paragraphs::Pair {
                o: Some(range), t: None, kind, relation: paragraphs::Relation::Unresolved,
            }).collect();
        }
        let before = tree(root);
        let error = repair::apply_with_gate(root, plan.clone(), Sensitivity::Private,
            |_| anyhow::bail!("revoked")).unwrap_err();
        assert_eq!(error.to_string(), "revoked");
        assert!(!root.join("catalog/item-update-journal").join(format!("{}.json", id.item_id)).exists());
        recover_pending_ingests(root).unwrap();
        assert_eq!(read_item(root, &id.item_id).unwrap().1.revision_id, id.revision_id);
        assert!(repair::committed(root, &plan, Sensitivity::Private).unwrap().is_none());
        for (p, bytes) in before {
            if p != Path::new("catalog/generation") {
                assert_eq!(fs::read(root.join(p)).unwrap(), bytes);
            }
        }
        let error = repair::apply_with_gate(root, plan.clone(), Sensitivity::Private, |publish| {
            publish()?;
            anyhow::bail!("lost_after_publication")
        }).unwrap_err();
        assert_eq!(error.to_string(), "lost_after_publication");
        // Both projections and the receipt are usable before invoking recovery.
        item_markdown(root, &id.item_id).unwrap();
        assert!(repair::committed(root, &plan, Sensitivity::Private).unwrap().is_some());
        recover_pending_ingests(root).unwrap();
        let saved = repair::committed(root, &plan, Sensitivity::Private).unwrap().unwrap();
        let replay = repair::apply_with_gate(root, plan, Sensitivity::Private,
            |_| panic!("receipt replay must not publish")).unwrap();
        assert_eq!(saved.revision_id, replay.revision_id);
        assert_eq!(saved.alignment_version, replay.alignment_version);
        assert_eq!(saved.revision_id != id.revision_id, change_body);
        item_markdown(root, &id.item_id).unwrap();
    }
}

#[test]
fn missing_zero_write_publish_pages_replay_and_projection_unchanged() {
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    let before = tree(root);
    assert!(!s.recorded_map);
    assert_eq!(
        page(root, id, "any", 0).unwrap().alignment.status,
        "missing"
    );
    assert!(receipt(root, id, "job-1", Sensitivity::Private)
        .unwrap()
        .is_none());
    assert_eq!(tree(root), before);
    let saved = save(root, id);
    assert_eq!(saved, save(root, id));
    assert_eq!(
        receipt(root, id, "job-1", Sensitivity::Private).unwrap(),
        Some(saved.clone())
    );
    assert!(receipt(root, id, "other", Sensitivity::Private)
        .unwrap()
        .is_none());
    assert_eq!(
        saved.map_version,
        reader_sha256(&fs::read(path(root, &id.source)).unwrap())
    );
    for i in 0..3 {
        let p = page(root, id, &saved.map_version, i).unwrap();
        assert_eq!(p.alignment.pairs, vec![pairs()[i].clone()]);
        assert_eq!(p.alignment.next_offset, (i < 2).then_some(i + 1));
        assert_eq!(p.alignment.status, "aligned");
        assert!(!p.certified);
        let json = serde_json::to_value(p).unwrap();
        assert_eq!(json["quality"], "not_certified");
        assert_eq!(json["basis"], "retrospective_model_alignment_v1");
    }
    assert!(page(root, id, "any", 1)
        .unwrap_err()
        .to_string()
        .contains("stale_chapter_alignment"));
    assert!(page(root, id, "missing", 0)
        .unwrap_err()
        .to_string()
        .contains("stale_chapter_alignment"));
    assert!(publish(
        root,
        id.clone(),
        provenance(),
        pairs(),
        "job-2",
        Sensitivity::Private
    )
    .unwrap_err()
    .to_string()
    .contains("stale_chapter_alignment"));
    let mut changed = provenance();
    changed.algorithm = "different".into();
    assert!(publish(
        root,
        id.clone(),
        changed,
        pairs(),
        "job-1",
        Sensitivity::Private
    )
    .is_err());
    let after = tree(root);
    for (p, bytes) in before {
        // The existing ingest writer lock intentionally bumps this cache token.
        // All source/projection/revision/receipt files remain byte-identical.
        if p != Path::new("catalog/generation") {
            assert_eq!(after.get(&p), Some(&bytes));
        }
    }
    assert_eq!(
        tr::alignment(
            root,
            &id.source.item_id,
            &id.source.revision_id,
            &id.source.original_chapter,
            &id.translation_version,
            0,
            1,
            Sensitivity::Private
        )
        .unwrap()
        .status,
        "missing"
    );
}

#[test]
fn inherited_versions_stale_pages_and_corrupt_proofs_never_fallback() {
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    let saved = save(root, id);
    let second = renamed(root, id, "Second synthetic title");
    assert_eq!(second.projection_identity, id.projection_identity);
    assert_eq!(second.projection_sha256, id.projection_sha256);
    assert_ne!(second.translation_version, id.translation_version);
    let inherited = page(root, &second, "any", 0).unwrap().alignment.version;
    assert_ne!(inherited, saved.map_version);
    assert!(page(root, &second, &saved.map_version, 1)
        .unwrap_err()
        .to_string()
        .contains("stale_chapter_alignment"));
    assert_eq!(
        page(root, &second, &inherited, 1)
            .unwrap()
            .alignment
            .pairs
            .len(),
        1
    );
    assert!(!path(root, &second.source).exists());
    assert!(page(root, id, "any", 0)
        .unwrap_err()
        .to_string()
        .contains("stale_item_revision"));
    assert_eq!(
        receipt(root, id, "job-1", Sensitivity::Private).unwrap(),
        Some(saved.clone())
    );
    let mut stale = second.clone();
    stale.translation_version = id.translation_version.clone();
    assert!(page(root, &stale, "any", 0)
        .unwrap_err()
        .to_string()
        .contains("stale_reader_translation"));
    let third = renamed(root, &second, "Third synthetic title");
    let intermediate = path(root, &second.source);
    fs::create_dir_all(intermediate.parent().unwrap()).unwrap();
    fs::write(&intermediate, b"{}").unwrap();
    assert!(page(root, &third, "any", 0).is_err());
    fs::remove_file(&intermediate).unwrap();
    let current = path(root, &third.source);
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    fs::write(&current, b"{}").unwrap();
    assert!(page(root, &third, "any", 0).is_err());
    assert!(publish(
        root,
        third,
        provenance(),
        pairs(),
        "new",
        Sensitivity::Private
    )
    .is_err());
}

#[test]
fn permissions_and_bad_partitions_are_rejected_without_writes() {
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    for case in 0..7 {
        let mut p = pairs();
        match case {
            0 => {
                p.pop();
            }
            1 => p[0].t = Some([0, 3]), // Chinese UTF-8 interior
            2 => p.swap(0, 1),
            3 => p[1].o.as_mut().unwrap()[0] += 1,
            4 => p[0].kind = paragraphs::Kind::Note,
            5 => p[0].relation = paragraphs::Relation::OriginalOnly,
            _ => p.clear(),
        }
        assert!(publish(
            root,
            id.clone(),
            provenance(),
            p,
            "bad",
            Sensitivity::Private
        )
        .is_err());
        assert!(!path(root, &id.source).exists());
    }
    assert!(source(
        root,
        &id.source.item_id,
        &id.source.revision_id,
        &id.source.original_chapter,
        &id.translation_version,
        Sensitivity::Public
    )
    .is_err());
    assert!(publish(
        root,
        id.clone(),
        provenance(),
        pairs(),
        "bad",
        Sensitivity::Public
    )
    .is_err());
    save(root, id);
    assert!(receipt(root, id, "job-1", Sensitivity::Public).is_err());
    assert!(read(
        root,
        &id.source.item_id,
        &id.source.revision_id,
        &id.source.original_chapter,
        &id.translation_version,
        "any",
        0,
        1,
        Sensitivity::Public
    )
    .is_err());
}

#[test]
fn publication_on_inherited_projection_keeps_storage_and_view_identity_separate() {
    let (dir, s) = fixture();
    let root = dir.path();
    let second = renamed(root, &s.identity, "Publication revision");
    let saved = save(root, &second);
    assert!(!path(root, &s.identity.source).exists());
    let third = renamed(root, &second, "Inherited map revision");
    let before = tree(root);
    let p = page(root, &third, "any", 0).unwrap();
    assert_ne!(p.alignment.version, saved.map_version);
    assert_eq!(p.alignment.status, "aligned");
    assert_eq!(
        receipt(root, &second, "job-1", Sensitivity::Private).unwrap(),
        Some(saved)
    );
    assert_eq!(tree(root), before);
    assert!(publish(
        root,
        third,
        provenance(),
        pairs(),
        "other",
        Sensitivity::Private
    )
    .unwrap_err()
    .to_string()
    .contains("stale_chapter_alignment"));
}

fn edit(root: &Path, id: &tr::Identity, text: &str, request: &str) -> tr::Identity {
    let stage = chapter_edit::begin(
        root,
        chapter_edit::Begin {
            item_id: id.item_id.clone(),
            revision_id: id.revision_id.clone(),
            side: "original".into(),
            chapter: id.original_chapter.clone(),
            expected_chapter_sha256: id.source_sha256.clone(),
            request_id: request.into(),
            bytes: text.len(),
            sha256: reader_sha256(text.as_bytes()),
        },
        "local_owner",
        Sensitivity::Private,
    )
    .unwrap();
    chapter_edit::chunk(
        root,
        &id.item_id,
        &stage.edit_id,
        0,
        text,
        "local_owner",
        Sensitivity::Private,
    )
    .unwrap();
    let r = chapter_edit::commit(
        root,
        &id.item_id,
        &stage.edit_id,
        "local_owner",
        Sensitivity::Private,
    )
    .unwrap();
    tr::source(
        root,
        &id.item_id,
        &r.revision_id,
        &id.original_chapter,
        Sensitivity::Private,
    )
    .unwrap()
    .identity
}

#[test]
fn changed_then_restored_does_not_jump_to_old_map_but_receipt_recovers() {
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    let saved = save(root, id);
    let changed = edit(root, &id.source, "# Synthetic\n\nChanged.\n", "change");
    let restored = edit(root, &changed, ORIGINAL, "restore");
    assert!(tr::metadata(
        root,
        &restored.item_id,
        &restored.revision_id,
        &restored.original_chapter,
        Sensitivity::Private
    )
    .unwrap()
    .is_none());
    assert!(source(
        root,
        &restored.item_id,
        &restored.revision_id,
        &restored.original_chapter,
        &id.translation_version,
        Sensitivity::Private
    )
    .err()
    .unwrap()
    .to_string()
    .contains("stale_reader_translation"));
    assert_eq!(
        receipt(root, id, "job-1", Sensitivity::Private).unwrap(),
        Some(saved)
    );
    let p = provenance();
    let m = tr::publish(
        root,
        restored.clone(),
        tr::Provenance {
            route: p.route,
            prompt_version: p.prompt_version,
            prompt_sha256: p.prompt_sha256,
            plan_sha256: reader_sha256(b"plan"),
        },
        TARGET.into(),
        Sensitivity::Private,
    )
    .unwrap();
    let fresh = source(
        root,
        &restored.item_id,
        &restored.revision_id,
        &restored.original_chapter,
        &m.version,
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(
        page(root, &fresh.identity, "any", 0)
            .unwrap()
            .alignment
            .status,
        "missing"
    );
}

#[test]
fn corrupt_intermediate_projection_and_protected_history_fail_closed() {
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    save(root, id);
    let second = renamed(root, id, "Intermediate revision");
    let third = renamed(root, &second, "Current revision");
    let file = root
        .join("catalog/chapter-translations")
        .join(&second.source.item_id)
        .join(&second.source.revision_id)
        .join(format!(
            "{}.json",
            reader_sha256(second.source.original_chapter.as_bytes())
        ));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"{}").unwrap();
    assert!(page(root, &third, "any", 0).is_err());
    fs::remove_file(file).unwrap();
    let (dir, s) = fixture();
    let root = dir.path();
    let id = &s.identity;
    save(root, id);
    // Synthetic lower-current-label fixture; historical Evidence remains private.
    let (mut item, mut current) = read_item(root, &id.source.item_id).unwrap();
    item.sensitivity = Sensitivity::Public;
    current.sensitivity = Sensitivity::Public;
    let integrity_path = root
        .join("integrity/items")
        .join(format!("{}.json", id.source.item_id));
    let mut integrity: ItemIntegrityManifest = load(root, integrity_path.clone()).unwrap();
    integrity.sensitivity = Sensitivity::Public;
    integrity.item_sha256 = format!(
        "sha256:{}",
        reader_sha256(&serde_json::to_vec(&item).unwrap())
    );
    integrity.revision_sha256 = revision_digest(&current).unwrap();
    let item_dir = root.join("items").join(&id.source.item_id);
    durable_replace(
        root,
        &item_dir.join("item.json"),
        &serde_json::to_vec(&item).unwrap(),
    )
    .unwrap();
    durable_replace(
        root,
        &item_dir
            .join("revisions")
            .join(format!("{}.json", current.revision_id)),
        &serde_json::to_vec(&current).unwrap(),
    )
    .unwrap();
    durable_replace(
        root,
        &integrity_path,
        &serde_json::to_vec(&integrity).unwrap(),
    )
    .unwrap();
    assert!(
        read_item_at_sensitivity(root, &id.source.item_id, Sensitivity::Public)
            .unwrap()
            .is_some()
    );
    assert!(receipt(root, id, "job-1", Sensitivity::Public).is_err());
    assert!(read(
        root,
        &id.source.item_id,
        &id.source.revision_id,
        &id.source.original_chapter,
        &id.translation_version,
        "any",
        0,
        1,
        Sensitivity::Public
    )
    .is_err());
}
