//! Trusted owner-local filing only; no transport authority or queue is created here.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PodcastArchiveSource {
    pub group_id: String,
    pub expected_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PodcastArchiveRequest {
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    pub target_group_id: String,
    pub expected_target_version: String,
    pub sources: Vec<PodcastArchiveSource>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_root_or_nested_target: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The caller must already be authenticated as the local owner. The ceiling only
/// narrows that authority. Exact replay returns a historical receipt, not a claim
/// about current membership; it never reapplies a move after subsequent edits.
pub fn archive_podcast(
    library: impl AsRef<Path>,
    mut request: PodcastArchiveRequest,
    ceiling: Sensitivity,
) -> Result<GroupEditReceipt> {
    anyhow::ensure!(
        valid_item_id(&request.item_id)
            && valid_revision_id(&request.expected_revision_id)
            && valid_text(&request.request_id, 256)
            && valid_group_id(&request.target_group_id)
            && valid_text(&request.expected_target_version, 128)
            && request.sources.len() <= 2
            && request.sources.iter().all(|s| valid_group_id(&s.group_id)
                && valid_text(&s.expected_version, 128)
                && s.group_id != request.target_group_id),
        "invalid_group_edit"
    );
    request.sources.sort_by(|a, b| a.group_id.cmp(&b.group_id));
    anyhow::ensure!(
        !request
            .sources
            .windows(2)
            .any(|s| s[0].group_id == s[1].group_id),
        "invalid_group_edit"
    );
    // Domain separation prevents collisions with ordinary GroupEditOp receipts.
    let request_sha256 = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(
            "owner_local_podcast_archive_v1",
            &request
        ))?)
    );
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = load_state(&root)?;
    let effective = effective_sensitivities(&state);
    let inactive = group_delete::inactive_nodes(&state);
    let authorized = |id: &str| -> Result<usize> {
        let index = state
            .nodes
            .iter()
            .position(|n| n.group_id == id)
            .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
        anyhow::ensure!(
            effective[id] <= ceiling && !inactive.contains(id),
            "library_resource_not_found"
        );
        let node = &state.nodes[index];
        anyhow::ensure!(
            node.owner_id == LOCAL_OWNER_ID && !node.read_only,
            "group_read_only"
        );
        Ok(index)
    };
    let target_index = authorized(&request.target_group_id)?;
    let target = &state.nodes[target_index];
    let root_index = authorized(&target.root_group_id)?;
    let library_root = &state.nodes[root_index];
    if request.allow_root_or_nested_target {
        // load_state validates kinds, same-root parent links, and acyclic paths
        // ending at the declared physical root. Reuse the current visibility,
        // activity and owner-write checks for EVERY node, not just endpoints.
        let mut current = target;
        while let Some(parent_id) = &current.parent_id {
            current = &state.nodes[authorized(parent_id)?];
        }
        anyhow::ensure!(
            current.group_id == library_root.group_id,
            "invalid_group_edit"
        );
    } else {
        anyhow::ensure!(
            target.kind == GroupKind::Collection
                && target.parent_id.as_deref() == Some(library_root.group_id.as_str())
                && library_root.parent_id.is_none(),
            "invalid_group_edit"
        );
    }
    let mut source_indices = Vec::new();
    for source in &request.sources {
        let index = authorized(&source.group_id)?;
        let node = &state.nodes[index];
        anyhow::ensure!(
            node.root_group_id == library_root.group_id
                && (index == root_index
                    || (node.kind == GroupKind::Collection
                        && node.parent_id.as_deref() == Some(library_root.group_id.as_str()))),
            "invalid_group_edit"
        );
        source_indices.push(index);
    }
    let item = read_item_metadata_at_sensitivity(&root, &request.item_id, ceiling)?
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    anyhow::ensure!(!is_item_trashed(&root, &request.item_id)?, "item_trashed");
    group_trash::ensure_members_removable(
        &root,
        &library_root.group_id,
        &[request.item_id.clone()],
    )?;
    if let Some(record) = state.requests.get(&request.request_id) {
        anyhow::ensure!(
            record.request_sha256 == request_sha256,
            "request_id_conflict"
        );
        // Old CAS/revision tokens identify the committed request, not a fresh
        // write. Rechecking them would break replay of our own version bumps.
        let receipt = record.receipt.clone();
        lock.unlock()?;
        return Ok(receipt);
    }
    anyhow::ensure!(
        item.revision_id == request.expected_revision_id,
        "stale_item_revision"
    );
    anyhow::ensure!(
        target.version_token() == request.expected_target_version,
        "stale_group_version"
    );
    for (source, index) in request.sources.iter().zip(&source_indices) {
        anyhow::ensure!(
            state.nodes[*index].version_token() == source.expected_version,
            "stale_group_version"
        );
        anyhow::ensure!(
            state
                .memberships
                .iter()
                .any(|m| m.group_id == source.group_id && m.item_id == request.item_id),
            "stale_group_membership"
        );
    }
    anyhow::ensure!(
        state.requests.len() < MAX_GROUP_REQUESTS,
        "group_request_limit_exceeded"
    );
    let membership = GroupMembership {
        group_id: request.target_group_id.clone(),
        item_id: request.item_id.clone(),
    };
    if !state.memberships.contains(&membership) {
        state.memberships.push(membership);
        state.nodes[target_index].version = state.nodes[target_index]
            .version
            .checked_add(1)
            .context("group_limit_exceeded")?;
    }
    state.memberships.retain(|m| {
        m.item_id != request.item_id || !request.sources.iter().any(|s| s.group_id == m.group_id)
    });
    state.memberships.sort();
    for index in source_indices {
        state.nodes[index].version = state.nodes[index]
            .version
            .checked_add(1)
            .context("group_limit_exceeded")?;
    }
    anyhow::ensure!(
        state.memberships.len() <= MAX_GROUP_MEMBERSHIPS,
        "group_limit_exceeded"
    );
    let receipt = GroupEditReceipt {
        schema_version: SCHEMA_VERSION,
        group_id: request.target_group_id,
        version: state.nodes[target_index].version_token(),
    };
    state.groups_version = state
        .groups_version
        .checked_add(1)
        .context("group_limit_exceeded")?;
    state.requests.insert(
        request.request_id,
        GroupRequestRecord {
            request_sha256,
            receipt: receipt.clone(),
        },
    );
    write_state(&root, &state)?;
    lock.unlock()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{capture, draft};

    const ALL: Sensitivity = Sensitivity::Restricted;

    fn fixture() -> (tempfile::TempDir, PodcastArchiveRequest) {
        let dir = tempfile::tempdir().unwrap();
        let item = ingest(dir.path(), &capture(), draft()).unwrap().item_ids[0].clone();
        let revision = read_item_metadata_at_sensitivity(dir.path(), &item, ALL)
            .unwrap()
            .unwrap()
            .revision_id;
        let mut state = GroupState {
            state_version: 1,
            ..Default::default()
        };
        for (id, parent, root, name) in [
            ("group_root", None, "group_root", "Library"),
            ("group_target", Some("group_root"), "group_root", "Podcasts"),
            ("group_inbox", Some("group_root"), "group_root", "Inbox"),
            ("group_decoy", Some("group_inbox"), "group_root", "Podcasts"),
            ("group_other", None, "group_other", "Other"),
        ] {
            state.nodes.push(GroupNode {
                group_id: id.into(),
                name: name.into(),
                kind: if parent.is_some() {
                    GroupKind::Collection
                } else {
                    GroupKind::User
                },
                parent_id: parent.map(str::to_owned),
                root_group_id: root.into(),
                source: None,
                owner_id: LOCAL_OWNER_ID.into(),
                sensitivity: Sensitivity::Public,
                read_only: false,
                version: 1,
            });
        }
        for id in ["group_decoy", "group_inbox", "group_other", "group_root"] {
            state.memberships.push(GroupMembership {
                group_id: id.into(),
                item_id: item.clone(),
            });
        }
        write_state(dir.path(), &state).unwrap();
        (
            dir,
            PodcastArchiveRequest {
                item_id: item,
                expected_revision_id: revision,
                request_id: "archive-1".into(),
                target_group_id: "group_target".into(),
                expected_target_version: "v1".into(),
                allow_root_or_nested_target: false,
                sources: vec![
                    PodcastArchiveSource {
                        group_id: "group_root".into(),
                        expected_version: "v1".into(),
                    },
                    PodcastArchiveSource {
                        group_id: "group_inbox".into(),
                        expected_version: "v1".into(),
                    },
                ],
            },
        )
    }

    fn reject_unchanged(
        path: &Path,
        request: PodcastArchiveRequest,
        ceiling: Sensitivity,
        error: &str,
    ) {
        let before = fs::read(path.join(GROUPS_PATH)).unwrap();
        assert_eq!(
            archive_podcast(path, request, ceiling)
                .unwrap_err()
                .to_string(),
            error
        );
        assert_eq!(fs::read(path.join(GROUPS_PATH)).unwrap(), before);
    }

    fn deep_fixture() -> (tempfile::TempDir, PodcastArchiveRequest) {
        let (dir, mut request) = fixture();
        let mut state = load_state(dir.path()).unwrap();
        let mut node = state
            .nodes
            .iter()
            .find(|n| n.group_id == "group_decoy")
            .unwrap()
            .clone();
        node.group_id = "group_deep".into();
        node.parent_id = Some("group_decoy".into());
        state.nodes.push(node);
        write_state(dir.path(), &state).unwrap();
        request.target_group_id = "group_deep".into();
        request.allow_root_or_nested_target = true;
        request.sources.clear();
        (dir, request)
    }

    #[test]
    fn podcast_archive_explicit_root_and_deep_targets_only_add_membership() {
        for target in ["group_other", "group_deep"] {
            let (dir, mut request) = deep_fixture();
            request.target_group_id = target.into();
            let mut before = load_state(dir.path()).unwrap();
            // Exercise a new root membership as well as a new nested one.
            before.memberships.retain(|m| m.group_id != "group_other");
            write_state(dir.path(), &before).unwrap();
            let receipt = archive_podcast(dir.path(), request.clone(), ALL).unwrap();
            assert_eq!(receipt.group_id, target);
            assert_eq!(receipt.version, "v2");
            let after = load_state(dir.path()).unwrap();
            assert_eq!(after.memberships.len(), before.memberships.len() + 1);
            assert!(before
                .memberships
                .iter()
                .all(|m| after.memberships.contains(m)));
            for node in &after.nodes {
                assert_eq!(node.version, if node.group_id == target { 2 } else { 1 });
            }
            assert_eq!(archive_podcast(dir.path(), request, ALL).unwrap(), receipt);
            assert_eq!(load_state(dir.path()).unwrap(), after);
        }
    }

    #[test]
    fn podcast_archive_deep_target_checks_all_ancestors_before_write_and_replay() {
        for replay in [false, true] {
            for id in ["group_deep", "group_decoy", "group_inbox", "group_root"] {
                for change in ["owner", "read_only", "hidden", "deleted"] {
                    let (dir, request) = deep_fixture();
                    if replay {
                        archive_podcast(dir.path(), request.clone(), ALL).unwrap();
                    }
                    if change == "deleted" {
                        let impact = group_delete::preview(dir.path(), id, ALL).unwrap();
                        group_delete::delete(
                            dir.path(),
                            LOCAL_OWNER_ID,
                            "delete",
                            &format!("sha256:{:x}", Sha256::digest(b"delete")),
                            id,
                            &impact.impact_sha256,
                            ALL,
                            std::time::Instant::now() + std::time::Duration::from_secs(60),
                        )
                        .unwrap();
                    } else {
                        let mut state = load_state(dir.path()).unwrap();
                        let node = state.nodes.iter_mut().find(|n| n.group_id == id).unwrap();
                        match change {
                            "owner" => node.owner_id = "other_owner".into(),
                            "read_only" => node.read_only = true,
                            _ => node.sensitivity = Sensitivity::Restricted,
                        }
                        write_state(dir.path(), &state).unwrap();
                    }
                    reject_unchanged(
                        dir.path(),
                        request,
                        Sensitivity::Private,
                        if matches!(change, "hidden" | "deleted") {
                            "library_resource_not_found"
                        } else {
                            "group_read_only"
                        },
                    );
                }
            }
        }
    }

    #[test]
    fn podcast_archive_explicit_targets_keep_exact_legacy_sources_contract() {
        for count in 0..=2 {
            let (dir, mut request) = deep_fixture();
            request.sources = ["group_root", "group_inbox"][..count]
                .iter()
                .map(|id| PodcastArchiveSource {
                    group_id: (*id).into(),
                    expected_version: "v1".into(),
                })
                .collect();
            let before = load_state(dir.path()).unwrap();
            archive_podcast(dir.path(), request.clone(), ALL).unwrap();
            let after = load_state(dir.path()).unwrap();
            for membership in before.memberships {
                assert_eq!(
                    after.memberships.contains(&membership),
                    !request
                        .sources
                        .iter()
                        .any(|s| s.group_id == membership.group_id)
                );
            }
            assert_eq!(after.memberships.len(), 5 - count);
        }
        let (dir, request) = deep_fixture();
        for ids in [
            vec!["group_decoy"],
            vec!["group_other"],
            vec!["group_deep"],
            vec!["group_root", "group_root"],
            vec!["group_root", "group_inbox", "group_target"],
        ] {
            let mut invalid = request.clone();
            invalid.sources = ids
                .iter()
                .map(|id| PodcastArchiveSource {
                    group_id: (*id).into(),
                    expected_version: "v1".into(),
                })
                .collect();
            reject_unchanged(dir.path(), invalid, ALL, "invalid_group_edit");
        }
        for target in ["group_root", "group_deep"] {
            let mut legacy = request.clone();
            legacy.target_group_id = target.into();
            legacy.allow_root_or_nested_target = false;
            reject_unchanged(dir.path(), legacy, ALL, "invalid_group_edit");
        }
        let mut root_target = request;
        root_target.target_group_id = "group_root".into();
        root_target.sources = vec![PodcastArchiveSource {
            group_id: "group_inbox".into(),
            expected_version: "v1".into(),
        }];
        archive_podcast(dir.path(), root_target, ALL).unwrap();
        assert_eq!(
            load_state(dir.path())
                .unwrap()
                .memberships
                .iter()
                .map(|m| m.group_id.as_str())
                .collect::<Vec<_>>(),
            ["group_decoy", "group_other", "group_root"]
        );
    }

    #[test]
    fn podcast_archive_legacy_serialization_and_digest_are_unchanged() {
        let (dir, mut request) = fixture();
        request.sources.sort_by(|a, b| a.group_id.cmp(&b.group_id));
        // Freeze the pre-extension field order and bytes, independently of the DTO.
        let old_json = format!(
            r#"{{"item_id":"{}","expected_revision_id":"{}","request_id":"archive-1","target_group_id":"group_target","expected_target_version":"v1","sources":[{{"group_id":"group_inbox","expected_version":"v1"}},{{"group_id":"group_root","expected_version":"v1"}}]}}"#,
            request.item_id, request.expected_revision_id,
        );
        let decoded: PodcastArchiveRequest = serde_json::from_str(&old_json).unwrap();
        assert!(!decoded.allow_root_or_nested_target);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), old_json);
        let old_digest = format!(
            "sha256:{:x}",
            Sha256::digest(format!(r#"["owner_local_podcast_archive_v1",{old_json}]"#).as_bytes())
        );
        let receipt = archive_podcast(dir.path(), decoded.clone(), ALL).unwrap();
        assert_eq!(
            load_state(dir.path()).unwrap().requests["archive-1"].request_sha256,
            old_digest
        );
        assert_eq!(archive_podcast(dir.path(), decoded, ALL).unwrap(), receipt);
        request.allow_root_or_nested_target = true;
        assert!(serde_json::to_string(&request)
            .unwrap()
            .contains("\"allow_root_or_nested_target\":true"));
        reject_unchanged(dir.path(), request, ALL, "request_id_conflict");
    }

    #[test]
    fn podcast_archive_exact_ids_preserve_other_memberships_and_replay_history() {
        let (dir, request) = fixture();
        let receipt = archive_podcast(dir.path(), request.clone(), ALL).unwrap();
        let state = load_state(dir.path()).unwrap();
        assert_eq!(
            state
                .memberships
                .iter()
                .map(|m| m.group_id.as_str())
                .collect::<Vec<_>>(),
            ["group_decoy", "group_other", "group_target"]
        );
        for node in &state.nodes {
            assert_eq!(
                node.version,
                if ["group_target", "group_root", "group_inbox"].contains(&node.group_id.as_str()) {
                    2
                } else {
                    1
                }
            );
        }
        let before = fs::read(dir.path().join(GROUPS_PATH)).unwrap();
        let mut reversed = request.clone();
        reversed.sources.reverse();
        assert_eq!(archive_podcast(dir.path(), reversed, ALL).unwrap(), receipt);
        assert_eq!(fs::read(dir.path().join(GROUPS_PATH)).unwrap(), before);
        let mut conflict = request.clone();
        conflict.sources.clear();
        reject_unchanged(dir.path(), conflict, ALL, "request_id_conflict");
        edit_groups(
            dir.path(),
            "later-remove",
            GroupEditOp::RemoveMembers {
                group_id: receipt.group_id.clone(),
                expected_version: receipt.version.clone(),
                item_ids: vec![request.item_id.clone()],
            },
            ALL,
        )
        .unwrap();
        let after_remove = fs::read(dir.path().join(GROUPS_PATH)).unwrap();
        assert_eq!(archive_podcast(dir.path(), request, ALL).unwrap(), receipt);
        assert_eq!(
            fs::read(dir.path().join(GROUPS_PATH)).unwrap(),
            after_remove
        );
    }

    #[test]
    fn podcast_archive_all_cas_and_membership_failures_are_atomic() {
        let (dir, request) = fixture();
        for index in 0..3 {
            let mut stale = request.clone();
            if index == 2 {
                stale.expected_target_version = "v0".into();
            } else {
                stale.sources[index].expected_version = "v0".into();
            }
            reject_unchanged(dir.path(), stale, ALL, "stale_group_version");
        }
        let mut stale = request.clone();
        stale.expected_revision_id = new_id("revision");
        reject_unchanged(dir.path(), stale, ALL, "stale_item_revision");
        let mut state = load_state(dir.path()).unwrap();
        state.memberships.retain(|m| m.group_id != "group_inbox");
        write_state(dir.path(), &state).unwrap();
        reject_unchanged(dir.path(), request, ALL, "stale_group_membership");
    }

    #[test]
    fn podcast_archive_rejects_wrong_shape_cross_root_and_ambiguous_names() {
        let (dir, request) = fixture();
        for id in ["group_decoy", "group_root"] {
            let mut invalid = request.clone();
            invalid.target_group_id = id.into();
            invalid.sources.clear();
            reject_unchanged(dir.path(), invalid, ALL, "invalid_group_edit");
        }
        for id in ["group_decoy", "group_other", "group_target"] {
            let mut invalid = request.clone();
            invalid.sources[0].group_id = id.into();
            reject_unchanged(dir.path(), invalid, ALL, "invalid_group_edit");
        }
        let mut duplicate = request.clone();
        duplicate.sources[0] = duplicate.sources[1].clone();
        reject_unchanged(dir.path(), duplicate, ALL, "invalid_group_edit");
        let mut too_many = request.clone();
        too_many.sources.push(too_many.sources[0].clone());
        reject_unchanged(dir.path(), too_many, ALL, "invalid_group_edit");
    }

    #[test]
    fn podcast_archive_current_authority_checked_even_on_replay() {
        for replay in [false, true] {
            for id in ["group_root", "group_target", "group_inbox"] {
                for change in ["owner", "read_only", "hidden"] {
                    let (dir, request) = fixture();
                    if replay {
                        archive_podcast(dir.path(), request.clone(), ALL).unwrap();
                    }
                    let mut state = load_state(dir.path()).unwrap();
                    let node = state.nodes.iter_mut().find(|n| n.group_id == id).unwrap();
                    match change {
                        "owner" => node.owner_id = "other_owner".into(),
                        "read_only" => node.read_only = true,
                        _ => node.sensitivity = Sensitivity::Restricted,
                    }
                    write_state(dir.path(), &state).unwrap();
                    reject_unchanged(
                        dir.path(),
                        request,
                        Sensitivity::Private,
                        if change == "hidden" {
                            "library_resource_not_found"
                        } else {
                            "group_read_only"
                        },
                    );
                }
            }
        }
    }

    #[test]
    fn podcast_archive_rejects_global_and_root_trash_before_replay() {
        for replay in [false, true] {
            for global in [false, true] {
                let (dir, request) = fixture();
                if replay {
                    archive_podcast(dir.path(), request.clone(), ALL).unwrap();
                }
                if global {
                    trash_item(
                        dir.path(),
                        &request.item_id,
                        &request.expected_revision_id,
                        "manual",
                        None,
                        None,
                    )
                    .unwrap();
                } else {
                    edit_group_trash(
                        dir.path(),
                        "group_root",
                        &request.item_id,
                        "trash",
                        &request.expected_revision_id,
                        None,
                        ALL,
                    )
                    .unwrap();
                }
                // Metadata reads intentionally hide globally trashed items;
                // root trash is a separately checked organization constraint.
                reject_unchanged(
                    dir.path(),
                    request,
                    ALL,
                    if global {
                        "library_resource_not_found"
                    } else {
                        "item_trashed"
                    },
                );
            }
        }
    }

    #[test]
    fn podcast_archive_existing_target_and_empty_sources_are_replayable() {
        let (dir, original) = fixture();
        let mut request = original.clone();
        request.sources.clear();
        archive_podcast(dir.path(), request.clone(), ALL).unwrap();
        request.request_id = "already-filed".into();
        request.expected_target_version = "v2".into();
        let receipt = archive_podcast(dir.path(), request.clone(), ALL).unwrap();
        assert_eq!(receipt.version, "v2");
        assert_eq!(archive_podcast(dir.path(), request, ALL).unwrap(), receipt);
        assert_eq!(load_state(dir.path()).unwrap().memberships.len(), 5);
        let mut remove_sources = original;
        remove_sources.request_id = "remove-sources".into();
        remove_sources.expected_target_version = "v2".into();
        assert_eq!(
            archive_podcast(dir.path(), remove_sources, ALL)
                .unwrap()
                .version,
            "v2"
        );
        assert_eq!(load_state(dir.path()).unwrap().memberships.len(), 3);
    }

    #[test]
    fn podcast_archive_hidden_item_and_deleted_nodes_reject() {
        let (dir, mut request) = fixture();
        let mut c = capture();
        c.capture.external_capture_id = "private-podcast".into();
        c.capture.sensitivity = Sensitivity::Private;
        let mut d = draft();
        d.sensitivity = Sensitivity::Private;
        request.item_id = ingest(dir.path(), &c, d).unwrap().item_ids[0].clone();
        request.expected_revision_id =
            read_item_metadata_at_sensitivity(dir.path(), &request.item_id, ALL)
                .unwrap()
                .unwrap()
                .revision_id;
        request.sources.clear();
        archive_podcast(dir.path(), request.clone(), ALL).unwrap();
        reject_unchanged(
            dir.path(),
            request.clone(),
            Sensitivity::Public,
            "library_resource_not_found",
        );
        request.request_id = "hidden-new".into();
        reject_unchanged(
            dir.path(),
            request,
            Sensitivity::Public,
            "library_resource_not_found",
        );
        for replay in [false, true] {
            for id in ["group_target", "group_inbox", "group_root"] {
                let (dir, request) = fixture();
                if replay {
                    archive_podcast(dir.path(), request.clone(), ALL).unwrap();
                }
                let impact = group_delete::preview(dir.path(), id, ALL).unwrap();
                group_delete::delete(
                    dir.path(),
                    LOCAL_OWNER_ID,
                    "delete",
                    &format!("sha256:{:x}", Sha256::digest(b"delete")),
                    id,
                    &impact.impact_sha256,
                    ALL,
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                )
                .unwrap();
                reject_unchanged(dir.path(), request, ALL, "library_resource_not_found");
            }
        }
    }

    #[test]
    fn podcast_archive_stopped_full_backup_restores_receipt_and_memberships() {
        fn copy_tree(from: &Path, to: &Path) {
            fs::create_dir_all(to).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let dest = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_tree(&entry.path(), &dest);
                } else {
                    fs::copy(entry.path(), dest).unwrap();
                }
            }
        }
        let (dir, request) = fixture();
        let receipt = archive_podcast(dir.path(), request.clone(), ALL).unwrap();
        // No writer is running: copy the whole synthetic store, not a groups-only rollback.
        let backup = tempfile::tempdir().unwrap();
        let restored = tempfile::tempdir().unwrap();
        copy_tree(dir.path(), backup.path());
        copy_tree(backup.path(), restored.path());
        let original = load_state(dir.path()).unwrap();
        assert_eq!(load_state(restored.path()).unwrap(), original);
        assert_eq!(
            archive_podcast(restored.path(), request.clone(), ALL).unwrap(),
            receipt
        );
        assert_eq!(load_state(restored.path()).unwrap(), original);
        assert_eq!(
            read_item_metadata_at_sensitivity(restored.path(), &request.item_id, ALL)
                .unwrap()
                .unwrap()
                .revision_id,
            request.expected_revision_id
        );
    }
}
