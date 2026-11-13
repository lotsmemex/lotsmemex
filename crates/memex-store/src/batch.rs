//! Bounded bulk persistence of already verified captures. No fetch or model work.
//! Existing per-item v1 journals remain the recovery authority; a failed call can
//! have committed items and is resumed with exactly the same source identities.
use super::*;
mod durability;

pub const MAX_INGEST_BATCH_ITEMS: usize = 500;
// One bounded commit group; callers split larger streams into requests.
const WINDOW_ITEMS: usize = 500;
const WORKERS: usize = 4;

#[cfg(test)]
thread_local! { static FAIL_BATCH_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }

fn checkpoint(step: usize) -> Result<()> {
    #[cfg(test)]
    if FAIL_BATCH_STEP.with(|value| value.get()) == Some(step) {
        anyhow::bail!("simulated batch interruption");
    }
    let _ = step;
    Ok(())
}

/// Accept up to 500 captures from any trusted adapter, returning receipts in input
/// order. Identical repeated identities share one item; conflicting input refuses
/// replay. This is NOT content similarity/DOI/URL deduplication or an all-or-nothing
/// 500-item transaction. Every published window is recoverable by the v1 reader.
pub fn ingest_batch(
    library: impl AsRef<Path>,
    requests: Vec<(VerifiedCapture, ItemDraft)>,
) -> Result<Vec<IngestReceipt>> {
    anyhow::ensure!(
        requests.len() <= MAX_INGEST_BATCH_ITEMS,
        "ingest batch exceeds 500 items"
    );
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let mut identities = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut positions = Vec::with_capacity(requests.len());
    let mut unique = Vec::new();
    for (capture, draft) in requests {
        validate_draft(&draft)?;
        captured_attachment_paths(&capture, &draft)?;
        anyhow::ensure!(
            valid_source_id(&capture.source.source_id),
            "invalid source id"
        );
        if let Some(existing) =
            sources.insert(capture.source.source_id.clone(), capture.source.clone())
        {
            anyhow::ensure!(
                existing == capture.source,
                "source profile conflicts within batch"
            );
        }
        let key = ingest_key(&capture);
        let digest = ingest_input_sha256(&capture, &draft)?;
        match identities.get(&key) {
            Some((prior, index)) => {
                if prior != &digest {
                    return Err(IngestConflict.into());
                }
                positions.push((*index, true));
            }
            None => {
                let index = unique.len();
                identities.insert(key, (digest, index));
                positions.push((index, false));
                unique.push((capture, draft));
            }
        }
    }
    let root = canonical_root(library.as_ref())?;
    let mut receipts = Vec::with_capacity(unique.len());
    let mut requests = unique.into_iter();
    loop {
        let window: Vec<_> = requests.by_ref().take(WINDOW_ITEMS).collect();
        if window.is_empty() {
            break;
        }
        let lock = ingest_lock(&root)?;
        recover_if_pending_locked(&root)?;
        let mut keys = catalog_keys(&root)?;
        // Establish the marker and common directories before parallel preparation.
        // Any failure leaves ordinary v1 journals for the existing recovery path.
        mark_recovery_pending(&root)?;
        for path in ["catalog/ingest-journal", COMMITTED_JOURNALS] {
            let directory = ensure_contained_dir(&root, Path::new(path))?;
            durability::same_device(&root, &directory)?;
        }
        let mut prepared = parallel(window, |(capture, draft)| {
            durability::stage(&root, plan_ingest_locked(&root, &capture, draft, &keys)?)
        })?;
        // Hidden stages may be discarded on failure. After this device barrier,
        // publication exposes only complete durable v1 journals to old recovery.
        durability::barrier(&root)?;
        checkpoint(4)?;
        durability::publish(&root, &mut prepared)?;
        checkpoint(0)?;
        prepared = parallel(prepared, |item| durability::install(&root, item))?;
        durability::barrier(&root)?;
        checkpoint(1)?;
        let mut changed = false;
        for item in &prepared {
            if let PreparedIngest::Journal(_, journal) = item {
                if let Some(previous) =
                    keys.insert(journal.ingest_key.clone(), journal.receipt_path.clone())
                {
                    anyhow::ensure!(previous == journal.receipt_path, "ingest catalog conflict");
                }
                changed = true;
            }
        }
        if changed {
            durable_replace(
                &root,
                &root.join("catalog/ingest-keys.json"),
                serde_json::to_string_pretty(&keys)?.as_bytes(),
            )?;
        }
        checkpoint(2)?;
        durability::commit(&root, &mut prepared)?;
        checkpoint(5)?;
        receipts.extend(parallel(prepared, |item| durability::archive(&root, item))?);
        durability::barrier(&root)?;
        checkpoint(3)?;
        clear_recovery_pending(&root)?;
        lock.unlock()?;
        // Let a waiting reader take its snapshot before the next write window.
        std::thread::yield_now();
    }
    Ok(positions
        .into_iter()
        .map(|(index, duplicate)| {
            let mut receipt = receipts[index].clone();
            receipt.idempotent_replay |= duplicate;
            receipt
        })
        .collect())
}

// Join every worker, including on error or panic, before returning to the lock
// owner. Ordered contiguous partitions preserve request/receipt correspondence.
fn parallel<T: Send, R: Send>(
    items: Vec<T>,
    operation: impl Fn(T) -> Result<R> + Sync,
) -> Result<Vec<R>> {
    let width = items.len().div_ceil(WORKERS).max(1);
    std::thread::scope(|scope| {
        let mut items = items.into_iter();
        let mut workers = Vec::new();
        loop {
            let group: Vec<_> = items.by_ref().take(width).collect();
            if group.is_empty() {
                break;
            }
            let operation = &operation;
            workers.push(
                scope.spawn(move || group.into_iter().map(operation).collect::<Result<Vec<_>>>()),
            );
        }
        // Collect join results without short circuiting; no detached writer on failure.
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("ingest batch worker failed"))?
            })
            .collect();
        results
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .map(|groups| groups.into_iter().flatten().collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{capture, draft};
    use tempfile::tempdir;

    fn requests(count: usize) -> Vec<(VerifiedCapture, ItemDraft)> {
        (0..count)
            .map(|index| {
                let mut capture = capture();
                capture.capture.external_capture_id = format!("source-item-{index}");
                let mut draft = draft();
                draft.title = format!("Title {index}");
                draft.content_markdown = format!("Unique body {index} 中文");
                (capture, draft)
            })
            .collect()
    }

    #[test]
    fn batch_preserves_order_identity_and_separate_equal_content_items() {
        let dir = tempdir().unwrap();
        let mut inputs = requests(17);
        // Equal content with distinct source IDs is NOT merged.
        inputs[1].1 = inputs[0].1.clone();
        inputs.push(inputs[0].clone());
        let first = ingest_batch(dir.path(), inputs.clone()).unwrap();
        assert_eq!(first.len(), 18);
        assert_ne!(first[0].item_ids, first[1].item_ids);
        assert_eq!(first[0].item_ids, first[17].item_ids);
        assert!(first[17].idempotent_replay);
        for (input, receipt) in inputs.iter().zip(&first) {
            let (_, revision) = read_item(dir.path(), &receipt.item_ids[0]).unwrap();
            assert_eq!(revision.content_markdown, input.1.content_markdown);
            assert_eq!(revision.title, input.1.title);
        }
        let replay = ingest_batch(dir.path(), inputs.clone()).unwrap();
        assert_eq!(list_items(dir.path()).unwrap().len(), 17);
        for (before, after) in first.iter().zip(&replay) {
            assert_eq!(before.item_ids, after.item_ids);
            assert_eq!(before.revision_ids, after.revision_ids);
            assert!(after.idempotent_replay);
        }
        inputs[17].1.title = "conflicting replay".into();
        assert!(ingest_batch(dir.path(), inputs).is_err());
        assert_eq!(list_items(dir.path()).unwrap().len(), 17);
    }

    #[test]
    fn interrupted_batch_recovers_with_existing_reader_and_stable_receipts() {
        for step in [0, 1, 2, 3, 5] {
            let dir = tempdir().unwrap();
            let inputs = requests(3);
            FAIL_BATCH_STEP.with(|fail| fail.set(Some(step)));
            assert!(ingest_batch(dir.path(), inputs.clone()).is_err());
            FAIL_BATCH_STEP.with(|fail| fail.set(None));
            // Ordinary daemon startup recovery, not a batch-specific reader.
            recover_pending_ingests(dir.path()).unwrap();
            let recovered = list_items(dir.path()).unwrap();
            assert_eq!(recovered.len(), 3, "step {step}");
            let ids: BTreeSet<_> = recovered
                .into_iter()
                .map(|(item, _)| item.item_id)
                .collect();
            let replay = ingest_batch(dir.path(), inputs).unwrap();
            assert!(replay.iter().all(|receipt| receipt.idempotent_replay));
            assert_eq!(
                ids,
                replay
                    .into_iter()
                    .flat_map(|receipt| receipt.item_ids)
                    .collect()
            );
        }
    }

    #[test]
    fn unpublished_batch_staging_is_discarded_before_retry() {
        let dir = tempdir().unwrap();
        let inputs = requests(3);
        FAIL_BATCH_STEP.with(|fail| fail.set(Some(4)));
        assert!(ingest_batch(dir.path(), inputs.clone()).is_err());
        FAIL_BATCH_STEP.with(|fail| fail.set(None));
        recover_pending_ingests(dir.path()).unwrap();
        assert!(list_items(dir.path()).unwrap().is_empty());
        let receipts = ingest_batch(dir.path(), inputs.clone()).unwrap();
        for (receipt, input) in receipts.iter().zip(&inputs) {
            let (_, revision) = read_item(dir.path(), &receipt.item_ids[0]).unwrap();
            assert_eq!(revision.content_markdown, input.1.content_markdown);
        }
        let replay = ingest_batch(dir.path(), inputs).unwrap();
        for (before, after) in receipts.iter().zip(replay) {
            assert_eq!(before.item_ids, after.item_ids);
            assert!(after.idempotent_replay);
        }
    }

    #[test]
    fn batch_and_single_writer_preserve_each_others_items_and_snapshot_integrity() {
        let dir = tempdir().unwrap();
        std::thread::scope(|scope| {
            let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
            let bulk_gate = gate.clone();
            let root = dir.path();
            let bulk = scope.spawn(move || {
                bulk_gate.wait();
                ingest_batch(root, requests(25)).unwrap()
            });
            let single_gate = gate.clone();
            let single = scope.spawn(move || {
                single_gate.wait();
                (0..5)
                    .map(|index| {
                        let mut capture = capture();
                        capture.capture.external_capture_id = format!("ordinary-{index}");
                        ingest(root, &capture, draft()).unwrap()
                    })
                    .collect::<Vec<_>>()
            });
            gate.wait();
            while !bulk.is_finished() || !single.is_finished() {
                with_consistent_snapshot(root, |root| {
                    let rows = list_items(root)?;
                    assert!(rows.len() <= 30);
                    for (item, revision) in rows {
                        assert_eq!(item.current_revision, revision.revision_id);
                    }
                    Ok(())
                })
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let receipts: Vec<_> = bulk
                .join()
                .unwrap()
                .into_iter()
                .chain(single.join().unwrap())
                .collect();
            let rows = with_consistent_snapshot(root, |root| list_items(root)).unwrap();
            assert_eq!(rows.len(), 30);
            assert_eq!(
                rows.into_iter()
                    .map(|(item, _)| item.item_id)
                    .collect::<BTreeSet<_>>(),
                receipts
                    .into_iter()
                    .flat_map(|receipt| receipt.item_ids)
                    .collect()
            );
            assert!(ingest_batch(root, requests(25))
                .unwrap()
                .iter()
                .all(|receipt| receipt.idempotent_replay));
        });
    }

    #[test]
    fn batch_limit_is_500_and_preflight_errors_do_not_write() {
        let dir = tempdir().unwrap();
        assert!(ingest_batch(dir.path(), requests(501)).is_err());
        assert!(list_items(dir.path()).unwrap().is_empty());
        let mut bad = requests(2);
        bad[1].0.source.name = "conflicting source".into();
        assert!(ingest_batch(dir.path(), bad).is_err());
        assert!(list_items(dir.path()).unwrap().is_empty());
        // Exercise the inclusive public request boundary without 500 disk copies.
        let input = requests(1).pop().unwrap();
        let receipts = ingest_batch(dir.path(), vec![input; 500]).unwrap();
        assert_eq!(receipts.len(), 500);
        assert_eq!(list_items(dir.path()).unwrap().len(), 1);
    }
}
