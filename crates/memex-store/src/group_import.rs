//! Owner-local initial materialization, not a synchronization or overwrite API.
//! Reuses groups' atomic state/request commit and Store lock/generation protocol.
use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGroupImport {
    pub request_id: String,
    pub root_group_id: String,
    pub nodes: Vec<GroupNode>,
    pub memberships: Vec<GroupMembership>,
    pub expected_revisions: BTreeMap<String, String>,
}

pub fn import_native_groups(
    library: impl AsRef<Path>,
    plan: &NativeGroupImport,
    max_sensitivity: Sensitivity,
) -> Result<GroupEditReceipt> {
    if !valid_text(&plan.request_id, 256) || plan.nodes.is_empty() {
        anyhow::bail!("invalid_group_import")
    }
    let mut imported = GroupState {
        state_version: GROUPS_STATE_VERSION,
        nodes: plan.nodes.clone(),
        memberships: plan.memberships.clone(),
        ..Default::default()
    };
    imported.memberships.sort();
    imported.memberships.dedup();
    validate_state(&imported)?;
    if imported.nodes.iter().any(|node| {
        node.root_group_id != plan.root_group_id
            || node.owner_id != LOCAL_OWNER_ID
            || node.read_only
            || node.version != 1
            || node.sensitivity > max_sensitivity
    }) {
        anyhow::bail!("invalid_group_import")
    }
    let request_sha256 = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(plan)?));
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = load_state(&root)?;
    if let Some(record) = state.requests.get(&plan.request_id) {
        let visible = effective_sensitivities(&state)
            .get(&record.receipt.group_id)
            .is_some_and(|level| *level <= max_sensitivity);
        if !visible {
            anyhow::bail!("library_resource_not_found")
        }
        if record.request_sha256 != request_sha256 {
            anyhow::bail!("request_id_conflict")
        }
        // A replay must NOT undo local renames, removals or trash after import.
        return Ok(record.receipt.clone());
    }
    if state.requests.len() >= MAX_GROUP_REQUESTS {
        anyhow::bail!("group_request_limit_exceeded")
    }
    let existing: BTreeSet<_> = state.nodes.iter().map(|n| &n.group_id).collect();
    if imported
        .nodes
        .iter()
        .any(|n| existing.contains(&n.group_id))
    {
        anyhow::bail!("group_import_identity_conflict")
    }
    let trashed = crate::trash::trashed_ids(&root)?;
    let items: BTreeSet<_> = imported.memberships.iter().map(|m| &m.item_id).collect();
    for id in items {
        if !crate::item_head_visible(&root, id, max_sensitivity, &trashed)? {
            anyhow::bail!("library_resource_not_found")
        }
        let metadata = read_item_metadata_at_sensitivity(&root, id, max_sensitivity)?
            .context("library_resource_not_found")?;
        if plan.expected_revisions.get(id) != Some(&metadata.revision_id) {
            anyhow::bail!("stale_import_item_revision")
        }
    }
    let receipt = GroupEditReceipt {
        schema_version: SCHEMA_VERSION,
        group_id: plan.root_group_id.clone(),
        version: version_token(1),
    };
    state.nodes.extend(imported.nodes);
    state.memberships.extend(imported.memberships);
    state.memberships.sort();
    state.groups_version += 1;
    state.requests.insert(
        plan.request_id.clone(),
        GroupRequestRecord {
            request_sha256,
            receipt: receipt.clone(),
        },
    );
    // Complete validation precedes the single durable replacement. No partial
    // directory tree, membership batch or separate replay marker is published.
    write_state(&root, &state)?;
    lock.unlock()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{capture, draft};

    fn plan(item: &str) -> NativeGroupImport {
        let root = GroupNode {
            group_id: "group_import_root".into(),
            name: "Library".into(),
            kind: GroupKind::ZoteroGroup,
            parent_id: None,
            root_group_id: "group_import_root".into(),
            source: Some(GroupSource {
                system: "zotero".into(),
                library_key: "group:7".into(),
                object_key: None,
            }),
            owner_id: LOCAL_OWNER_ID.into(),
            sensitivity: Sensitivity::Private,
            read_only: false,
            version: 1,
        };
        let folder = GroupNode {
            group_id: "group_import_child".into(),
            name: "Empty or populated".into(),
            kind: GroupKind::Collection,
            parent_id: Some(root.group_id.clone()),
            ..root.clone()
        };
        NativeGroupImport {
            request_id: "import-fixture-v1".into(),
            root_group_id: root.group_id.clone(),
            nodes: vec![folder.clone(), root],
            memberships: vec![GroupMembership {
                group_id: folder.group_id,
                item_id: item.into(),
            }],
            expected_revisions: BTreeMap::new(),
        }
    }

    #[test]
    fn native_import_is_atomic_scoped_and_replay_preserves_owner_changes() {
        let dir = tempfile::tempdir().unwrap();
        let item = ingest(dir.path(), &capture(), draft()).unwrap().item_ids[0].clone();
        let mut p = plan(&item);
        p.expected_revisions.insert(
            item.clone(),
            read_item(dir.path(), &item).unwrap().1.revision_id,
        );
        let private = Sensitivity::Private;
        assert!(import_native_groups(dir.path(), &p, Sensitivity::Public).is_err());
        let mut stale = p.clone();
        stale
            .expected_revisions
            .insert(item.clone(), "revision_stale".into());
        assert!(import_native_groups(dir.path(), &stale, private).is_err());
        let mut bad = p.clone();
        bad.memberships.push(GroupMembership {
            group_id: p.root_group_id.clone(),
            item_id: "item_missing".into(),
        });
        assert!(import_native_groups(dir.path(), &bad, private).is_err());
        assert!(!dir.path().join(GROUPS_PATH).exists());
        let generation = crate::catalog_generation(dir.path()).unwrap();
        let receipt = import_native_groups(dir.path(), &p, private).unwrap();
        assert_ne!(generation, crate::catalog_generation(dir.path()).unwrap());
        let tree = read_group_tree(dir.path(), private, 0, 100, None).unwrap();
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(tree.nodes[0].subtree_item_count, 1);
        assert!(!tree.nodes[0].read_only);
        edit_groups(
            dir.path(),
            "rename-after-import",
            GroupEditOp::Rename {
                group_id: p.root_group_id.clone(),
                expected_version: "v1".into(),
                name: "My title".into(),
            },
            private,
        )
        .unwrap();
        edit_groups(
            dir.path(),
            "remove-after-import",
            GroupEditOp::RemoveMembers {
                group_id: "group_import_child".into(),
                expected_version: "v1".into(),
                item_ids: vec![item],
            },
            private,
        )
        .unwrap();
        let before = fs::read(dir.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(
            import_native_groups(dir.path(), &p, private).unwrap(),
            receipt
        );
        assert_eq!(fs::read(dir.path().join(GROUPS_PATH)).unwrap(), before);
        let mut altered = p.clone();
        altered.nodes[0].name = "changed input".into();
        assert!(import_native_groups(dir.path(), &altered, private).is_err());
        let mut collision = p.clone();
        collision.request_id = "another-run".into();
        assert!(import_native_groups(dir.path(), &collision, private).is_err());
        assert_eq!(fs::read(dir.path().join(GROUPS_PATH)).unwrap(), before);
    }
}
