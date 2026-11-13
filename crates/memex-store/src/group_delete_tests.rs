use super::*;
use crate::tests::{capture, draft};
use std::time::{Duration, Instant};

const CEILING: Sensitivity = Sensitivity::Restricted;

fn fixture() -> (tempfile::TempDir, NativeGroupImport, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut item_draft = draft();
    item_draft.sensitivity = Sensitivity::Private;
    let item = ingest(dir.path(), &capture(), item_draft).unwrap().item_ids[0].clone();
    let root = GroupNode {
        group_id: "group_main".into(),
        name: "Main".into(),
        kind: GroupKind::User,
        parent_id: None,
        root_group_id: "group_main".into(),
        owner_id: LOCAL_OWNER_ID.into(),
        source: Some(GroupSource {
            system: "zotero".into(),
            library_key: "local:fixture".into(),
            object_key: None,
        }),
        sensitivity: Sensitivity::Public,
        read_only: false,
        version: 1,
    };
    let child = GroupNode {
        group_id: "group_child".into(),
        name: "Child".into(),
        kind: GroupKind::Collection,
        parent_id: Some(root.group_id.clone()),
        ..root.clone()
    };
    let deep = GroupNode {
        group_id: "group_deep".into(),
        name: "Deep".into(),
        parent_id: Some(child.group_id.clone()),
        ..child.clone()
    };
    let plan = NativeGroupImport {
        request_id: "native-import".into(),
        root_group_id: root.group_id.clone(),
        nodes: vec![root, child, deep],
        memberships: vec![GroupMembership {
            group_id: "group_deep".into(),
            item_id: item.clone(),
        }],
        expected_revisions: BTreeMap::from([(
            item.clone(),
            read_item(dir.path(), &item).unwrap().1.revision_id,
        )]),
    };
    import_native_groups(dir.path(), &plan, CEILING).unwrap();
    (dir, plan, item)
}

fn remove(root: &Path, id: &str, request: &str) -> Receipt {
    let p = preview(root, id, CEILING).unwrap();
    delete(
        root,
        LOCAL_OWNER_ID,
        request,
        &digest(&request).unwrap(),
        id,
        &p.impact_sha256,
        CEILING,
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap()
}

fn recover(root: &Path, receipt: &Receipt, request: &str) -> Receipt {
    restore(
        root,
        LOCAL_OWNER_ID,
        request,
        &digest(&request).unwrap(),
        &receipt.deletion_id,
        CEILING,
    )
    .unwrap()
}

#[test]
fn v1_read_preview_ordinary_write_stays_v1_and_delete_retains_identity() {
    let (dir, plan, _) = fixture();
    let root = dir.path();
    let before = fs::read(root.join(GROUPS_PATH)).unwrap();
    let p = preview(root, "group_main", CEILING).unwrap();
    assert_eq!(
        (
            p.affected_node_count,
            p.affected_membership_count,
            p.distinct_item_count
        ),
        (3, 1, 1)
    );
    assert!(list(root, CEILING).unwrap().is_empty());
    assert!(replay(
        root,
        LOCAL_OWNER_ID,
        "not-yet",
        &digest(&0).unwrap(),
        CEILING
    )
    .unwrap()
    .is_none());
    assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), before);
    let op = GroupEditOp::Rename {
        group_id: "group_deep".into(),
        expected_version: "v1".into(),
        name: "Renamed".into(),
    };
    let old_receipt = edit_groups(root, "old-rename", op.clone(), CEILING).unwrap();
    assert_eq!(load_state(root).unwrap().state_version, 1);
    let deleted = remove(root, "group_main", "delete-main");
    let saved = load_state(root).unwrap();
    assert_eq!(saved.state_version, 2);
    assert_eq!(saved.nodes.len(), 3);
    assert_eq!(saved.memberships, plan.memberships);
    assert_eq!(saved.nodes[0].source, plan.nodes[0].source);
    assert!(read_visible_groups(root, CEILING).unwrap().nodes.is_empty());
    assert_eq!(
        edit_groups(root, "old-rename", op.clone(), CEILING).unwrap(),
        old_receipt
    );
    assert!(edit_groups(root, "new-rename", op, CEILING).is_err());
    import_native_groups(root, &plan, CEILING).unwrap();
    let mut conflict = plan.clone();
    conflict.request_id = "new-import".into();
    assert!(import_native_groups(root, &conflict, CEILING).is_err());
    assert_eq!(
        list(root, CEILING).unwrap()[0].deletion_id,
        deleted.deletion_id
    );
    recover(root, &deleted, "restore-main");
    assert_eq!(read_visible_groups(root, CEILING).unwrap().nodes.len(), 3);
    assert_eq!(load_state(root).unwrap().nodes[2].name, "Renamed");
    assert!(load_state(root).unwrap().nodes[2].version > 2);
    // Simulates a new daemon with no token cache, using only persisted state.
    let historical = replay(
        root,
        LOCAL_OWNER_ID,
        "delete-main",
        &digest(&"delete-main").unwrap(),
        CEILING,
    )
    .unwrap()
    .unwrap();
    assert_eq!(historical, deleted);
    assert_eq!(
        delete(
            root,
            LOCAL_OWNER_ID,
            "delete-main",
            &digest(&"delete-main").unwrap(),
            "group_main",
            "expired-impact",
            CEILING,
            Instant::now()
        )
        .unwrap(),
        deleted
    );
    assert_eq!(read_visible_groups(root, CEILING).unwrap().nodes.len(), 3);
    assert!(list(root, CEILING).unwrap().is_empty());
}

#[test]
fn impact_detects_deep_edit_trash_and_current_authority_before_any_write() {
    let (dir, _, item) = fixture();
    let root = dir.path();
    let p = preview(root, "group_main", CEILING).unwrap();
    let ancestor_version = load_state(root).unwrap().nodes[0].version;
    edit_groups(
        root,
        "deep-edit",
        GroupEditOp::Rename {
            group_id: "group_deep".into(),
            expected_version: "v1".into(),
            name: "Changed".into(),
        },
        CEILING,
    )
    .unwrap();
    assert_eq!(load_state(root).unwrap().nodes[0].version, ancestor_version);
    let bytes = fs::read(root.join(GROUPS_PATH)).unwrap();
    assert_eq!(
        delete(
            root,
            LOCAL_OWNER_ID,
            "stale",
            &digest(&1).unwrap(),
            "group_main",
            &p.impact_sha256,
            CEILING,
            Instant::now() + Duration::from_secs(60)
        )
        .unwrap_err()
        .to_string(),
        "stale_group_delete_preview"
    );
    assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), bytes);
    let p = preview(root, "group_main", CEILING).unwrap();
    let revision = read_item(root, &item).unwrap().1.revision_id;
    edit_group_trash(root, "group_main", &item, "trash", &revision, None, CEILING).unwrap();
    assert_ne!(
        preview(root, "group_main", CEILING).unwrap().impact_sha256,
        p.impact_sha256
    );
    let p = preview(root, "group_main", CEILING).unwrap();
    trash_item(root, &item, &revision, "manual", None, None).unwrap();
    assert_ne!(
        preview(root, "group_main", CEILING).unwrap().impact_sha256,
        p.impact_sha256
    );
    // Global trash does not evade relationship authorization or block owner delete.
    assert!(preview(root, "group_main", Sensitivity::Public).is_err());
    let deleted = remove(root, "group_main", "allowed");
    recover(root, &deleted, "allowed-restore");
    assert!(is_item_trashed(root, &item).unwrap());
    assert!(read_visible_groups(root, CEILING)
        .unwrap()
        .trashed_item_ids
        .contains(&item));
}

#[test]
fn hidden_and_effectively_readonly_descendants_reject_whole_subtree_and_list() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let original = load_state(root).unwrap();
    for index in [0, 1, 2] {
        let mut state = original.clone();
        state.nodes[index].read_only = true;
        write_state(root, &state).unwrap();
        let bytes = fs::read(root.join(GROUPS_PATH)).unwrap();
        assert_eq!(
            preview(root, "group_deep", CEILING)
                .unwrap_err()
                .to_string(),
            "library_resource_not_found"
        );
        assert_eq!(
            preview(root, "group_main", CEILING)
                .unwrap_err()
                .to_string(),
            "library_resource_not_found"
        );
        assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), bytes);
    }
    write_state(root, &original).unwrap();
    let d = remove(root, "group_main", "delete");
    let mut state = load_state(root).unwrap();
    state.nodes[2].sensitivity = Sensitivity::Restricted;
    write_state(root, &state).unwrap();
    assert!(list(root, Sensitivity::Private).unwrap().is_empty());
    assert!(replay(
        root,
        LOCAL_OWNER_ID,
        "delete",
        &digest(&"delete").unwrap(),
        Sensitivity::Private
    )
    .is_err());
    assert!(restore(
        root,
        LOCAL_OWNER_ID,
        "restore",
        &digest(&3).unwrap(),
        &d.deletion_id,
        Sensitivity::Private
    )
    .is_err());
    assert_eq!(list(root, CEILING).unwrap().len(), 1);
    assert!(replay(
        root,
        "untrusted",
        "delete",
        &digest(&"delete").unwrap(),
        CEILING
    )
    .is_err());
}

#[test]
fn nested_deletions_restore_only_their_own_nodes_and_preserve_later_order() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let child = remove(root, "group_child", "child");
    let p = preview(root, "group_main", CEILING).unwrap();
    assert_eq!(
        (
            p.affected_node_count,
            p.affected_membership_count,
            p.distinct_item_count
        ),
        (1, 0, 0)
    );
    let parent = remove(root, "group_main", "parent");
    assert!(preview(root, "group_child", CEILING).is_err());
    assert!(restore(
        root,
        LOCAL_OWNER_ID,
        "early-child",
        &digest(&0).unwrap(),
        &child.deletion_id,
        CEILING
    )
    .is_err());
    let other = edit_groups(
        root,
        "other",
        GroupEditOp::CreateRoot {
            name: "Other".into(),
            sensitivity: Sensitivity::Public,
        },
        CEILING,
    )
    .unwrap();
    let last = edit_groups(
        root,
        "last",
        GroupEditOp::CreateRoot {
            name: "Last".into(),
            sensitivity: Sensitivity::Public,
        },
        CEILING,
    )
    .unwrap();
    edit_groups(
        root,
        "reorder",
        GroupEditOp::ReorderRoot {
            group_id: last.group_id.clone(),
            expected_version: "v1".into(),
            before_id: Some(other.group_id.clone()),
        },
        CEILING,
    )
    .unwrap();
    let slots: Vec<_> = load_state(root)
        .unwrap()
        .nodes
        .into_iter()
        .map(|n| n.group_id)
        .collect();
    recover(root, &parent, "restore-parent");
    let shown = read_visible_groups(root, CEILING).unwrap();
    assert!(shown.nodes.iter().any(|n| n.group_id == "group_main"));
    assert!(!shown.nodes.iter().any(|n| n.group_id == "group_child"));
    recover(root, &child, "restore-child");
    assert_eq!(
        load_state(root)
            .unwrap()
            .nodes
            .into_iter()
            .map(|n| n.group_id)
            .collect::<Vec<_>>(),
        slots
    );
    let again = remove(root, "group_child", "child-again");
    // Old restoration replay cannot clear the newer deletion.
    recover(root, &child, "restore-child");
    assert!(inactive_nodes(&load_state(root).unwrap()).contains("group_child"));
    assert_eq!(
        list(root, CEILING).unwrap()[0].deletion_id,
        again.deletion_id
    );
}

#[test]
fn retained_trash_stays_suppressed_and_old_restore_stays_reachable() {
    let (dir, _, item) = fixture();
    let root = dir.path();
    let revision = read_item(root, &item).unwrap().1.revision_id;
    edit_group_trash(
        root,
        "group_main",
        &item,
        "trash-item",
        &revision,
        None,
        CEILING,
    )
    .unwrap();
    let trash_bytes = fs::read(root.join("catalog/groups/trash.json")).unwrap();
    let deleted = remove(root, "group_main", "delete");
    assert_eq!(
        fs::read(root.join("catalog/groups/trash.json")).unwrap(),
        trash_bytes
    );
    let shown = read_visible_groups(root, CEILING).unwrap();
    assert!(shown.nodes.is_empty() && shown.memberships.is_empty());
    assert!(shown.trashed_item_ids.contains(&item));
    let entries = list_group_trash(root, "group_main", None, CEILING)
        .unwrap()
        .0;
    assert_eq!(entries.len(), 1);
    edit_group_trash(
        root,
        "group_main",
        &item,
        "restore-item",
        &revision,
        entries[0].plan_id.as_deref(),
        CEILING,
    )
    .unwrap();
    let shown = read_visible_groups(root, CEILING).unwrap();
    assert!(
        shown.nodes.is_empty() && shown.memberships.is_empty() && shown.trashed_item_ids.is_empty()
    );
    // Old item-trash replay does not re-delete after explicit item restoration.
    edit_group_trash(
        root,
        "group_main",
        &item,
        "trash-item",
        &revision,
        None,
        CEILING,
    )
    .unwrap();
    recover(root, &deleted, "restore-organization");
    assert!(read_visible_groups(root, CEILING)
        .unwrap()
        .trashed_item_ids
        .is_empty());
    assert_eq!(read_item(root, &item).unwrap().1.revision_id, revision);
}

#[test]
fn external_memberships_override_only_when_active_visible_and_not_trashed() {
    let (dir, _, item) = fixture();
    let root = dir.path();
    let other = edit_groups(
        root,
        "other",
        GroupEditOp::CreateRoot {
            name: "Shared".into(),
            sensitivity: Sensitivity::Public,
        },
        CEILING,
    )
    .unwrap();
    edit_groups(
        root,
        "add-other",
        GroupEditOp::AddMembers {
            group_id: other.group_id.clone(),
            expected_version: "v1".into(),
            item_ids: vec![item.clone()],
        },
        CEILING,
    )
    .unwrap();
    let revision = read_item(root, &item).unwrap().1.revision_id;
    edit_group_trash(root, "group_main", &item, "trash", &revision, None, CEILING).unwrap();
    remove(root, "group_main", "delete-main");
    let shown = read_visible_groups(root, CEILING).unwrap();
    assert!(!shown.trashed_item_ids.contains(&item));
    assert_eq!(shown.memberships.len(), 1);
    remove(root, &other.group_id, "delete-other");
    assert!(read_visible_groups(root, CEILING)
        .unwrap()
        .trashed_item_ids
        .contains(&item));
    let mut state = load_state(root).unwrap();
    state.nodes[0].sensitivity = Sensitivity::Restricted;
    write_state(root, &state).unwrap();
    for ceiling in [
        Sensitivity::Public,
        Sensitivity::Internal,
        Sensitivity::Private,
    ] {
        assert!(read_visible_groups(root, ceiling)
            .unwrap()
            .trashed_item_ids
            .is_empty());
    }
    assert!(read_visible_groups(root, CEILING)
        .unwrap()
        .trashed_item_ids
        .contains(&item));
}

#[test]
fn expiry_after_lock_wait_and_at_publish_gate_leaves_canonical_bytes_unchanged() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let p = preview(root, "group_main", CEILING).unwrap();
    let bytes = fs::read(root.join(GROUPS_PATH)).unwrap();
    let canonical = existing_canonical_root(root).unwrap();
    let held = ingest_lock(&canonical).unwrap();
    let expires = Instant::now() + Duration::from_millis(30);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            delete(
                root,
                LOCAL_OWNER_ID,
                "expired",
                &digest(&1).unwrap(),
                "group_main",
                &p.impact_sha256,
                CEILING,
                expires,
            )
        });
        std::thread::sleep(Duration::from_millis(60));
        held.unlock().unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err().to_string(),
            "group_delete_preview_expired"
        );
    });
    assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), bytes);
    let mut changed = load_state(root).unwrap();
    changed.nodes[0].name = "must-not-publish".into();
    assert_eq!(
        write_state_before(root, &changed, Some(Instant::now()))
            .unwrap_err()
            .to_string(),
        "group_delete_preview_expired"
    );
    assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), bytes);
    assert!(fs::read_dir(root.join("catalog/groups"))
        .unwrap()
        .all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".journal")));
}

#[test]
fn malformed_tombstones_and_request_conflicts_fail_closed() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let deleted = remove(root, "group_main", "delete");
    let original = load_state(root).unwrap();
    let mut bad = original.clone();
    bad.deletions
        .get_mut(&deleted.deletion_id)
        .unwrap()
        .affected_node_ids
        .remove("group_deep");
    assert!(validate_state(&bad).is_err());
    let mut bad = original.clone();
    bad.deletion_requests.clear();
    assert!(validate_state(&bad).is_err());
    let mut bad = original.clone();
    bad.state_version = 1;
    assert!(validate_state(&bad).is_err());
    let mut bad = original.clone();
    bad.deletions
        .get_mut(&deleted.deletion_id)
        .unwrap()
        .restored = true;
    assert!(validate_state(&bad).is_err());
    let bytes = fs::read(root.join(GROUPS_PATH)).unwrap();
    assert_eq!(
        replay(
            root,
            LOCAL_OWNER_ID,
            "delete",
            &digest(&"different").unwrap(),
            CEILING
        )
        .unwrap_err()
        .to_string(),
        "request_id_conflict"
    );
    assert_eq!(
        restore(
            root,
            LOCAL_OWNER_ID,
            "delete",
            &digest(&"restore").unwrap(),
            &deleted.deletion_id,
            CEILING
        )
        .unwrap_err()
        .to_string(),
        "request_id_conflict"
    );
    assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), bytes);
}

#[test]
fn preview_ignores_unrelated_hidden_writes_and_does_not_invent_owner_acl() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let mut state = load_state(root).unwrap();
    state.nodes[0].owner_id = "historical-owner".into();
    write_state(root, &state).unwrap();
    let p = preview(root, "group_main", Sensitivity::Private).unwrap();
    let hidden = edit_groups(
        root,
        "hidden",
        GroupEditOp::CreateRoot {
            name: "Hidden".into(),
            sensitivity: Sensitivity::Restricted,
        },
        CEILING,
    )
    .unwrap();
    edit_groups(
        root,
        "hidden-rename",
        GroupEditOp::Rename {
            group_id: hidden.group_id.clone(),
            expected_version: "v1".into(),
            name: "Hidden edit".into(),
        },
        CEILING,
    )
    .unwrap();
    assert_eq!(
        preview(root, "group_main", Sensitivity::Private).unwrap(),
        p
    );
    // Fixture insertion before the target models an unrelated slot shift.
    let mut state = load_state(root).unwrap();
    let n = state.nodes.pop().unwrap();
    state.nodes.insert(0, n);
    write_state(root, &state).unwrap();
    assert_eq!(
        preview(root, "group_main", Sensitivity::Private).unwrap(),
        p
    );
    delete(
        root,
        LOCAL_OWNER_ID,
        "delete",
        &digest(&"delete").unwrap(),
        "group_main",
        &p.impact_sha256,
        Sensitivity::Private,
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
}

#[test]
fn committed_response_loss_child() {
    let Ok(path) = std::env::var("MEMEX_GROUP_DELETE_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&path);
    // This helper is run only by the synthetic parent test below.
    assert!(root.join("synthetic-group-delete-fixture").is_file());
    match std::env::var("MEMEX_GROUP_DELETE_TEST_ACTION")
        .unwrap()
        .as_str()
    {
        "delete" => {
            remove(root, "group_main", "process-delete");
        }
        "restore" => {
            let receipt = replay(
                root,
                LOCAL_OWNER_ID,
                "process-delete",
                &digest(&"process-delete").unwrap(),
                CEILING,
            )
            .unwrap()
            .unwrap();
            recover(root, &receipt, "process-restore");
        }
        _ => panic!("unexpected synthetic test action"),
    }
    // Exit without delivering the durable receipt to the parent/client.
    std::process::exit(86);
}

#[test]
fn process_exit_after_commit_keeps_replay_and_restoration_across_restart() {
    let (dir, _, item) = fixture();
    let root = dir.path();
    fs::write(root.join("synthetic-group-delete-fixture"), b"synthetic").unwrap();
    let revision = read_item(root, &item).unwrap().1;
    for action in ["delete", "restore"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "groups::group_delete::tests::committed_response_loss_child",
                "--nocapture",
            ])
            .env("MEMEX_GROUP_DELETE_TEST_ROOT", root)
            .env("MEMEX_GROUP_DELETE_TEST_ACTION", action)
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(86),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let receipt = replay(
            root,
            LOCAL_OWNER_ID,
            "process-delete",
            &digest(&"process-delete").unwrap(),
            CEILING,
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.outcome, "deleted");
        assert_eq!(
            list(root, CEILING).unwrap().len(),
            usize::from(action == "delete")
        );
        assert_eq!(
            read_visible_groups(root, CEILING).unwrap().nodes.len(),
            if action == "delete" { 0 } else { 3 }
        );
        assert_eq!(read_item(root, &item).unwrap().1, revision);
    }
    let state = load_state(root).unwrap();
    assert_eq!(state.deletions.len(), 1);
    assert_eq!(state.deletion_requests.len(), 2);
    assert_eq!(state.nodes.len(), 3);
}

#[test]
fn independent_receipt_pool_reserves_outstanding_restore_at_capacity() {
    let (dir, _, _) = fixture();
    let root = dir.path();
    let first = remove(root, "group_main", "first");
    recover(root, &first, "first-restore");
    let mut state = load_state(root).unwrap();
    let historical = state.deletions[&first.deletion_id].clone();
    // Synthetic persisted history fills the real production-sized pool, not a
    // weakened test limit. Repeated deletions of one stable identity are valid.
    for i in 1..(MAX_GROUP_REQUESTS / 2 - 1) {
        let id = format!("deletion_history{i}");
        state.deletions.insert(id.clone(), historical.clone());
        for outcome in ["deleted", "restored"] {
            state.deletion_requests.insert(
                digest(&(i, outcome)).unwrap(),
                Request {
                    request_sha256: digest(&(outcome, i)).unwrap(),
                    receipt: Receipt {
                        group_id: "group_main".into(),
                        deletion_id: id.clone(),
                        outcome: outcome.into(),
                    },
                },
            );
        }
    }
    write_state(root, &state).unwrap();
    let last = remove(root, "group_main", "last");
    let other = edit_groups(
        root,
        "other",
        GroupEditOp::CreateRoot {
            name: "Other".into(),
            sensitivity: Sensitivity::Public,
        },
        CEILING,
    )
    .unwrap();
    let p = preview(root, &other.group_id, CEILING).unwrap();
    assert_eq!(
        delete(
            root,
            LOCAL_OWNER_ID,
            "over-limit",
            &digest(&"over-limit").unwrap(),
            &other.group_id,
            &p.impact_sha256,
            CEILING,
            Instant::now() + Duration::from_secs(120)
        )
        .unwrap_err()
        .to_string(),
        "group_request_limit_exceeded"
    );
    recover(root, &last, "last-restore");
    let state = load_state(root).unwrap();
    assert_eq!(state.deletion_requests.len(), MAX_GROUP_REQUESTS);
    assert!(inactive_nodes(&state).is_empty());
}
