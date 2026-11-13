//! Owner-local webpage capture: strict source reuse and filing under the Store writer lock.
use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebCaptureReceipt {
    pub schema_version: u32,
    pub item_id: String,
    pub revision_id: String,
    pub reused: bool,
    pub target_group_id: Option<String>,
    pub filed: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    input_sha256: String,
    receipt: WebCaptureReceipt,
}

fn identity(value: &str) -> Result<String> {
    let mut url = Url::parse(value)?;
    anyhow::ensure!(matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
        && url.username().is_empty() && url.password().is_none(), "invalid_web_url");
    url.set_fragment(None);
    Ok(url.to_string())
}

/// Strict exact source matching, not scheme/www/path folding or untrusted canonical aliases.
/// Any unreadable header aborts the scan. Independent imports are not silently adopted.
pub fn find_web_capture(library: &Path, source_url: &str) -> Result<Option<ItemReadMetadata>> {
    with_consistent_snapshot(library, |root| find_locked(root, source_url))
}
fn find_locked(root: &Path, source_url: &str) -> Result<Option<ItemReadMetadata>> {
    let wanted = identity(source_url)?;
    let mut matches = Vec::new();
    scan_item_metadata_at_sensitivity(root, Sensitivity::Restricted, |item| {
        if identity(&item.source_url).ok().as_ref() == Some(&wanted) { matches.push(item); }
        Ok(())
    })?;
    anyhow::ensure!(matches.len() <= 1, "web_duplicate_conflict");
    let found = matches.pop();
    if let Some(item) = &found {
        anyhow::ensure!(item_origin(root, &item.item_id)? == Some(memex_core::ItemOrigin::Capture), "web_duplicate_conflict");
    }
    Ok(found)
}

/// A durable content commit and an idempotent add-only filing are separate recoverable phases.
/// A filing failure never deletes the committed article. Retry the same immutable request.
pub fn ingest_web_capture(
    library: &Path, capture: &VerifiedCapture, draft: ItemDraft, target: Option<&str>,
) -> Result<WebCaptureReceipt> {
    validate_draft(&draft)?;
    identity(&capture.capture.source_url)?;
    let input_sha256 = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&(
        "web-capture-v1", ingest_input_sha256(capture, &draft)?, target,
    ))?));
    let root = canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    if let Some(target) = target { groups::web_filing_locked(&root, target, None, None)?; }
    let key = hex_key(&ingest_key(capture));
    let parent = ensure_contained_dir(&root, Path::new("catalog/web-captures"))?;
    let path = parent.join(format!("{key}.json"));
    // Bind the entire request, including its destination, before any content
    // journal can commit. A crash must not allow the same ID to change targets.
    let intent = parent.join(format!("{key}.intent.json"));
    match fs::symlink_metadata(&intent) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            durable_replace(&root, &intent, &serde_json::to_vec(&input_sha256)?)?;
        }
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
            anyhow::ensure!(load::<String>(&root, intent)? == input_sha256, "request_identity_conflict");
        }
        Ok(_) => anyhow::bail!("unsafe_web_capture_request"),
        Err(e) => return Err(e.into()),
    }
    let old = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Some(load::<Request>(&root, path.clone())?),
        Ok(_) => anyhow::bail!("unsafe_web_capture_request"),
        Err(e) => return Err(e.into()),
    };
    let mut record = if let Some(record) = old {
        anyhow::ensure!(record.input_sha256 == input_sha256, "request_identity_conflict");
        // Even a historical receipt cannot disclose a now missing/trashed article.
        read_item_metadata_at_sensitivity(&root, &record.receipt.item_id, Sensitivity::Restricted)?
            .context("library_resource_not_found")?;
        let (_, revision) = read_item(&root, &record.receipt.item_id)?;
        anyhow::ensure!(revision.revision_id == record.receipt.revision_id, "web_content_conflict");
        record
    } else {
        let keys = catalog_keys(&root)?;
        // Replay existing ingest identity before dedupe: altered bytes must never be accepted.
        let existing_request = keys.contains_key(&ingest_key(capture));
        let (item_id, revision_id, reused) = if existing_request {
            let PreparedIngest::Replay(receipt) = prepare_ingest_locked(&root, capture, draft.clone(), &keys)? else { anyhow::bail!("web_request_recovery_failed") };
            (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone(), false)
        } else if let Some(item) = find_locked(&root, &draft.source_url)? {
            let (_, revision) = read_item(&root, &item.item_id)?;
            anyhow::ensure!(revision.content_markdown == draft.content_markdown, "web_content_conflict");
            (item.item_id, item.revision_id, true)
        } else {
            let prepared = prepare_ingest_locked(&root, capture, draft, &keys)?;
            let receipt = match prepared {
                PreparedIngest::Replay(receipt) => receipt,
                PreparedIngest::Journal(path, mut journal) => { apply_journal(&root, &path, &mut journal)?; journal.receipt }
            };
            (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone(), false)
        };
        let record = Request { input_sha256: input_sha256.clone(), receipt: WebCaptureReceipt {
            schema_version: 1, item_id, revision_id, reused, target_group_id: target.map(str::to_owned), filed: target.is_none(),
        }};
        durable_replace(&root, &path, &serde_json::to_vec(&record)?)?;
        record
    };
    if !record.receipt.filed {
        groups::web_filing_locked(&root, target.context("invalid_web_target")?, Some(&record.receipt.item_id), Some((&key, &input_sha256)))?;
        record.receipt.filed = true;
        durable_replace(&root, &path, &serde_json::to_vec(&record)?)?;
    }
    clear_recovery_pending(&root)?;
    lock.unlock()?;
    Ok(record.receipt)
}

fn hex_key(value: &str) -> String { format!("{:x}", Sha256::digest(value.as_bytes())) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_collection_files_once_at_a_deep_target_and_replay_does_not_undo_owner_removal() {
        let dir = tempfile::tempdir().unwrap();
        canonical_root(dir.path()).unwrap();
        let root = groups::edit_groups(dir.path(), "root", groups::GroupEditOp::CreateRoot {
            name: "Main".into(), sensitivity: Sensitivity::Internal,
        }, Sensitivity::Restricted).unwrap();
        let folder = groups::edit_groups(dir.path(), "folder", groups::GroupEditOp::CreateFolder {
            parent_id: root.group_id, expected_parent_version: root.version, name: "AI".into(),
        }, Sensitivity::Restricted).unwrap();
        let target = groups::edit_groups(dir.path(), "target", groups::GroupEditOp::CreateFolder {
            parent_id: folder.group_id, expected_parent_version: folder.version, name: "Posts".into(),
        }, Sensitivity::Restricted).unwrap();
        let mut capture = crate::tests::capture();
        capture.capture.source_url = "https://example.test/deep".into();
        capture.capture.capture_method = "browser_snapshot_v1".into();
        let mut draft = crate::tests::draft();
        draft.source_url = capture.capture.source_url.clone();
        let receipts = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2).map(|i| {
                let mut capture = capture.clone();
                capture.capture.external_capture_id = format!("concurrent-{i}");
                let draft = draft.clone(); let path = dir.path(); let target = &target.group_id;
                scope.spawn(move || ingest_web_capture(path, &capture, draft, Some(target)).unwrap())
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(receipts[0].item_id, receipts[1].item_id);
        assert_eq!(receipts.iter().filter(|r| r.reused).count(), 1);
        let state = groups::read_visible_groups(dir.path(), Sensitivity::Restricted).unwrap();
        assert_eq!(state.memberships.len(), 1);
        let target_node = state.nodes.iter().find(|n| n.group_id == target.group_id).unwrap();
        groups::edit_groups(dir.path(), "owner-removal", groups::GroupEditOp::RemoveMembers {
            group_id: target.group_id.clone(), expected_version: target_node.version_token(), item_ids: vec![receipts[0].item_id.clone()],
        }, Sensitivity::Restricted).unwrap();
        capture.capture.external_capture_id = "concurrent-0".into();
        ingest_web_capture(dir.path(), &capture, draft, Some(&target.group_id)).unwrap();
        assert!(groups::read_visible_groups(dir.path(), Sensitivity::Restricted).unwrap().memberships.is_empty());
        assert_eq!(list_items(dir.path()).unwrap().len(), 1);
    }
    #[test]
    fn source_reuse_is_serialized_and_request_payload_is_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let mut capture = crate::tests::capture();
        capture.capture.source_url = "https://example.test/article".into();
        capture.capture.capture_method = "browser_snapshot_v1".into();
        let mut draft = crate::tests::draft();
        draft.source_url = capture.capture.source_url.clone();
        let first = ingest_web_capture(dir.path(), &capture, draft.clone(), None).unwrap();
        assert!(!first.reused);
        let again = ingest_web_capture(dir.path(), &capture, draft.clone(), None).unwrap();
        assert_eq!(first.item_id, again.item_id);
        capture.capture.external_capture_id = "second".into();
        let second = ingest_web_capture(dir.path(), &capture, draft.clone(), None).unwrap();
        assert!(second.reused);
        assert_eq!(first.item_id, second.item_id);
        draft.content_markdown = "changed bytes".into();
        assert_eq!(ingest_web_capture(dir.path(), &capture, draft.clone(), None).unwrap_err().to_string(), "request_identity_conflict");
        capture.capture.external_capture_id = "third".into();
        assert_eq!(ingest_web_capture(dir.path(), &capture, draft, None).unwrap_err().to_string(), "web_content_conflict");
        assert_eq!(list_items(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn interrupted_receipt_publication_cannot_retarget_an_already_committed_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut capture = crate::tests::capture();
        capture.capture.capture_method = "browser_snapshot_v1".into();
        capture.capture.source_url = "https://example.test/crash-recovery".into();
        let mut draft = crate::tests::draft();
        draft.source_url = capture.capture.source_url.clone();
        let first = ingest_web_capture(dir.path(), &capture, draft.clone(), None).unwrap();
        // Simulate content committed and intent durable, but final Web receipt
        // absent after an interrupted publication. Do not remove the ingest journal.
        let key = hex_key(&ingest_key(&capture));
        fs::remove_file(dir.path().join(format!("catalog/web-captures/{key}.json"))).unwrap();
        let target = groups::edit_groups(dir.path(), "target", groups::GroupEditOp::CreateRoot {
            name: "Other target".into(), sensitivity: Sensitivity::Internal,
        }, Sensitivity::Restricted).unwrap();
        assert_eq!(ingest_web_capture(dir.path(), &capture, draft.clone(), Some(&target.group_id)).unwrap_err().to_string(), "request_identity_conflict");
        let replay = ingest_web_capture(dir.path(), &capture, draft, None).unwrap();
        assert_eq!(replay.item_id, first.item_id);
        assert!(groups::read_visible_groups(dir.path(), Sensitivity::Restricted).unwrap().memberships.is_empty());
        assert_eq!(list_items(dir.path()).unwrap().len(), 1);
    }
}
