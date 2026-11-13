use super::*;
use memex_core::{BlobDescriptor, VerifiedBlob};

const ORIGINAL: &str = "# Heading\n\nEnglish 🌹 text.\n\n[^1]: Note.\n";
const TARGET: &str = "# 标题\n\n译🌹文。\n\n内部段。\n\n[^1]: 注。";

fn provenance() -> Provenance {
    Provenance {
        route: Route {
            provider_id: "synthetic".into(),
            model_id: "fixture".into(),
            reasoning_effort: None,
        },
        prompt_version: "test-v1".into(),
        prompt_sha256: reader_sha256(b"prompt"),
        plan_sha256: reader_sha256(b"plan"),
    }
}

fn mapping() -> Vec<TranslationBlock> {
    let heading_end = TARGET.find("译").unwrap();
    let note_start = TARGET.find("[^1]").unwrap();
    paragraphs::blocks(ORIGINAL)
        .into_iter()
        .zip([
            [0, heading_end],
            [heading_end, note_start],
            [note_start, TARGET.len()],
        ])
        .enumerate()
        .map(
            |(i, ((source_range, kind), target_range))| TranslationBlock {
                id: format!("block-{i}"),
                source_range,
                target_range,
                kind,
            },
        )
        .collect()
}

fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let mut capture = crate::tests::capture();
    let mut draft = crate::tests::draft();
    capture.capture.sensitivity = Sensitivity::Private;
    draft.sensitivity = Sensitivity::Private;
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
    let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
    let doc = reader_document(
        dir.path(),
        &receipt.item_ids[0],
        &receipt.revision_ids[0],
        Sensitivity::Private,
    )
    .unwrap();
    let chapter = &doc.editions[0].chapters[0].key;
    let s = source(
        dir.path(),
        &receipt.item_ids[0],
        &receipt.revision_ids[0],
        chapter,
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(s.markdown, ORIGINAL);
    (dir, s)
}

fn page(
    root: &Path,
    id: &Identity,
    version: &str,
    offset: usize,
    limit: usize,
) -> Result<AlignmentPage> {
    alignment(
        root,
        &id.item_id,
        &id.revision_id,
        &id.original_chapter,
        version,
        offset,
        limit,
        Sensitivity::Private,
    )
}

#[test]
fn legacy_exact_serialization_and_missing_map_stay_readable() {
    let (dir, s) = fixture();
    let saved = publish(
        dir.path(),
        s.identity.clone(),
        provenance(),
        TARGET.into(),
        Sensitivity::Private,
    )
    .unwrap();
    // Independent pre-map struct preserves the old serialization order/field set.
    #[derive(Serialize)]
    struct Legacy<'a> {
        schema_version: u32,
        identity: &'a Identity,
        provenance: Provenance,
        sha256: String,
        markdown: &'a str,
    }
    let old = serde_json::to_vec(&Legacy {
        schema_version: 1,
        identity: &s.identity,
        provenance: provenance(),
        sha256: reader_sha256(TARGET.as_bytes()),
        markdown: TARGET,
    })
    .unwrap();
    assert_eq!(saved.version, reader_sha256(&old));
    let file = path(dir.path(), &s.identity);
    assert_eq!(fs::read(&file).unwrap(), old);
    let p = page(dir.path(), &s.identity, &saved.version, 0, 1).unwrap();
    assert_eq!(p.status, "missing");
    assert_eq!(p.total, 0);
    assert!(p.pairs.is_empty() && p.next_offset.is_none());
    assert!(page(dir.path(), &s.identity, &saved.version, 1, 1).is_err());
    let replay = publish_mapped(
        dir.path(),
        s.identity.clone(),
        provenance(),
        TARGET.into(),
        mapping(),
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(replay, saved);
    assert_eq!(fs::read(file).unwrap(), old);
    assert_eq!(
        chunk(
            dir.path(),
            &s.identity.item_id,
            &s.identity.revision_id,
            &s.identity.original_chapter,
            &saved.version,
            0,
            8192,
            Sensitivity::Private
        )
        .unwrap()
        .markdown,
        TARGET
    );
}

#[test]
fn mapped_utf8_paging_hash_binding_inheritance_and_scope() {
    let (dir, s) = fixture();
    let id = &s.identity;
    assert!(page(dir.path(), id, &"0".repeat(64), 0, 1).is_err());
    let saved = publish_mapped(
        dir.path(),
        id.clone(),
        provenance(),
        TARGET.into(),
        mapping(),
        Sensitivity::Private,
    )
    .unwrap();
    let bytes = fs::read(path(dir.path(), id)).unwrap();
    assert_eq!(saved.version, reader_sha256(&bytes));
    let mut projection: Projection = serde_json::from_slice(&bytes).unwrap();
    projection.blocks.as_mut().unwrap()[0].target_range[1] += 1;
    assert_ne!(projection.metadata().unwrap().version, saved.version);
    let expected = mapping();
    for (i, block) in expected.iter().enumerate() {
        let p = page(dir.path(), id, &saved.version, i, 1).unwrap();
        assert_eq!(p.total, 3);
        assert_eq!(p.status, "aligned");
        assert_eq!(p.next_offset, (i < 2).then_some(i + 1));
        assert_eq!(p.original_bytes, ORIGINAL.len());
        assert_eq!(p.translation_bytes, TARGET.len());
        assert_eq!(p.original_sha256, reader_sha256(ORIGINAL.as_bytes()));
        assert_eq!(p.translation_sha256, reader_sha256(TARGET.as_bytes()));
        assert_eq!(p.pairs[0].o, Some(block.source_range));
        assert_eq!(p.pairs[0].t, Some(block.target_range));
        assert_eq!(p.pairs[0].kind, block.kind);
        let json = serde_json::to_value(p).unwrap();
        assert_eq!(json["quality"], "not_certified");
        assert_eq!(json["basis"], "translation_blocks");
        assert_eq!(json["pairs"][0]["relation"], "matched");
        assert!(json["translation_chapter"].is_null());
    }
    for version in ["any", "missing", &saved.sha256] {
        assert!(page(dir.path(), id, version, 0, 1).is_err());
    }
    for (offset, limit) in [(4, 1), (usize::MAX, 1), (0, 0), (0, 257)] {
        assert!(page(dir.path(), id, &saved.version, offset, limit).is_err());
    }
    assert!(alignment(
        dir.path(),
        &id.item_id,
        &id.revision_id,
        &id.original_chapter,
        &saved.version,
        0,
        1,
        Sensitivity::Public
    )
    .is_err());
    assert!(alignment(
        dir.path(),
        &id.item_id,
        &id.revision_id,
        "other-chapter",
        &saved.version,
        0,
        1,
        Sensitivity::Private
    )
    .is_err());
    let renamed = revise_item_metadata(
        dir.path(),
        &id.item_id,
        ItemMetadataUpdate {
            expected_revision_id: id.revision_id.clone(),
            title: "New synthetic title".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let mut current = id.clone();
    current.revision_id = renamed.revision_id;
    let meta = metadata(
        dir.path(),
        &current.item_id,
        &current.revision_id,
        &current.original_chapter,
        Sensitivity::Private,
    )
    .unwrap()
    .unwrap();
    assert_ne!(meta.version, saved.version);
    assert!(page(dir.path(), &current, &saved.version, 0, 1).is_err());
    assert_eq!(
        page(dir.path(), &current, &meta.version, 0, 256)
            .unwrap()
            .pairs
            .len(),
        3
    );
    assert_eq!(fs::read(path(dir.path(), id)).unwrap(), bytes);
    assert!(!path(dir.path(), &current).exists());
    let corrupt = path(dir.path(), &current);
    fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    fs::write(corrupt, b"{}").unwrap();
    assert!(page(dir.path(), &current, &meta.version, 0, 1).is_err());
}

#[test]
fn malformed_maps_reject_publication_and_all_read_paths() {
    let (dir, s) = fixture();
    for case in 0..10 {
        let mut blocks = mapping();
        match case {
            0 => {
                blocks.pop();
            }
            1 => blocks[1].id = blocks[0].id.clone(),
            2 => blocks.swap(0, 1),
            3 => blocks[0].source_range[1] -= 1,
            4 => blocks[0].kind = paragraphs::Kind::Paragraph,
            5 => blocks[0].target_range[1] = 3, // inside Chinese UTF-8
            6 => blocks[1].target_range[0] += 1,
            7 => blocks[2].target_range[1] -= 1,
            8 => blocks[2].target_range[1] = usize::MAX,
            _ => blocks.clear(),
        }
        assert!(publish_mapped(
            dir.path(),
            s.identity.clone(),
            provenance(),
            TARGET.into(),
            blocks,
            Sensitivity::Private
        )
        .is_err());
        assert!(!path(dir.path(), &s.identity).exists());
    }
    let saved = publish_mapped(
        dir.path(),
        s.identity.clone(),
        provenance(),
        TARGET.into(),
        mapping(),
        Sensitivity::Private,
    )
    .unwrap();
    let file = path(dir.path(), &s.identity);
    let original = fs::read(&file).unwrap();
    for case in 0..4 {
        let mut p: Projection = serde_json::from_slice(&original).unwrap();
        match case {
            0 => p.blocks = None,
            1 => p.blocks.as_mut().unwrap()[1].id = "block-0".into(),
            2 => p.identity.source_sha256 = "0".repeat(64),
            _ => p.blocks.as_mut().unwrap()[1].source_range[0] += 1,
        }
        fs::write(&file, serde_json::to_vec(&p).unwrap()).unwrap();
        let id = &s.identity;
        assert!(metadata(
            dir.path(),
            &id.item_id,
            &id.revision_id,
            &id.original_chapter,
            Sensitivity::Private
        )
        .is_err());
        assert!(page(dir.path(), id, &saved.version, 0, 1).is_err());
        assert!(chunk(
            dir.path(),
            &id.item_id,
            &id.revision_id,
            &id.original_chapter,
            &saved.version,
            0,
            8192,
            Sensitivity::Private
        )
        .is_err());
    }
}
