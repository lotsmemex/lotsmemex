//! Synthetic paired Evidence and complete paragraph index, through real Store APIs.
use anyhow::Result;
use memex_core::*;
use memex_store::reader::{self, paragraphs::*};
use std::path::Path;

pub fn seed(library: &Path) -> Result<reader::ReaderDocument> {
    seed_named(library, "paragraph-book")
}

pub fn seed_named(library: &Path, name: &str) -> Result<reader::ReaderDocument> {
    let original = "# First light\n\nOne morning.\n\nTwo birds.\n\nThree trees.\n\nUnknown.\n\n";
    let translation = "# 晨光\n\n清晨。\n\n两个片段。\n\n鸟和树。\n\n[^1]: 合成注释。\n\n未知。\n";
    seed_text(library, original, translation, true, name, false)
}

pub fn paired_chapters(library: &Path) -> Result<reader::ReaderDocument> {
    seed_text(
        library,
        "# First\n\nA morning.\n\n",
        "# 第一章\n\n清晨。\n\n",
        false,
        "paired-chapters",
        true,
    )
}

pub fn fragmented(library: &Path) -> Result<reader::ReaderDocument> {
    seed_text(library,
        "# Opening\n\nSection A 1\n\nThey called her Mira\n\nat the station.\n\nA bright morning.\n",
        "# 开篇\n\n车站有人叫她米拉。\n\n[^1]: ① 合成注释。\n连续行\n\n    断\n\n    行。\n\n明亮的清晨。\n",
        false, "paragraph-book", false)
}

fn seed_text(
    library: &Path,
    original: &str,
    translation: &str,
    indexed: bool,
    name: &str,
    paired_appendix: bool,
) -> Result<reader::ReaderDocument> {
    let appendix = "# Appendix\n\nOriginal only.\n";
    let bodies = [
        format!("{original}{appendix}"),
        format!(
            "{translation}{}",
            if paired_appendix {
                "# 附录\n\n附录正文。\n"
            } else {
                ""
            }
        ),
    ];
    let editions = bodies
        .iter()
        .enumerate()
        .map(|(side, body)| {
            let mut chapters = vec![reader::Chapter {
                key: "chapter-1".into(),
                title: if side == 0 { "First light" } else { "晨光" }.into(),
                level: 1,
                start: 0,
                end: if side == 0 {
                    original.len()
                } else {
                    translation.len()
                },
                counterpart: Some("chapter-1".into()),
            }];
            if side == 0 || paired_appendix {
                chapters.push(reader::Chapter {
                    key: "appendix".into(),
                    title: "Appendix".into(),
                    level: 1,
                    start: if side == 0 {
                        original.len()
                    } else {
                        translation.len()
                    },
                    end: body.len(),
                    counterpart: paired_appendix.then(|| "appendix".into()),
                });
            }
            reader::Edition {
                side: if side == 0 { "original" } else { "translation" }.into(),
                language: if side == 0 { "en" } else { "zh" }.into(),
                sha256: reader::reader_sha256(body.as_bytes()),
                bytes: body.len(),
                chapters,
            }
        })
        .collect();
    let manifest = reader::BookManifest {
        schema_version: 1,
        alignment: "declared_chapter_order_not_paragraph_alignment".into(),
        editions,
    };
    let blobs: Vec<_> = [
        (
            "original.md",
            "text/markdown",
            bodies[0].as_bytes().to_vec(),
        ),
        (
            "translation.md",
            "text/markdown",
            bodies[1].as_bytes().to_vec(),
        ),
        (
            "manifest.json",
            reader::BOOK_MANIFEST,
            serde_json::to_vec(&manifest)?,
        ),
    ]
    .into_iter()
    .map(|(locator, media, bytes)| {
        let sha256 = reader::reader_sha256(&bytes);
        VerifiedBlob {
            descriptor: BlobDescriptor {
                schema_version: 1,
                locator: locator.into(),
                media_type: media.into(),
                byte_size: bytes.len() as u64,
                sha256: sha256.clone(),
                source_url: None,
            },
            blob_id: format!("sha256:{sha256}"),
            source_backend_ref: None,
            bytes,
        }
    })
    .collect();
    let url = format!("https://synthetic.example.invalid/{name}");
    let capture = VerifiedCapture {
        source: SourceProfile {
            schema_version: 1,
            source_id: "synthetic-paragraph-book".into(),
            name: "Synthetic only".into(),
            source_kind: "web".into(),
            canonical_uris: vec![],
            evidence_roles: vec![],
            default_sensitivity: Sensitivity::Public,
            preferred_extractor: None,
        },
        capture: CaptureEnvelope {
            schema_version: 1,
            adapter_id: "mobile-fixture".into(),
            adapter_version: "1".into(),
            external_capture_id: name.into(),
            source_id: "synthetic-paragraph-book".into(),
            captured_at: "2026-01-01T00:00:00Z".into(),
            capture_method: "browser_snapshot_v1".into(),
            sensitivity: Sensitivity::Public,
            route_summary: None,
            title: Some("Synthetic paragraph book".into()),
            source_url: url.clone(),
            canonical_url: None,
            artifacts: blobs.iter().map(|b| b.descriptor.clone()).collect(),
        },
        blobs,
    };
    let receipt = reader::ingest_reader_book(
        library,
        capture,
        ItemDraft {
            item_type: "document".into(),
            title: "Synthetic paragraph book / 双语阅读".into(),
            source_url: url,
            canonical_url: None,
            content_markdown: bodies[1].clone(),
            sensitivity: Sensitivity::Public,
            translator_id: "mobile-fixture".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: vec![],
        },
    )?;
    let item = &receipt.item_ids[0];
    let revision = &receipt.revision_ids[0];
    if !indexed {
        return reader::reader_document(library, item, revision, Sensitivity::Public);
    }
    // Fixture construction uses explicit supplied correspondences, never inference or fractions.
    let o = blocks(original);
    let t = blocks(translation);
    let pairs = vec![
        Pair {
            o: Some(o[0].0),
            t: Some(t[0].0),
            kind: Kind::Heading,
            relation: Relation::Matched,
        },
        Pair {
            o: Some(o[1].0),
            t: Some([t[1].0[0], t[2].0[1]]),
            kind: Kind::Paragraph,
            relation: Relation::Matched,
        },
        Pair {
            o: Some([o[2].0[0], o[3].0[1]]),
            t: Some(t[3].0),
            kind: Kind::Paragraph,
            relation: Relation::Matched,
        },
        Pair {
            o: None,
            t: Some(t[4].0),
            kind: Kind::Note,
            relation: Relation::TranslationOnly,
        },
        Pair {
            o: Some(o[4].0),
            t: None,
            kind: Kind::Paragraph,
            relation: Relation::Unresolved,
        },
        Pair {
            o: None,
            t: Some(t[5].0),
            kind: Kind::Paragraph,
            relation: Relation::Unresolved,
        },
    ];
    save(
        library,
        "missing",
        Index {
            schema_version: 1,
            identity: identity(library, item, revision, "chapter-1", Sensitivity::Public)?,
            algorithm: "chapter-paragraphs-v1".into(),
            input_profile_sha256: reader::reader_sha256(b"synthetic-no-model"),
            pairs,
        },
        Sensitivity::Public,
    )?;
    reader::reader_document(library, item, revision, Sensitivity::Public)
}
