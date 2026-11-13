use memex_core::*;
use memex_library::{LibraryQuery, LibraryQueryRequest};
use memex_query::{Query, SortField};
use std::{fs, path::Path};

fn ingest(root: &Path, name: &str, sensitivity: Sensitivity) -> IngestReceipt {
    ingest_source(
        root,
        name,
        sensitivity,
        &format!("https://example.invalid/{name}"),
    )
}

fn ingest_source(
    root: &Path,
    name: &str,
    sensitivity: Sensitivity,
    source_url: &str,
) -> IngestReceipt {
    ingest_method(root, name, sensitivity, source_url, "synthetic")
}

fn ingest_method(
    root: &Path,
    name: &str,
    sensitivity: Sensitivity,
    source_url: &str,
    method: &str,
) -> IngestReceipt {
    ingest_metadata(root, name, sensitivity, source_url, method, Default::default())
}

fn ingest_metadata(
    root: &Path,
    name: &str,
    sensitivity: Sensitivity,
    source_url: &str,
    method: &str,
    metadata: ItemMetadata,
) -> IngestReceipt {
    let source_url = source_url.to_owned();
    let capture = VerifiedCapture {
        source: SourceProfile {
            schema_version: 1,
            source_id: name.into(),
            name: name.into(),
            source_kind: "web".into(),
            canonical_uris: vec![],
            evidence_roles: vec![],
            default_sensitivity: sensitivity,
            preferred_extractor: None,
        },
        capture: CaptureEnvelope {
            schema_version: 1,
            adapter_id: "synthetic".into(),
            adapter_version: "1".into(),
            external_capture_id: name.into(),
            source_id: name.into(),
            captured_at: "2026-01-01T00:00:00Z".into(),
            capture_method: method.into(),
            sensitivity,
            route_summary: None,
            title: Some(name.into()),
            source_url: source_url.clone(),
            canonical_url: None,
            artifacts: vec![],
        },
        blobs: vec![VerifiedBlob {
            descriptor: BlobDescriptor {
                schema_version: 1,
                locator: "text".into(),
                media_type: "text/plain".into(),
                byte_size: 1,
                sha256: "synthetic".into(),
                source_url: None,
            },
            blob_id: "sha256:synthetic".into(),
            source_backend_ref: Some("capture-bundle:synthetic".into()),
            bytes: b"x".to_vec(),
        }],
    };
    memex_store::ingest(
        root,
        &capture,
        ItemDraft {
            item_type: "webpage".into(),
            title: name.into(),
            source_url,
            canonical_url: None,
            content_markdown: format!("合成正文：{name}"),
            sensitivity,
            translator_id: "synthetic".into(),
            translator_version: "1".into(),
            metadata,
            attachments: vec![],
        },
    )
    .unwrap()
}

#[test]
fn publication_summary_tracks_rss_current_revision_without_hidden_leaks() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().unwrap();
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let visible = ingest(root.path(), "visible", Sensitivity::Public);
    let hidden = ingest(root.path(), "hidden", Sensitivity::Restricted);
    let request = browse();
    let before = library.query(&request).unwrap();
    let json = serde_json::to_value(&before.items[0]).unwrap();
    assert_eq!(json["published_at"], serde_json::Value::Null);
    assert_eq!(json["duration_seconds"], serde_json::Value::Null);
    assert!(before.items[0].captured_at.is_some());
    let initial = memex_store::read_item_metadata_v2(root.path(), &visible.item_ids[0], Sensitivity::Public).unwrap().unwrap();
    assert!(initial.published_date.is_none() && initial.duration_seconds.is_none());

    for (receipt, name, ceiling) in [(&hidden, "hidden", Sensitivity::Restricted), (&visible, "visible", Sensitivity::Public)] {
        let url = format!("https://example.invalid/{name}");
        let raw = "Fri, 06 Nov 2026 23:00:00 -0800";
        let rss = format!("<rss><item><guid>{name}</guid><link>{url}</link><pubDate>{raw}</pubDate><duration>1:02:03</duration></item></rss>");
        let baseline = fs::read(root.path().join("items").join(&receipt.item_ids[0]).join("item.json")).unwrap();
        memex_store::podcast_metadata::fill_podcast_metadata(root.path(), memex_store::podcast_metadata::PodcastMetadataFill {
            item_id: receipt.item_ids[0].clone(), expected_revision_id: receipt.revision_ids[0].clone(),
            request_id: format!("summary-{name}"),
            source: memex_store::podcast_metadata::PodcastRssSource {
                feed_url: "https://example.invalid/feed".into(), episode_url: url, guid: name.into(),
                rss_sha256: format!("sha256:{:x}", Sha256::digest(rss.as_bytes())),
                source_start_byte: rss.find("<item>").unwrap(), source_end_byte: rss.find("</item>").unwrap() + 7,
                published_date: Some(PartialDate { value: Some("2026-11-06".into()), precision: DatePrecision::Day, raw: Some(raw.into()) }),
                published_raw: Some(raw.into()), published_instant: Some("2026-11-07T07:00:00Z".into()),
                duration_seconds: Some(3723), duration_raw: Some("1:02:03".into()),
            },
        }, rss.as_bytes(), ceiling).unwrap();
        assert_eq!(baseline, fs::read(root.path().join("items").join(&receipt.item_ids[0]).join("item.json")).unwrap());
        if name == "hidden" {
            assert_eq!(library.query(&request).unwrap(), before);
            assert!(memex_store::read_item_metadata_v2(root.path(), &receipt.item_ids[0], Sensitivity::Public).unwrap().is_none());
        }
    }
    let after = library.query(&request).unwrap();
    assert_ne!(after.view, before.view);
    assert_eq!(after.total, 1);
    assert_eq!(after.items[0].published_at.as_deref(), Some("2026-11-06"));
    assert_eq!(after.items[0].duration_seconds, Some(3723));
    assert_ne!(after.items[0].revision_id, visible.revision_ids[0]);
    let metadata = memex_store::read_item_metadata_v2(root.path(), &visible.item_ids[0], Sensitivity::Public).unwrap().unwrap();
    assert_eq!(metadata.item.revision_id, after.items[0].revision_id);
    assert_eq!(metadata.published_date.unwrap().value.as_deref(), Some("2026-11-06"));
    assert_eq!(metadata.duration_seconds, Some(3723));
    assert_eq!(library.query(&request).unwrap(), after);
}

#[test]
fn publication_summary_keeps_partial_and_unknown_dates_null() {
    let root = tempfile::tempdir().unwrap();
    let provenance = MetadataProvenance { origin: MetadataOrigin::IntakeProvided, provider_id: "synthetic".into(), provider_version: "1".into(), source_refs: vec![] };
    for (name, raw, expected) in [("year", "2025", None), ("month", "2025-02", None), ("unknown", "circa 2025", None), ("day", "2025-02-03", Some("2025-02-03")), ("stamp", "2025-02-03T23:00:00-08:00", Some("2025-02-03"))] {
        let receipt = ingest_metadata(root.path(), name, Sensitivity::Public, &format!("https://example.invalid/{name}"), "synthetic", ItemMetadata {
            published_date: Some(Sourced { value: raw.into(), provenance: provenance.clone() }), ..Default::default()
        });
        let mut library = LibraryQuery::new(root.path()).unwrap();
        let mut request = browse(); request.query.text = name.into();
        let page = library.query(&request).unwrap();
        assert_eq!(page.items[0].item_id, receipt.item_ids[0]);
        assert_eq!(page.items[0].published_at.as_deref(), expected);
        assert!(page.items[0].duration_seconds.is_none());
    }
    let metadata = ItemMetadata {
        published_partial_date: Some(Sourced { value: PartialDate::parse_legacy("2025"), provenance: provenance.clone() }),
        published_date: Some(Sourced { value: "2025-02-03".into(), provenance }), ..Default::default()
    };
    assert_eq!(metadata.published_day(), None, "typed partial date suppresses full legacy fallback");
}

fn browse() -> LibraryQueryRequest {
    LibraryQueryRequest {
        query: Query {
            text: "合成".into(),
            source: None,
            item_type: None,
            origin: None,
            sort: SortField::Title,
            descending: false,
            group_id: None,
            include_descendants: false,
            unfiled: false,
        },
        max_sensitivity: Sensitivity::Public,
        offset: 0,
        limit: 1,
        expected_view: None,
    }
}

#[test]
fn origin_filter_uses_creation_receipts_and_preserves_paging_facets_and_edits() {
    let root = tempfile::tempdir().unwrap();
    let url = "https://example.invalid/same-source";
    let capture = ingest_method(
        root.path(),
        "B",
        Sensitivity::Public,
        url,
        "browser_snapshot_v1",
    );
    let imported = ingest_method(
        root.path(),
        "A",
        Sensitivity::Public,
        url,
        "owner_authorized_offline_import",
    );
    ingest_method(
        root.path(),
        "C",
        Sensitivity::Public,
        url,
        "legacy_normalized_jsonl_v0",
    );
    ingest_method(
        root.path(),
        "D",
        Sensitivity::Private,
        url,
        "safe_http_fetch",
    );
    ingest_method(
        root.path(),
        "E",
        Sensitivity::Public,
        url,
        "unknown_adapter_method",
    );
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let mut request = browse();
    assert_eq!(library.query(&request).unwrap().total, 4);
    request.query.origin = Some(ItemOrigin::Import);
    let first = library.query(&request).unwrap();
    assert_eq!(first.total, 2);
    assert_eq!(first.sources[0].count, 2);
    assert_eq!(first.item_types[0].count, 2);
    assert_eq!(first.items[0].item_id, imported.item_ids[0]);
    request.offset = first.next_offset.unwrap();
    request.expected_view = Some(first.view);
    assert_eq!(library.query(&request).unwrap().items[0].title, "C");
    request.query.origin = Some(ItemOrigin::Capture);
    assert_eq!(
        library.query(&request).unwrap_err().to_string(),
        "stale_query_view"
    );
    request.offset = 0;
    request.expected_view = None;
    let page = library.query(&request).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].item_id, capture.item_ids[0]);
    assert_eq!(page.items[0].origin, Some(ItemOrigin::Capture));
    request.max_sensitivity = Sensitivity::Private;
    assert_eq!(library.query(&request).unwrap().total, 2);
    memex_store::revise_item_metadata(
        root.path(),
        &capture.item_ids[0],
        memex_store::ItemMetadataUpdate {
            expected_revision_id: capture.revision_ids[0].clone(),
            title: "F".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    request.max_sensitivity = Sensitivity::Public;
    let page = library.query(&request).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].title, "F");
    assert_eq!(page.items[0].origin, Some(ItemOrigin::Capture));
    request.query.origin = None;
    assert_eq!(
        library.query(&request).unwrap().total,
        4,
        "unknown origin remains in all items"
    );
}

#[test]
fn native_zotero_source_facets_group_items_without_changing_source_identity() {
    let root = tempfile::tempdir().unwrap();
    let a = ingest_source(
        root.path(),
        "A",
        Sensitivity::Public,
        "urn:zotero:group:7:item:ABCDEFGH",
    );
    ingest_source(
        root.path(),
        "B",
        Sensitivity::Public,
        "urn:zotero:group:7:item:12345678",
    );
    ingest_source(
        root.path(),
        "C",
        Sensitivity::Private,
        "urn:zotero:group:7:item:PRIVATE1",
    );
    ingest_source(
        root.path(),
        "D",
        Sensitivity::Public,
        "urn:zotero:group:19:item:ABCDEFGH",
    );
    ingest_source(
        root.path(),
        "E",
        Sensitivity::Public,
        "urn:other:group:7:item:ABCDEFGH",
    );
    ingest_source(
        root.path(),
        "F",
        Sensitivity::Public,
        "urn:zotero:group:7:item:broken:extra",
    );
    ingest(root.path(), "G", Sensitivity::Public);
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let page = library.query(&browse()).unwrap();
    let counts: std::collections::BTreeMap<_, _> = page
        .sources
        .iter()
        .map(|f| (f.value.as_str(), f.count))
        .collect();
    assert_eq!(counts.len(), 5);
    assert_eq!(counts["urn:zotero:group:7"], 2);
    assert_eq!(counts["urn:zotero:group:19"], 1);
    assert_eq!(counts["https://example.invalid"], 1);
    assert_eq!(counts["urn:other:group:7:item:ABCDEFGH"], 1);
    assert_eq!(counts["urn:zotero:group:7:item:broken:extra"], 1);
    let mut filtered = browse();
    filtered.query.source = Some("urn:zotero:group:7".into());
    assert_eq!(library.query(&filtered).unwrap().total, 2);
    filtered.max_sensitivity = Sensitivity::Private;
    assert_eq!(library.query(&filtered).unwrap().total, 3);
    let original =
        memex_library::read_entry(root.path(), &a.item_ids[0], Sensitivity::Public).unwrap();
    assert_eq!(original.source_url, "urn:zotero:group:7:item:ABCDEFGH");
    assert_eq!(original.revision_id, a.revision_ids[0]);
}

#[test]
fn store_writes_refresh_pages_without_new_writer_markers() {
    let root = tempfile::tempdir().unwrap();
    let a = ingest(root.path(), "A", Sensitivity::Public);
    ingest(root.path(), "Z-private", Sensitivity::Private);
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let first = library.query(&browse()).unwrap();
    assert_eq!(first.total, 1);
    assert_eq!(first.items[0].item_id, a.item_ids[0]);
    assert_eq!(
        first.items[0].added_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(first.sources[0].value, "https://example.invalid");
    assert_eq!(first.sources[0].count, 1);
    assert_eq!(first.item_types[0].count, 1);
    assert_eq!(library.query(&browse()).unwrap().view, first.view);

    let b = ingest(root.path(), "B", Sensitivity::Public);
    let mut next = browse();
    next.offset = 1;
    next.expected_view = Some(first.view);
    assert_eq!(
        library.query(&next).unwrap_err().to_string(),
        "stale_query_view"
    );
    let refreshed = library.query(&browse()).unwrap();
    assert_eq!(refreshed.total, 2);
    assert_eq!(refreshed.sources[0].count, 2);
    next.expected_view = Some(refreshed.view.clone());
    assert_eq!(
        library.query(&next).unwrap().items[0].item_id,
        b.item_ids[0]
    );

    // The existing metadata writer knows nothing about this query service.
    let revised = memex_store::revise_item_metadata(
        root.path(),
        &a.item_ids[0],
        memex_store::ItemMetadataUpdate {
            expected_revision_id: a.revision_ids[0].clone(),
            title: "C".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        library.query(&next).unwrap_err().to_string(),
        "stale_query_view"
    );
    let current = library.query(&browse()).unwrap();
    assert_eq!(current.items[0].item_id, b.item_ids[0]);
    next.expected_view = Some(current.view.clone());
    let last = library.query(&next).unwrap();
    assert_eq!(last.items[0].title, "C");
    assert_eq!(last.items[0].revision_id, revised.revision_id);

    let mut private = browse();
    private.max_sensitivity = Sensitivity::Private;
    let private_page = library.query(&private).unwrap();
    assert_eq!(private_page.total, 3);
    next.expected_view = Some(private_page.view);
    assert_eq!(
        library.query(&next).unwrap_err().to_string(),
        "stale_query_view"
    );
    let mut restarted = LibraryQuery::new(root.path()).unwrap();
    next.expected_view = Some(current.view);
    assert_eq!(
        restarted.query(&next).unwrap_err().to_string(),
        "stale_query_view"
    );
}

#[test]
fn corrupted_canonical_never_falls_back_to_a_previously_valid_index() {
    let root = tempfile::tempdir().unwrap();
    let receipt = ingest(root.path(), "A", Sensitivity::Public);
    for name in ["B", "C", "D", "E"] {
        ingest(root.path(), name, Sensitivity::Public);
    }
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let first = library.query(&browse()).unwrap();
    let revision_path = root
        .path()
        .join("items")
        .join(&receipt.item_ids[0])
        .join("revisions")
        .join(format!("{}.json", receipt.revision_ids[0]));
    let original = fs::read(&revision_path).unwrap();
    fs::write(&revision_path, b"{}").unwrap();
    // Warm queries no longer re-read every item (out-of-band edits that bypass the Store
    // writer are not watched); every index build still verifies the whole graph, including
    // items outside the returned window, and a failure clears the index instead of serving it.
    let mut restarted = LibraryQuery::new(root.path()).unwrap();
    for _ in 0..2 {
        let error = restarted.query(&browse()).unwrap_err().to_string();
        assert_eq!(error, "library_query_failed");
        assert!(!error.contains(&receipt.item_ids[0]));
        assert_eq!(
            restarted
                .group_tree(Sensitivity::Public, 0, 500, None)
                .unwrap_err()
                .to_string(),
            "library_query_failed"
        );
    }
    fs::write(&revision_path, original).unwrap();
    let rebuilt = restarted.query(&browse()).unwrap();
    assert_eq!(rebuilt.total, 5);
    assert_eq!(rebuilt.items, first.items);
    assert_ne!(rebuilt.view, first.view);
    assert_eq!(library.query(&browse()).unwrap().items, first.items);

    // A changed canonical head must invalidate already-warm views at every
    // ceiling, not just fail a new process's initial index build.
    for ceiling in [Sensitivity::Public, Sensitivity::Internal, Sensitivity::Private, Sensitivity::Restricted] {
        let mut request = browse();
        request.max_sensitivity = ceiling;
        library.query(&request).unwrap();
        library.group_tree(ceiling, 0, 500, None).unwrap();
    }
    memex_store::revise_item_metadata(
        root.path(),
        &receipt.item_ids[0],
        memex_store::ItemMetadataUpdate {
            expected_revision_id: receipt.revision_ids[0].clone(),
            title: "Changed".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let head = root
        .path()
        .join("items")
        .join(&receipt.item_ids[0])
        .join("current.json");
    let original_head = fs::read(&head).unwrap();
    fs::write(&head, b"{}").unwrap();
    for ceiling in [Sensitivity::Public, Sensitivity::Internal, Sensitivity::Private, Sensitivity::Restricted] {
        assert_eq!(
            library
                .group_tree(ceiling, 0, 500, None)
                .unwrap_err()
                .to_string(),
            "library_query_failed"
        );
        let mut request = browse();
        request.max_sensitivity = ceiling;
        assert_eq!(
            library.query(&request).unwrap_err().to_string(),
            "library_query_failed"
        );
    }
    fs::write(head, original_head).unwrap();
    assert_eq!(library.query(&browse()).unwrap().total, 5);
}

#[test]
fn all_ceiling_views_keep_hidden_changes_out_of_pages_and_tokens() {
    let root = tempfile::tempdir().unwrap();
    let levels = [Sensitivity::Public, Sensitivity::Internal, Sensitivity::Private, Sensitivity::Restricted];
    let receipts: Vec<_> = levels.iter().enumerate()
        .map(|(i, level)| ingest(root.path(), &format!("A{i}"), *level)).collect();
    let mut library = LibraryQuery::new(root.path()).unwrap();
    let mut before = Vec::new();
    // Build all four views; filtered totals/facets precede the one-item page.
    for (i, level) in levels.iter().enumerate() {
        let mut r = browse(); r.max_sensitivity = *level;
        let page = library.query(&r).unwrap();
        assert_eq!(page.total, (i + 1) as u64);
        assert_eq!(page.sources[0].count, (i + 1) as u64);
        assert_eq!(page.item_types[0].count, (i + 1) as u64);
        for offset in 0..=i {
            r.offset = offset; r.expected_view = Some(page.view.clone());
            assert_eq!(library.query(&r).unwrap().items[0].item_id, receipts[offset].item_ids[0]);
        }
        before.push(page);
    }
    memex_store::revise_item_metadata(root.path(), &receipts[3].item_ids[0],
        memex_store::ItemMetadataUpdate {
            expected_revision_id: receipts[3].revision_ids[0].clone(),
            title: "Z-hidden-change".into(), manual_tags: vec![],
        }).unwrap();
    // Updating the shared physical index must not rotate lower view identities.
    let mut high = browse(); high.max_sensitivity = Sensitivity::Restricted;
    assert_ne!(library.query(&high).unwrap().view, before[3].view);
    for (i, level) in levels[..3].iter().enumerate() {
        let mut r = browse(); r.max_sensitivity = *level; r.expected_view = Some(before[i].view.clone());
        assert_eq!(library.query(&r).unwrap(), before[i]);
        r.expected_view = None; r.query.text = "Z-hidden-change".into();
        assert_eq!(library.query(&r).unwrap().total, 0);
        r.query.text.clear();
        assert_eq!(library.query(&r).unwrap().total, (i + 1) as u64);
    }
    high.query.text = "Z-hidden-change".into();
    assert_eq!(library.query(&high).unwrap().total, 1);
}
