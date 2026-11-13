//! Folder transfers share edit_groups' writer lock, durable commit and replay
//! receipt. A mapping copies organization only; canonical items are untouched.
use super::*;

pub(super) struct Scope {
    nodes: BTreeSet<String>,
    versions: BTreeMap<String, String>,
    tops: Vec<String>,
}

pub(super) fn scope(state: &GroupState, ids: &[String], target: &str, ceiling: Sensitivity) -> Result<Scope> {
    let nodes: BTreeMap<_, _> = state.nodes.iter().map(|n| (n.group_id.as_str(), n)).collect();
    let levels = effective_sensitivities(state);
    let inactive = group_delete::inactive_nodes(state);
    let authorized = |id: &str| -> Result<&GroupNode> {
        let n = nodes.get(id).copied().context("library_resource_not_found")?;
        anyhow::ensure!(levels[id] <= ceiling && !inactive.contains(id), "library_resource_not_found");
        anyhow::ensure!(n.owner_id == LOCAL_OWNER_ID && !n.read_only, "group_read_only");
        Ok(n)
    };
    let mut parent = authorized(target)?;
    while let Some(id) = &parent.parent_id { parent = authorized(id)?; }
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for n in &state.nodes {
        if let Some(parent) = &n.parent_id { children.entry(parent).or_default().push(&n.group_id); }
    }
    let selected: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let mut versions = BTreeMap::new();
    let mut tops = vec![];
    for id in ids {
        let mut n = authorized(id)?;
        anyhow::ensure!(n.parent_id.is_some(), "invalid_group_edit");
        let mut covered = false;
        while let Some(id) = &n.parent_id {
            n = authorized(id)?;
            covered |= selected.contains(id.as_str());
            versions.insert(n.group_id.clone(), n.version_token());
        }
        if !covered { tops.push(id.clone()); }
    }
    let mut found = BTreeSet::new();
    let mut stack: Vec<_> = ids.iter().map(String::as_str).collect();
    while let Some(id) = stack.pop() {
        if !found.insert(id.to_owned()) { continue; }
        let n = authorized(id)?;
        versions.insert(n.group_id.clone(), n.version_token());
        stack.extend(children.get(id).into_iter().flatten().copied());
    }
    Ok(Scope { nodes: found, versions, tops })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply(root: &Path, state: &mut GroupState, ids: &[String], target: &str, expected_parent: &str,
    expected: &BTreeMap<String, String>, moving: bool, ceiling: Sensitivity) -> Result<()> {
    let scope = scope(state, ids, target, ceiling)?;
    // Admission checks must not hide a committed historical receipt when the
    // current tree has subsequently moved the old target below the old source.
    anyhow::ensure!(!scope.nodes.contains(target), "invalid_group_edit");
    let parent = state.nodes.iter().find(|n| n.group_id == target).unwrap().clone();
    anyhow::ensure!(parent.version_token() == expected_parent && scope.versions == *expected, "stale_group_version");
    let memberships: Vec<_> = state.memberships.iter().filter(|m| scope.nodes.contains(&m.group_id)).cloned().collect();
    let items: BTreeSet<_> = memberships.iter().map(|m| m.item_id.clone()).collect();
    let trashed = crate::trash::trashed_ids(root)?;
    for id in &items {
        anyhow::ensure!(crate::item_head_visible(root, id, ceiling, &trashed)?, "library_resource_not_found");
    }
    // Retained trash memberships are recovery state, not transferable active
    // organization. Never move or map them into a different trash context.
    let roots: BTreeSet<_> = state.nodes.iter().filter(|n| scope.nodes.contains(&n.group_id))
        .map(|n| n.root_group_id.clone()).chain(std::iter::once(parent.root_group_id.clone())).collect();
    let item_ids: Vec<_> = items.into_iter().collect();
    for group in roots { group_trash::ensure_members_removable(root, &group, &item_ids)?; }
    let levels = effective_sensitivities(state);
    let mut bump = BTreeSet::from([target.to_owned()]);
    if moving {
        let changed: BTreeSet<_> = scope.tops.iter().filter(|id| state.nodes.iter()
            .any(|n| &n.group_id == *id && n.parent_id.as_deref() != Some(target))).cloned().collect();
        anyhow::ensure!(!changed.is_empty(), "groups_unchanged");
        for n in &state.nodes {
            if changed.contains(&n.group_id) { bump.insert(n.parent_id.clone().unwrap()); }
        }
        for n in &mut state.nodes {
            if scope.nodes.contains(&n.group_id) {
                // Preserve inherited sensitivity when detaching from ancestors.
                n.sensitivity = levels[&n.group_id];
                n.root_group_id = parent.root_group_id.clone();
                bump.insert(n.group_id.clone());
            }
            if changed.contains(&n.group_id) { n.parent_id = Some(target.into()); }
        }
    } else {
        anyhow::ensure!(state.nodes.len() + scope.nodes.len() <= MAX_GROUP_NODES
            && state.memberships.len() + memberships.len() <= MAX_GROUP_MEMBERSHIPS, "group_limit_exceeded");
        let identities: BTreeMap<_, _> = scope.nodes.iter().map(|id| (id.clone(), new_id("group"))).collect();
        let copies: Vec<_> = state.nodes.iter().filter(|n| scope.nodes.contains(&n.group_id)).map(|n| GroupNode {
            group_id: identities[&n.group_id].clone(), name: n.name.clone(), kind: GroupKind::Collection,
            parent_id: Some(if scope.tops.contains(&n.group_id) { target.into() } else { identities[n.parent_id.as_ref().unwrap()].clone() }),
            root_group_id: parent.root_group_id.clone(), source: None, owner_id: parent.owner_id.clone(),
            sensitivity: levels[&n.group_id].max(levels[target]), read_only: false, version: 1,
        }).collect();
        state.nodes.extend(copies);
        state.memberships.extend(memberships.into_iter().map(|m| GroupMembership {
            group_id: identities[&m.group_id].clone(), item_id: m.item_id,
        }));
        state.memberships.sort();
    }
    for n in &mut state.nodes {
        if bump.contains(&n.group_id) { n.version = n.version.checked_add(1).context("group_limit_exceeded")?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{capture, draft};
    const ALL: Sensitivity = Sensitivity::Restricted;

    fn setup() -> (tempfile::TempDir, String) {
        let d = tempfile::tempdir().unwrap();
        let item = ingest(d.path(), &capture(), draft()).unwrap().item_ids[0].clone();
        let revision = read_item(d.path(), &item).unwrap().1.revision_id;
        let node = |id: &str, parent: Option<&str>, root: &str| GroupNode {
            group_id: id.into(), name: id.into(), kind: if parent.is_none() { GroupKind::User } else { GroupKind::Collection },
            parent_id: parent.map(str::to_owned), root_group_id: root.into(), source: None,
            owner_id: LOCAL_OWNER_ID.into(), sensitivity: Sensitivity::Public, read_only: false, version: 1,
        };
        for (root, nodes, memberships) in [
            ("group_a", vec![node("group_a", None, "group_a"), node("group_folder", Some("group_a"), "group_a"),
                node("group_child", Some("group_folder"), "group_a"), node("group_other", Some("group_a"), "group_a")],
                vec![GroupMembership { group_id: "group_child".into(), item_id: item.clone() }]),
            ("group_b", vec![node("group_b", None, "group_b"), node("group_target", Some("group_b"), "group_b")], vec![]),
        ] {
            import_native_groups(d.path(), &NativeGroupImport { request_id: root.into(), root_group_id: root.into(), nodes,
                memberships, expected_revisions: BTreeMap::from([(item.clone(), revision.clone())]) }, ALL).unwrap();
        }
        (d, item)
    }

    fn intent(root: &Path, ids: &[&str], target: &str, moving: bool) -> GroupEditOp {
        let tree = read_group_tree(root, ALL, 0, 500, None).unwrap();
        // These fixture sources contain exactly folder/child/other, and their
        // ancestor group_a. Do not derive the expected scope via the helper.
        let included = if ids.contains(&"group_other") { vec!["group_a", "group_folder", "group_child", "group_other"] }
            else { vec!["group_a", "group_folder", "group_child"] };
        GroupEditOp::TransferFolders { source_ids: ids.iter().map(|id| (*id).into()).collect(), target_parent_id: target.into(),
            expected_parent_version: tree.nodes.iter().find(|n| n.id == target).unwrap().version.clone(),
            expected_versions: tree.nodes.iter().filter(|n| included.contains(&n.id.as_str())).map(|n| (n.id.clone(), n.version.clone())).collect(), moving }
    }

    #[test]
    fn folder_mapping_is_a_replay_safe_directory_snapshot_of_the_same_items() {
        let (d, item) = setup();
        let before_item = serde_json::to_vec(&read_item(d.path(), &item).unwrap()).unwrap();
        let op = intent(d.path(), &["group_folder", "group_child", "group_other"], "group_target", false);
        let receipt = edit_groups(d.path(), "map", op.clone(), ALL).unwrap();
        let bytes = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(edit_groups(d.path(), "map", op.clone(), ALL).unwrap(), receipt);
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), bytes);
        let tree = read_group_tree(d.path(), ALL, 0, 500, None).unwrap();
        assert_eq!(tree.nodes.len(), 9); // parent+child selected once, plus other
        let mapped = tree.nodes.iter().find(|n| n.name == "group_folder" && n.id != "group_folder").unwrap();
        let child = tree.nodes.iter().find(|n| n.name == "group_child" && n.id != "group_child").unwrap();
        assert_eq!(mapped.parent_id.as_deref(), Some("group_target"));
        assert_eq!(child.parent_id.as_deref(), Some(mapped.id.as_str()));
        assert_eq!(child.root_group_id, "group_b");
        let state = load_state(&d.path().canonicalize().unwrap()).unwrap();
        assert_eq!(state.memberships, [GroupMembership { group_id: "group_child".into(), item_id: item.clone() },
            GroupMembership { group_id: child.id.clone(), item_id: item.clone() }].into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>());
        assert_eq!(state.state_version, 1, "no schema activation");
        // A later source edit is not synchronized into the mapped directory.
        edit_groups(d.path(), "new-child", GroupEditOp::CreateFolder { parent_id: "group_folder".into(), expected_parent_version: "v1".into(), name: "later".into() }, ALL).unwrap();
        let tree = read_group_tree(d.path(), ALL, 0, 500, None).unwrap();
        assert_eq!(tree.nodes.iter().filter(|n| n.parent_id.as_deref() == Some(mapped.id.as_str())).count(), 1);
        assert_eq!(serde_json::to_vec(&read_item(d.path(), &item).unwrap()).unwrap(), before_item);
        assert_eq!(edit_groups(d.path(), "map", op, ALL).unwrap(), receipt, "replay after source edits is historical");
    }

    #[test]
    fn folder_move_preserves_id_subtree_memberships_and_inherited_sensitivity() {
        let (d, _) = setup();
        let root = d.path().canonicalize().unwrap();
        let mut state = load_state(&root).unwrap();
        state.nodes.iter_mut().find(|n| n.group_id == "group_a").unwrap().sensitivity = Sensitivity::Private;
        write_state(&root, &state).unwrap();
        let memberships = state.memberships.clone();
        let op = intent(d.path(), &["group_folder", "group_child", "group_other"], "group_target", true);
        let receipt = edit_groups(d.path(), "move", op.clone(), ALL).unwrap();
        let after = load_state(&root).unwrap();
        assert_eq!(after.nodes.len(), 6);
        assert_eq!(after.memberships, memberships);
        for id in ["group_folder", "group_child", "group_other"] {
            let n = after.nodes.iter().find(|n| n.group_id == id).unwrap();
            assert_eq!(n.root_group_id, "group_b");
            assert_eq!(n.sensitivity, Sensitivity::Private);
            assert_eq!(n.version, 2);
        }
        assert_eq!(after.nodes.iter().find(|n| n.group_id == "group_child").unwrap().parent_id.as_deref(), Some("group_folder"));
        assert_eq!(after.nodes.iter().find(|n| n.group_id == "group_folder").unwrap().parent_id.as_deref(), Some("group_target"));
        assert_eq!(edit_groups(d.path(), "move", op.clone(), ALL).unwrap(), receipt);
        assert_eq!(edit_groups(d.path(), "move", op, Sensitivity::Public).unwrap_err().to_string(), "library_resource_not_found");
    }

    #[test]
    fn folder_transfer_rejects_cycles_roots_stale_descendants_and_request_conflicts_without_writes() {
        let (d, _) = setup();
        for (ids, target, expected_error) in [
            (vec!["group_folder"], "group_folder", "invalid_group_edit"),
            (vec!["group_folder"], "group_child", "invalid_group_edit"),
            (vec!["group_a"], "group_target", "invalid_group_edit"),
        ] {
            let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
            let op = intent(d.path(), &ids, target, false);
            assert_eq!(edit_groups(d.path(), "reject", op, ALL).unwrap_err().to_string(), expected_error);
            assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
        }
        let stale = intent(d.path(), &["group_folder"], "group_target", false);
        edit_groups(d.path(), "rename-child", GroupEditOp::Rename { group_id: "group_child".into(), expected_version: "v1".into(), name: "changed".into() }, ALL).unwrap();
        let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(edit_groups(d.path(), "stale", stale, ALL).unwrap_err().to_string(), "stale_group_version");
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
        let op = intent(d.path(), &["group_folder"], "group_target", false);
        edit_groups(d.path(), "id", op, ALL).unwrap();
        let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        let changed = intent(d.path(), &["group_folder"], "group_target", true);
        assert_eq!(edit_groups(d.path(), "id", changed, ALL).unwrap_err().to_string(), "request_id_conflict");
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
    }

    #[test]
    fn folder_transfer_checks_every_ancestor_descendant_and_target_before_publishing() {
        for (id, restriction, ceiling, reason) in [
            ("group_child", "readonly", ALL, "group_read_only"),
            ("group_a", "readonly", ALL, "group_read_only"),
            ("group_b", "readonly", ALL, "group_read_only"),
            ("group_target", "owner", ALL, "group_read_only"),
            ("group_child", "sensitivity", Sensitivity::Public, "library_resource_not_found"),
        ] {
            let (d, _) = setup();
            let op = intent(d.path(), &["group_folder", "group_other"], "group_target", false);
            let root = d.path().canonicalize().unwrap();
            let mut state = load_state(&root).unwrap();
            let node = state.nodes.iter_mut().find(|n| n.group_id == id).unwrap();
            match restriction {
                "readonly" => node.read_only = true,
                "owner" => node.owner_id = "different-owner".into(),
                _ => node.sensitivity = Sensitivity::Private,
            }
            write_state(&root, &state).unwrap();
            let before = fs::read(root.join(GROUPS_PATH)).unwrap();
            assert_eq!(edit_groups(&root, "denied", op, ceiling).unwrap_err().to_string(), reason, "{id}");
            assert_eq!(fs::read(root.join(GROUPS_PATH)).unwrap(), before);
        }
        // A child added after drag start must not silently join the snapshot.
        let (d, _) = setup();
        let op = intent(d.path(), &["group_folder"], "group_target", false);
        edit_groups(d.path(), "child", GroupEditOp::CreateFolder { parent_id: "group_child".into(), expected_parent_version: "v1".into(), name: "new".into() }, ALL).unwrap();
        let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(edit_groups(d.path(), "stale-scope", op, ALL).unwrap_err().to_string(), "stale_group_version");
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
    }

    #[test]
    fn folder_transfer_does_not_escape_recoverable_node_or_item_trash() {
        for moving in [false, true] {
            let (d, item) = setup();
            let op = intent(d.path(), &["group_folder"], "group_target", moving);
            let revision = read_item(d.path(), &item).unwrap().1.revision_id;
            edit_group_trash(d.path(), "group_a", &item, "trash", &revision, None, ALL).unwrap();
            let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
            assert_eq!(edit_groups(d.path(), "transfer-trashed", op, ALL).unwrap_err().to_string(), "item_trashed");
            assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
            assert_eq!(list_group_trash(d.path(), "group_a", None, ALL).unwrap().0.len(), 1);
        }
        let (d, _) = setup();
        let impact = group_delete::preview(d.path(), "group_child", ALL).unwrap();
        group_delete::delete(d.path(), LOCAL_OWNER_ID, "delete", &format!("sha256:{}", "a".repeat(64)), "group_child", &impact.impact_sha256, ALL,
            std::time::Instant::now() + std::time::Duration::from_secs(60)).unwrap();
        let op = intent(d.path(), &["group_folder"], "group_target", false);
        let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(edit_groups(d.path(), "deleted-child", op, ALL).unwrap_err().to_string(), "library_resource_not_found");
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
    }

    #[test]
    fn folder_transfer_replays_after_the_target_is_legitimately_moved_inside_the_original_source() {
        let (d, _) = setup();
        let op = intent(d.path(), &["group_folder"], "group_target", false);
        let receipt = edit_groups(d.path(), "map-unknown", op.clone(), ALL).unwrap();
        let tree = read_group_tree(d.path(), ALL, 0, 500, None).unwrap();
        let expected: BTreeMap<_, _> = tree.nodes.iter().filter(|n| n.id == "group_b" || n.id == "group_target" || !n.id.starts_with("group_"))
            .map(|n| (n.id.clone(), n.version.clone())).collect();
        // The mapped IDs are generated group_* too: bind every actual target
        // descendant explicitly from the now-public tree.
        let mut expected = expected;
        for n in &tree.nodes {
            if n.parent_id.as_deref() == Some("group_target") || n.parent_id.as_deref().is_some_and(|id| tree.nodes.iter().any(|p| p.id == id && p.parent_id.as_deref() == Some("group_target"))) {
                expected.insert(n.id.clone(), n.version.clone());
            }
        }
        let target_version = tree.nodes.iter().find(|n| n.id == "group_folder").unwrap().version.clone();
        edit_groups(d.path(), "move-target", GroupEditOp::TransferFolders { source_ids: vec!["group_target".into()], target_parent_id: "group_folder".into(), expected_parent_version: target_version, expected_versions: expected, moving: true }, ALL).unwrap();
        let before = fs::read(d.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(edit_groups(d.path(), "map-unknown", op, ALL).unwrap(), receipt);
        assert_eq!(fs::read(d.path().join(GROUPS_PATH)).unwrap(), before);
    }

    #[test]
    fn restored_folder_history_allows_reparent_but_keeps_tombstones_and_receipt_integrity() {
        let (d, _) = setup();
        let impact = group_delete::preview(d.path(), "group_folder", ALL).unwrap();
        let deleted = group_delete::delete(d.path(), LOCAL_OWNER_ID, "delete-parent", &format!("sha256:{}", "a".repeat(64)), "group_folder", &impact.impact_sha256, ALL,
            std::time::Instant::now() + std::time::Duration::from_secs(60)).unwrap();
        let state_path = d.path().join(GROUPS_PATH);
        let tombstone = fs::read(&state_path).unwrap();
        let mut broken: serde_json::Value = serde_json::from_slice(&tombstone).unwrap();
        broken["nodes"].as_array_mut().unwrap().iter_mut().find(|n| n["group_id"] == "group_child").unwrap()["parent_id"] = "group_other".into();
        fs::write(&state_path, serde_json::to_vec(&broken).unwrap()).unwrap();
        assert!(read_group_tree(d.path(), ALL, 0, 500, None).is_err(), "an active tombstone cannot lose its descendant");
        fs::write(&state_path, &tombstone).unwrap();
        group_delete::restore(d.path(), LOCAL_OWNER_ID, "restore-parent", &format!("sha256:{}", "b".repeat(64)), &deleted.deletion_id, ALL).unwrap();
        let restored: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        let op = intent(d.path(), &["group_child"], "group_target", true);
        edit_groups(d.path(), "move-restored-child", op, ALL).unwrap();
        let moved = fs::read(&state_path).unwrap();
        let state: serde_json::Value = serde_json::from_slice(&moved).unwrap();
        assert_eq!(state["deletions"], restored["deletions"]);
        assert_eq!(state["deletion_requests"], restored["deletion_requests"]);
        assert_eq!(read_group_tree(d.path(), ALL, 0, 500, None).unwrap().nodes.iter().find(|n| n.id == "group_child").unwrap().parent_id.as_deref(), Some("group_target"));
        for field in ["identity", "receipt"] {
            let mut broken = state.clone();
            if field == "identity" { broken["deletions"][&deleted.deletion_id]["target_group_id"] = "group_missing".into(); }
            else { broken["deletion_requests"].as_object_mut().unwrap().values_mut().next().unwrap()["receipt"]["group_id"] = "group_other".into(); }
            fs::write(&state_path, serde_json::to_vec(&broken).unwrap()).unwrap();
            assert!(read_group_tree(d.path(), ALL, 0, 500, None).is_err(), "{field} corruption is still rejected");
        }
        fs::write(&state_path, moved).unwrap();
    }

    #[test]
    fn competing_folder_maps_publish_once_and_replay_without_new_nodes() {
        let (d, _) = setup();
        let op = intent(d.path(), &["group_folder"], "group_target", false);
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2).map(|i| {
            let (path, op, gate) = (d.path().to_owned(), op.clone(), gate.clone());
            std::thread::spawn(move || { gate.wait(); edit_groups(&path, &format!("race-{i}"), op, ALL) })
        }).collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| r.as_ref().is_err_and(|e| e.to_string() == "stale_group_version")).count(), 1);
        assert_eq!(read_group_tree(d.path(), ALL, 0, 500, None).unwrap().nodes.len(), 8);
    }
}
