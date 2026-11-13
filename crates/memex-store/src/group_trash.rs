//! Per-library recoverable deletion. Canonical items and folder memberships are
//! untouched. One atomic sidecar contains both tombstones and replay receipts;
//! it is read under the same Store snapshot/writer lock as groups.
use super::*;

const PATH: &str = "catalog/groups/trash.json";

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    records: BTreeMap<String, BTreeMap<String, TrashRecord>>,
    requests: BTreeMap<String, String>,
}

fn read(root: &Path) -> Result<State> {
    match fs::symlink_metadata(root.join(PATH)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State { version: 1, ..Default::default() }),
        Err(e) => return Err(e.into()),
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {},
        Ok(_) => anyhow::bail!("invalid group trash"),
    }
    let state: State = load(root, root.join(PATH))?;
    if state.version != 1 || state.requests.len() > MAX_GROUP_REQUESTS
        || state.records.values().map(BTreeMap::len).sum::<usize>() > MAX_GROUP_MEMBERSHIPS {
        anyhow::bail!("invalid group trash")
    }
    for (group, records) in &state.records {
        if !valid_group_id(group) { anyhow::bail!("invalid group trash") }
        for (id, r) in records {
            if !valid_item_id(id) || r.item_id != *id || !valid_revision_id(&r.revision_id)
                || r.schema_version != SCHEMA_VERSION || r.reason != "manual"
                || r.plan_id.as_ref().is_none_or(|id| !valid_text(id, 256))
                || chrono::DateTime::parse_from_rfc3339(&r.trashed_at).is_err() {
                anyhow::bail!("invalid group trash")
            }
        }
    }
    if state.requests.iter().any(|(id, hash)| !valid_text(id, 256) || !valid_sha256(hash)) {
        anyhow::bail!("invalid group trash")
    }
    Ok(state)
}

fn authorized_root<'a>(groups: &'a GroupState, id: &str, ceiling: Sensitivity) -> Result<&'a GroupNode> {
    groups.nodes.iter().find(|g| g.group_id == id && g.parent_id.is_none() && g.sensitivity <= ceiling)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))
}

/// Apply a deletion, or restore the exact deletion identified by `restore_plan`.
/// Request replay is durable, including after restoration and process restart.
pub fn edit_group_trash(
    library: impl AsRef<Path>, group_id: &str, item_id: &str, request_id: &str,
    expected_revision: &str, restore_plan: Option<&str>, ceiling: Sensitivity,
) -> Result<()> {
    if !valid_text(request_id, 256) || !valid_item_id(item_id) || !valid_revision_id(expected_revision)
        || restore_plan.is_some_and(|id| !valid_text(id, 256)) {
        anyhow::bail!("invalid_trash_request")
    }
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let groups = load_state(&root)?;
    authorized_root(&groups, group_id, ceiling)?;
    let item = read_item_metadata_at_sensitivity(&root, item_id, ceiling)?
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    let effective = effective_sensitivities(&groups);
    let allowed: BTreeSet<&str> = groups.nodes.iter()
        .filter(|g| g.root_group_id == group_id && effective[&g.group_id] <= ceiling)
        .map(|g| g.group_id.as_str()).collect();
    if !groups.memberships.iter().any(|m| m.item_id == item_id && allowed.contains(m.group_id.as_str())) {
        anyhow::bail!("library_resource_not_found")
    }
    let mut state = read(&root)?;
    let hash = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&(group_id, item_id, expected_revision, restore_plan))?));
    if let Some(previous) = state.requests.get(request_id) {
        if previous != &hash { anyhow::bail!("request_id_conflict") }
        lock.unlock()?;
        return Ok(());
    }
    // A root-wide operation must not mutate visibility in folders above the
    // caller's ceiling merely because one sibling membership is visible.
    if groups.memberships.iter().any(|m| m.item_id == item_id
        && groups.nodes.iter().any(|g| g.group_id == m.group_id && g.root_group_id == group_id
            && effective[&g.group_id] > ceiling)) {
        anyhow::bail!("library_resource_not_found")
    }
    if item.revision_id != expected_revision { anyhow::bail!("stale_item_revision") }
    if state.requests.len() >= MAX_GROUP_REQUESTS { anyhow::bail!("group_request_limit_exceeded") }
    let active = state.records.values().map(BTreeMap::len).sum::<usize>();
    // Every accepted deletion reserves its future restoration receipt too.
    if restore_plan.is_none() && state.requests.len() + active + 2 > MAX_GROUP_REQUESTS {
        anyhow::bail!("group_request_limit_exceeded")
    }
    let records = state.records.entry(group_id.to_owned()).or_default();
    if let Some(plan) = restore_plan {
        let record = records.get(item_id).ok_or_else(|| anyhow::anyhow!("item_not_trashed"))?;
        if record.plan_id.as_deref() != Some(plan) { anyhow::bail!("stale_item_revision") }
        records.remove(item_id);
    } else {
        if records.contains_key(item_id) { anyhow::bail!("item_already_trashed") }
        records.insert(item_id.to_owned(), TrashRecord {
            schema_version: SCHEMA_VERSION, item_id: item_id.into(), revision_id: item.revision_id,
            trashed_at: chrono::Utc::now().to_rfc3339(), reason: "manual".into(),
            kept_item_id: None, plan_id: Some(request_id.into()),
        });
    }
    state.requests.insert(request_id.into(), hash);
    ensure_contained_dir(&root, Path::new("catalog/groups"))?;
    durable_replace(&root, &root.join(PATH), &serde_json::to_vec(&state)?)?;
    lock.unlock()?;
    Ok(())
}

/// Retained memberships are the restoration path; callers must restore first.
pub(super) fn ensure_members_removable(root: &Path, group: &str, item_ids: &[String]) -> Result<()> {
    let state = read(root)?;
    if state.records.get(group).is_some_and(|records| item_ids.iter().any(|id| records.contains_key(id))) {
        anyhow::bail!("item_trashed")
    }
    Ok(())
}

/// Filter only this ceiling's memberships. An item disappears from the global
/// query only when every visible library containing it has trashed it.
pub(super) fn filter_memberships(root: &Path, nodes: &[GroupNode], memberships: &mut Vec<GroupMembership>) -> Result<BTreeSet<String>> {
    let state = read(root)?;
    if state.records.values().all(BTreeMap::is_empty) { return Ok(BTreeSet::new()) }
    let roots: BTreeMap<&str, &str> = nodes.iter().map(|g| (g.group_id.as_str(), g.root_group_id.as_str())).collect();
    let before: BTreeSet<String> = memberships.iter().map(|m| m.item_id.clone()).collect();
    memberships.retain(|m| !roots.get(m.group_id.as_str())
        .and_then(|root| state.records.get(*root)).is_some_and(|r| r.contains_key(&m.item_id)));
    let after: BTreeSet<&str> = memberships.iter().map(|m| m.item_id.as_str()).collect();
    Ok(before.into_iter().filter(|id| !after.contains(id.as_str())).collect())
}

pub(super) fn marked_items(root: &Path, nodes: &[GroupNode], memberships: &[GroupMembership]) -> Result<BTreeSet<String>> {
    let state = read(root)?;
    let roots: BTreeMap<_, _> = nodes.iter().map(|n| (&n.group_id, &n.root_group_id)).collect();
    Ok(memberships.iter().filter(|m| roots.get(&m.group_id)
        .and_then(|id| state.records.get(*id)).is_some_and(|r| r.contains_key(&m.item_id)))
        .map(|m| m.item_id.clone()).collect())
}

/// Only restoration dependencies of the physical subtree, not unrelated
/// requests or hidden sibling relationships, participate in its confirmation.
pub(super) fn deletion_dependencies(root: &Path, pairs: &BTreeSet<(String, String)>) -> Result<Vec<(String, String, Option<TrashRecord>)>> {
    let state = read(root)?;
    Ok(pairs.iter().map(|(group, item)| (group.clone(), item.clone(),
        state.records.get(group).and_then(|r| r.get(item)).cloned())).collect())
}

pub fn list_group_trash(library: impl AsRef<Path>, group_id: &str, cursor: Option<&str>, ceiling: Sensitivity) -> Result<(Vec<TrashEntry>, Option<String>)> {
    if cursor.is_some_and(|id| !valid_item_id(id)) { anyhow::bail!("invalid_cursor") }
    with_consistent_snapshot(library, |root| {
        let groups = load_state(root)?;
        authorized_root(&groups, group_id, ceiling)?;
        let effective = effective_sensitivities(&groups);
        let allowed: BTreeSet<&str> = groups.nodes.iter()
            .filter(|g| g.root_group_id == group_id && effective[&g.group_id] <= ceiling)
            .map(|g| g.group_id.as_str()).collect();
        let members: BTreeSet<&str> = groups.memberships.iter()
            .filter(|m| allowed.contains(m.group_id.as_str())).map(|m| m.item_id.as_str()).collect();
        let state = read(root)?;
        let mut entries = Vec::new();
        if let Some(records) = state.records.get(group_id) {
            for (id, record) in records.iter().filter(|(id, _)| cursor.is_none_or(|c| id.as_str() > c)) {
                if !members.contains(id.as_str()) { continue }
                let Some(item) = read_item_metadata_at_sensitivity(root, id, ceiling)? else { continue };
                entries.push(TrashEntry {
                    item_id: id.clone(), revision_id: item.revision_id, title: item.title,
                    source_url: item.source_url, trashed_at: record.trashed_at.clone(), reason: "manual".into(),
                    kept_item_id: None, plan_id: record.plan_id.clone(),
                });
                if entries.len() > TRASH_PAGE_SIZE { break }
            }
        }
        let more = entries.len() > TRASH_PAGE_SIZE;
        entries.truncate(TRASH_PAGE_SIZE);
        let next = if more { entries.last().map(|e| e.item_id.clone()) } else { None };
        Ok((entries, next))
    })
}
