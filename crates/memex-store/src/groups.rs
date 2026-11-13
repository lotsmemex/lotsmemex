//! groups_v1 draft3: persisted group/collection trees and many-to-many item
//! membership. Membership never copies item content; removing it never deletes
//! an item or Evidence. Initial native import is a separate atomic operation;
//! general move/reparent and source synchronization remain unimplemented.
//!
//! Storage: one canonical `catalog/groups/state.json`, replaced atomically
//! (`durable_replace`) under the Store writer lock. Each write's durable
//! request identity is recorded inside the same file, so the state change and
//! its replay record commit together.

use super::*;

#[path = "group_import.rs"]
mod group_import;
pub use group_import::{import_native_groups, NativeGroupImport};

#[path = "group_trash.rs"]
mod group_trash;
pub use group_trash::{edit_group_trash, list_group_trash};

#[path = "group_delete.rs"]
pub mod group_delete;

pub mod podcast_archive;
pub use podcast_archive::{archive_podcast, PodcastArchiveRequest, PodcastArchiveSource};

mod transfer;

const GROUPS_PATH: &str = "catalog/groups/state.json";
const GROUPS_STATE_VERSION: u32 = 1;
pub const MAX_GROUP_NODES: usize = 65_536;
pub const MAX_GROUP_MEMBERSHIPS: usize = 1_000_000;
pub const MAX_GROUP_REQUESTS: usize = 65_536;
pub const MAX_GROUP_EDIT_ITEMS: usize = 256;
pub const MAX_GROUP_TREE_PAGE: usize = 500;
const MAX_GROUP_NAME_BYTES: usize = 1024;
pub const LOCAL_OWNER_ID: &str = "owner_local";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    ZoteroGroup,
    Collection,
    User,
}

impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZoteroGroup => "zotero_group",
            Self::Collection => "collection",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSource {
    pub system: String,
    pub library_key: String,
    pub object_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupNode {
    pub group_id: String,
    pub name: String,
    pub kind: GroupKind,
    pub parent_id: Option<String>,
    pub root_group_id: String,
    pub source: Option<GroupSource>,
    pub owner_id: String,
    pub sensitivity: Sensitivity,
    pub read_only: bool,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMembership {
    pub group_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupEditReceipt {
    pub schema_version: u32,
    pub group_id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupRequestRecord {
    request_sha256: String,
    receipt: GroupEditReceipt,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupState {
    state_version: u32,
    groups_version: u64,
    nodes: Vec<GroupNode>,
    memberships: Vec<GroupMembership>,
    requests: BTreeMap<String, GroupRequestRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    deletions: BTreeMap<String, group_delete::Deletion>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    deletion_requests: BTreeMap<String, group_delete::Request>,
}

fn version_token(version: u64) -> String {
    format!("v{version}")
}

impl GroupNode {
    /// The `expected_version` token for writes to this node.
    pub fn version_token(&self) -> String {
        version_token(self.version)
    }
}

fn valid_group_id(value: &str) -> bool {
    safe_component(value, "group_") && value.len() <= 128
}

fn invalid_state() -> anyhow::Error {
    anyhow::anyhow!("invalid group state")
}

/// Full structural validation: unique IDs, kinds, single parents within one
/// root, no cycles/orphans, sorted unique memberships, bounded sizes.
fn validate_state(state: &GroupState) -> Result<()> {
    if !matches!(state.state_version, 1 | 2)
        || state.nodes.len() > MAX_GROUP_NODES
        || state.memberships.len() > MAX_GROUP_MEMBERSHIPS
        || state.requests.len() > MAX_GROUP_REQUESTS
        || !strictly_sorted(&state.memberships)
    {
        return Err(invalid_state());
    }
    let nodes: BTreeMap<&str, &GroupNode> = state
        .nodes
        .iter()
        .map(|node| (node.group_id.as_str(), node))
        .collect();
    if nodes.len() != state.nodes.len() {
        return Err(invalid_state());
    }
    for node in &state.nodes {
        let source_ok = node.source.as_ref().is_none_or(|source| {
            valid_text(&source.system, 64)
                && valid_text(&source.library_key, 256)
                && valid_optional_text(&source.object_key, 256)
        });
        let shape_ok = match (&node.parent_id, node.kind) {
            (None, GroupKind::User | GroupKind::ZoteroGroup) => node.root_group_id == node.group_id,
            (Some(parent), GroupKind::Collection) => nodes
                .get(parent.as_str())
                .is_some_and(|parent| parent.root_group_id == node.root_group_id),
            _ => false,
        };
        if !valid_group_id(&node.group_id)
            || !valid_text(&node.name, MAX_GROUP_NAME_BYTES)
            || !valid_text(&node.owner_id, 128)
            || !source_ok
            || !shape_ok
        {
            return Err(invalid_state());
        }
        // Walking parents must reach this node's root without revisiting.
        let mut seen = BTreeSet::new();
        let mut current: &GroupNode = node;
        while let Some(parent) = &current.parent_id {
            if !seen.insert(current.group_id.as_str()) {
                return Err(invalid_state());
            }
            current = nodes
                .get(parent.as_str())
                .copied()
                .ok_or_else(invalid_state)?;
        }
        if current.group_id != node.root_group_id {
            return Err(invalid_state());
        }
    }
    for membership in &state.memberships {
        if !nodes.contains_key(membership.group_id.as_str()) || !valid_item_id(&membership.item_id)
        {
            return Err(invalid_state());
        }
    }
    for (request_id, record) in &state.requests {
        if !valid_text(request_id, 256)
            || !valid_sha256(&record.request_sha256)
            || !nodes.contains_key(record.receipt.group_id.as_str())
        {
            return Err(invalid_state());
        }
    }
    group_delete::validate(state)?;
    Ok(())
}

fn load_state(root: &Path) -> Result<GroupState> {
    let path = root.join(GROUPS_PATH);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GroupState {
                state_version: GROUPS_STATE_VERSION,
                ..Default::default()
            })
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("unsafe group state")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let state: GroupState = load(root, path).map_err(|_| invalid_state())?;
    validate_state(&state)?;
    Ok(state)
}

fn write_state(root: &Path, state: &GroupState) -> Result<()> {
    write_state_before(root, state, None)
}

/// Called only while web capture holds the Store writer lock. No move/removal,
/// no caller-provided path, and authorization includes every ancestor.
pub(super) fn web_filing_locked(root: &Path, target: &str, item: Option<&str>, request: Option<(&str, &str)>) -> Result<()> {
    anyhow::ensure!(valid_group_id(target), "invalid_web_target");
    let mut state = load_state(root)?;
    let inactive = group_delete::inactive_nodes(&state);
    let index = state.nodes.iter().position(|n| n.group_id == target).context("library_resource_not_found")?;
    let mut node = &state.nodes[index];
    loop {
        anyhow::ensure!(node.owner_id == LOCAL_OWNER_ID && !node.read_only && !inactive.contains(&node.group_id), "group_read_only");
        let Some(parent) = &node.parent_id else { break; };
        node = state.nodes.iter().find(|n| &n.group_id == parent).context("invalid_group_state")?;
    }
    let Some(item) = item else { return Ok(()) };
    anyhow::ensure!(read_item_metadata_at_sensitivity(root, item, Sensitivity::Restricted)?.is_some(), "library_resource_not_found");
    let (key, digest) = request.context("invalid_web_request")?;
    let request_id = format!("web-capture:{key}");
    if let Some(record) = state.requests.get(&request_id) {
        anyhow::ensure!(record.request_sha256 == digest && record.receipt.group_id == target, "request_identity_conflict");
        return Ok(());
    }
    anyhow::ensure!(state.requests.len() < MAX_GROUP_REQUESTS && state.memberships.len() < MAX_GROUP_MEMBERSHIPS, "group_limit_exceeded");
    let membership = GroupMembership { group_id: target.into(), item_id: item.into() };
    if !state.memberships.contains(&membership) {
        state.memberships.push(membership);
        state.memberships.sort();
        state.nodes[index].version = state.nodes[index].version.checked_add(1).context("group_limit_exceeded")?;
        state.groups_version = state.groups_version.checked_add(1).context("group_limit_exceeded")?;
    }
    let receipt = GroupEditReceipt { schema_version: 1, group_id: target.into(), version: state.nodes[index].version_token() };
    state.requests.insert(request_id, GroupRequestRecord { request_sha256: digest.into(), receipt });
    write_state(root, &state)
}

fn write_state_before(root: &Path, state: &GroupState, expires: Option<std::time::Instant>) -> Result<()> {
    validate_state(state)?;
    let dir = ensure_contained_dir(root, Path::new("catalog/groups"))?;
    #[cfg(unix)]
    fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    durable_replace_with_gate(
        root,
        &root.join(GROUPS_PATH),
        serde_json::to_string_pretty(state)?.as_bytes(),
        |publish| {
            if let Some(expires) = expires {
                anyhow::ensure!(std::time::Instant::now() < expires, "group_delete_preview_expired");
            }
            publish()
        },
    )
}

/// Effective sensitivity is the maximum along the ancestor chain, so a node
/// is never visible while its parent is hidden (no orphaned placeholders).
fn effective_sensitivities(state: &GroupState) -> BTreeMap<String, Sensitivity> {
    let nodes: BTreeMap<&str, &GroupNode> = state
        .nodes
        .iter()
        .map(|node| (node.group_id.as_str(), node))
        .collect();
    state
        .nodes
        .iter()
        .map(|node| {
            let mut level = node.sensitivity;
            let mut current = node;
            while let Some(parent) = current.parent_id.as_deref().and_then(|id| nodes.get(id)) {
                level = level.max(parent.sensitivity);
                current = parent;
            }
            (node.group_id.clone(), level)
        })
        .collect()
}

/// Authorized projection of the trees at one ceiling. Memberships are
/// limited to visible groups; callers must still filter by item visibility.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VisibleGroups {
    pub nodes: Vec<GroupNode>,
    pub effective_sensitivity: BTreeMap<String, Sensitivity>,
    pub memberships: Vec<GroupMembership>,
    pub trashed_item_ids: BTreeSet<String>,
}

/// Reads groups under an already-held consistent snapshot (`root` is the
/// canonical library root passed to `with_consistent_snapshot`).
pub fn read_visible_groups(root: &Path, max_sensitivity: Sensitivity) -> Result<VisibleGroups> {
    let state = load_state(root)?;
    let effective = effective_sensitivities(&state);
    let visible: BTreeSet<&str> = effective
        .iter()
        .filter(|(_, level)| **level <= max_sensitivity)
        .map(|(id, _)| id.as_str())
        .collect();
    let mut groups = VisibleGroups {
        nodes: state
            .nodes
            .iter()
            .filter(|node| visible.contains(node.group_id.as_str()))
            .cloned()
            .collect(),
        effective_sensitivity: effective
            .iter()
            .filter(|(id, _)| visible.contains(id.as_str()))
            .map(|(id, level)| (id.clone(), *level))
            .collect(),
        memberships: state
            .memberships
            .iter()
            .filter(|membership| visible.contains(membership.group_id.as_str()))
            .cloned()
            .collect(),
        trashed_item_ids: BTreeSet::new(),
    };
    groups.trashed_item_ids = group_trash::filter_memberships(root, &groups.nodes, &mut groups.memberships)?;
    let inactive = group_delete::inactive_nodes(&state);
    // Retained visible trash relationships continue suppressing Inbox even
    // when their organization is inactive. Only active non-trash membership
    // can override that suppression.
    let marked = group_trash::marked_items(root, &groups.nodes, &state.memberships)?;
    groups.nodes.retain(|node| !inactive.contains(&node.group_id));
    groups.effective_sensitivity.retain(|id, _| !inactive.contains(id));
    groups.memberships.retain(|m| !inactive.contains(&m.group_id));
    let active_items: BTreeSet<_> = groups.memberships.iter().map(|m| &m.item_id).collect();
    groups.trashed_item_ids.extend(marked.into_iter().filter(|id| !active_items.contains(id)));
    Ok(groups)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupNodeView {
    pub id: String,
    pub name: String,
    pub kind: GroupKind,
    pub parent_id: Option<String>,
    pub root_group_id: String,
    pub item_count: u64,
    pub subtree_item_count: u64,
    pub source: Option<GroupSource>,
    pub owner_id: String,
    pub sensitivity: Sensitivity,
    pub read_only: bool,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupTreePage {
    pub schema_version: u32,
    /// Binds the authorized tree, counts and ceiling; changes reject offsets.
    pub view: String,
    pub total: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    /// Preorder: Main first, then roots in saved order; folders by name/id.
    pub nodes: Vec<GroupNodeView>,
}

/// Children by name/id; roots retain their saved order, with Main pinned first.
pub fn group_children(nodes: &[GroupNode]) -> (Vec<&GroupNode>, BTreeMap<&str, Vec<&GroupNode>>) {
    let mut roots = Vec::new();
    let mut children: BTreeMap<&str, Vec<&GroupNode>> = BTreeMap::new();
    for node in nodes {
        match &node.parent_id {
            None => roots.push(node),
            Some(parent) => children.entry(parent.as_str()).or_default().push(node),
        }
    }
    let order =
        |a: &&GroupNode, b: &&GroupNode| (&a.name, &a.group_id).cmp(&(&b.name, &b.group_id));
    roots.sort_by_key(|node| !is_main(node));
    for list in children.values_mut() {
        list.sort_by(order);
    }
    (roots, children)
}

fn is_main(node: &GroupNode) -> bool {
    node.parent_id.is_none() && node.kind == GroupKind::User && node.name.eq_ignore_ascii_case("Main")
}

fn preorder<'a>(
    roots: &[&'a GroupNode],
    children: &BTreeMap<&str, Vec<&'a GroupNode>>,
) -> Vec<&'a GroupNode> {
    let mut out = Vec::new();
    let mut stack: Vec<&GroupNode> = roots.iter().rev().copied().collect();
    while let Some(node) = stack.pop() {
        out.push(node);
        if let Some(list) = children.get(node.group_id.as_str()) {
            stack.extend(list.iter().rev().copied());
        }
    }
    out
}

/// Navigation tree with distinct direct and subtree counts of authorized
/// items. Counts ignore any text query; hidden nodes and items are absent.
pub fn read_group_tree(
    library: impl AsRef<Path>,
    max_sensitivity: Sensitivity,
    offset: usize,
    limit: usize,
    expected_view: Option<&str>,
) -> Result<GroupTreePage> {
    if limit == 0 || limit > MAX_GROUP_TREE_PAGE || (offset > 0 && expected_view.is_none()) {
        anyhow::bail!("invalid_group_window")
    }
    with_consistent_snapshot(library, |root| {
        let groups = read_visible_groups(root, max_sensitivity)?;
        let trashed = crate::trash::trashed_ids(root)?;
        let mut visible_items = BTreeMap::new();
        for membership in &groups.memberships {
            if !visible_items.contains_key(membership.item_id.as_str()) {
                let visible = crate::item_head_visible(
                    root,
                    &membership.item_id,
                    max_sensitivity,
                    &trashed,
                )?;
                visible_items.insert(membership.item_id.as_str(), visible);
            }
        }
        let visible_items = visible_items
            .into_iter()
            .filter_map(|(id, visible)| visible.then_some(id))
            .collect();
        GroupTreeSnapshot::from_verified_items(&groups, &visible_items, max_sensitivity)?
            .page(offset, limit, expected_view)
    })
}

/// Derived navigation at a verified Store generation and sensitivity ceiling.
/// Reuse the library's verified item set instead of reopening every item header
/// for every page. Callers invalidate this together with their query index.
pub struct GroupTreeSnapshot {
    view: String,
    nodes: Vec<GroupNodeView>,
}

impl GroupTreeSnapshot {
    /// `groups` and `visible_items` must come from the same consistent Store
    /// snapshot and ceiling; this pure projection does not authorize item IDs.
    pub fn from_verified_items(
        groups: &VisibleGroups,
        visible_items: &BTreeSet<&str>,
        max_sensitivity: Sensitivity,
    ) -> Result<Self> {
        let mut direct: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for membership in &groups.memberships {
            if visible_items.contains(membership.item_id.as_str()) {
                direct
                    .entry(membership.group_id.as_str())
                    .or_default()
                    .insert(membership.item_id.as_str());
            }
        }
        let (roots, children) = group_children(&groups.nodes);
        let ordered = preorder(&roots, &children);
        // Post-order union: each node's subtree set is its members plus its
        // children's subtree sets; an item in several folders counts once.
        let mut subtree: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for node in ordered.iter().rev() {
            let mut union = direct
                .get(node.group_id.as_str())
                .cloned()
                .unwrap_or_default();
            for child in children.get(node.group_id.as_str()).into_iter().flatten() {
                union.extend(subtree[child.group_id.as_str()].iter().copied());
            }
            subtree.insert(node.group_id.as_str(), union);
        }
        let views: Vec<GroupNodeView> = ordered
            .iter()
            .map(|node| GroupNodeView {
                id: node.group_id.clone(),
                name: node.name.clone(),
                kind: node.kind,
                parent_id: node.parent_id.clone(),
                root_group_id: node.root_group_id.clone(),
                item_count: direct
                    .get(node.group_id.as_str())
                    .map_or(0, |set| set.len() as u64),
                subtree_item_count: subtree[node.group_id.as_str()].len() as u64,
                source: node.source.clone(),
                owner_id: node.owner_id.clone(),
                sensitivity: groups.effective_sensitivity[&node.group_id],
                read_only: node.read_only,
                version: version_token(node.version),
            })
            .collect();
        let view = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&(&views, max_sensitivity))?)
        );
        Ok(Self { view, nodes: views })
    }

    pub fn page(
        &self,
        offset: usize,
        limit: usize,
        expected_view: Option<&str>,
    ) -> Result<GroupTreePage> {
        if limit == 0 || limit > MAX_GROUP_TREE_PAGE || (offset > 0 && expected_view.is_none()) {
            anyhow::bail!("invalid_group_window")
        }
        if expected_view.is_some_and(|expected| expected != self.view) {
            anyhow::bail!("stale_group_view")
        }
        if offset > self.nodes.len() {
            anyhow::bail!("invalid_group_window")
        }
        let page: Vec<_> = self.nodes.iter().skip(offset).take(limit).cloned().collect();
        let next = offset + page.len();
        Ok(GroupTreePage {
            schema_version: 1,
            view: self.view.clone(),
            total: self.nodes.len(),
            offset,
            next_offset: (next < self.nodes.len()).then_some(next),
            nodes: page,
        })
    }
}

/// Store-level write intents. Not exposed over IPC in this increment: the
/// spec has not frozen wire commands. No move/reparent operation exists.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupEditOp {
    CreateRoot {
        name: String,
        sensitivity: Sensitivity,
    },
    CreateFolder {
        parent_id: String,
        expected_parent_version: String,
        name: String,
    },
    Rename {
        group_id: String,
        expected_version: String,
        name: String,
    },
    ReorderRoot {
        group_id: String,
        expected_version: String,
        /// Move before this root, or to the end when absent. Never reparents folders.
        before_id: Option<String>,
    },
    AddMembers {
        group_id: String,
        expected_version: String,
        item_ids: Vec<String>,
    },
    RemoveMembers {
        group_id: String,
        expected_version: String,
        item_ids: Vec<String>,
    },
    TransferFolders {
        source_ids: Vec<String>,
        target_parent_id: String,
        expected_parent_version: String,
        expected_versions: BTreeMap<String, String>,
        moving: bool,
    },
}

fn normalized_op(op: GroupEditOp) -> Result<GroupEditOp> {
    let invalid = || anyhow::anyhow!("invalid_group_edit");
    let name_ok = |name: &str| valid_text(name, MAX_GROUP_NAME_BYTES);
    let items = |mut ids: Vec<String>| -> Result<Vec<String>> {
        ids.sort();
        ids.dedup();
        if ids.is_empty()
            || ids.len() > MAX_GROUP_EDIT_ITEMS
            || !ids.iter().all(|id| valid_item_id(id))
        {
            return Err(invalid());
        }
        Ok(ids)
    };
    Ok(match op {
        GroupEditOp::CreateRoot { name, .. }
        | GroupEditOp::CreateFolder { name, .. }
        | GroupEditOp::Rename { name, .. }
            if !name_ok(&name) =>
        {
            return Err(invalid())
        }
        GroupEditOp::AddMembers {
            group_id,
            expected_version,
            item_ids,
        } => GroupEditOp::AddMembers {
            group_id,
            expected_version,
            item_ids: items(item_ids)?,
        },
        GroupEditOp::RemoveMembers {
            group_id,
            expected_version,
            item_ids,
        } => GroupEditOp::RemoveMembers {
            group_id,
            expected_version,
            item_ids: items(item_ids)?,
        },
        GroupEditOp::TransferFolders { mut source_ids, target_parent_id, expected_parent_version, expected_versions, moving } => {
            source_ids.sort();
            source_ids.dedup();
            if source_ids.is_empty() || source_ids.len() > MAX_GROUP_EDIT_ITEMS
                || !source_ids.iter().all(|id| valid_group_id(id)) || !valid_group_id(&target_parent_id)
                || expected_versions.len() > MAX_GROUP_NODES {
                return Err(invalid());
            }
            GroupEditOp::TransferFolders { source_ids, target_parent_id, expected_parent_version, expected_versions, moving }
        }
        other => other,
    })
}

/// Applies one versioned, replay-safe group write. Authorization (node and
/// item visibility at the ceiling) precedes replay and version disclosure.
pub fn edit_groups(
    library: impl AsRef<Path>,
    request_id: &str,
    op: GroupEditOp,
    max_sensitivity: Sensitivity,
) -> Result<GroupEditReceipt> {
    if !valid_text(request_id, 256) {
        anyhow::bail!("invalid_group_edit")
    }
    let op = normalized_op(op)?;
    let request_sha256 = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&op)?));
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = load_state(&root)?;
    let effective = effective_sensitivities(&state);
    let visible = |id: &str| {
        effective
            .get(id)
            .is_some_and(|level| *level <= max_sensitivity)
    };
    let not_found = || anyhow::anyhow!("library_resource_not_found");
    let target = match &op {
        GroupEditOp::CreateRoot { sensitivity, .. } => {
            if *sensitivity > max_sensitivity {
                anyhow::bail!("invalid_group_edit")
            }
            None
        }
        GroupEditOp::CreateFolder { parent_id, .. } => Some(parent_id.clone()),
        GroupEditOp::TransferFolders { target_parent_id, .. } => Some(target_parent_id.clone()),
        GroupEditOp::Rename { group_id, .. }
        | GroupEditOp::ReorderRoot { group_id, .. }
        | GroupEditOp::AddMembers { group_id, .. }
        | GroupEditOp::RemoveMembers { group_id, .. } => Some(group_id.clone()),
    };
    if target.as_deref().is_some_and(|id| !visible(id)) {
        return Err(not_found());
    }
    if let GroupEditOp::ReorderRoot { before_id: Some(id), .. } = &op {
        if !visible(id) {
            return Err(not_found());
        }
    }
    if let GroupEditOp::TransferFolders { source_ids, target_parent_id, .. } = &op {
        // Recheck current authority before replay disclosure, but only check
        // the frozen versions for a new request, not a historical receipt.
        transfer::scope(&state, source_ids, target_parent_id, max_sensitivity)?;
    }
    if let Some(record) = state.requests.get(request_id) {
        if record.request_sha256 == request_sha256 && visible(&record.receipt.group_id) {
            lock.unlock()?;
            return Ok(record.receipt.clone());
        }
        anyhow::bail!("request_id_conflict")
    }
    let inactive = group_delete::inactive_nodes(&state);
    if target.as_ref().is_some_and(|id| inactive.contains(id))
        || matches!(&op, GroupEditOp::ReorderRoot { before_id: Some(id), .. } if inactive.contains(id))
    {
        return Err(not_found());
    }
    if let GroupEditOp::AddMembers { item_ids, .. } | GroupEditOp::RemoveMembers { item_ids, .. } =
        &op
    {
        // Membership cannot reveal or attach items above the ceiling.
        for item_id in item_ids {
            if read_item_metadata_at_sensitivity(&root, item_id, max_sensitivity)?.is_none() {
                return Err(not_found());
            }
        }
    }
    let index_of =
        |state: &GroupState, id: &str| state.nodes.iter().position(|node| node.group_id == id);
    let check_writable = |node: &GroupNode, expected: &str| -> Result<()> {
        if node.read_only {
            anyhow::bail!("group_read_only")
        }
        if version_token(node.version) != expected {
            anyhow::bail!("stale_group_version")
        }
        Ok(())
    };
    let receipt_group = match op {
        GroupEditOp::TransferFolders { source_ids, target_parent_id, expected_parent_version, expected_versions, moving } => {
            transfer::apply(&root, &mut state, &source_ids, &target_parent_id, &expected_parent_version, &expected_versions, moving, max_sensitivity)?;
            target_parent_id
        }
        GroupEditOp::ReorderRoot { group_id, expected_version, before_id } => {
            let index = index_of(&state, &group_id).ok_or_else(not_found)?;
            let node = &state.nodes[index];
            if node.parent_id.is_some() || is_main(node) || before_id.as_deref() == Some(&group_id) {
                anyhow::bail!("invalid_group_edit")
            }
            if node.version_token() != expected_version {
                anyhow::bail!("stale_group_version")
            }
            if let Some(id) = &before_id {
                let before = &state.nodes[index_of(&state, id).ok_or_else(not_found)?];
                if before.parent_id.is_some() || is_main(before) {
                    anyhow::bail!("invalid_group_edit")
                }
            }
            // Sorting is a local library preference, including for read-only imported roots.
            // The unchanged v1 state format remains readable by older readers.
            let mut node = state.nodes.remove(index);
            node.version += 1;
            let at = before_id.as_deref().and_then(|id| index_of(&state, id)).unwrap_or(state.nodes.len());
            state.nodes.insert(at, node);
            group_id
        }
        GroupEditOp::CreateRoot { name, sensitivity } => {
            let group_id = new_id("group");
            state.nodes.push(GroupNode {
                group_id: group_id.clone(),
                name,
                kind: GroupKind::User,
                parent_id: None,
                root_group_id: group_id.clone(),
                source: None,
                owner_id: LOCAL_OWNER_ID.into(),
                sensitivity,
                read_only: false,
                version: 1,
            });
            group_id
        }
        GroupEditOp::CreateFolder {
            parent_id,
            expected_parent_version,
            name,
        } => {
            let parent_index = index_of(&state, &parent_id).ok_or_else(not_found)?;
            check_writable(&state.nodes[parent_index], &expected_parent_version)?;
            let parent = &mut state.nodes[parent_index];
            parent.version += 1;
            let group_id = new_id("group");
            let node = GroupNode {
                group_id: group_id.clone(),
                name,
                kind: GroupKind::Collection,
                parent_id: Some(parent_id.clone()),
                root_group_id: parent.root_group_id.clone(),
                source: None,
                owner_id: parent.owner_id.clone(),
                sensitivity: effective[&parent_id],
                read_only: false,
                version: 1,
            };
            state.nodes.push(node);
            group_id
        }
        GroupEditOp::Rename {
            group_id,
            expected_version,
            name,
        } => {
            let index = index_of(&state, &group_id).ok_or_else(not_found)?;
            check_writable(&state.nodes[index], &expected_version)?;
            if state.nodes[index].name == name {
                anyhow::bail!("groups_unchanged")
            }
            state.nodes[index].name = name;
            state.nodes[index].version += 1;
            group_id
        }
        GroupEditOp::AddMembers {
            group_id,
            expected_version,
            item_ids,
        } => {
            let index = index_of(&state, &group_id).ok_or_else(not_found)?;
            check_writable(&state.nodes[index], &expected_version)?;
            let before = state.memberships.len();
            let mut set: BTreeSet<GroupMembership> = state.memberships.drain(..).collect();
            for item_id in item_ids {
                set.insert(GroupMembership {
                    group_id: group_id.clone(),
                    item_id,
                });
            }
            state.memberships = set.into_iter().collect();
            if state.memberships.len() == before {
                anyhow::bail!("groups_unchanged")
            }
            state.nodes[index].version += 1;
            group_id
        }
        GroupEditOp::RemoveMembers {
            group_id,
            expected_version,
            item_ids,
        } => {
            let index = index_of(&state, &group_id).ok_or_else(not_found)?;
            check_writable(&state.nodes[index], &expected_version)?;
            group_trash::ensure_members_removable(&root, &state.nodes[index].root_group_id, &item_ids)?;
            let before = state.memberships.len();
            let remove: BTreeSet<&String> = item_ids.iter().collect();
            state
                .memberships
                .retain(|m| m.group_id != group_id || !remove.contains(&m.item_id));
            if state.memberships.len() == before {
                anyhow::bail!("groups_unchanged")
            }
            state.nodes[index].version += 1;
            group_id
        }
    };
    if state.nodes.len() > MAX_GROUP_NODES || state.memberships.len() > MAX_GROUP_MEMBERSHIPS {
        anyhow::bail!("group_limit_exceeded")
    }
    if state.requests.len() >= MAX_GROUP_REQUESTS {
        anyhow::bail!("group_request_limit_exceeded")
    }
    let node = &state.nodes[index_of(&state, &receipt_group).context("group receipt")?];
    let receipt = GroupEditReceipt {
        schema_version: SCHEMA_VERSION,
        group_id: receipt_group.clone(),
        version: version_token(node.version),
    };
    state.groups_version += 1;
    state.requests.insert(
        request_id.to_owned(),
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
pub(crate) mod tests {
    use super::*;
    use crate::metadata_v2::tests::*;
    use crate::tests::*;
    use memex_core::Sensitivity;
    use tempfile::tempdir;

    #[test]
    fn root_order_persists_with_main_pinned_and_replay_safe() {
        let d = tempdir().unwrap();
        ingest(d.path(), &capture(), draft()).unwrap();
        let ceiling = Sensitivity::Restricted;
        let create = |name: &str| edit_groups(d.path(), name, GroupEditOp::CreateRoot {
            name: name.into(), sensitivity: Sensitivity::Public,
        }, ceiling).unwrap();
        let z = create("Zulu");
        let a = create("Alpha");
        let main = create("Main");
        let names = || read_group_tree(d.path(), ceiling, 0, 500, None).unwrap()
            .nodes.into_iter().map(|n| n.name).collect::<Vec<_>>();
        assert_eq!(names(), ["Main", "Zulu", "Alpha"]);
        let op = GroupEditOp::ReorderRoot {
            group_id: a.group_id.clone(), expected_version: a.version,
            before_id: Some(z.group_id.clone()),
        };
        let receipt = edit_groups(d.path(), "move-alpha", op.clone(), ceiling).unwrap();
        assert_eq!(names(), ["Main", "Alpha", "Zulu"]);
        assert_eq!(edit_groups(d.path(), "move-alpha", op, ceiling).unwrap(), receipt);
        for (id, before, version) in [
            (main.group_id.clone(), None, main.version),
            (z.group_id.clone(), Some(main.group_id), z.version),
        ] {
            assert!(edit_groups(d.path(), "invalid-move", GroupEditOp::ReorderRoot {
                group_id: id, expected_version: version, before_id: before,
            }, ceiling).is_err());
        }
        assert_eq!(names(), ["Main", "Alpha", "Zulu"]);
    }

    #[test]
    fn group_edits_are_versioned_replay_safe_and_authorized_before_disclosure() {
        let d = tempdir().unwrap();
        let public = ingest(d.path(), &capture(), draft()).unwrap().item_ids[0].clone();
        let mut c = capture();
        c.capture.external_capture_id = "group-private".into();
        c.capture.sensitivity = Sensitivity::Private;
        let mut value = draft();
        value.sensitivity = Sensitivity::Private;
        let private = ingest(d.path(), &c, value).unwrap().item_ids[0].clone();
        let all = Sensitivity::Restricted;
        let root_op = GroupEditOp::CreateRoot {
            name: "Root".into(),
            sensitivity: Sensitivity::Public,
        };
        let root = edit_groups(d.path(), "r1", root_op.clone(), all).unwrap();
        assert_eq!(
            edit_groups(d.path(), "r1", root_op, all).unwrap(),
            root,
            "replay"
        );
        let before = tree(d.path());
        assert_eq!(
            edit_groups(
                d.path(),
                "r1",
                GroupEditOp::CreateRoot {
                    name: "Other".into(),
                    sensitivity: Sensitivity::Public
                },
                all
            )
            .unwrap_err()
            .to_string(),
            "request_id_conflict"
        );
        assert_eq!(tree(d.path()), before);
        let folder = edit_groups(
            d.path(),
            "r2",
            GroupEditOp::CreateFolder {
                parent_id: root.group_id.clone(),
                expected_parent_version: root.version.clone(),
                name: "Folder".into(),
            },
            all,
        )
        .unwrap();
        // The parent's version advanced; the old token is now stale, zero writes.
        let before = tree(d.path());
        assert_eq!(
            edit_groups(
                d.path(),
                "r3",
                GroupEditOp::Rename {
                    group_id: root.group_id.clone(),
                    expected_version: root.version.clone(),
                    name: "X".into()
                },
                all
            )
            .unwrap_err()
            .to_string(),
            "stale_group_version"
        );
        assert_eq!(tree(d.path()), before);
        // A Public session cannot attach (or learn of) a Private item.
        assert_eq!(
            edit_groups(
                d.path(),
                "r4",
                GroupEditOp::AddMembers {
                    group_id: folder.group_id.clone(),
                    expected_version: folder.version.clone(),
                    item_ids: vec![private.clone()]
                },
                Sensitivity::Public
            )
            .unwrap_err()
            .to_string(),
            "library_resource_not_found"
        );
        assert_eq!(tree(d.path()), before);
        let added = edit_groups(
            d.path(),
            "r5",
            GroupEditOp::AddMembers {
                group_id: folder.group_id.clone(),
                expected_version: folder.version.clone(),
                item_ids: vec![public.clone(), public.clone()],
            },
            all,
        )
        .unwrap();
        assert_eq!(
            edit_groups(
                d.path(),
                "r6",
                GroupEditOp::AddMembers {
                    group_id: folder.group_id.clone(),
                    expected_version: added.version.clone(),
                    item_ids: vec![public.clone()]
                },
                all
            )
            .unwrap_err()
            .to_string(),
            "groups_unchanged"
        );
        // Private root and its folders are invisible below Private, even to replay.
        let hidden = edit_groups(
            d.path(),
            "r7",
            GroupEditOp::CreateRoot {
                name: "Hidden".into(),
                sensitivity: Sensitivity::Private,
            },
            all,
        )
        .unwrap();
        assert_eq!(
            edit_groups(
                d.path(),
                "r8",
                GroupEditOp::Rename {
                    group_id: hidden.group_id.clone(),
                    expected_version: "v999".into(),
                    name: "Y".into()
                },
                Sensitivity::Public
            )
            .unwrap_err()
            .to_string(),
            "library_resource_not_found"
        );
        assert_eq!(
            edit_groups(
                d.path(),
                "r9",
                GroupEditOp::CreateRoot {
                    name: "Too high".into(),
                    sensitivity: Sensitivity::Private
                },
                Sensitivity::Public
            )
            .unwrap_err()
            .to_string(),
            "invalid_group_edit"
        );
        let page = read_group_tree(d.path(), Sensitivity::Public, 0, 500, None).unwrap();
        assert_eq!(
            page.nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            ["Root", "Folder"]
        );
        assert_eq!(page.nodes[1].root_group_id, root.group_id);
        assert_eq!(
            page.nodes[1].parent_id.as_deref(),
            Some(root.group_id.as_str())
        );
        // Removing membership never touches the item.
        let item_before = read_item(d.path(), &public).unwrap();
        edit_groups(
            d.path(),
            "r10",
            GroupEditOp::RemoveMembers {
                group_id: folder.group_id.clone(),
                expected_version: added.version,
                item_ids: vec![public.clone()],
            },
            all,
        )
        .unwrap();
        assert_eq!(read_item(d.path(), &public).unwrap(), item_before);
        // Tree paging binds a view.
        let first = read_group_tree(d.path(), Sensitivity::Public, 0, 1, None).unwrap();
        assert_eq!((first.total, first.next_offset), (2, Some(1)));
        read_group_tree(d.path(), Sensitivity::Public, 1, 1, Some(&first.view)).unwrap();
        assert_eq!(
            read_group_tree(d.path(), Sensitivity::Public, 1, 1, None)
                .unwrap_err()
                .to_string(),
            "invalid_group_window"
        );
        edit_groups(
            d.path(),
            "r11",
            GroupEditOp::Rename {
                group_id: folder.group_id.clone(),
                expected_version: read_group_tree(d.path(), all, 0, 500, None)
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|n| n.id == folder.group_id)
                    .unwrap()
                    .version
                    .clone(),
                name: "Renamed".into(),
            },
            all,
        )
        .unwrap();
        assert_eq!(
            read_group_tree(d.path(), Sensitivity::Public, 1, 1, Some(&first.view))
                .unwrap_err()
                .to_string(),
            "stale_group_view"
        );
    }

    #[test]
    fn group_state_rejects_cycles_orphans_and_read_only_writes() {
        let d = tempdir().unwrap();
        ingest(d.path(), &capture(), draft()).unwrap();
        let all = Sensitivity::Restricted;
        let root = edit_groups(
            d.path(),
            "c1",
            GroupEditOp::CreateRoot {
                name: "Root".into(),
                sensitivity: Sensitivity::Public,
            },
            all,
        )
        .unwrap();
        let a = edit_groups(
            d.path(),
            "c2",
            GroupEditOp::CreateFolder {
                parent_id: root.group_id.clone(),
                expected_parent_version: root.version.clone(),
                name: "A".into(),
            },
            all,
        )
        .unwrap();
        let path = d.path().join("catalog/groups/state.json");
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mutate = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut value = original.clone();
            f(&mut value);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            read_group_tree(d.path(), all, 0, 500, None)
                .map(|_| ())
                .unwrap_err()
                .to_string()
        };
        let index = |value: &serde_json::Value, id: &str| {
            value["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .position(|n| n["group_id"] == id)
                .unwrap()
        };
        // Root made a child of its own folder: a cycle, rejected.
        assert_eq!(
            mutate(&|v| {
                let i = index(v, &root.group_id);
                v["nodes"][i]["parent_id"] = a.group_id.clone().into();
                v["nodes"][i]["kind"] = "collection".into();
            }),
            "invalid group state"
        );
        assert_eq!(
            mutate(&|v| {
                let i = index(v, &a.group_id);
                v["nodes"][i]["parent_id"] = "group_missing".into();
            }),
            "invalid group state"
        );
        assert_eq!(
            mutate(&|v| {
                let i = index(v, &a.group_id);
                v["nodes"][i]["root_group_id"] = a.group_id.clone().into();
            }),
            "invalid group state"
        );
        assert_eq!(
            mutate(&|v| {
                v["memberships"] =
                    serde_json::json!([{"group_id": "group_missing", "item_id": "item_x"}]);
            }),
            "invalid group state"
        );
        // Server-side read_only blocks every write to that node.
        let mut value = original.clone();
        let i = index(&value, &a.group_id);
        value["nodes"][i]["read_only"] = true.into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let page = read_group_tree(d.path(), all, 0, 500, None).unwrap();
        let node = page.nodes.iter().find(|n| n.id == a.group_id).unwrap();
        assert!(node.read_only);
        assert_eq!(
            edit_groups(
                d.path(),
                "c3",
                GroupEditOp::Rename {
                    group_id: a.group_id.clone(),
                    expected_version: node.version.clone(),
                    name: "B".into()
                },
                all
            )
            .unwrap_err()
            .to_string(),
            "group_read_only"
        );
    }
}
