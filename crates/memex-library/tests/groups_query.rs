use memex_core::*;
use memex_library::{LibraryQuery, LibraryQueryRequest};
use memex_query::{Query, QueryPage, SortField};
use memex_store::{edit_groups, read_group_tree, GroupEditOp, GroupEditReceipt};
use std::path::Path;

fn ingest(root: &Path, name: &str, sensitivity: Sensitivity) -> IngestReceipt {
    let source_url = format!("https://example.invalid/{name}");
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
            capture_method: "synthetic".into(),
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
            metadata: Default::default(),
            attachments: vec![],
        },
    )
    .unwrap()
}

fn request(
    group_id: Option<&str>,
    include_descendants: bool,
    text: &str,
    ceiling: Sensitivity,
    offset: usize,
    view: Option<String>,
) -> LibraryQueryRequest {
    LibraryQueryRequest {
        query: Query {
            text: text.into(),
            source: None,
            item_type: None,
            origin: None,
            sort: SortField::Title,
            descending: false,
            group_id: group_id.map(str::to_owned),
            include_descendants,
            unfiled: false,
        },
        max_sensitivity: ceiling,
        offset,
        limit: 1,
        expected_view: view,
    }
}

fn all_ids(
    queries: &mut LibraryQuery,
    group: Option<&str>,
    descendants: bool,
    ceiling: Sensitivity,
) -> (u64, Vec<String>) {
    let mut out = Vec::new();
    let first = queries
        .query(&request(group, descendants, "", ceiling, 0, None))
        .unwrap();
    let mut page: QueryPage = first.clone();
    loop {
        out.extend(page.items.iter().map(|item| item.item_id.clone()));
        let Some(next) = page.next_offset else { break };
        page = queries
            .query(&request(
                group,
                descendants,
                "",
                ceiling,
                next,
                Some(first.view.clone()),
            ))
            .unwrap();
    }
    out.sort();
    (first.total, out)
}

fn edit(root: &Path, request_id: &str, op: GroupEditOp, ceiling: Sensitivity) -> GroupEditReceipt {
    edit_groups(root, request_id, op, ceiling).unwrap()
}

#[test]
fn inbox_uses_visible_memberships_before_paging_and_never_revives_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let public = Sensitivity::Public;
    let high = Sensitivity::Restricted;
    let a = ingest(root, "alpha", public).item_ids[0].clone();
    let b = ingest(root, "beta", public).item_ids[0].clone();
    ingest(root, "secret", high);
    let private = edit(root, "private", GroupEditOp::CreateRoot {
        name: "Private".into(), sensitivity: Sensitivity::Private,
    }, high);
    let mut queries = LibraryQuery::new(root).unwrap();
    let mut inbox = request(None, false, "", public, 0, None);
    inbox.query.unfiled = true;
    let before = queries.query(&inbox).unwrap();
    assert_eq!(before.total, 2);
    assert_eq!(before.items[0].item_id, a);
    assert_eq!(before.sources.iter().map(|f| f.count).sum::<u64>(), 2);
    assert_eq!(before.item_types[0].count, 2);
    assert!(before.groups.is_empty());
    inbox.query.text = "beta".into();
    assert_eq!(queries.query(&inbox).unwrap().items[0].item_id, b);
    inbox.query.source = Some("alpha".into());
    assert_eq!(queries.query(&inbox).unwrap().total, 0);
    inbox.query.source = None;
    inbox.query.text.clear();
    inbox.query.origin = Some(ItemOrigin::Import);
    assert_eq!(queries.query(&inbox).unwrap().total, 0);
    inbox.query.origin = None;
    inbox.query.unfiled = false;
    inbox.expected_view = Some(before.view.clone());
    assert_eq!(queries.query(&inbox).unwrap_err().to_string(), "stale_query_view");
    inbox.query.unfiled = true;
    inbox.expected_view = None;
    edit(root, "hidden-member", GroupEditOp::AddMembers {
        group_id: private.group_id, expected_version: private.version, item_ids: vec![a.clone()],
    }, high);
    // Hidden-only membership changes cannot invalidate a low-ceiling page.
    inbox.offset = 1;
    inbox.expected_view = Some(before.view.clone());
    assert_eq!(queries.query(&inbox).unwrap().items[0].item_id, b);
    inbox.offset = 0;
    inbox.expected_view = None;
    for (ceiling, total) in [(public, 2), (Sensitivity::Internal, 2), (Sensitivity::Private, 1), (high, 2)] {
        inbox.max_sensitivity = ceiling;
        assert_eq!(queries.query(&inbox).unwrap().total, total);
    }
    inbox.max_sensitivity = public;
    let visible = edit(root, "visible", GroupEditOp::CreateRoot {
        name: "Main".into(), sensitivity: public,
    }, high);
    edit(root, "visible-member", GroupEditOp::AddMembers {
        group_id: visible.group_id.clone(), expected_version: visible.version, item_ids: vec![b.clone()],
    }, high);
    assert_eq!(queries.query(&inbox).unwrap().total, 1);
    inbox.expected_view = Some(before.view);
    assert_eq!(queries.query(&inbox).unwrap_err().to_string(), "stale_query_view");
    inbox.expected_view = None;
    let revision = memex_store::read_item(root, &b).unwrap().1.revision_id;
    memex_store::edit_group_trash(root, &visible.group_id, &b, "trash-b", &revision, None, high).unwrap();
    assert_eq!(queries.query(&inbox).unwrap().total, 1, "removed trash membership is not an Inbox admission");
    let revision = memex_store::read_item(root, &a).unwrap().1.revision_id;
    memex_store::trash_item(root, &a, &revision, "synthetic", None, None).unwrap();
    assert_eq!(queries.query(&inbox).unwrap().total, 0);
    assert_eq!(LibraryQuery::new(root).unwrap().query(&inbox).unwrap().total, 0);
    inbox.query.group_id = Some(visible.group_id);
    assert_eq!(queries.query(&inbox).unwrap_err().to_string(), "invalid_group_query");
    inbox.query.group_id = None;
    inbox.query.include_descendants = true;
    assert_eq!(queries.query(&inbox).unwrap_err().to_string(), "invalid_group_query");
}

#[test]
fn group_selection_counts_and_facets_are_distinct_and_hide_unauthorized_members() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let id = |receipt: IngestReceipt| receipt.item_ids[0].clone();
    let alpha = id(ingest(root, "alpha", Sensitivity::Public));
    let beta = id(ingest(root, "beta", Sensitivity::Public));
    let gamma = id(ingest(root, "gamma", Sensitivity::Public));
    let hidden = id(ingest(root, "hidden", Sensitivity::Private));

    let r = edit(
        root,
        "g1",
        GroupEditOp::CreateRoot {
            name: "Root".into(),
            sensitivity: Sensitivity::Public,
        },
        Sensitivity::Restricted,
    );
    let p = edit(
        root,
        "g2",
        GroupEditOp::CreateFolder {
            parent_id: r.group_id.clone(),
            expected_parent_version: r.version.clone(),
            name: "Parent".into(),
        },
        Sensitivity::Restricted,
    );
    let k = edit(
        root,
        "g3",
        GroupEditOp::CreateFolder {
            parent_id: p.group_id.clone(),
            expected_parent_version: p.version.clone(),
            name: "Child".into(),
        },
        Sensitivity::Restricted,
    );
    let p_version = read_group_tree(root, Sensitivity::Restricted, 0, 500, None)
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.id == p.group_id)
        .unwrap()
        .version;
    let p = edit(
        root,
        "g4",
        GroupEditOp::AddMembers {
            group_id: p.group_id.clone(),
            expected_version: p_version,
            item_ids: vec![alpha.clone(), beta.clone()],
        },
        Sensitivity::Restricted,
    );
    let k = edit(
        root,
        "g5",
        GroupEditOp::AddMembers {
            group_id: k.group_id.clone(),
            expected_version: k.version.clone(),
            item_ids: vec![alpha.clone(), gamma.clone(), hidden.clone()],
        },
        Sensitivity::Restricted,
    );
    let q = edit(
        root,
        "g6",
        GroupEditOp::CreateRoot {
            name: "Private root".into(),
            sensitivity: Sensitivity::Private,
        },
        Sensitivity::Restricted,
    );
    let q = edit(
        root,
        "g7",
        GroupEditOp::AddMembers {
            group_id: q.group_id.clone(),
            expected_version: q.version.clone(),
            item_ids: vec![alpha.clone()],
        },
        Sensitivity::Restricted,
    );

    // Navigation counts: distinct, never a sum of children; hidden excluded.
    let tree = read_group_tree(root, Sensitivity::Public, 0, 500, None).unwrap();
    let counts: Vec<_> = tree
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.item_count, n.subtree_item_count))
        .collect();
    assert_eq!(counts, [("Root", 0, 3), ("Parent", 2, 3), ("Child", 2, 2)]);
    let tree = read_group_tree(root, Sensitivity::Private, 0, 500, None).unwrap();
    let counts: Vec<_> = tree
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.item_count, n.subtree_item_count))
        .collect();
    assert_eq!(
        counts,
        [
            ("Root", 0, 4),
            ("Parent", 2, 4),
            ("Child", 3, 3),
            ("Private root", 1, 1)
        ]
    );

    let mut queries = LibraryQuery::new(root).unwrap();
    let public = Sensitivity::Public;
    let public_tree = queries.group_tree(public, 0, 500, None).unwrap();
    assert_eq!(
        public_tree,
        read_group_tree(root, public, 0, 500, None).unwrap()
    );
    assert_eq!(
        queries
            .group_tree(Sensitivity::Private, 0, 500, None)
            .unwrap(),
        tree
    );
    let page = queries
        .group_tree(public, 1, 1, Some(&public_tree.view))
        .unwrap();
    assert_eq!(page.nodes, public_tree.nodes[1..2]);
    assert_eq!(page.next_offset, Some(2));
    assert_eq!(
        all_ids(&mut queries, Some(&p.group_id), false, public),
        (2, sorted(&[&alpha, &beta]))
    );
    assert_eq!(
        all_ids(&mut queries, Some(&p.group_id), true, public),
        (3, sorted(&[&alpha, &beta, &gamma]))
    );
    assert_eq!(
        all_ids(&mut queries, Some(&k.group_id), false, public),
        (2, sorted(&[&alpha, &gamma]))
    );
    assert_eq!(
        all_ids(&mut queries, Some(&r.group_id), false, public),
        (0, vec![])
    );
    assert_eq!(
        all_ids(&mut queries, Some(&r.group_id), true, Sensitivity::Private),
        (4, sorted(&[&alpha, &beta, &gamma, &hidden]))
    );

    // Facets cover the complete filtered result, not the one-item page.
    let browse = queries
        .query(&request(None, false, "", public, 0, None))
        .unwrap();
    let facets: Vec<_> = browse
        .groups
        .iter()
        .map(|f| (f.name.as_str(), f.count, f.subtree_count))
        .collect();
    assert_eq!(facets, [("Child", 2, 2), ("Parent", 2, 3), ("Root", 0, 3)]);
    let filtered = queries
        .query(&request(None, false, "gamma", public, 0, None))
        .unwrap();
    let facets: Vec<_> = filtered
        .groups
        .iter()
        .map(|f| (f.name.as_str(), f.count, f.subtree_count))
        .collect();
    assert_eq!(facets, [("Child", 1, 1), ("Parent", 0, 1), ("Root", 0, 1)]);
    // Group selection precedes the text filter and facets.
    let scoped = queries
        .query(&request(Some(&p.group_id), false, "gamma", public, 0, None))
        .unwrap();
    assert_eq!(scoped.total, 0);
    assert!(scoped.groups.is_empty());
    let page = queries
        .query(&request(Some(&k.group_id), false, "", public, 0, None))
        .unwrap();
    let alpha_row = page
        .items
        .iter()
        .find(|item| item.item_id == alpha)
        .unwrap();
    let mut expected = vec![p.group_id.clone(), k.group_id.clone()];
    expected.sort();
    assert_eq!(
        alpha_row.group_ids, expected,
        "private root membership is not exposed"
    );

    // Hidden or invalid group selections.
    for (group, descendants, code) in [
        (
            Some(q.group_id.as_str()),
            false,
            "library_resource_not_found",
        ),
        (Some("group_missing"), false, "library_resource_not_found"),
        (None, true, "invalid_group_query"),
    ] {
        let error = queries
            .query(&request(group, descendants, "", public, 0, None))
            .unwrap_err();
        assert_eq!(error.to_string(), code);
    }
    let private = queries
        .query(&request(
            Some(&q.group_id),
            false,
            "",
            Sensitivity::Private,
            0,
            None,
        ))
        .unwrap();
    assert_eq!(private.total, 1);

    // A hidden-only rename keeps the public view; a visible one stales it.
    let first = queries
        .query(&request(Some(&p.group_id), true, "", public, 0, None))
        .unwrap();
    edit(
        root,
        "g8",
        GroupEditOp::Rename {
            group_id: q.group_id.clone(),
            expected_version: q.version.clone(),
            name: "Private renamed".into(),
        },
        Sensitivity::Restricted,
    );
    queries
        .query(&request(
            Some(&p.group_id),
            true,
            "",
            public,
            1,
            Some(first.view.clone()),
        ))
        .unwrap();
    assert_eq!(
        queries.group_tree(public, 0, 500, None).unwrap(),
        public_tree
    );
    edit(
        root,
        "g9",
        GroupEditOp::Rename {
            group_id: k.group_id.clone(),
            expected_version: k.version.clone(),
            name: "Child renamed".into(),
        },
        Sensitivity::Restricted,
    );
    let error = queries
        .query(&request(
            Some(&p.group_id),
            true,
            "",
            public,
            1,
            Some(first.view),
        ))
        .unwrap_err();
    assert_eq!(error.to_string(), "stale_query_view");
    assert_eq!(
        queries
            .group_tree(public, 1, 1, Some(&public_tree.view))
            .unwrap_err()
            .to_string(),
        "stale_group_view"
    );

    // A tree-first read must observe ordinary Store trash/restore, without a
    // preceding list request to refresh it, and counts must remain distinct.
    let revision = memex_store::read_item(root, &beta).unwrap().1.revision_id;
    memex_store::trash_item(root, &beta, &revision, "synthetic", None, None).unwrap();
    let current = queries.group_tree(public, 0, 500, None).unwrap();
    assert_eq!(
        current
            .nodes
            .iter()
            .map(|n| (n.item_count, n.subtree_item_count))
            .collect::<Vec<_>>(),
        [(0, 2), (1, 2), (2, 2)]
    );
    assert_eq!(
        all_ids(&mut queries, Some(&p.group_id), true, public),
        (2, sorted(&[&alpha, &gamma]))
    );
    memex_store::restore_item(root, &beta).unwrap();
    let restored = queries.group_tree(public, 0, 500, None).unwrap();
    assert_eq!(
        restored
            .nodes
            .iter()
            .map(|n| (n.item_count, n.subtree_item_count))
            .collect::<Vec<_>>(),
        [(0, 3), (2, 3), (2, 2)]
    );

    // Removing the last visible membership must clear the page summary too,
    // without relying on a new revision to invalidate the document.
    let before_remove = queries
        .query(&request(None, false, "", public, 0, None))
        .unwrap();
    edit(
        root,
        "g10",
        GroupEditOp::RemoveMembers {
            group_id: p.group_id.clone(),
            expected_version: restored
                .nodes
                .iter()
                .find(|n| n.id == p.group_id)
                .unwrap()
                .version
                .clone(),
            item_ids: vec![beta.clone()],
        },
        Sensitivity::Restricted,
    );
    let removed = queries.group_tree(public, 0, 500, None).unwrap();
    assert_eq!(
        removed
            .nodes
            .iter()
            .map(|n| (n.item_count, n.subtree_item_count))
            .collect::<Vec<_>>(),
        [(0, 2), (1, 2), (2, 2)]
    );
    assert_eq!(
        all_ids(&mut queries, Some(&p.group_id), true, public),
        (2, sorted(&[&alpha, &gamma]))
    );
    let current = queries
        .query(&request(None, false, "beta", public, 0, None))
        .unwrap();
    assert!(current
        .items
        .iter()
        .find(|i| i.item_id == beta)
        .unwrap()
        .group_ids
        .is_empty());
    assert_eq!(
        queries
            .query(&request(
                None,
                false,
                "",
                public,
                0,
                Some(before_remove.view)
            ))
            .unwrap_err()
            .to_string(),
        "stale_query_view"
    );
    assert_eq!(
        queries
            .group_tree(public, 0, 500, Some(&restored.view))
            .unwrap_err()
            .to_string(),
        "stale_group_view"
    );

    // Libraries without groups keep the old page shape.
    let plain = tempfile::tempdir().unwrap();
    ingest(plain.path(), "plain", Sensitivity::Public);
    let page = LibraryQuery::new(plain.path())
        .unwrap()
        .query(&request(None, false, "", public, 0, None))
        .unwrap();
    let json = serde_json::to_value(&page).unwrap();
    assert!(json.get("groups").is_none());
    assert!(json["items"][0].get("group_ids").is_none());
    let old_request: LibraryQueryRequest = serde_json::from_value(serde_json::json!({
        "query": {"text": "", "source": null, "item_type": null, "sort": "title", "descending": false},
        "max_sensitivity": "public", "offset": 0, "limit": 1, "expected_view": null
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&old_request.query)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        5
    );
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut out: Vec<String> = ids.iter().map(|id| (*id).clone()).collect();
    out.sort();
    out
}

#[test]
fn library_trash_preserves_other_roots_restores_folders_and_replays_after_restart() {
    use memex_store::{edit_group_trash, list_group_trash};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let ceiling = Sensitivity::Public;
    let receipt = ingest(root, "shared", ceiling);
    let id = &receipt.item_ids[0];
    let revision = memex_store::read_item(root, id).unwrap().1.revision_id;
    let mut roots = vec![];
    for name in ["Main", "Research"] {
        let r = edit(
            root,
            name,
            GroupEditOp::CreateRoot {
                name: name.into(),
                sensitivity: ceiling,
            },
            ceiling,
        );
        let r = edit(
            root,
            &format!("add-{name}"),
            GroupEditOp::AddMembers {
                group_id: r.group_id,
                expected_version: r.version,
                item_ids: vec![id.clone()],
            },
            ceiling,
        );
        roots.push(r);
    }
    let folder = edit(
        root,
        "child-folder",
        GroupEditOp::CreateFolder {
            parent_id: roots[0].group_id.clone(),
            expected_parent_version: roots[0].version.clone(),
            name: "Inbox".into(),
        },
        ceiling,
    );
    let folder = edit(
        root,
        "child-members",
        GroupEditOp::AddMembers {
            group_id: folder.group_id,
            expected_version: folder.version,
            item_ids: vec![id.clone()],
        },
        ceiling,
    );
    roots[0].version = read_group_tree(root, ceiling, 0, 500, None)
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == roots[0].group_id)
        .unwrap()
        .version
        .clone();
    let mut queries = LibraryQuery::new(root).unwrap();
    let before = queries
        .query(&request(None, false, "", ceiling, 0, None))
        .unwrap();
    assert_eq!(before.total, 1);
    edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "delete-main",
        &revision,
        None,
        ceiling,
    )
    .unwrap();
    assert_eq!(
        all_ids(&mut queries, Some(&roots[0].group_id), true, ceiling).0,
        0
    );
    assert_eq!(
        all_ids(&mut queries, Some(&roots[1].group_id), true, ceiling).1,
        [id.clone()]
    );
    assert_eq!(all_ids(&mut queries, None, false, ceiling).0, 1);
    assert_eq!(
        queries
            .query(&request(None, false, "", ceiling, 0, Some(before.view)))
            .unwrap_err()
            .to_string(),
        "stale_query_view"
    );
    assert_eq!(
        list_group_trash(root, &roots[0].group_id, None, ceiling)
            .unwrap()
            .0[0]
            .plan_id
            .as_deref(),
        Some("delete-main")
    );
    assert_eq!(
        edit_groups(
            root,
            "remove-child",
            GroupEditOp::RemoveMembers {
                group_id: folder.group_id.clone(),
                expected_version: folder.version,
                item_ids: vec![id.clone()],
            },
            ceiling
        )
        .unwrap_err()
        .to_string(),
        "item_trashed"
    );
    for (i, r) in roots.iter().enumerate() {
        if i == 1 {
            edit_group_trash(
                root,
                &r.group_id,
                id,
                "delete-research",
                &revision,
                None,
                ceiling,
            )
            .unwrap();
        }
        assert_eq!(
            edit_groups(
                root,
                &format!("remove-{i}"),
                GroupEditOp::RemoveMembers {
                    group_id: r.group_id.clone(),
                    expected_version: r.version.clone(),
                    item_ids: vec![id.clone()],
                },
                ceiling
            )
            .unwrap_err()
            .to_string(),
            "item_trashed"
        );
    }
    assert_eq!(all_ids(&mut queries, None, false, ceiling).0, 0);
    assert_eq!(
        list_group_trash(root, &roots[1].group_id, None, ceiling)
            .unwrap()
            .0
            .len(),
        1
    );
    drop(queries);
    let mut queries = LibraryQuery::new(root).unwrap();
    assert_eq!(all_ids(&mut queries, None, false, ceiling).0, 0);
    assert!(edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "wrong-plan",
        &revision,
        Some("wrong"),
        ceiling
    )
    .is_err());
    edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "restore-main",
        &revision,
        Some("delete-main"),
        ceiling,
    )
    .unwrap();
    edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "delete-main",
        &revision,
        None,
        ceiling,
    )
    .unwrap();
    assert_eq!(
        all_ids(&mut queries, Some(&roots[0].group_id), true, ceiling).1,
        [id.clone()]
    );
    assert_eq!(
        all_ids(&mut queries, Some(&folder.group_id), false, ceiling).1,
        [id.clone()]
    );
    assert_eq!(
        all_ids(&mut queries, Some(&roots[1].group_id), true, ceiling).0,
        0
    );
    edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "delete-main-2",
        &revision,
        None,
        ceiling,
    )
    .unwrap();
    edit_group_trash(
        root,
        &roots[0].group_id,
        id,
        "restore-main",
        &revision,
        Some("delete-main"),
        ceiling,
    )
    .unwrap();
    assert_eq!(
        all_ids(&mut queries, None, false, ceiling).0,
        0,
        "an old restore must not undo a later deletion"
    );
    assert_eq!(
        memex_store::read_item(root, id).unwrap().1.revision_id,
        revision
    );
}

#[test]
fn library_trash_requires_all_affected_memberships_and_reserves_restore_capacity() {
    use memex_store::{
        edit_group_trash, import_native_groups, list_group_trash, GroupKind, GroupMembership,
        GroupNode, NativeGroupImport,
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let receipt = ingest(root, "mixed", Sensitivity::Public);
    let id = &receipt.item_ids[0];
    let revision = memex_store::read_item(root, id).unwrap().1.revision_id;
    let other = ingest(root, "second", Sensitivity::Public).item_ids[0].clone();
    let other_revision = memex_store::read_item(root, &other).unwrap().1.revision_id;
    let nodes = [
        ("group_root", None, Sensitivity::Public),
        ("group_hidden", Some("group_root"), Sensitivity::Private),
    ]
    .map(|(id, parent, sensitivity)| GroupNode {
        group_id: id.into(),
        name: id.into(),
        kind: if parent.is_some() {
            GroupKind::Collection
        } else {
            GroupKind::User
        },
        parent_id: parent.map(str::to_owned),
        root_group_id: "group_root".into(),
        source: None,
        owner_id: "owner_local".into(),
        sensitivity,
        read_only: false,
        version: 1,
    });
    import_native_groups(
        root,
        &NativeGroupImport {
            request_id: "import-mixed".into(),
            root_group_id: "group_root".into(),
            memberships: nodes
                .iter()
                .map(|n| GroupMembership {
                    group_id: n.group_id.clone(),
                    item_id: id.clone(),
                })
                .chain([GroupMembership {
                    group_id: "group_root".into(),
                    item_id: other.clone(),
                }])
                .collect(),
            nodes: nodes.to_vec(),
            expected_revisions: [
                (id.clone(), revision.clone()),
                (other.clone(), other_revision.clone()),
            ]
            .into_iter()
            .collect(),
        },
        Sensitivity::Private,
    )
    .unwrap();
    let mut queries = LibraryQuery::new(root).unwrap();
    assert_eq!(
        edit_group_trash(
            root,
            "group_root",
            id,
            "low-delete",
            &revision,
            None,
            Sensitivity::Public
        )
        .unwrap_err()
        .to_string(),
        "library_resource_not_found"
    );
    assert_eq!(
        all_ids(
            &mut queries,
            Some("group_hidden"),
            true,
            Sensitivity::Private
        )
        .0,
        1
    );
    edit_group_trash(
        root,
        "group_root",
        id,
        "high-delete",
        &revision,
        None,
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(
        edit_group_trash(
            root,
            "group_root",
            id,
            "low-restore",
            &revision,
            Some("high-delete"),
            Sensitivity::Public
        )
        .unwrap_err()
        .to_string(),
        "library_resource_not_found"
    );
    assert_eq!(
        all_ids(
            &mut queries,
            Some("group_hidden"),
            true,
            Sensitivity::Private
        )
        .0,
        0
    );
    edit_group_trash(
        root,
        "group_root",
        id,
        "high-restore",
        &revision,
        Some("high-delete"),
        Sensitivity::Private,
    )
    .unwrap();

    // Accelerate a synthetic receipt history to the boundary instead of doing 65K fsyncs.
    let path = root.join("catalog/groups/trash.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let receipts = state["requests"].as_object_mut().unwrap();
    for i in receipts.len()..65_536 - 3 {
        receipts.insert(
            format!("past-{i}"),
            format!("sha256:{}", "0".repeat(64)).into(),
        );
    }
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    edit_group_trash(
        root,
        "group_root",
        id,
        "last-delete",
        &revision,
        None,
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(
        edit_group_trash(
            root,
            "group_root",
            &other,
            "competing-delete",
            &other_revision,
            None,
            Sensitivity::Private
        )
        .unwrap_err()
        .to_string(),
        "group_request_limit_exceeded"
    );
    edit_group_trash(
        root,
        "group_root",
        id,
        "last-restore",
        &revision,
        Some("last-delete"),
        Sensitivity::Private,
    )
    .unwrap();
    assert!(
        list_group_trash(root, "group_root", None, Sensitivity::Private)
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        edit_group_trash(
            root,
            "group_root",
            id,
            "no-room",
            &revision,
            None,
            Sensitivity::Private
        )
        .unwrap_err()
        .to_string(),
        "group_request_limit_exceeded"
    );
    edit_group_trash(
        root,
        "group_root",
        id,
        "last-delete",
        &revision,
        None,
        Sensitivity::Private,
    )
    .unwrap();
    assert!(
        list_group_trash(root, "group_root", None, Sensitivity::Private)
            .unwrap()
            .0
            .is_empty()
    );
}
