use memex_core::Sensitivity;
use memex_query::{IndexedDocument, ItemSummary, Query, QueryIndex, QueryPage, SortField};

#[test]
fn effective_dates_interleave_and_ignore_import_clocks_with_stable_pages() {
    use memex_core::bibliographic::{EffectiveCaptureOrigin, EffectiveDateBasis};
    let mut documents = Vec::new();
    for (id, date, basis, capture) in [
        ("year", Some("2024"), EffectiveDateBasis::Published, None),
        ("month", Some("2024-01"), EffectiveDateBasis::Published, None),
        ("a", Some("2024-01-01"), EffectiveDateBasis::Published, Some("2020-01-01T00:00:00Z")),
        ("z", Some("2024-01-01"), EffectiveDateBasis::Captured, Some("2023-12-31T16:00:00Z")),
        ("b", Some("2024-01-01"), EffectiveDateBasis::InheritedFromParent, Some("2020-01-01T08:00:00+08:00")),
        ("missing-capture", Some("2024-01-01"), EffectiveDateBasis::Published, None),
        ("early", Some("2023-12-31"), EffectiveDateBasis::Published, None),
        ("unknown-a", None, EffectiveDateBasis::Unknown, None),
        ("unknown-z", None, EffectiveDateBasis::Unknown, None),
    ] {
        let mut doc = document(id, "Date fixture", "synthetic", Sensitivity::Public);
        doc.summary.dates.effective_date = date.map(memex_core::PartialDate::parse_legacy);
        doc.summary.dates.effective_date_basis = basis;
        doc.summary.dates.effective_capture_at = capture.map(str::to_owned);
        doc.summary.dates.effective_capture_origin = capture.map(|_| EffectiveCaptureOrigin::ZoteroDateAdded);
        // Deliberately reverse import clocks: neither may affect the new order.
        doc.summary.added_at = Some(if id == "a" { "2099-01-01T00:00:00Z" } else { "1900-01-01T00:00:00Z" }.into());
        documents.push(doc);
    }
    let mut hidden = documents[0].clone();
    hidden.summary.item_id = "hidden".into();
    hidden.summary.sensitivity = Sensitivity::Restricted;
    documents.push(hidden);
    let mut index = QueryIndex::new().unwrap();
    index.replace(&documents).unwrap();
    let mut q = query("");
    q.sort = SortField::PublishedAt;
    let asc = index.query(&q, Sensitivity::Public, 0, 50, None).unwrap();
    assert_eq!(ids(&asc), ["early", "year", "month", "z", "a", "b", "missing-capture", "unknown-a", "unknown-z"]);
    q.descending = true;
    let first = index.query(&q, Sensitivity::Public, 0, 3, None).unwrap();
    assert_eq!(ids(&first), ["z", "a", "b"]);
    let second = index.query(&q, Sensitivity::Public, 3, 50, Some(&first.view)).unwrap();
    assert_eq!(ids(&second), ["missing-capture", "month", "year", "early", "unknown-a", "unknown-z"]);
    let mut changed = documents[0].clone();
    changed.summary.dates.effective_date = Some(memex_core::PartialDate::parse_legacy("2025"));
    let generation = index.generation().to_owned();
    index.apply(&generation, &[changed], &[]).unwrap();
    assert!(index.query(&q, Sensitivity::Public, 3, 50, Some(&first.view)).is_err());
}

fn document(id: &str, title: &str, body: &str, sensitivity: Sensitivity) -> IndexedDocument {
    IndexedDocument {
        summary: ItemSummary {
            item_id: id.into(),
            revision_id: format!("revision_{id}"),
            title: title.into(),
            source: "source_a".into(),
            item_type: "article".into(),
            captured_at: Some("2026-11-01T00:00:00Z".into()),
            published_at: None,
            duration_seconds: None,
            added_at: None,
            origin: None,
            dates: Default::default(),
            sensitivity,
            group_ids: Vec::new(),
            translated_title: None,
            title_source: None,
        },
        content_markdown: body.into(),
    }
}

fn query(text: &str) -> Query {
    Query {
        text: text.into(),
        source: None,
        item_type: None,
        origin: None,
        sort: SortField::Title,
        descending: false,
        group_id: None,
        include_descendants: false,
        unfiled: false,
    }
}

fn ids(page: &QueryPage) -> Vec<&str> {
    page.items.iter().map(|d| d.item_id.as_str()).collect()
}

#[test]
fn inbox_wire_defaults_false_but_rejects_null_and_non_booleans() {
    let mut wire = serde_json::to_value(query("")).unwrap();
    assert!(wire.get("unfiled").is_none());
    assert!(!serde_json::from_value::<Query>(wire.clone()).unwrap().unfiled);
    wire["unfiled"] = true.into();
    assert!(serde_json::from_value::<Query>(wire.clone()).unwrap().unfiled);
    for bad in [serde_json::Value::Null, 1.into(), "true".into()] {
        wire["unfiled"] = bad;
        assert!(serde_json::from_value::<Query>(wire.clone()).is_err());
    }
    wire["unfiled"] = true.into();
    wire["cluster_id"] = "g".into();
    assert!(serde_json::from_value::<Query>(wire).is_err());
}

#[test]
fn literal_search_handles_chinese_short_queries_punctuation_and_unicode() {
    let mut index = QueryIndex::new().unwrap();
    index
        .replace(&[
            document(
                "a",
                "研究人工智能",
                "Rust 中文：研究者👩‍🔬 e\u{301} 100% OR quote\"",
                Sensitivity::Public,
            ),
            document("b", "另一篇", "人 工智能，研究者X。", Sensitivity::Public),
            document("c", "人", "工智能", Sensitivity::Public),
            document(
                "d",
                "人工智能",
                "restricted canary",
                Sensitivity::Restricted,
            ),
        ])
        .unwrap();
    for (text, expected) in [
        ("人工", vec!["a"]),
        ("人工智能", vec!["a"]),
        ("工", vec!["c", "b", "a"]),
        ("研究者👩‍🔬", vec!["a"]),
        ("rUsT", vec!["a"]),
        ("100%", vec!["a"]),
        ("OR", vec!["a"]),
        ("quote\"", vec!["a"]),
        ("e\u{301}", vec!["a"]),
        ("é", vec![]),
        ("智能人", vec![]),
        ("研究 者", vec![]),
        ("\" OR *", vec![]),
        ("資料", vec![]),
    ] {
        let page = index
            .query(&query(text), Sensitivity::Public, 0, 20, None)
            .unwrap();
        assert_eq!(ids(&page), expected, "query={text}");
        assert_eq!(page.total, expected.len() as u64);
    }
}

#[test]
fn counts_facets_and_windows_share_authorized_filters_and_order() {
    let mut index = QueryIndex::new().unwrap();
    let mut a = document("a", "Same", "term", Sensitivity::Public);
    a.summary.captured_at = Some("2026-11-01T00:00:00.000000002Z".into());
    a.summary.added_at = Some("2026-11-02T00:00:00.000000001Z".into());
    let mut b = document("b", "same", "term", Sensitivity::Internal);
    b.summary.source = "source_b".into();
    b.summary.item_type = "book".into();
    b.summary.captured_at = Some("2026-11-01T02:00:00.000000001+02:00".into());
    b.summary.added_at = Some("2026-11-02T02:00:00.000000002+02:00".into());
    let mut c = document("c", "Alpha", "term", Sensitivity::Public);
    c.summary.captured_at = None;
    let mut secret = document("secret", "CANARY", "term", Sensitivity::Private);
    secret.summary.source = "SECRET_SOURCE".into();
    index.replace(&[b, secret, a, c]).unwrap();
    let q = query("term");
    let first = index.query(&q, Sensitivity::Internal, 0, 1, None).unwrap();
    assert_eq!(ids(&first), ["c"]);
    assert_eq!(first.total, 3);
    assert_eq!(first.next_offset, Some(1));
    assert_eq!(
        first
            .sources
            .iter()
            .map(|f| (f.value.as_str(), f.count))
            .collect::<Vec<_>>(),
        [("source_a", 2), ("source_b", 1)]
    );
    assert_eq!(
        first
            .item_types
            .iter()
            .map(|f| (f.value.as_str(), f.count))
            .collect::<Vec<_>>(),
        [("article", 2), ("book", 1)]
    );
    assert!(!serde_json::to_string(&first).unwrap().contains("SECRET"));
    let last = index
        .query(&q, Sensitivity::Internal, 2, 2, Some(&first.view))
        .unwrap();
    assert_eq!(ids(&last), ["b"]);
    assert_eq!(last.next_offset, None);
    assert_eq!(last.sources, first.sources);
    assert!(index
        .query(&q, Sensitivity::Public, 1, 1, Some(&first.view))
        .is_err());
    assert!(index.query(&q, Sensitivity::Internal, 1, 1, None).is_err());
    let mut filtered = q.clone();
    filtered.source = Some("source_b".into());
    assert!(index
        .query(&filtered, Sensitivity::Internal, 0, 1, Some(&first.view))
        .is_err());
    let page = index
        .query(&filtered, Sensitivity::Internal, 0, 5, None)
        .unwrap();
    assert_eq!(ids(&page), ["b"]);
    assert_eq!(page.total, 1);
    assert_eq!(page.sources.len(), 1);
    filtered.item_type = Some("article".into());
    assert_eq!(
        index
            .query(&filtered, Sensitivity::Internal, 0, 5, None)
            .unwrap()
            .total,
        0
    );
    for (field, descending, expected) in [
        (SortField::Title, true, vec!["a", "b", "c"]),
        (SortField::Source, false, vec!["a", "c", "b"]),
        (SortField::ItemType, false, vec!["a", "c", "b"]),
        (SortField::CapturedAt, false, vec!["b", "a", "c"]),
        (SortField::CapturedAt, true, vec!["a", "b", "c"]),
        (SortField::AddedAt, false, vec!["a", "b", "c"]),
        (SortField::AddedAt, true, vec!["b", "a", "c"]),
    ] {
        let mut sorted = q.clone();
        sorted.sort = field;
        sorted.descending = descending;
        assert_eq!(
            ids(&index
                .query(&sorted, Sensitivity::Internal, 0, 5, None)
                .unwrap()),
            expected
        );
    }
}

#[test]
fn atomic_changes_invalidate_windows_and_failed_batches_preserve_snapshot() {
    let mut index = QueryIndex::new().unwrap();
    assert!(index
        .query(&query(""), Sensitivity::Public, 0, 10, None)
        .is_err());
    let mut a = document("a", "old", "旧词", Sensitivity::Public);
    let b = document("b", "B", "删除", Sensitivity::Public);
    index.replace(&[a.clone(), b.clone()]).unwrap();
    let before = index
        .query(&query(""), Sensitivity::Public, 0, 10, None)
        .unwrap();
    let generation = index.generation().to_owned();
    let mut invalid = b.clone();
    invalid.summary.captured_at = Some("yesterday".into());
    a.summary.title = "new".into();
    a.content_markdown = "新词".into();
    assert!(index
        .apply(&generation, &[a.clone(), invalid.clone()], &[])
        .is_err());
    assert_eq!(
        index
            .query(&query(""), Sensitivity::Public, 0, 10, None)
            .unwrap(),
        before
    );
    assert!(index.replace(&[a.clone(), invalid]).is_err());
    assert_eq!(index.generation(), generation);
    assert!(index
        .apply(&generation, &[a.clone(), a.clone()], &[])
        .is_err());
    a.summary.revision_id = "revision_new".into();
    index
        .apply(&generation, &[a.clone()], &["b".into()])
        .unwrap();
    assert!(index
        .query(&query(""), Sensitivity::Public, 0, 10, Some(&before.view))
        .is_err());
    assert!(index.apply(&generation, &[b], &[]).is_err());
    assert_eq!(
        index
            .query(&query("旧词"), Sensitivity::Public, 0, 10, None)
            .unwrap()
            .total,
        0
    );
    let after = index
        .query(&query("新词"), Sensitivity::Public, 0, 10, None)
        .unwrap();
    assert_eq!(ids(&after), ["a"]);
    assert_eq!(after.items[0].revision_id, "revision_new");
    a.summary.sensitivity = Sensitivity::Private;
    index
        .apply(&index.generation().to_owned(), &[a], &[])
        .unwrap();
    assert_eq!(
        index
            .query(&query("新词"), Sensitivity::Public, 0, 10, None)
            .unwrap()
            .total,
        0
    );
    index.invalidate();
    assert!(index
        .query(&query(""), Sensitivity::Restricted, 0, 10, None)
        .is_err());
    assert!(index
        .apply(&index.generation().to_owned(), &[], &[])
        .is_err());
    index.replace(&[]).unwrap();
    assert_eq!(
        index
            .query(&query(""), Sensitivity::Restricted, 0, 10, None)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn random_windows_cross_old_scan_ceiling_without_duplicates() {
    let mut index = QueryIndex::new().unwrap();
    let docs = (0..4103)
        .rev()
        .map(|i| {
            document(
                &format!("item_{i:05}"),
                "相同标题",
                &format!("合成正文 {i} 中文索引"),
                Sensitivity::Public,
            )
        })
        .collect::<Vec<_>>();
    index.replace(&docs).unwrap();
    let q = query("索引");
    let first = index.query(&q, Sensitivity::Public, 0, 100, None).unwrap();
    assert_eq!(first.total, 4103);
    for offset in [4095, 4096, 4097, 4103, 4000] {
        let page = index
            .query(&q, Sensitivity::Public, offset, 100, Some(&first.view))
            .unwrap();
        let expected = (offset..(offset + 100).min(4103))
            .map(|i| format!("item_{i:05}"))
            .collect::<Vec<_>>();
        assert_eq!(ids(&page), expected);
    }
    assert!(index.query(&q, Sensitivity::Public, 0, 201, None).is_err());
    assert!(index.query(&q, Sensitivity::Public, 0, 0, None).is_err());
}

#[test]
fn streaming_failure_restores_snapshot_and_view_while_empty_apply_keeps_generation() {
    let mut index = QueryIndex::new().unwrap();
    index
        .replace(&[document("a", "Original", "retained", Sensitivity::Public)])
        .unwrap();
    let mut other = index.new_shared().unwrap();
    other
        .replace(&[document("b", "Other", "retained", Sensitivity::Private)])
        .unwrap();
    let before = index
        .query(&query("retained"), Sensitivity::Public, 0, 10, None)
        .unwrap();
    let generation = index.generation().to_owned();
    for full in [false, true] {
        let feed = |writer: &mut memex_query::IndexWriter<'_>| -> anyhow::Result<()> {
            writer.remove("a")?;
            writer.upsert(&document("x", "New", "transient", Sensitivity::Public))?;
            anyhow::bail!("synthetic validator failure after writes")
        };
        let failed = if full {
            index.replace_streaming(feed)
        } else {
            index.apply_streaming(&generation, feed)
        };
        assert!(failed.is_err());
        assert_eq!(index.generation(), generation);
        assert_eq!(
            ids(&index
                .query(
                    &query("retained"),
                    Sensitivity::Public,
                    0,
                    10,
                    Some(&before.view)
                )
                .unwrap()),
            ["a"]
        );
        assert_eq!(
            index
                .query(&query("transient"), Sensitivity::Public, 0, 10, None)
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            ids(&other
                .query(&query("retained"), Sensitivity::Private, 0, 10, None)
                .unwrap()),
            ["b"]
        );
    }
    index.apply_streaming(&generation, |_| Ok(())).unwrap();
    assert_eq!(index.generation(), generation);
    index.invalidate();
    assert!(index
        .replace_streaming(|_| anyhow::bail!("synthetic full scan failure"))
        .is_err());
    assert!(index
        .query(&query("retained"), Sensitivity::Public, 0, 10, None)
        .is_err());
    index
        .replace_streaming(|writer| {
            writer.upsert(&document("z", "Recovered", "fresh", Sensitivity::Public))
        })
        .unwrap();
    assert_eq!(
        ids(&index
            .query(&query("fresh"), Sensitivity::Public, 0, 10, None)
            .unwrap()),
        ["z"]
    );
    assert_eq!(
        index
            .query(&query("retained"), Sensitivity::Public, 0, 10, None)
            .unwrap()
            .total,
        0
    );
}
