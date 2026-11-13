use super::durable::*;
use super::*;
use edits::{Basis, ClassifiedBlock, Kind, ProposedEdit, Reason, Role};

const OWNER: &str = "local_owner";
const PRIVATE: Sensitivity = Sensitivity::Private;

fn fixture(body: &str) -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    capture.capture.sensitivity = PRIVATE;
    let mut draft = crate::tests::draft();
    draft.sensitivity = PRIVATE;
    draft.content_markdown = body.into();
    let r = ingest(dir.path(), &capture, draft).unwrap();
    let s = source(
        dir.path(),
        &r.item_ids[0],
        &r.revision_ids[0],
        Target::Body {},
        PRIVATE,
    )
    .unwrap();
    (dir, s)
}

fn layout(s: &Source, old: &str, new: &str) -> edits::Applied {
    let start = s.markdown.find(old).unwrap();
    let classified: Vec<_> = edits::blocks(&s.markdown)
        .unwrap()
        .into_iter()
        .map(|block| ClassifiedBlock {
            block,
            role: Role::Body,
            source_evidence_ids: vec![],
        })
        .collect();
    edits::apply(
        &s.markdown,
        &s.identity.source_sha256,
        &classified,
        &s.identity.source_evidence_ids,
        &[ProposedEdit {
            old_range: [start, start + old.len()],
            old_sha256: reader_sha256(old.as_bytes()),
            replacement: new.into(),
            kind: Kind::Whitespace,
            basis: Basis::Deterministic,
            reason_code: Reason::ParagraphLayout,
            source_evidence_ids: vec![],
            block_id: None,
        }],
    )
    .unwrap()
}

fn put(dir: &Path, s: &Source, a: &edits::Applied, request: &str) -> Result<Receipt> {
    publish(
        dir,
        s.identity.clone(),
        request,
        OWNER,
        a,
        serde_json::json!({"policy":"synthetic-v1"}),
        PRIVATE,
    )
}

#[test]
fn publish_review_restore_exact_bytes_replay_and_cas() {
    let (dir, s) = fixture("中文🌹  abstract 2026 [1].\r\n\n[^1]: exact note\r\n");
    let a = layout(&s, "  ", "\n\n");
    let r = put(dir.path(), &s, &a, "format-one").unwrap();
    assert!(r.changed);
    assert_eq!(
        (r.applied_count, r.correction_count, r.deleted_blocks),
        (1, 0, 0)
    );
    assert_eq!(put(dir.path(), &s, &a, "format-one").unwrap(), r);
    let (_, next) = read_item(dir.path(), &r.item_id).unwrap();
    assert_eq!(next.content_markdown, a.output());
    let root = existing_canonical_root(dir.path()).unwrap();
    let (_, base) = read_legacy_item(&root, &r.item_id).unwrap();
    assert_eq!(next.title, base.title);
    assert_eq!(next.metadata, base.metadata);
    assert_eq!(next.attachments, base.attachments);
    assert_eq!(
        &next.evidence_ids[..base.evidence_ids.len()],
        base.evidence_ids
    );
    let page = changes_list(
        dir.path(),
        "format-one",
        OWNER,
        &r.changes_version,
        0,
        1,
        PRIVATE,
    )
    .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.next_offset, None);
    let text = change_text(
        dir.path(),
        "format-one",
        OWNER,
        &r.changes_version,
        &page.changes[0].id,
        "before",
        0,
        1,
        PRIVATE,
    )
    .unwrap();
    assert_eq!(text.text, " ");
    assert_eq!(text.next_offset, Some(1));
    assert!(changes_list(dir.path(), "format-one", OWNER, "wrong", 0, 1, PRIVATE).is_err());
    assert!(changes_list(
        dir.path(),
        "format-one",
        &"a".repeat(64),
        &r.changes_version,
        0,
        1,
        PRIVATE
    )
    .is_err());
    assert!(changes_list(
        dir.path(),
        "format-one",
        OWNER,
        &r.changes_version,
        0,
        1,
        Sensitivity::Public
    )
    .is_err());
    let old = revision_text::chunk(
        dir.path(),
        &r.item_id,
        &r.input_revision_id,
        Target::Body {},
        0,
        8192,
        "any",
        PRIVATE,
    )
    .unwrap();
    assert_eq!(old.text, s.markdown);
    let restored = restore(
        dir.path(),
        "format-one",
        OWNER,
        &r.changes_version,
        &r.result_revision_id,
        "restore-one",
        PRIVATE,
    )
    .unwrap();
    assert!(restored.changed);
    assert_ne!(restored.revision_id, r.input_revision_id);
    assert_ne!(restored.revision_id, r.result_revision_id);
    assert_eq!(
        read_item(dir.path(), &r.item_id)
            .unwrap()
            .1
            .content_markdown,
        s.markdown
    );
    assert_eq!(
        restore(
            dir.path(),
            "format-one",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "restore-one",
            PRIVATE
        )
        .unwrap(),
        restored
    );
    assert_eq!(put(dir.path(), &s, &a, "format-one").unwrap(), r);
    assert!(restore(
        dir.path(),
        "format-one",
        OWNER,
        &r.changes_version,
        &r.result_revision_id,
        "restore-other",
        PRIVATE
    )
    .is_err());
    assert!(put(dir.path(), &s, &a, "format-other").is_err());
    assert!(publish(
        dir.path(),
        s.identity.clone(),
        "format-one",
        OWNER,
        &a,
        serde_json::json!({"policy":"other"}),
        PRIVATE
    )
    .is_err());
}

#[test]
fn immutable_empty_proof_no_revision_and_noop_restore() {
    let (dir, s) = fixture("Unchanged 中文 body.\n");
    let classified: Vec<_> = edits::blocks(&s.markdown)
        .unwrap()
        .into_iter()
        .map(|block| ClassifiedBlock {
            block,
            role: Role::Body,
            source_evidence_ids: vec![],
        })
        .collect();
    let a = edits::apply(
        &s.markdown,
        &s.identity.source_sha256,
        &classified,
        &s.identity.source_evidence_ids,
        &[],
    )
    .unwrap();
    let r = put(dir.path(), &s, &a, "noop").unwrap();
    assert!(!r.changed);
    assert_eq!(r.result_revision_id, s.identity.revision_id);
    assert_eq!(put(dir.path(), &s, &a, "noop").unwrap(), r);
    let page = changes_list(
        dir.path(),
        "noop",
        OWNER,
        &r.changes_version,
        0,
        64,
        PRIVATE,
    )
    .unwrap();
    assert_eq!(page.total, 0);
    let restored = restore(
        dir.path(),
        "noop",
        OWNER,
        &r.changes_version,
        &r.result_revision_id,
        "noop-restore",
        PRIVATE,
    )
    .unwrap();
    assert!(!restored.changed);
    assert_eq!(restored.revision_id, r.result_revision_id);
    assert_eq!(
        read_item(dir.path(), &r.item_id).unwrap().1.revision_number,
        1
    );
}

#[test]
fn journal_interruptions_recover_one_format_and_restore_revision() {
    for step in 0..=4 {
        let (dir, s) = fixture("Body  with exact bytes.\n");
        let a = layout(&s, "  ", " ");
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(Some(step)));
        let first = put(dir.path(), &s, &a, "format-crash");
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(None));
        assert!(first.is_err());
        let recovered = receipt(
            dir.path(),
            &s.identity.item_id,
            "format-crash",
            OWNER,
            PRIVATE,
        )
        .unwrap();
        assert_eq!(recovered.is_some(), step != 0);
        let r = put(dir.path(), &s, &a, "format-crash").unwrap();
        if let Some(recovered) = recovered {
            assert_eq!(recovered, r);
        }
        assert_eq!(
            read_item(dir.path(), &r.item_id).unwrap().1.revision_number,
            2
        );
        assert_eq!(put(dir.path(), &s, &a, "format-crash").unwrap(), r);
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(Some(step)));
        let first = restore(
            dir.path(),
            "format-crash",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "restore-crash",
            PRIVATE,
        );
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(None));
        assert!(first.is_err());
        let restored = restore(
            dir.path(),
            "format-crash",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "restore-crash",
            PRIVATE,
        )
        .unwrap();
        let next = read_item(dir.path(), &r.item_id).unwrap().1;
        assert_eq!(next.revision_number, 3);
        assert_eq!(next.content_markdown, s.markdown);
        assert_eq!(
            restore(
                dir.path(),
                "format-crash",
                OWNER,
                &r.changes_version,
                &r.result_revision_id,
                "restore-crash",
                PRIVATE
            )
            .unwrap(),
            restored
        );
    }
}

#[test]
fn source_identity_and_evidence_cannot_be_forged() {
    let (dir, s) = fixture("Body  text.\n");
    let a = layout(&s, "  ", " ");
    for field in 0..4 {
        let mut id = s.identity.clone();
        match field {
            0 => id.source_bytes += 1,
            1 => id.source_evidence_ids.clear(),
            2 => id.revision_sha256 = format!("sha256:{}", "0".repeat(64)),
            _ => id.sensitivity = Sensitivity::Public,
        }
        assert!(publish(
            dir.path(),
            id,
            "forged",
            OWNER,
            &a,
            serde_json::Value::Null,
            PRIVATE
        )
        .is_err());
    }
    assert_eq!(
        read_item(dir.path(), &s.identity.item_id)
            .unwrap()
            .1
            .revision_id,
        s.identity.revision_id
    );
}

fn book_fixture() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    capture.capture.sensitivity = PRIVATE;
    let mut draft = crate::tests::draft();
    draft.sensitivity = PRIVATE;
    let original =
        "# One\r\n\nOriginal  text[^1].\r\n\n[^1]: untouched note\r\n\n# Two\n\nOther chapter.\n";
    draft.content_markdown = "# 一\n\n中文  正文。\n\n# 二\n\n其他章节。\n".into();
    let mut en = heading_edition(original);
    en.side = "original".into();
    let mut zh = heading_edition(&draft.content_markdown);
    zh.side = "translation".into();
    let manifest = BookManifest {
        schema_version: 1,
        alignment: "declared_chapter_order_not_paragraph_alignment".into(),
        editions: vec![en, zh],
    };
    capture.blobs = [
        ("original.md", "text/markdown", original.as_bytes().to_vec()),
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
    ]
    .into_iter()
    .map(|(name, media, bytes)| {
        let sha = reader_sha256(&bytes);
        memex_core::VerifiedBlob {
            descriptor: memex_core::BlobDescriptor {
                schema_version: 1,
                locator: name.into(),
                media_type: media.into(),
                byte_size: bytes.len() as u64,
                sha256: sha.clone(),
                source_url: None,
            },
            blob_id: format!("sha256:{sha}"),
            source_backend_ref: None,
            bytes,
        }
    })
    .collect();
    capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
    let r = ingest_reader_book(dir.path(), capture, draft).unwrap();
    (dir, r.item_ids[0].clone(), r.revision_ids[0].clone())
}

#[test]
fn both_book_sides_preserve_unselected_bytes_manifests_notes_and_history() {
    for side in ["original", "translation"] {
        let (dir, item, rev) = book_fixture();
        let root = existing_canonical_root(dir.path()).unwrap();
        let base = read_item(dir.path(), &item).unwrap().1;
        let (doc, bodies) = document_revision_locked(&root, &base, PRIVATE).unwrap();
        let chapter = doc
            .editions
            .iter()
            .find(|e| e.side == side)
            .unwrap()
            .chapters[0]
            .key
            .clone();
        let target = Target::Chapter {
            side: side.into(),
            chapter,
        };
        let s = source(dir.path(), &item, &rev, target.clone(), PRIVATE).unwrap();
        let a = layout(&s, "  ", " ");
        let r = put(dir.path(), &s, &a, "book-format").unwrap();
        let next = read_item(dir.path(), &item).unwrap().1;
        let (newdoc, newbodies) = document_revision_locked(&root, &next, PRIVATE).unwrap();
        let other = if side == "original" {
            "translation"
        } else {
            "original"
        };
        assert_eq!(bodies[other].0, newbodies[other].0);
        assert_eq!(
            doc.editions.iter().find(|e| e.side == other),
            newdoc.editions.iter().find(|e| e.side == other)
        );
        let e = doc.editions.iter().find(|e| e.side == side).unwrap();
        let ne = newdoc.editions.iter().find(|e| e.side == side).unwrap();
        for (old, new) in e.chapters.iter().zip(&ne.chapters).skip(1) {
            assert_eq!(
                &bodies[side].0[old.start..old.end],
                &newbodies[side].0[new.start..new.end]
            );
            assert_eq!(old.key, new.key);
            assert_eq!(old.title, new.title);
            assert_eq!(old.counterpart, new.counterpart);
        }
        assert_eq!(
            source(
                dir.path(),
                &item,
                &r.result_revision_id,
                target.clone(),
                PRIVATE
            )
            .unwrap()
            .markdown,
            a.output()
        );
        assert_eq!(
            revision_text::chunk(dir.path(), &item, &rev, target, 0, 8192, "any", PRIVATE)
                .unwrap()
                .text,
            s.markdown
        );
        let restored = restore(
            dir.path(),
            "book-format",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "book-restore",
            PRIVATE,
        )
        .unwrap();
        let final_revision = read_item(dir.path(), &item).unwrap().1;
        assert_eq!(final_revision.revision_id, restored.revision_id);
        let (finaldoc, finalbodies) =
            document_revision_locked(&root, &final_revision, PRIVATE).unwrap();
        assert_eq!(doc.editions, finaldoc.editions);
        for side in ["original", "translation"] {
            assert_eq!(bodies[side].0, finalbodies[side].0);
        }
    }
}

#[test]
fn deletion_text_large_paging_and_utf8_correction() {
    let body = format!(
        "Next{}Body about navigation is legitimate prose.\n\n錯字 stays.\n",
        "\n".repeat(9000)
    );
    let (dir, s) = fixture(&body);
    let blocks = edits::blocks(&s.markdown).unwrap();
    let classified: Vec<_> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| ClassifiedBlock {
            block: block.clone(),
            role: if i == 0 {
                Role::Furniture {
                    reason: Reason::Navigation,
                    basis: Basis::ContextInferred,
                }
            } else {
                Role::Body
            },
            source_evidence_ids: vec![],
        })
        .collect();
    let old = &s.markdown[blocks[0].range[0]..blocks[0].range[1]];
    let start = s.markdown.find("錯").unwrap();
    let a = edits::apply(
        &s.markdown,
        &s.identity.source_sha256,
        &classified,
        &s.identity.source_evidence_ids,
        &[
            ProposedEdit {
                old_range: blocks[0].range,
                old_sha256: reader_sha256(old.as_bytes()),
                replacement: String::new(),
                kind: Kind::GarbageDelete,
                basis: Basis::ContextInferred,
                reason_code: Reason::Navigation,
                source_evidence_ids: vec![],
                block_id: Some(blocks[0].id.clone()),
            },
            ProposedEdit {
                old_range: [start, start + 3],
                old_sha256: reader_sha256("錯".as_bytes()),
                replacement: "错".into(),
                kind: Kind::Typo,
                basis: Basis::ContextInferred,
                reason_code: Reason::ShortCorrection,
                source_evidence_ids: vec![],
                block_id: None,
            },
        ],
    )
    .unwrap();
    let r = put(dir.path(), &s, &a, "deletion").unwrap();
    assert_eq!((r.deleted_blocks, r.correction_count), (1, 1));
    let page = changes_list(
        dir.path(),
        "deletion",
        OWNER,
        &r.changes_version,
        0,
        1,
        PRIVATE,
    )
    .unwrap();
    assert_eq!(page.next_offset, Some(1));
    let id = &page.changes[0].id;
    let mut reconstructed = String::new();
    loop {
        let chunk = change_text(
            dir.path(),
            "deletion",
            OWNER,
            &r.changes_version,
            id,
            "before",
            reconstructed.len(),
            8192,
            PRIVATE,
        )
        .unwrap();
        reconstructed.push_str(&chunk.text);
        if chunk.next_offset.is_none() {
            break;
        }
    }
    assert_eq!(reconstructed, old);
    let empty = change_text(
        dir.path(),
        "deletion",
        OWNER,
        &r.changes_version,
        id,
        "after",
        0,
        1,
        PRIVATE,
    )
    .unwrap();
    assert_eq!(empty.text, "");
    assert_eq!(empty.total_bytes, 0);
    let page = changes_list(
        dir.path(),
        "deletion",
        OWNER,
        &r.changes_version,
        1,
        1,
        PRIVATE,
    )
    .unwrap();
    let id = &page.changes[0].id;
    assert!(change_text(
        dir.path(),
        "deletion",
        OWNER,
        &r.changes_version,
        id,
        "before",
        0,
        1,
        PRIVATE
    )
    .is_err());
    assert!(change_text(
        dir.path(),
        "deletion",
        OWNER,
        &r.changes_version,
        id,
        "before",
        1,
        3,
        PRIVATE
    )
    .is_err());
    assert_eq!(
        change_text(
            dir.path(),
            "deletion",
            OWNER,
            &r.changes_version,
            id,
            "before",
            0,
            3,
            PRIVATE
        )
        .unwrap()
        .text,
        "錯"
    );
}

#[test]
fn later_metadata_conflicts_restore_but_original_receipt_survives_and_trash_denies_reads() {
    let (dir, s) = fixture("Body  text.\n");
    let a = layout(&s, "  ", " ");
    assert_eq!(
        receipt(dir.path(), &s.identity.item_id, "job", OWNER, PRIVATE).unwrap(),
        None
    );
    let r = put(dir.path(), &s, &a, "job").unwrap();
    let later = revise_item_metadata(
        dir.path(),
        &r.item_id,
        ItemMetadataUpdate {
            expected_revision_id: r.result_revision_id.clone(),
            title: "Later owner title".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        restore(
            dir.path(),
            "job",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "restore",
            PRIVATE
        )
        .unwrap_err()
        .to_string(),
        "content_format_restore_conflict"
    );
    let current = read_item(dir.path(), &r.item_id).unwrap().1;
    assert_eq!(current.title, "Later owner title");
    assert_eq!(current.content_markdown, a.output());
    assert_eq!(
        receipt(dir.path(), &r.item_id, "job", OWNER, PRIVATE).unwrap(),
        Some(r.clone())
    );
    assert!(receipt(dir.path(), &r.item_id, "job", OWNER, Sensitivity::Public).is_err());
    trash_item(
        dir.path(),
        &r.item_id,
        &later.revision_id,
        "synthetic",
        None,
        None,
    )
    .unwrap();
    assert!(receipt(dir.path(), &r.item_id, "job", OWNER, PRIVATE).is_err());
    assert!(changes_list(dir.path(), "job", OWNER, &r.changes_version, 0, 64, PRIVATE).is_err());
    assert!(put(dir.path(), &s, &a, "job").is_err());
}

#[test]
fn historical_protection_is_independent_of_a_lower_current_label() {
    let (dir, s) = fixture("Retained private source.\n");
    let classified: Vec<_> = edits::blocks(&s.markdown)
        .unwrap()
        .into_iter()
        .map(|block| ClassifiedBlock {
            block,
            role: Role::Body,
            source_evidence_ids: vec![],
        })
        .collect();
    let applied = edits::apply(
        &s.markdown,
        &s.identity.source_sha256,
        &classified,
        &s.identity.source_evidence_ids,
        &[],
    )
    .unwrap();
    let r = put(dir.path(), &s, &applied, "private-noop").unwrap();
    // No production relabel API exists in this Store. Construct a synthetic
    // lower-current-label fixture, retaining the old proof and Evidence labels.
    let root = existing_canonical_root(dir.path()).unwrap();
    let (mut item, mut current) = read_item(&root, &r.item_id).unwrap();
    item.sensitivity = Sensitivity::Public;
    current.sensitivity = Sensitivity::Public;
    let integrity_path = root
        .join("integrity/items")
        .join(format!("{}.json", r.item_id));
    let mut integrity: ItemIntegrityManifest = load(&root, integrity_path.clone()).unwrap();
    integrity.sensitivity = Sensitivity::Public;
    integrity.item_sha256 = format!(
        "sha256:{}",
        reader_sha256(&serde_json::to_vec(&item).unwrap())
    );
    integrity.revision_sha256 = revision_digest(&current).unwrap();
    let item_dir = root.join("items").join(&r.item_id);
    durable_replace(
        &root,
        &item_dir.join("item.json"),
        &serde_json::to_vec(&item).unwrap(),
    )
    .unwrap();
    durable_replace(
        &root,
        &item_dir
            .join("revisions")
            .join(format!("{}.json", current.revision_id)),
        &serde_json::to_vec(&current).unwrap(),
    )
    .unwrap();
    durable_replace(
        &root,
        &integrity_path,
        &serde_json::to_vec(&integrity).unwrap(),
    )
    .unwrap();
    assert!(
        read_item_at_sensitivity(&root, &r.item_id, Sensitivity::Public)
            .unwrap()
            .is_some()
    );
    assert!(receipt(
        &root,
        &r.item_id,
        "private-noop",
        OWNER,
        Sensitivity::Public
    )
    .is_err());
    assert!(changes_list(
        &root,
        "private-noop",
        OWNER,
        &r.changes_version,
        0,
        64,
        Sensitivity::Public
    )
    .is_err());
    assert_eq!(
        changes_list(
            &root,
            "private-noop",
            OWNER,
            &r.changes_version,
            0,
            64,
            PRIVATE
        )
        .unwrap()
        .total,
        0
    );
    assert_eq!(
        restore(
            &root,
            "private-noop",
            OWNER,
            &r.changes_version,
            &r.result_revision_id,
            "restore-lowered",
            PRIVATE
        )
        .unwrap_err()
        .to_string(),
        "content_format_restore_conflict"
    );
    assert_eq!(read_item(&root, &r.item_id).unwrap().1.revision_number, 1);
}
