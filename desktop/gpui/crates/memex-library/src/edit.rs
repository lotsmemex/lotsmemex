//! Group writes (`groups_v1`, `group_edit`): new root group, new subfolder,
//! rename, and adding items to a folder. Pure state: each edit carries a
//! request id, reused only when the same edit is retried after an unknown
//! outcome; a stale tree is refreshed and the edit starts over. Selections are
//! added (or moved: added, then removed from the source) in bounded batches;
//! there is no move op on the wire and no folder delete here.

use memex_client::{biblio::new_request_id, Client, ClientError, GroupEditOp};

/// Rename only the frozen revision, preserving the canonical body's manual tags.
/// A fresh read is not permission to overwrite a revision newer than the selected row.
pub fn rename_item(client: &Client, row: &crate::model::Row, title: &str) -> Result<(), ClientError> {
    let item = client.item_read(&row.item_id)?;
    if item.revision_id != row.revision_id {
        return Err(ClientError::Daemon("stale_item_revision".into()));
    }
    let tags = item.manual_tags.ok_or(ClientError::InvalidFrame)?;
    client.update_metadata(&row.item_id, &row.revision_id, title, &tags)?;
    Ok(())
}

/// What an inline name field is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draft {
    NewRoot,
    NewChild { parent_id: String },
    Rename { id: String },
}

impl Draft {
    /// The operation for `name`, or `None` for a blank name.
    pub fn op(&self, name: &str) -> Option<GroupEditOp> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let name = name.to_string();
        Some(match self {
            Draft::NewRoot => GroupEditOp::CreateRoot { name },
            Draft::NewChild { parent_id } => GroupEditOp::CreateFolder { parent_id: parent_id.clone(), name },
            Draft::Rename { id } => GroupEditOp::Rename { id: id.clone(), name },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub request_id: String,
    /// The target node's version (the parent's for a new subfolder); none for a new root.
    pub expected_version: Option<String>,
    pub op: GroupEditOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The outcome is unknown; submitting the same edit again reuses its request.
    Unknown,
    /// The tree changed elsewhere: refresh, then the user edits again.
    Stale,
    Rejected,
}

#[derive(Debug, Clone, Default)]
pub struct GroupEditor {
    /// The last request whose outcome is unknown, kept for a retry.
    pending: Option<Pending>,
    in_flight: bool,
    message: Option<String>,
}

impl GroupEditor {
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn clear_message(&mut self) {
        self.message = None;
    }

    pub fn pending_transfer(&self) -> Option<Pending> {
        self.pending.as_ref().filter(|p| !self.in_flight && matches!(p.op, GroupEditOp::TransferFolders { .. })).cloned()
    }

    /// The request to send (one op, as the agent accepts), or `None` while
    /// another is in flight.
    pub fn prepare(&mut self, expected_version: Option<String>, op: GroupEditOp) -> Option<Pending> {
        if self.in_flight {
            return None;
        }
        if self.pending_transfer().is_some_and(|p| p.op != op || p.expected_version != expected_version) {
            self.message = Some(memex_client::tx("上次文件夹拖放结果未知，请先重试原请求").into());
            return None;
        }
        let request = match &self.pending {
            Some(p) if p.op == op && p.expected_version == expected_version => p.clone(),
            _ => Pending { request_id: new_request_id(), expected_version, op },
        };
        self.pending = Some(request.clone());
        self.in_flight = true;
        self.message = None;
        Some(request)
    }

    pub fn finish(&mut self, result: Result<(), ClientError>) -> Outcome {
        self.in_flight = false;
        let transfer = self.pending.as_ref().is_some_and(|p| matches!(p.op, GroupEditOp::TransferFolders { .. }));
        let outcome = match &result {
            Ok(()) => Outcome::Done,
            Err(e) if e.is_transport() => Outcome::Unknown,
            // Publishing state+receipt may precede a sync/generation failure.
            // A generic daemon error cannot prove this mapping was rejected.
            Err(ClientError::Daemon(code)) if transfer && matches!(code.as_str(), "internal_error" | "result_too_large") => Outcome::Unknown,
            // A previous attempt with this identity may still be committing.
            Err(ClientError::Daemon(code)) if code == "library_loading" => Outcome::Unknown,
            Err(e) if e.is_stale_group() => Outcome::Stale,
            Err(_) => Outcome::Rejected,
        };
        if outcome != Outcome::Unknown {
            self.pending = None;
        }
        self.message = match (&result, outcome) {
            (Err(e), Outcome::Unknown) => Some(memex_client::txf("分组修改结果未知：{0}。再次提交会重试同一请求，不会重复写入。", &[&e.message()])),
            (Err(e), _) => Some(e.message()),
            _ => None,
        };
        outcome
    }
}

/// How 添加到文件夹… / 映射到其他库… over a selection ended. `done` counts
/// the items of batches the agent confirmed (or that were already there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberOutcome {
    Done { changed: bool },
    /// Batches before the failing one were applied; `unknown` when the failing
    /// batch's outcome is not known (it may have been applied).
    Failed { done: usize, unknown: bool, error: ClientError },
}

/// `add_members` of `item_ids` to `group_id`, at most `MAX_GROUP_EDIT_ITEMS`
/// per request. The first batch names `version` (as the UI read it); each
/// later one the version this flow's own previous receipt reported, never a
/// version read afresh. `groups_unchanged` means that batch was already there.
/// Stops at the first failure.
pub fn add_members(client: &Client, group_id: &str, version: Option<String>, item_ids: &[String]) -> MemberOutcome {
    let mut version = version;
    let mut changed = false;
    let mut done = 0;
    let batches: Vec<&[String]> = item_ids.chunks(memex_client::MAX_GROUP_EDIT_ITEMS).collect();
    for (i, batch) in batches.iter().enumerate() {
        let op = GroupEditOp::AddMembers { group_id: group_id.to_string(), item_ids: batch.to_vec() };
        match client.group_edit_receipt(&new_request_id(), version.as_deref(), op) {
            Ok(receipt) => {
                changed = true;
                match receipt.filter(|r| r.group_id == group_id) {
                    Some(r) => version = Some(r.version),
                    // No receipt: the next batch would have to guess the version.
                    None if i + 1 < batches.len() => return MemberOutcome::Failed { done: done + batch.len(), unknown: false, error: ClientError::InvalidFrame },
                    None => {}
                }
            }
            Err(ClientError::Daemon(code)) if code == "groups_unchanged" => {}
            Err(error) => {
                let unknown = error.is_transport();
                return MemberOutcome::Failed { done, unknown, error };
            }
        }
        done += batch.len();
    }
    MemberOutcome::Done { changed }
}

/// One request of a move, frozen when first sent: a retry after an unknown
/// outcome resends exactly the same request (id and versions).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    request_id: String,
    versions: (Option<String>, Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MoveBatch {
    item_ids: Vec<String>,
    /// `add_members` to the target (or the whole `move_members`), then `remove_members`.
    add: Option<Sent>,
    added: bool,
    remove: Option<Sent>,
    removed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveStage {
    /// Adding to the target (two-step): nothing has left the source.
    Add,
    /// Removing from the source after a confirmed add.
    Remove,
    /// One atomic `move_members` request.
    Atomic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved,
    /// `added`: items confirmed in the target; `moved`: items also out of the
    /// source. `unknown`: the failing request may have been applied; resuming
    /// the plan resends it unchanged.
    Failed { stage: MoveStage, added: usize, moved: usize, unknown: bool, error: ClientError },
}

/// 移动到文件夹…: one selection from one folder to another folder of the same
/// library, frozen (source, target, items, request ids). With `move_members`
/// advertised, each batch is one atomic request. Otherwise the transition:
/// every batch is added to the target first; only after all adds are
/// confirmed (a `groups_unchanged` add is a verified existing membership)
/// is anything removed from the source. An unknown outcome stops the plan
/// for an identical retry; a version conflict or refusal is final (no other
/// version is substituted). The target is never cleaned up as a rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovePlan {
    pub source: String,
    pub target: String,
    atomic: bool,
    batches: Vec<MoveBatch>,
    /// The version each node's next request names; advanced only by this
    /// plan's own receipts (atomic: by reading the tree, as its contract asks).
    source_version: Option<String>,
    target_version: Option<String>,
}

impl MovePlan {
    pub fn new(source: String, source_version: Option<String>, target: String, target_version: Option<String>, item_ids: &[String], atomic: bool) -> Self {
        let batches = item_ids.chunks(memex_client::MAX_GROUP_EDIT_ITEMS).map(|b| MoveBatch { item_ids: b.to_vec(), add: None, added: false, remove: None, removed: false }).collect();
        Self { source, target, atomic, batches, source_version, target_version }
    }

    pub fn len(&self) -> usize {
        self.batches.iter().map(|b| b.item_ids.len()).sum()
    }

    /// The same move (same folders, same items) as this plan: retrying it
    /// resumes this plan's requests instead of starting new ones.
    pub fn is_same(&self, source: &str, target: &str, item_ids: &[String]) -> bool {
        let mut mine: Vec<&String> = self.batches.iter().flat_map(|b| &b.item_ids).collect();
        let mut theirs: Vec<&String> = item_ids.iter().collect();
        mine.sort();
        theirs.sort();
        self.source == source && self.target == target && mine == theirs
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    fn count(&self, f: impl Fn(&MoveBatch) -> bool) -> usize {
        self.batches.iter().filter(|b| f(b)).map(|b| b.item_ids.len()).sum()
    }

    fn failed(&self, stage: MoveStage, error: ClientError) -> MoveOutcome {
        let unknown = error.is_transport();
        MoveOutcome::Failed { stage, added: self.count(|b| b.added), moved: self.count(|b| b.removed), unknown, error }
    }

    /// Runs (or resumes) the plan until it is done or a request fails.
    pub fn run(&mut self, client: &Client) -> MoveOutcome {
        if self.source == self.target {
            return self.failed(MoveStage::Add, ClientError::Daemon("invalid_group_edit".into()));
        }
        if self.atomic {
            return self.run_atomic(client);
        }
        for i in 0..self.batches.len() {
            if self.batches[i].added { continue; }
            let sent = self.batches[i].add.get_or_insert_with(|| Sent { request_id: new_request_id(), versions: (self.target_version.clone(), None) }).clone();
            let op = GroupEditOp::AddMembers { group_id: self.target.clone(), item_ids: self.batches[i].item_ids.clone() };
            match client.group_edit_receipt(&sent.request_id, sent.versions.0.as_deref(), op) {
                Ok(receipt) => {
                    self.batches[i].added = true;
                    self.target_version = receipt.filter(|r| r.group_id == self.target).map(|r| r.version);
                }
                Err(ClientError::Daemon(code)) if code == "groups_unchanged" => self.batches[i].added = true,
                Err(error) => {
                    // Known failures are final: the next attempt is a new request.
                    if !error.is_transport() { self.batches[i].add = None; }
                    return self.failed(MoveStage::Add, error);
                }
            }
        }
        for i in 0..self.batches.len() {
            if self.batches[i].removed { continue; }
            let sent = self.batches[i].remove.get_or_insert_with(|| Sent { request_id: new_request_id(), versions: (self.source_version.clone(), None) }).clone();
            let op = GroupEditOp::RemoveMembers { group_id: self.source.clone(), item_ids: self.batches[i].item_ids.clone() };
            match client.group_edit_receipt(&sent.request_id, sent.versions.0.as_deref(), op) {
                Ok(receipt) => {
                    self.batches[i].removed = true;
                    self.source_version = receipt.filter(|r| r.group_id == self.source).map(|r| r.version);
                }
                Err(ClientError::Daemon(code)) if code == "groups_unchanged" => self.batches[i].removed = true,
                Err(error) => {
                    if !error.is_transport() { self.batches[i].remove = None; }
                    return self.failed(MoveStage::Remove, error);
                }
            }
        }
        MoveOutcome::Moved
    }

    fn run_atomic(&mut self, client: &Client) -> MoveOutcome {
        for i in 0..self.batches.len() {
            if self.batches[i].removed { continue; }
            if self.batches[i].add.is_none() && self.batches[..i].iter().any(|b| b.removed) {
                // Refresh both nodes before a later batch (the receipt names the target only).
                match client.groups() {
                    Ok(nodes) => {
                        let v = |id: &str| nodes.iter().find(|n| n.id == id).map(|n| n.version.clone());
                        (self.source_version, self.target_version) = (v(&self.source), v(&self.target));
                    }
                    Err(error) => return self.failed(MoveStage::Atomic, error),
                }
            }
            let sent = self.batches[i].add.get_or_insert_with(|| Sent { request_id: new_request_id(), versions: (self.target_version.clone(), self.source_version.clone()) }).clone();
            let op = GroupEditOp::MoveMembers { source_group_id: self.source.clone(), target_group_id: self.target.clone(), expected_source_version: sent.versions.1.clone().unwrap_or_default(), item_ids: self.batches[i].item_ids.clone() };
            match client.group_edit_receipt(&sent.request_id, sent.versions.0.as_deref(), op) {
                Ok(_) => { self.batches[i].added = true; self.batches[i].removed = true; }
                Err(error) => {
                    if !error.is_transport() { self.batches[i].add = None; }
                    return self.failed(MoveStage::Atomic, error);
                }
            }
        }
        MoveOutcome::Moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Client, Command, GroupNode, Sensitivity};

    fn wire(expected_version: Option<&str>, op: GroupEditOp) -> serde_json::Value {
        serde_json::to_value(Command::GroupEdit { max_sensitivity: Sensitivity::Restricted, request_id: "q1".into(), expected_version: expected_version.map(str::to_string), ops: vec![op] }).unwrap()
    }

    #[test]
    fn rename_keeps_body_and_tags_and_refuses_a_newer_revision() {
        let fixture = FixtureBackend::new(30);
        let client = Client::new(fixture.clone());
        let id = "item_fixture_000008";
        let before = client.item_read(id).unwrap();
        client.update_metadata(id, &before.revision_id, &before.title, &["manual-A".into(), "手动-B".into()]).unwrap();
        let before = client.item_read(id).unwrap();
        let query = client.library_query(&Default::default(), Sensitivity::Restricted, 0, 30, None).unwrap();
        let row = crate::model::Row::from(query.items.iter().find(|r| r.item_id == id).unwrap());
        rename_item(&client, &row, "新标题").unwrap();
        let after = client.item_read(id).unwrap();
        assert_eq!(after.title, "新标题");
        assert_eq!((after.markdown, after.manual_tags), (before.markdown, before.manual_tags));
        assert!(matches!(rename_item(&client, &row, "stale"), Err(ClientError::Daemon(c)) if c == "stale_item_revision"));
        assert_eq!(client.item_read(id).unwrap().title, "新标题");
    }

    #[test]
    fn opt_in_group_trash_restores_memberships_and_replays_without_touching_other_libraries() {
        let fixture = FixtureBackend::new(40);
        let client = Client::new(fixture.clone());
        assert!(!client.capabilities().unwrap().group_trash);
        fixture.set_controls(memex_client::fixture::FixtureControls { group_trash: true, ..fixture.controls() });
        client.forget_capabilities();
        let id = "item_fixture_000008";
        let before = client.item_read(id).unwrap();
        let add = |group: &str| {
            let version = client.groups().unwrap().into_iter().find(|g| g.id == group).unwrap().version;
            client.group_edit(&memex_client::biblio::new_request_id(), Some(&version), GroupEditOp::AddMembers { group_id: group.into(), item_ids: vec![id.into()] }).unwrap();
        };
        add("grp_c_sleep"); add("grp_c_multi");
        let count = || client.groups().unwrap().into_iter().find(|g| g.id == "grp_c_multi").unwrap().item_count;
        let other = count();
        client.group_trash_edit("grp_z_psych", id, &before.revision_id, "delete-once", None).unwrap();
        client.group_trash_edit("grp_z_psych", id, &before.revision_id, "delete-once", None).unwrap();
        let trash = client.group_trash_all("grp_z_psych").unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(count(), other);
        assert_eq!(client.item_read(id).unwrap().markdown, before.markdown);
        client.group_trash_edit("grp_z_psych", id, &before.revision_id, "restore-once", trash[0].plan_id.as_deref()).unwrap();
        client.group_trash_edit("grp_z_psych", id, &before.revision_id, "restore-once", trash[0].plan_id.as_deref()).unwrap();
        // Replaying the old delete must not delete it again after restoration.
        client.group_trash_edit("grp_z_psych", id, &before.revision_id, "delete-once", None).unwrap();
        assert!(client.group_trash_all("grp_z_psych").unwrap().is_empty());
        assert_eq!(count(), other);
        assert!(client.group_trash_edit("grp_z_ai", id, &before.revision_id, "delete-once", None).is_err());
    }

    #[test]
    fn group_edit_wire_shapes() {
        // A new root group names no version (the agent rejects one).
        assert_eq!(
            wire(None, Draft::NewRoot.op(" 读书会 ").unwrap()),
            serde_json::json!({"command": "group_edit", "max_sensitivity": "restricted", "request_id": "q1", "ops": [{"op": "create_root", "name": "读书会"}]})
        );
        assert_eq!(
            wire(Some("v2"), Draft::NewChild { parent_id: "grp_a".into() }.op("第一期").unwrap()),
            serde_json::json!({"command": "group_edit", "max_sensitivity": "restricted", "request_id": "q1", "expected_version": "v2", "ops": [{"op": "create_folder", "parent_id": "grp_a", "name": "第一期"}]})
        );
        assert_eq!(wire(Some("v5"), Draft::Rename { id: "grp_b".into() }.op("新名字").unwrap())["ops"], serde_json::json!([{"op": "rename", "id": "grp_b", "name": "新名字"}]));
        assert_eq!(
            wire(Some("v1"), GroupEditOp::AddMembers { group_id: "grp_a".into(), item_ids: vec!["item_1".into()] })["ops"],
            serde_json::json!([{"op": "add_members", "group_id": "grp_a", "item_ids": ["item_1"]}])
        );
        // The contract's atomic move: the target version on top, the source's in the op.
        assert_eq!(
            wire(Some("vt"), GroupEditOp::MoveMembers { source_group_id: "f_a".into(), target_group_id: "f_b".into(), expected_source_version: "vs".into(), item_ids: vec!["item_1".into()] }),
            serde_json::json!({"command": "group_edit", "max_sensitivity": "restricted", "request_id": "q1", "expected_version": "vt", "ops": [{"op": "move_members", "source_group_id": "f_a", "target_group_id": "f_b", "expected_source_version": "vs", "item_ids": ["item_1"]}]})
        );
        assert_eq!(Draft::NewRoot.op("   "), None);
    }

    #[test]
    fn request_id_is_reused_only_for_a_retry_of_the_same_edit() {
        let mut editor = GroupEditor::default();
        let op = Draft::NewRoot.op("读书会").unwrap();
        let first = editor.prepare(None, op.clone()).unwrap();
        assert_eq!(editor.prepare(None, op.clone()), None, "one edit at a time");
        assert_eq!(editor.finish(Err(ClientError::Timeout)), Outcome::Unknown);
        assert!(editor.message().is_some_and(|m| m.contains("不会重复写入")));
        let retry = editor.prepare(None, op.clone()).unwrap();
        assert_eq!(retry.request_id, first.request_id);
        assert_eq!(editor.finish(Err(ClientError::Daemon("library_loading".into()))), Outcome::Unknown);
        let retry = editor.prepare(None, op.clone()).unwrap();
        assert_eq!(retry.request_id, first.request_id, "busy retry must not discard an unresolved write identity");
        // A different edit, or a success, starts a new request.
        assert_eq!(editor.finish(Err(ClientError::Unavailable)), Outcome::Unknown);
        let other = editor.prepare(None, Draft::NewRoot.op("别的").unwrap()).unwrap();
        assert_ne!(other.request_id, first.request_id);
        assert_eq!(editor.finish(Ok(())), Outcome::Done);
        let rename = Draft::Rename { id: "g".into() }.op("新").unwrap();
        let again = editor.prepare(Some("v2".into()), rename.clone()).unwrap();
        assert_ne!(again.request_id, first.request_id);
        // A stale node is refreshed; the next attempt (at the new version) is a new request.
        assert_eq!(editor.finish(Err(ClientError::Daemon("stale_group_version".into()))), Outcome::Stale);
        assert_ne!(editor.prepare(Some("v3".into()), rename).unwrap().request_id, again.request_id);
    }

    #[test]
    fn folder_transfer_uncertain_daemon_errors_keep_the_frozen_identity() {
        let mut editor = GroupEditor::default();
        let op = GroupEditOp::TransferFolders { source_ids: vec!["source".into()], target_parent_id: "target".into(), expected_versions: Default::default(), moving: false };
        let first = editor.prepare(Some("v1".into()), op.clone()).unwrap();
        for code in ["internal_error", "result_too_large"] {
            assert_eq!(editor.finish(Err(ClientError::Daemon(code.into()))), Outcome::Unknown);
            assert_eq!(editor.pending_transfer(), Some(first.clone()));
            assert!(editor.prepare(Some("v2".into()), op.clone()).is_none());
            assert_eq!(editor.prepare(first.expected_version.clone(), op.clone()), Some(first.clone()));
        }
        assert_eq!(editor.finish(Ok(())), Outcome::Done);
        assert!(editor.pending_transfer().is_none());
    }

    fn version(client: &Client, id: &str) -> Option<String> {
        client.groups().unwrap().into_iter().find(|n| n.id == id).map(|n| n.version)
    }

    /// Every item of `spec`, window by window.
    fn all_ids(client: &Client, spec: &memex_client::QuerySpec) -> Vec<String> {
        let first = client.library_query(spec, Sensitivity::Restricted, 0, memex_client::MAX_QUERY_LIMIT, None).unwrap();
        let mut out: Vec<String> = first.items.iter().map(|i| i.item_id.clone()).collect();
        while out.len() < first.total {
            let w = client.library_query(spec, Sensitivity::Restricted, out.len(), memex_client::MAX_QUERY_LIMIT, Some(first.view.clone())).unwrap();
            out.extend(w.items.into_iter().map(|i| i.item_id));
        }
        out
    }

    fn direct(client: &Client, id: &str) -> Vec<String> {
        all_ids(client, &memex_client::QuerySpec { group: Some(memex_client::GroupScope { group_id: id.into(), include_descendants: false }), ..Default::default() })
    }

    fn controls(fixture: &FixtureBackend, f: impl FnOnce(&mut memex_client::fixture::FixtureControls)) {
        let mut c = fixture.controls();
        f(&mut c);
        fixture.set_controls(c);
    }

    #[test]
    fn a_move_adds_first_and_removes_only_after_a_confirmed_add() {
        let fixture = FixtureBackend::new(120);
        let client = Client::new(fixture.clone());
        let (from, to) = ("grp_c_sleep", "grp_c_attention");
        let ids: Vec<String> = direct(&client, from).into_iter().take(5).collect();
        assert_eq!(ids.len(), 5);
        assert!(ids.iter().all(|id| !direct(&client, to).contains(id)));
        let plan = |client: &Client| MovePlan::new(from.into(), version(client, from), to.into(), version(client, to), &ids, false);
        let in_source = |client: &Client| ids.iter().filter(|id| direct(client, from).contains(id)).count();
        let in_target = |client: &Client| ids.iter().filter(|id| direct(client, to).contains(id)).count();

        // The add is refused: the source is untouched, and the remove is never
        // sent (its armed failure is still pending afterwards).
        controls(&fixture, |c| { c.fail_next_add_members = Some("group_read_only".into()); c.fail_next_remove_members = Some("internal_error".into()); });
        assert!(matches!(plan(&client).run(&client), MoveOutcome::Failed { stage: MoveStage::Add, added: 0, unknown: false, .. }));
        assert_eq!((in_source(&client), in_target(&client)), (5, 0));
        assert_eq!(fixture.controls().fail_next_remove_members.as_deref(), Some("internal_error"));

        // An add with an unknown outcome stops there; resuming resends that same
        // request (a replay), and only then removes from the source.
        controls(&fixture, |c| { c.fail_next_add_members = Some("library_loading".into()); c.fail_next_remove_members = None; });
        let mut p = plan(&client);
        assert!(matches!(p.run(&client), MoveOutcome::Failed { stage: MoveStage::Add, unknown: true, .. }));
        assert_eq!(in_source(&client), 5);
        assert_eq!(p.run(&client), MoveOutcome::Moved);
        assert_eq!((in_source(&client), in_target(&client)), (0, 5));
    }

    #[test]
    fn a_failed_remove_leaves_the_items_in_both_folders_and_never_cleans_the_target() {
        let fixture = FixtureBackend::new(120);
        let client = Client::new(fixture.clone());
        let (from, to) = ("grp_c_sleep", "grp_c_attention");
        let ids: Vec<String> = direct(&client, from).into_iter().take(4).collect();
        let both = |client: &Client| ids.iter().all(|id| direct(client, from).contains(id) && direct(client, to).contains(id));
        controls(&fixture, |c| c.fail_next_remove_members = Some("internal_error".into()));
        let mut p = MovePlan::new(from.into(), version(&client, from), to.into(), version(&client, to), &ids, false);
        assert!(matches!(p.run(&client), MoveOutcome::Failed { stage: MoveStage::Remove, added: 4, moved: 0, unknown: false, .. }));
        assert!(both(&client), "added to the target, still in the source");
        // A source changed elsewhere is a conflict, not a reason to use its new version.
        let mut stale = MovePlan::new(from.into(), Some("v0".into()), to.into(), version(&client, to), &ids, false);
        assert!(matches!(stale.run(&client), MoveOutcome::Failed { stage: MoveStage::Remove, unknown: false, error: ClientError::Daemon(ref c), .. } if c == "stale_group_version"));
        assert!(both(&client));
        // Moving again: the add is verified as already there, the remove runs.
        let mut again = MovePlan::new(from.into(), version(&client, from), to.into(), version(&client, to), &ids, false);
        assert_eq!(again.run(&client), MoveOutcome::Moved);
        assert!(ids.iter().all(|id| !direct(&client, from).contains(id) && direct(&client, to).contains(id)));
        // Never into the folder it came from.
        let mut same = MovePlan::new(to.into(), version(&client, to), to.into(), version(&client, to), &ids, false);
        assert!(matches!(same.run(&client), MoveOutcome::Failed { stage: MoveStage::Add, added: 0, .. }));
        assert!(ids.iter().all(|id| direct(&client, to).contains(id)));
    }

    #[test]
    fn move_members_is_used_only_where_the_agent_lists_it() {
        let fixture = FixtureBackend::new(120);
        let client = Client::new(fixture.clone());
        let (from, to) = ("grp_c_sleep", "grp_c_attention");
        let ids: Vec<String> = direct(&client, from).into_iter().take(3).collect();
        // Not advertised: the atomic op is refused without being sent.
        assert!(!client.capabilities().unwrap().group_move_members);
        let mut p = MovePlan::new(from.into(), version(&client, from), to.into(), version(&client, to), &ids, true);
        assert!(matches!(p.run(&client), MoveOutcome::Failed { stage: MoveStage::Atomic, error: ClientError::Unsupported, .. }));
        assert_eq!(ids.iter().filter(|id| direct(&client, from).contains(id)).count(), 3);
        // Advertised in write_ops: one request moves them.
        controls(&fixture, |c| c.move_members_op = true);
        client.forget_capabilities();
        assert!(client.capabilities().unwrap().group_move_members);
        // The add/remove failures do not apply: no add_members or remove_members is sent.
        controls(&fixture, |c| { c.fail_next_add_members = Some("internal_error".into()); c.fail_next_remove_members = Some("internal_error".into()); });
        let mut p = MovePlan::new(from.into(), version(&client, from), to.into(), version(&client, to), &ids, true);
        assert_eq!(p.run(&client), MoveOutcome::Moved);
        assert!(ids.iter().all(|id| !direct(&client, from).contains(id) && direct(&client, to).contains(id)));
        assert!(fixture.controls().fail_next_add_members.is_some() && fixture.controls().fail_next_remove_members.is_some());
    }

    #[test]
    fn a_large_selection_is_added_and_moved_in_batches_within_the_agent_limit() {
        let fixture = FixtureBackend::new(300);
        let client = Client::new(fixture);
        let all = all_ids(&client, &Default::default());
        assert!(all.len() > memex_client::MAX_GROUP_EDIT_ITEMS);
        // One request over the limit is refused outright, as by the Store.
        let op = GroupEditOp::AddMembers { group_id: "grp_c_week".into(), item_ids: all.clone() };
        assert_eq!(client.group_edit("q-big", version(&client, "grp_c_week").as_deref(), op), Err(ClientError::Daemon("invalid_group_edit".into())));
        assert_eq!(add_members(&client, "grp_c_week", version(&client, "grp_c_week"), &all), MemberOutcome::Done { changed: true });
        assert_eq!(direct(&client, "grp_c_week").len(), 300);
        // Adding them again changes nothing, and says so.
        assert_eq!(add_members(&client, "grp_c_week", version(&client, "grp_c_week"), &all), MemberOutcome::Done { changed: false });
        // Moved on to a sibling folder in two batches, each at the version its
        // previous batch's receipt reported.
        let mut p = MovePlan::new("grp_c_week".into(), version(&client, "grp_c_week"), "grp_c_toread".into(), version(&client, "grp_c_toread"), &all, false);
        assert_eq!(p.run(&client), MoveOutcome::Moved);
        assert!(direct(&client, "grp_c_week").is_empty());
        assert_eq!(direct(&client, "grp_c_toread").len(), 300);
        assert_eq!(client.library_query(&Default::default(), Sensitivity::Restricted, 0, 1, None).unwrap().total, 300);
    }

    #[test]
    fn fixture_creates_a_folder_with_a_subfolder_and_replays_a_retry() {
        let fixture = FixtureBackend::new(12);
        fixture.clear_groups();
        let client = Client::new(fixture);
        assert!(client.groups().unwrap().is_empty());
        let version = |nodes: &[GroupNode], id: &str| nodes.iter().find(|n| n.id == id).map(|n| n.version.clone());
        let mut editor = GroupEditor::default();
        let send = |editor: &mut GroupEditor, v: Option<String>, op: GroupEditOp| {
            let p = editor.prepare(v, op).unwrap();
            let outcome = editor.finish(client.group_edit(&p.request_id, p.expected_version.as_deref(), p.op.clone()));
            (outcome, p)
        };
        let (outcome, p) = send(&mut editor, None, Draft::NewRoot.op("读书会").unwrap());
        assert_eq!(outcome, Outcome::Done);
        let nodes = client.groups().unwrap();
        assert_eq!(nodes.len(), 1);
        // Replaying the same request (an unknown outcome retried) writes nothing twice.
        client.group_edit(&p.request_id, None, p.op.clone()).unwrap();
        assert_eq!(client.groups().unwrap().len(), 1);
        let root = nodes[0].id.clone();
        let old = version(&nodes, &root);
        let (outcome, _) = send(&mut editor, old.clone(), Draft::NewChild { parent_id: root.clone() }.op("第一期").unwrap());
        assert_eq!(outcome, Outcome::Done);
        // The parent's version moved on: a write naming the old one is stale.
        let stale = client.group_edit("q-stale", old.as_deref(), Draft::Rename { id: root.clone() }.op("x").unwrap());
        assert!(stale.as_ref().is_err_and(ClientError::is_stale_group));
        let nodes = client.groups().unwrap();
        let (outcome, _) = send(&mut editor, version(&nodes, &root), GroupEditOp::AddMembers { group_id: root.clone(), item_ids: vec!["item_fixture_000003".into()] });
        assert_eq!(outcome, Outcome::Done);
        let nodes = client.groups().unwrap();
        let names: Vec<_> = nodes.iter().map(|n| (n.name.as_str(), n.parent_id.is_some(), n.item_count)).collect();
        assert_eq!(names, [("读书会", false, 1), ("第一期", true, 0)]);
    }
}
