use super::*;
use memex_core::Sensitivity;
use serde_json::{json, Value};

impl Queue {
    pub fn set_active_group(&mut self, library: &Path, request_id: &str, group_id: Option<&str>, ceiling: Sensitivity) -> Result<Value> {
        anyhow::ensure!(valid_identity(request_id), "invalid_request");
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS ui_group_requests(request_id TEXT PRIMARY KEY,digest TEXT NOT NULL,receipt TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS ui_group_current(singleton INTEGER PRIMARY KEY CHECK(singleton=1),receipt TEXT NOT NULL);")?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&(group_id, ceiling))?));
        if let Some((old, receipt)) = tx.query_row("SELECT digest,receipt FROM ui_group_requests WHERE request_id=?1", [request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            anyhow::ensure!(old == digest, "request_identity_conflict");
            // A replay is not a fresh selection, but must still respect visibility.
            validate_selection(library, group_id, ceiling)?;
            return Ok(serde_json::from_str(&receipt)?);
        }
        validate_selection(library, group_id, ceiling)?;
        let receipt = json!({"schema_version":1,"request_id":request_id,"group_id":group_id,"updated_at":Utc::now().to_rfc3339()});
        let encoded = serde_json::to_string(&receipt)?;
        tx.execute("INSERT INTO ui_group_requests VALUES(?1,?2,?3)", params![request_id,digest,encoded])?;
        tx.execute("INSERT INTO ui_group_current VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET receipt=excluded.receipt", [&encoded])?;
        tx.commit()?;
        self.secure()?;
        Ok(receipt)
    }

    pub fn active_group(&self, library: &Path) -> Result<Value> {
        let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ui_group_current')", [], |r| r.get(0))?;
        let receipt: Option<String> = if exists {
            self.conn.query_row("SELECT receipt FROM ui_group_current WHERE singleton=1", [], |r|r.get(0)).optional()?
        } else { None };
        let saved: Value = receipt.map(|v|serde_json::from_str(&v)).transpose()?.unwrap_or(Value::Null);
        let empty = json!({"schema_version":1,"group_id":null,"path":[],"root_group_id":null,"writable":false,"updated_at":saved["updated_at"]});
        let Some(id) = saved["group_id"].as_str() else { return Ok(empty); };
        memex_store::with_consistent_snapshot(library, |root| {
            let groups = memex_store::read_visible_groups(root, Sensitivity::Restricted)?;
            let Some(selected) = groups.nodes.iter().find(|n|n.group_id == id && n.owner_id == "owner_local") else { return Ok(empty); };
            let mut path = vec![];
            let mut node = selected;
            let mut writable = true;
            loop {
                if node.owner_id != "owner_local" { return Ok(empty); }
                path.push(node.name.clone());
                writable &= !node.read_only;
                let Some(parent) = node.parent_id.as_deref() else { break; };
                let Some(parent) = groups.nodes.iter().find(|n|n.group_id == parent) else { return Ok(empty); };
                node = parent;
            }
            path.reverse();
            Ok(json!({"schema_version":1,"group_id":id,"path":path,"root_group_id":selected.root_group_id,"writable":writable,"updated_at":saved["updated_at"]}))
        })
    }
}

fn validate_selection(library: &Path, id: Option<&str>, ceiling: Sensitivity) -> Result<()> {
    if let Some(id) = id {
        memex_store::with_consistent_snapshot(library, |root| {
            let groups = memex_store::read_visible_groups(root, ceiling)?;
            let mut node = groups.nodes.iter().find(|n| n.group_id == id).context("library_resource_not_found")?;
            loop {
                anyhow::ensure!(node.owner_id == "owner_local", "library_resource_not_found");
                let Some(parent) = node.parent_id.as_deref() else { break; };
                node = groups.nodes.iter().find(|n| n.group_id == parent).context("library_resource_not_found")?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_replay_clear_restart_and_current_visibility() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let root = memex_store::edit_groups(&library,"root",memex_store::GroupEditOp::CreateRoot { name:"Synthetic".into(), sensitivity:Sensitivity::Private },Sensitivity::Restricted).unwrap();
        let folder = memex_store::edit_groups(&library,"folder",memex_store::GroupEditOp::CreateFolder { parent_id:root.group_id.clone(), expected_parent_version:root.version, name:"Target".into() },Sensitivity::Restricted).unwrap();
        let db = temp.path().join("agent.db");
        let mut queue = Queue::open(&db).unwrap();
        assert!(queue.active_group(&library).unwrap()["group_id"].is_null());
        assert!(queue.set_active_group(&library,"denied",Some(&folder.group_id),Sensitivity::Public).is_err());
        let first = queue.set_active_group(&library,"one",Some(&folder.group_id),Sensitivity::Restricted).unwrap();
        assert_eq!(queue.active_group(&library).unwrap()["path"],json!(["Synthetic","Target"]));
        queue.set_active_group(&library,"two",None,Sensitivity::Restricted).unwrap();
        drop(queue);
        let mut queue = Queue::open(&db).unwrap();
        assert_eq!(queue.set_active_group(&library,"one",Some(&folder.group_id),Sensitivity::Restricted).unwrap(),first);
        assert!(queue.active_group(&library).unwrap()["group_id"].is_null());
        assert_eq!(queue.set_active_group(&library,"one",Some(&root.group_id),Sensitivity::Restricted).unwrap_err().to_string(),"request_identity_conflict");
        queue.set_active_group(&library,"three",Some(&folder.group_id),Sensitivity::Restricted).unwrap();
        let state_path = library.join("catalog/groups/state.json");
        let mut state:Value=serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        for n in state["nodes"].as_array_mut().unwrap() { if n["group_id"]==folder.group_id { n["owner_id"]="foreign_owner".into(); } }
        fs::write(state_path,serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(queue.active_group(&library).unwrap()["group_id"].is_null());
        assert_eq!(queue.active_group(&library).unwrap()["path"],json!([]));
        assert!(queue.set_active_group(&library,"three",Some(&folder.group_id),Sensitivity::Restricted).is_err());
    }
}
