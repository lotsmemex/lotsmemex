//! Owner-local recoverable organization deletion. All canonical changes and
//! replay receipts share groups/state.json; item trash is only read, never reset.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deletion {
    target_group_id: String,
    affected_node_ids: BTreeSet<String>,
    deleted_at: String,
    restored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    request_sha256: String,
    receipt: GroupDeleteReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupDeleteReceipt {
    pub group_id: String,
    pub deletion_id: String,
    /// Historical outcome on replay, not the current activity of the subtree.
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GroupDeleteImpact {
    pub group_id: String,
    pub name: String,
    pub impact_sha256: String,
    pub affected_node_count: usize,
    pub affected_membership_count: usize,
    pub distinct_item_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GroupDeletionEntry {
    pub deletion_id: String,
    pub group_id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub deleted_at: String,
}

pub type Impact = GroupDeleteImpact;
pub type Receipt = GroupDeleteReceipt;
pub type Entry = GroupDeletionEntry;

fn digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value)?)
    ))
}

fn owner(principal: &str) -> Result<()> {
    anyhow::ensure!(principal == LOCAL_OWNER_ID, "permission_denied");
    Ok(())
}

pub(super) fn inactive_nodes(state: &GroupState) -> BTreeSet<String> {
    state
        .deletions
        .values()
        .filter(|d| !d.restored)
        .flat_map(|d| d.affected_node_ids.iter().cloned())
        .collect()
}

fn subtree(state: &GroupState, id: &str) -> BTreeSet<String> {
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for n in &state.nodes {
        if let Some(parent) = &n.parent_id {
            children.entry(parent).or_default().push(&n.group_id);
        }
    }
    let mut found = BTreeSet::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if found.insert(id.to_owned()) {
            stack.extend(children.get(id).into_iter().flatten().copied());
        }
    }
    found
}

pub(super) fn validate(state: &GroupState) -> Result<()> {
    if state.state_version == 1 {
        anyhow::ensure!(
            state.deletions.is_empty() && state.deletion_requests.is_empty(),
            "invalid group state"
        );
        return Ok(());
    }
    let outstanding = state.deletions.values().filter(|d| !d.restored).count();
    anyhow::ensure!(
        !state.deletions.is_empty()
            && state.deletion_requests.len() + outstanding <= MAX_GROUP_REQUESTS,
        "invalid group state"
    );
    let nodes: BTreeMap<_, _> = state.nodes.iter().map(|n| (&n.group_id, n)).collect();
    let inactive = inactive_nodes(state);
    let mut assigned = BTreeSet::new();
    let mut receipt_counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in state.deletion_requests.values() {
        let counts = receipt_counts.entry(&r.receipt.deletion_id).or_default();
        match r.receipt.outcome.as_str() {
            "deleted" => counts.0 += 1,
            "restored" => counts.1 += 1,
            _ => return Err(invalid_state()),
        }
    }
    let mut closures = BTreeMap::new();
    for (id, d) in &state.deletions {
        anyhow::ensure!(
            safe_component(id, "deletion_")
                && id.len() <= 128
                && nodes.contains_key(&d.target_group_id)
                && d.affected_node_ids.contains(&d.target_group_id)
                && chrono::DateTime::parse_from_rfc3339(&d.deleted_at).is_ok(),
            "invalid group state"
        );
        let closure = closures
            .entry(&d.target_group_id)
            .or_insert_with(|| subtree(state, &d.target_group_id));
        for node in &d.affected_node_ids {
            anyhow::ensure!(
                nodes.contains_key(node) && (d.restored || closure.contains(node)),
                "invalid group state"
            );
            if !d.restored {
                anyhow::ensure!(assigned.insert(node), "invalid group state");
            }
        }
        if !d.restored {
            anyhow::ensure!(closure.is_subset(&inactive), "invalid group state");
        }
        anyhow::ensure!(
            receipt_counts.get(id.as_str()) == Some(&(1, usize::from(d.restored))),
            "invalid group state"
        );
    }
    for (key, r) in &state.deletion_requests {
        let d = state
            .deletions
            .get(&r.receipt.deletion_id)
            .ok_or_else(invalid_state)?;
        anyhow::ensure!(
            valid_sha256(key)
                && valid_sha256(&r.request_sha256)
                && r.receipt.group_id == d.target_group_id
                && matches!(r.receipt.outcome.as_str(), "deleted" | "restored"),
            "invalid group state"
        );
    }
    Ok(())
}

/// This authorizes physical relationships, including globally trashed items.
/// Query-visible metadata helpers intentionally cannot serve this purpose.
fn authorize(
    root: &Path,
    state: &GroupState,
    id: &str,
    ceiling: Sensitivity,
) -> Result<BTreeSet<String>> {
    let nodes: BTreeMap<_, _> = state
        .nodes
        .iter()
        .map(|n| (n.group_id.as_str(), n))
        .collect();
    anyhow::ensure!(nodes.contains_key(id), "library_resource_not_found");
    let ids = subtree(state, id);
    for id in &ids {
        let mut node = nodes[id.as_str()];
        loop {
            anyhow::ensure!(
                node.sensitivity <= ceiling && !node.read_only,
                "library_resource_not_found"
            );
            match &node.parent_id {
                Some(parent) => node = nodes[parent.as_str()],
                None => break,
            }
        }
    }
    let items: BTreeSet<_> = state
        .memberships
        .iter()
        .filter(|m| ids.contains(&m.group_id))
        .map(|m| &m.item_id)
        .collect();
    for id in items {
        let (item, _) = load_verified_item_header(root, id)
            .map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
        anyhow::ensure!(item.sensitivity <= ceiling, "library_resource_not_found");
    }
    Ok(ids)
}

fn impact(
    root: &Path,
    state: &GroupState,
    id: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<GroupDeleteImpact> {
    owner(principal)?;
    let ids = authorize(root, state, id, ceiling)?;
    let inactive = inactive_nodes(state);
    anyhow::ensure!(!inactive.contains(id), "group_node_inactive");
    let target = state
        .nodes
        .iter()
        .find(|n| n.group_id == id)
        .ok_or_else(invalid_state)?;
    let mut authority = Vec::new();
    let mut parent = target.parent_id.as_ref();
    while let Some(id) = parent {
        let node = state
            .nodes
            .iter()
            .find(|n| n.group_id == *id)
            .ok_or_else(invalid_state)?;
        authority.push((
            &node.group_id,
            &node.parent_id,
            &node.root_group_id,
            node.sensitivity,
            node.read_only,
        ));
        parent = node.parent_id.as_ref();
    }
    // Preserve subtree-relative slots; unrelated/hidden roots moving before it
    // must not invalidate confirmation. Reordering the target bumps its version.
    let nodes: Vec<_> = state
        .nodes
        .iter()
        .filter(|n| ids.contains(&n.group_id))
        .collect();
    let memberships: Vec<_> = state
        .memberships
        .iter()
        .filter(|m| ids.contains(&m.group_id))
        .collect();
    let mut item_state = Vec::new();
    for id in memberships
        .iter()
        .map(|m| &m.item_id)
        .collect::<BTreeSet<_>>()
    {
        let header = load_verified_item_header(root, id)?;
        let marker = crate::trash::read_trash_record(root, id)?;
        item_state.push((id, header, marker));
    }
    let roots: BTreeMap<_, _> = state
        .nodes
        .iter()
        .map(|n| (&n.group_id, &n.root_group_id))
        .collect();
    let pairs = memberships
        .iter()
        .map(|m| (roots[&m.group_id].clone(), m.item_id.clone()))
        .collect();
    let trash = group_trash::deletion_dependencies(root, &pairs)?;
    let deletions: Vec<_> = state
        .deletions
        .iter()
        .filter(|(_, d)| !d.restored && ids.contains(&d.target_group_id))
        .collect();
    let active_members: Vec<_> = memberships
        .iter()
        .filter(|m| !inactive.contains(&m.group_id))
        .collect();
    Ok(GroupDeleteImpact {
        group_id: id.into(),
        name: target.name.clone(),
        impact_sha256: digest(&(
            root,
            principal,
            ceiling,
            &nodes,
            &authority,
            &memberships,
            &item_state,
            &trash,
            &deletions,
        ))?,
        affected_node_count: ids.difference(&inactive).count(),
        affected_membership_count: active_members.len(),
        distinct_item_count: active_members
            .iter()
            .map(|m| &m.item_id)
            .collect::<BTreeSet<_>>()
            .len(),
    })
}

pub fn preview(library: impl AsRef<Path>, group_id: &str, ceiling: Sensitivity) -> Result<Impact> {
    with_consistent_snapshot(library, |root| {
        impact(root, &load_state(root)?, group_id, LOCAL_OWNER_ID, ceiling)
    })
}

fn request_key(principal: &str, request_id: &str) -> Result<String> {
    owner(principal)?;
    anyhow::ensure!(valid_text(request_id, 256), "invalid_group_delete_request");
    digest(&(principal, request_id))
}

fn committed_replay(
    state: &GroupState,
    key: &str,
    hash: &str,
) -> Result<Option<GroupDeleteReceipt>> {
    match state.deletion_requests.get(key) {
        Some(previous) => {
            anyhow::ensure!(previous.request_sha256 == hash, "request_id_conflict");
            Ok(Some(previous.receipt.clone()))
        }
        None => Ok(None),
    }
}

fn advance_versions(
    state: &mut GroupState,
    affected: &BTreeSet<String>,
    target: &str,
) -> Result<()> {
    let parent = state
        .nodes
        .iter()
        .find(|n| n.group_id == target)
        .and_then(|n| n.parent_id.clone());
    for n in &mut state.nodes {
        if affected.contains(&n.group_id) || parent.as_ref() == Some(&n.group_id) {
            n.version = n.version.checked_add(1).ok_or_else(invalid_state)?;
        }
    }
    state.groups_version = state
        .groups_version
        .checked_add(1)
        .ok_or_else(invalid_state)?;
    Ok(())
}

/// Agent derives request_sha256 from command/normalized payload/token digest.
/// No raw bearer confirmation token crosses the Store interface.
pub fn delete(
    library: impl AsRef<Path>,
    principal: &str,
    request_id: &str,
    request_sha256: &str,
    group_id: &str,
    expected_impact: &str,
    ceiling: Sensitivity,
    expires: std::time::Instant,
) -> Result<GroupDeleteReceipt> {
    let key = request_key(principal, request_id)?;
    anyhow::ensure!(valid_sha256(request_sha256), "invalid_group_delete_request");
    let hash = request_sha256.to_owned();
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = load_state(&root)?;
    let ids = authorize(&root, &state, group_id, ceiling)?;
    if let Some(receipt) = committed_replay(&state, &key, &hash)? {
        return Ok(receipt);
    }
    anyhow::ensure!(
        std::time::Instant::now() < expires,
        "group_delete_preview_expired"
    );
    let current = impact(&root, &state, group_id, principal, ceiling)?;
    anyhow::ensure!(
        current.impact_sha256 == expected_impact,
        "stale_group_delete_preview"
    );
    let outstanding = state.deletions.values().filter(|d| !d.restored).count();
    anyhow::ensure!(
        state.deletion_requests.len() + outstanding + 2 <= MAX_GROUP_REQUESTS,
        "group_request_limit_exceeded"
    );
    let affected: BTreeSet<_> = ids.difference(&inactive_nodes(&state)).cloned().collect();
    let deletion_id = new_id("deletion");
    let receipt = GroupDeleteReceipt {
        group_id: group_id.into(),
        deletion_id: deletion_id.clone(),
        outcome: "deleted".into(),
    };
    advance_versions(&mut state, &affected, group_id)?;
    state.deletions.insert(
        deletion_id,
        Deletion {
            target_group_id: group_id.into(),
            affected_node_ids: affected,
            deleted_at: chrono::Utc::now().to_rfc3339(),
            restored: false,
        },
    );
    state.deletion_requests.insert(
        key,
        Request {
            request_sha256: hash,
            receipt: receipt.clone(),
        },
    );
    state.state_version = 2;
    write_state_before(&root, &state, Some(expires))?;
    lock.unlock()?;
    Ok(receipt)
}

pub fn restore(
    library: impl AsRef<Path>,
    principal: &str,
    request_id: &str,
    request_sha256: &str,
    deletion_id: &str,
    ceiling: Sensitivity,
) -> Result<GroupDeleteReceipt> {
    let key = request_key(principal, request_id)?;
    anyhow::ensure!(valid_sha256(request_sha256), "invalid_group_delete_request");
    let hash = request_sha256.to_owned();
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = load_state(&root)?;
    let deletion = state
        .deletions
        .get(deletion_id)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?
        .clone();
    authorize(&root, &state, &deletion.target_group_id, ceiling)?;
    if let Some(receipt) = committed_replay(&state, &key, &hash)? {
        return Ok(receipt);
    }
    anyhow::ensure!(!deletion.restored, "group_delete_restore_conflict");
    let inactive = inactive_nodes(&state);
    let target = state
        .nodes
        .iter()
        .find(|n| n.group_id == deletion.target_group_id)
        .ok_or_else(invalid_state)?;
    anyhow::ensure!(
        !target
            .parent_id
            .as_ref()
            .is_some_and(|p| inactive.contains(p)),
        "group_delete_restore_conflict"
    );
    let receipt = GroupDeleteReceipt {
        group_id: deletion.target_group_id.clone(),
        deletion_id: deletion_id.into(),
        outcome: "restored".into(),
    };
    advance_versions(
        &mut state,
        &deletion.affected_node_ids,
        &deletion.target_group_id,
    )?;
    state
        .deletions
        .get_mut(deletion_id)
        .ok_or_else(invalid_state)?
        .restored = true;
    state.deletion_requests.insert(
        key,
        Request {
            request_sha256: hash,
            receipt: receipt.clone(),
        },
    );
    write_state(&root, &state)?;
    lock.unlock()?;
    Ok(receipt)
}

/// Lookup before the Agent's ephemeral token cache, with current complete
/// disclosure authorization even for historical restored deletion receipts.
pub fn replay(
    library: impl AsRef<Path>,
    principal: &str,
    request_id: &str,
    request_sha256: &str,
    ceiling: Sensitivity,
) -> Result<Option<Receipt>> {
    let key = request_key(principal, request_id)?;
    anyhow::ensure!(valid_sha256(request_sha256), "invalid_group_delete_request");
    with_consistent_snapshot(library, |root| {
        let state = load_state(root)?;
        let Some(record) = state.deletion_requests.get(&key) else {
            return Ok(None);
        };
        authorize(root, &state, &record.receipt.group_id, ceiling)?;
        committed_replay(&state, &key, request_sha256)
    })
}

/// Agent binds and paginates this already authorized snapshot (50 per page).
pub fn list(library: impl AsRef<Path>, ceiling: Sensitivity) -> Result<Vec<Entry>> {
    with_consistent_snapshot(library, |root| {
        let state = load_state(root)?;
        let mut entries = Vec::new();
        for (id, d) in &state.deletions {
            if d.restored {
                continue;
            }
            if let Err(error) = authorize(root, &state, &d.target_group_id, ceiling) {
                if error.to_string() == "library_resource_not_found" {
                    continue;
                }
                return Err(error);
            }
            let node = state
                .nodes
                .iter()
                .find(|n| n.group_id == d.target_group_id)
                .ok_or_else(invalid_state)?;
            entries.push(GroupDeletionEntry {
                deletion_id: id.clone(),
                group_id: node.group_id.clone(),
                name: node.name.clone(),
                parent_id: node.parent_id.clone(),
                deleted_at: d.deleted_at.clone(),
            });
        }
        Ok(entries)
    })
}

#[cfg(test)]
#[path = "group_delete_tests.rs"]
mod tests;
