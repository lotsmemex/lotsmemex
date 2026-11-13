//! IPC handlers for groups_v1: tree read and single-op writes over Store `edit_groups`.
//! Query-side `group_id` / `include_descendants` travel inside `library_query`.

use crate::ipc::{Command, Response};
use anyhow::{bail, Result};
use memex_core::Sensitivity;
use memex_store::GroupEditOp;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) mod deletion;

/// Advertised in `groups_v1.write_ops`. Root sorting does not reparent folders.
pub(crate) const WRITE_OPS: [&str; 7] = [
    "create_root",
    "create_folder",
    "rename",
    "add_members",
    "remove_members",
    "reorder_root",
    "transfer_folders",
];

/// Wire form of one group write; the node version travels as `expected_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupEditWireOp {
    CreateRoot {
        name: String,
        /// Defaults to `private`; must not exceed the request ceiling.
        #[serde(default)]
        sensitivity: Option<Sensitivity>,
    },
    CreateFolder {
        parent_id: String,
        name: String,
    },
    Rename {
        id: String,
        name: String,
    },
    ReorderRoot {
        group_id: String,
        before_id: Option<String>,
    },
    AddMembers {
        group_id: String,
        item_ids: Vec<String>,
    },
    RemoveMembers {
        group_id: String,
        item_ids: Vec<String>,
    },
    TransferFolders {
        source_ids: Vec<String>,
        target_parent_id: String,
        expected_versions: std::collections::BTreeMap<String, String>,
        moving: bool,
    },
}

fn store_op(op: GroupEditWireOp, expected_version: Option<String>) -> Result<GroupEditOp> {
    let version = || {
        expected_version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("invalid_group_edit"))
    };
    Ok(match op {
        GroupEditWireOp::CreateRoot { name, sensitivity } => {
            if expected_version.is_some() {
                bail!("invalid_group_edit")
            }
            GroupEditOp::CreateRoot {
                name,
                sensitivity: sensitivity.unwrap_or(Sensitivity::Private),
            }
        }
        GroupEditWireOp::CreateFolder { parent_id, name } => GroupEditOp::CreateFolder {
            parent_id,
            expected_parent_version: version()?,
            name,
        },
        GroupEditWireOp::Rename { id, name } => GroupEditOp::Rename {
            group_id: id,
            expected_version: version()?,
            name,
        },
        GroupEditWireOp::ReorderRoot { group_id, before_id } => GroupEditOp::ReorderRoot {
            group_id,
            expected_version: version()?,
            before_id,
        },
        GroupEditWireOp::AddMembers { group_id, item_ids } => GroupEditOp::AddMembers {
            group_id,
            expected_version: version()?,
            item_ids,
        },
        GroupEditWireOp::RemoveMembers { group_id, item_ids } => GroupEditOp::RemoveMembers {
            group_id,
            expected_version: version()?,
            item_ids,
        },
        GroupEditWireOp::TransferFolders { source_ids, target_parent_id, expected_versions, moving } => GroupEditOp::TransferFolders {
            source_ids, target_parent_id, expected_parent_version: version()?, expected_versions, moving,
        },
    })
}

/// Handles groups_v1 commands; returns any other command unchanged.
pub(crate) fn dispatch(
    library: &Path,
    queries: &mut memex_library::LibraryQuery,
    c: Command,
) -> Result<std::result::Result<Response, Command>> {
    let mut response = Response::empty_ok();
    match c {
        command @ (Command::GroupDeletePreview { .. } | Command::GroupDelete { .. }
            | Command::GroupDeleteList { .. } | Command::GroupDeleteRestore { .. }) => {
            return Ok(Ok(deletion::dispatch(library, command)?));
        }
        Command::GroupTreeRead {
            max_sensitivity,
            offset,
            limit,
            expected_view,
        } => {
            response.group_tree = Some(queries.group_tree(
                max_sensitivity,
                offset,
                limit,
                expected_view.as_deref(),
            )?);
        }
        Command::GroupEdit {
            max_sensitivity,
            request_id,
            expected_version,
            ops,
        } => {
            // Store writes are single-op atomic with one replay record; a batch would not be.
            let [op]: [GroupEditWireOp; 1] = ops
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid_group_edit"))?;
            let transfer = matches!(&op, GroupEditWireOp::TransferFolders { .. });
            let receipt = memex_store::edit_groups(
                library,
                &request_id,
                store_op(op, expected_version)?,
                max_sensitivity,
            )?;
            // Transfers return a small durable receipt only. An optional large
            // tree must never overflow the frame and hide a committed mapping.
            if !transfer {
                response.group_tree = Some(queries.group_tree(
                    max_sensitivity, 0, memex_store::MAX_GROUP_TREE_PAGE, None,
                )?);
            }
            response.group_edit = Some(receipt);
        }
        other => return Ok(Err(other)),
    }
    Ok(Ok(response))
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folder_transfer_wire_dispatch_replays_its_receipt_even_if_projection_refresh_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ceiling = Sensitivity::Restricted;
        let library = memex_store::edit_groups(root, "root", GroupEditOp::CreateRoot { name: "library".into(), sensitivity: Sensitivity::Public }, ceiling).unwrap();
        let source = memex_store::edit_groups(root, "source", GroupEditOp::CreateFolder { parent_id: library.group_id.clone(), expected_parent_version: "v1".into(), name: "source".into() }, ceiling).unwrap();
        let target = memex_store::edit_groups(root, "target", GroupEditOp::CreateFolder { parent_id: library.group_id.clone(), expected_parent_version: "v2".into(), name: "target".into() }, ceiling).unwrap();
        let request = json!({"command":"group_edit","max_sensitivity":"restricted","request_id":"folder-wire","expected_version":"v1",
            "ops":[{"op":"transfer_folders","source_ids":[source.group_id],"target_parent_id":target.group_id,
                "expected_versions":{library.group_id.clone():"v3",source.group_id.clone():"v1"},"moving":false}]});
        for remove in ["expected_versions", "moving", "target_parent_id"] {
            let mut bad = request.clone();
            bad["ops"][0].as_object_mut().unwrap().remove(remove);
            assert!(serde_json::from_value::<Command>(bad).is_err(), "{remove}");
        }
        let mut injected = request.clone();
        injected["ops"][0]["owner_id"] = "remote".into();
        assert!(serde_json::from_value::<Command>(injected).is_err());
        let mut no_version = request.clone();
        no_version.as_object_mut().unwrap().remove("expected_version");
        let mut queries = memex_library::LibraryQuery::new(root).unwrap();
        assert_eq!(dispatch(root, &mut queries, serde_json::from_value(no_version).unwrap()).unwrap_err().to_string(), "invalid_group_edit");

        // The Store is real and synthetic. Only the read-projection root is
        // broken; a committed receipt must still reach the caller.
        let bad_root = root.join("not-a-directory");
        std::fs::write(&bad_root, b"fixture").unwrap();
        let mut broken_queries = memex_library::LibraryQuery::new(&bad_root).unwrap();
        assert!(broken_queries.group_tree(ceiling, 0, 200, None).is_err());
        let reply = dispatch(root, &mut broken_queries, serde_json::from_value(request.clone()).unwrap()).unwrap().unwrap();
        assert!(reply.ok && reply.group_tree.is_none());
        let receipt = reply.group_edit.unwrap();
        assert_eq!(receipt.group_id, target.group_id);
        let before = std::fs::read(root.join("catalog/groups/state.json")).unwrap();
        let replay = dispatch(root, &mut queries, serde_json::from_value(request).unwrap()).unwrap().unwrap();
        assert_eq!(replay.group_edit, Some(receipt));
        assert!(replay.group_tree.is_none());
        assert_eq!(memex_store::read_group_tree(root, ceiling, 0, 500, None).unwrap().nodes.len(), 4);
        assert_eq!(std::fs::read(root.join("catalog/groups/state.json")).unwrap(), before);
        assert!(WRITE_OPS.contains(&"transfer_folders"));
    }

    #[test]
    fn folder_transfer_receipt_stays_bounded_when_the_committed_tree_exceeds_the_ipc_frame() {
        use memex_store::{GroupKind, GroupNode, NativeGroupImport};
        use std::collections::BTreeMap;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let node = |id: String, parent: Option<String>| GroupNode {
            group_id: id, name: "n".repeat(1024), kind: if parent.is_none() { GroupKind::User } else { GroupKind::Collection },
            parent_id: parent, root_group_id: "group_root".into(), source: None, owner_id: "owner_local".into(),
            sensitivity: Sensitivity::Public, read_only: false, version: 1,
        };
        let mut nodes = vec![node("group_root".into(), None), node("group_source".into(), Some("group_root".into()))];
        nodes.extend((0..140).map(|i| node(format!("group_child_{i}"), Some("group_source".into()))));
        let expected_versions: BTreeMap<_, _> = nodes.iter().map(|n| (n.group_id.clone(), "v1".to_owned())).collect();
        memex_store::import_native_groups(root, &NativeGroupImport { request_id: "large-tree".into(), root_group_id: "group_root".into(),
            nodes, memberships: vec![], expected_revisions: BTreeMap::new() }, Sensitivity::Restricted).unwrap();
        let command = || Command::GroupEdit { max_sensitivity: Sensitivity::Restricted, request_id: "large-transfer".into(), expected_version: Some("v1".into()),
            ops: vec![GroupEditWireOp::TransferFolders { source_ids: vec!["group_source".into()], target_parent_id: "group_root".into(), expected_versions: expected_versions.clone(), moving: false }] };
        let mut queries = memex_library::LibraryQuery::new(root).unwrap();
        let reply = dispatch(root, &mut queries, command()).unwrap().unwrap();
        assert!(reply.ok && reply.group_tree.is_none());
        let encoded = serde_json::to_vec(&reply).unwrap();
        assert!(encoded.len() < 4096, "receipt fits even with other fixed response fields");
        let tree = memex_store::read_group_tree(root, Sensitivity::Restricted, 0, 500, None).unwrap();
        assert_eq!(tree.nodes.len(), 283);
        assert!(serde_json::to_vec(&tree).unwrap().len() > 256 * 1024, "regression actually exceeds the old response bound");
        let before = std::fs::read(root.join("catalog/groups/state.json")).unwrap();
        let replay = dispatch(root, &mut queries, command()).unwrap().unwrap();
        assert_eq!(replay.group_edit, reply.group_edit);
        assert_eq!(std::fs::read(root.join("catalog/groups/state.json")).unwrap(), before);
    }
}
