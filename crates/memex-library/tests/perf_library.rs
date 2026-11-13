//! Read-path performance on a synthetic library (ignored by default; run with
//! `MEMEX_BENCH_ITEMS=2000 cargo test -p memex-library --release --test perf_library -- --ignored --nocapture`).
//!
//! Builds N items through the real Store ingest, optionally puts their committed ingest journals
//! back where pre-compaction daemons left them (the live library's state), then times the IPC
//! read paths: `library_query` (limit 5), the first `library_list` page and the group tree.

use memex_core::*;
use memex_library::{LibraryQuery, LibraryQueryRequest};
use memex_query::{Query, SortField};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

fn ingest(root: &Path, index: usize) -> IngestReceipt {
    let name = format!("bench-{index:06}");
    let source_url = format!("https://site{}.example.invalid/{name}", index % 40);
    let capture = VerifiedCapture {
        source: SourceProfile {
            schema_version: 1,
            source_id: "bench".into(),
            name: "bench".into(),
            source_kind: "web".into(),
            canonical_uris: vec![],
            evidence_roles: vec![],
            default_sensitivity: Sensitivity::Public,
            preferred_extractor: None,
        },
        capture: CaptureEnvelope {
            schema_version: 1,
            adapter_id: "synthetic".into(),
            adapter_version: "1".into(),
            external_capture_id: name.clone(),
            source_id: "bench".into(),
            captured_at: "2026-01-01T00:00:00Z".into(),
            capture_method: "synthetic".into(),
            sensitivity: Sensitivity::Public,
            route_summary: None,
            title: Some(name.clone()),
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
            title: format!("Synthetic article {name}"),
            source_url,
            canonical_url: None,
            content_markdown: format!(
                "# {name}\n\n{}",
                "Synthetic paragraph about a harmless benchmark topic. ".repeat(80)
            ),
            sensitivity: Sensitivity::Public,
            translator_id: "synthetic".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: vec![],
        },
    )
    .unwrap()
}

fn request(view: Option<String>) -> LibraryQueryRequest {
    LibraryQueryRequest {
        query: Query {
            text: String::new(),
            source: None,
            item_type: None,
            origin: None,
            sort: SortField::Title,
            descending: false,
            group_id: None,
            include_descendants: false,
            unfiled: false,
        },
        max_sensitivity: Sensitivity::Restricted,
        offset: 0,
        limit: 5,
        expected_view: view,
    }
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    samples[((samples.len() as f64 - 1.0) * p).round() as usize]
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed())
}

/// Moves committed journals back to `catalog/ingest-journal` (pre-compaction layout).
fn restore_legacy_journals(root: &Path) -> usize {
    let archive = root.join("catalog/ingest-journal-committed");
    let Ok(entries) = fs::read_dir(&archive) else {
        return 0;
    };
    let mut moved = 0;
    for entry in entries {
        let entry = entry.unwrap();
        fs::rename(
            entry.path(),
            root.join("catalog/ingest-journal").join(entry.file_name()),
        )
        .unwrap();
        moved += 1;
    }
    moved
}

#[test]
#[ignore = "benchmark: builds a large synthetic library"]
fn library_reads_stay_within_the_ipc_budget() {
    let items: usize = std::env::var("MEMEX_BENCH_ITEMS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut receipts = Vec::with_capacity(items);
    let mut ingest_times = Vec::with_capacity(items);
    let started = Instant::now();
    for index in 0..items {
        let (receipt, elapsed) = time(|| ingest(&root, index));
        receipts.push(receipt);
        ingest_times.push(elapsed);
    }
    let tail = &mut ingest_times[items.saturating_sub(20)..].to_vec();
    println!(
        "built {items} items in {:?}; ingest (writer lock held) first p50 {:?}, last-20 p50 {:?}",
        started.elapsed(),
        percentile(&mut ingest_times[..items.min(20)].to_vec(), 0.5),
        percentile(tail, 0.5)
    );
    // Every item in a folder, so group counts and facets do real work.
    let mut group = memex_store::edit_groups(
        &root,
        "bench-root",
        memex_store::GroupEditOp::CreateRoot {
            name: "Bench".into(),
            sensitivity: Sensitivity::Public,
        },
        Sensitivity::Restricted,
    )
    .unwrap();
    for (n, chunk) in receipts.chunks(50).enumerate() {
        group = memex_store::edit_groups(
            &root,
            &format!("bench-members-{n}"),
            memex_store::GroupEditOp::AddMembers {
                group_id: group.group_id.clone(),
                expected_version: group.version.clone(),
                item_ids: chunk.iter().map(|r| r.item_ids[0].clone()).collect(),
            },
            Sensitivity::Restricted,
        )
        .unwrap();
    }
    let legacy = restore_legacy_journals(&root);
    println!("retained committed journals restored to the legacy location: {legacy}");

    // Startup: the daemon's recovery pass (also migrates legacy journals once).
    let (recovered, startup) = time(|| memex_store::recover_pending_ingests(&root).unwrap());
    println!("startup recovery: {startup:?} (prepared recovered: {recovered})");

    let mut queries = LibraryQuery::new(&root).unwrap();
    let (first, cold) = time(|| queries.query(&request(None)).unwrap());
    assert_eq!(first.total as usize, items);
    println!("library_query cold (index build): {cold:?}");
    let mut samples: Vec<Duration> = (0..30)
        .map(|_| time(|| queries.query(&request(None)).unwrap()).1)
        .collect();
    let warm_p95 = percentile(&mut samples, 0.95);
    println!(
        "library_query warm p50 {:?} p95 {warm_p95:?}",
        percentile(&mut samples, 0.5)
    );

    let mut lists: Vec<Duration> = (0..20)
        .map(|_| time(|| memex_store::page_all_item_metadata(&root, None, 50).unwrap()).1)
        .collect();
    let list_p95 = percentile(&mut lists, 0.95);
    println!("library_list p95 {list_p95:?}");
    let mut trees: Vec<Duration> = (0..20)
        .map(|_| {
            time(|| {
                queries
                    .group_tree(Sensitivity::Restricted, 0, 100, None)
                    .unwrap()
            })
            .1
        })
        .collect();
    let tree_p95 = percentile(&mut trees, 0.95);
    println!("group tree p95 {tree_p95:?}");

    // After a write (a metadata edit), the next query refreshes only what changed.
    let mut after_write = Vec::new();
    for receipt in receipts.iter().take(10) {
        memex_store::revise_item_metadata(
            &root,
            &receipt.item_ids[0],
            memex_store::ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "Edited synthetic title".into(),
                manual_tags: vec!["bench".into()],
            },
        )
        .unwrap();
        after_write.push(time(|| queries.query(&request(None)).unwrap()).1);
    }
    let write_p95 = percentile(&mut after_write, 0.95);
    println!("library_query right after a write p95 {write_p95:?}");

    let bound = if items <= 2_000 {
        Duration::from_millis(150)
    } else {
        Duration::from_secs(1)
    };
    for (name, value) in [
        ("query", warm_p95),
        ("list", list_p95),
        ("tree", tree_p95),
        ("query after write", write_p95),
    ] {
        assert!(value < bound, "{name} p95 {value:?} exceeds {bound:?}");
    }
}

/// The incremental index (generation token + head stamps) follows every Store write path.
#[test]
fn query_index_follows_store_writes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let first = ingest(&root, 1);
    let mut queries = LibraryQuery::new(&root).unwrap();
    assert_eq!(queries.query(&request(None)).unwrap().total, 1);
    // Unchanged Store: served from the index.
    assert_eq!(queries.query(&request(None)).unwrap().total, 1);

    let second = ingest(&root, 2);
    assert_eq!(queries.query(&request(None)).unwrap().total, 2);

    memex_store::revise_item_metadata(
        &root,
        &first.item_ids[0],
        memex_store::ItemMetadataUpdate {
            expected_revision_id: first.revision_ids[0].clone(),
            title: "Renamed article".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let page = queries.query(&request(None)).unwrap();
    let titles = serde_json::to_string(&page).unwrap();
    assert!(titles.contains("Renamed article"), "{titles}");

    memex_store::trash_item(
        &root,
        &second.item_ids[0],
        &second.revision_ids[0],
        "duplicate",
        None,
        None,
    )
    .unwrap();
    assert_eq!(queries.query(&request(None)).unwrap().total, 1);
    memex_store::restore_item(&root, &second.item_ids[0]).unwrap();
    assert_eq!(queries.query(&request(None)).unwrap().total, 2);
}
