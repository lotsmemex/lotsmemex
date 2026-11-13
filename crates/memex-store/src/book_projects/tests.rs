use super::*;
use crate::tests::{capture, draft};

fn fixture() -> (ImportPlan, VerifiedCapture, ItemDraft) {
    let article = capture();
    let text = draft();
    let mut container = article.clone();
    container.capture.external_capture_id = "container".into();
    (ImportPlan {
        source_identity: "synthetic-six-volumes".into(),
        sources_sha256: format!("sha256:{}", "a".repeat(64)),
        sensitivity: Sensitivity::Private,
        volume_count: 6,
        container: PlannedItem::from_input(&container, &text).unwrap(),
        articles: BTreeMap::from([("source-position-1".into(), PlannedItem::from_input(&article, &text).unwrap())]),
    }, article, text)
}

#[test]
fn durable_reservation_reuses_ids_across_requests_without_creating_items() {
    let dir = tempfile::tempdir().unwrap();
    let (plan, _, _) = fixture();
    let first = reserve(dir.path(), "request-a", plan.clone(), Sensitivity::Private).unwrap();
    assert_eq!(first.volume_ids.len(), 6);
    assert!(!dir.path().join("items").join(&first.container_item_id).exists());
    assert_eq!(reserve(dir.path(), "request-a", plan.clone(), Sensitivity::Private).unwrap(), first);
    assert_eq!(reserve(dir.path(), "request-b", plan.clone(), Sensitivity::Private).unwrap(), first);
    let before = fs::read(dir.path().join(STATE_PATH)).unwrap();
    let mut changed = plan.clone();
    changed.volume_count = 5;
    assert!(reserve(dir.path(), "request-a", changed.clone(), Sensitivity::Private).is_err());
    assert!(reserve(dir.path(), "request-c", changed, Sensitivity::Private).is_err());
    assert!(reserve(dir.path(), "request-c", plan, Sensitivity::Public).is_err());
    assert_eq!(fs::read(dir.path().join(STATE_PATH)).unwrap(), before);
}

#[test]
fn committed_article_replays_but_trash_and_changed_input_do_not_resurrect() {
    let dir = tempfile::tempdir().unwrap();
    let (plan, capture, draft) = fixture();
    let reserved = reserve(dir.path(), "request", plan.clone(), Sensitivity::Private).unwrap();
    let id = &reserved.articles["source-position-1"].item_id;
    let receipt = ingest_part(dir.path(), &reserved.project_id, Some("source-position-1"),
        &capture, draft.clone(), Sensitivity::Private).unwrap();
    assert_eq!(receipt.item_ids, [id.as_str()]);
    assert!(!receipt.idempotent_replay);
    let replay = ingest_part(dir.path(), &reserved.project_id, Some("source-position-1"),
        &capture, draft.clone(), Sensitivity::Private).unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(replay.revision_ids, receipt.revision_ids);
    let mut changed = draft.clone();
    changed.content_markdown.push_str("different");
    assert!(ingest_part(dir.path(), &reserved.project_id, Some("source-position-1"),
        &capture, changed, Sensitivity::Private).is_err());
    trash_item(dir.path(), id, &receipt.revision_ids[0], "test", None, None).unwrap();
    assert!(ingest_part(dir.path(), &reserved.project_id, Some("source-position-1"),
        &capture, draft, Sensitivity::Private).is_err());
    assert!(reserve(dir.path(), "another-request", plan, Sensitivity::Private).is_err());
    assert!(read_item_at_sensitivity(dir.path(), id, Sensitivity::Private).unwrap().is_none());
}

#[test]
fn ingest_interruption_recovers_the_reserved_identity() {
    for step in 1..=8 {
        let dir = tempfile::tempdir().unwrap();
        let (plan, capture, draft) = fixture();
        let reservation = reserve(dir.path(), "initial", plan.clone(), Sensitivity::Private).unwrap();
        crate::FAIL_APPLY_STEP.with(|value| value.set(Some(step)));
        let attempt = ingest_part(dir.path(), &reservation.project_id, Some("source-position-1"),
            &capture, draft.clone(), Sensitivity::Private);
        crate::FAIL_APPLY_STEP.with(|value| value.set(None));
        assert!(attempt.is_err(), "fault point {step} must be exercised");
        let resumed = reserve(dir.path(), "resumed", plan, Sensitivity::Private).unwrap();
        assert_eq!(resumed, reservation);
        let receipt = ingest_part(dir.path(), &resumed.project_id, Some("source-position-1"),
            &capture, draft, Sensitivity::Private).unwrap();
        assert_eq!(receipt.item_ids, [reservation.articles["source-position-1"].item_id.clone()]);
        assert_eq!(read_item_at_sensitivity(dir.path(), &receipt.item_ids[0], Sensitivity::Private)
            .unwrap().unwrap().1.content_markdown, "b");
    }
}
