use super::*;
use memex_store::{edit_groups, GroupEditOp};
#[path = "../../../examples/fixtures/reader_book.rs"]
mod book;

fn group(library: &Path, item: &str) -> memex_store::GroupEditReceipt {
    let root = edit_groups(
        library,
        "root",
        GroupEditOp::CreateRoot {
            name: "Synthetic directory".into(),
            sensitivity: Sensitivity::Public,
        },
        Sensitivity::Public,
    )
    .unwrap();
    edit_groups(
        library,
        "members",
        GroupEditOp::AddMembers {
            group_id: root.group_id,
            expected_version: root.version,
            item_ids: vec![item.into()],
        },
        Sensitivity::Public,
    )
    .unwrap()
}

fn directory(group_id: String) -> Selection {
    Selection::Group {
        group_id,
        include_descendants: true,
    }
}

#[test]
fn directory_admission_and_poll_do_not_read_member_bodies_and_survive_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = book::seed(&library).unwrap();
    let root = group(&library, &doc.item_id);
    let database = tmp.path().join("queue.db");
    let q = Queue::open(&database).unwrap();
    // An unreadable member proves the admission and polling path does not
    // expand members, without a timing assertion or a real large library.
    let head = library.join("items").join(&doc.item_id).join("item.json");
    let bytes = std::fs::read(&head).unwrap();
    std::fs::write(&head, b"invalid synthetic head").unwrap();
    let p = preview(
        &q,
        &library,
        "local_owner",
        directory(root.group_id),
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(p.state, "preparing");
    assert_eq!(
        preview_status(
            &q,
            &library,
            "local_owner",
            &p.preview_id,
            Sensitivity::Public
        )
        .unwrap()
        .state,
        "preparing"
    );
    assert_eq!(
        targets(
            &q,
            &library,
            "local_owner",
            &p.preview_id,
            0,
            10,
            Sensitivity::Public
        )
        .unwrap()["total"],
        0
    );
    assert!(preview_status(
        &q,
        &library,
        &"a".repeat(64),
        &p.preview_id,
        Sensitivity::Public
    )
    .is_err());
    drop(q);
    std::fs::write(head, bytes).unwrap();
    let q = Queue::open(&database).unwrap();
    assert!(prepare_one(&q, &library, |_, _| panic!(
        "zero eligible must not resolve credentials"
    ))
    .unwrap());
    let p = preview_status(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(p.state, "ready");
    let quote = p.quote.unwrap();
    assert_eq!(quote.counts["targets"], 1);
    assert_eq!(quote.counts["skipped_by_reason"]["chapter_required"], 1);
    assert_eq!(quote.budget["max_requests"], 0);
    assert!(quote.route.is_none());
}

#[test]
fn persisted_expansion_excludes_arrivals_after_snapshot_on_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let first = book::seed(&library).unwrap();
    let root = group(&library, &first.item_id);
    let database = tmp.path().join("queue.db");
    let q = Queue::open(&database).unwrap();
    let p = preview(
        &q,
        &library,
        "local_owner",
        directory(root.group_id.clone()),
        Sensitivity::Public,
    )
    .unwrap();
    let mut frozen = load_preview(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert!(frozen.expansion_pending);
    expand(&q, &mut frozen).unwrap();
    assert_eq!(frozen.targets.len(), 1);
    drop(q);
    let second = book::seed_named(&library, "later-arrival").unwrap();
    edit_groups(
        &library,
        "later",
        GroupEditOp::AddMembers {
            group_id: root.group_id,
            expected_version: root.version,
            item_ids: vec![second.item_id],
        },
        Sensitivity::Public,
    )
    .unwrap();
    let q = Queue::open(&database).unwrap();
    prepare_one(&q, &library, |_, _| panic!("no credential lookup")).unwrap();
    let rows = targets(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        0,
        100,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(rows["total"], 1);
    assert_eq!(rows["targets"][0]["item_id"], first.item_id);
    assert_eq!(
        preview_status(
            &q,
            &library,
            "local_owner",
            &p.preview_id,
            Sensitivity::Public
        )
        .unwrap()
        .state,
        "ready"
    );
}

#[test]
fn supported_chapter_without_saved_low_route_fails_not_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = book::seed(&library).unwrap();
    let q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    let p = preview(
        &q,
        &library,
        "local_owner",
        Selection::Item {
            item_id: doc.item_id,
            revision_id: doc.revision_id,
            target: store::Target::Chapter {
                side: "original".into(),
                chapter: "chapter-1".into(),
            },
        },
        Sensitivity::Public,
    )
    .unwrap();
    prepare_one(&q, &library, |_, _| {
        panic!("absent route must not resolve credentials")
    })
    .unwrap();
    let p = preview_status(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(p.state, "failed");
    assert_eq!(p.error_code.as_deref(), Some("no_low_dial_provider"));
    assert!(p.quote.is_none());
}

#[test]
fn pending_directory_rechecks_ceiling() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    book::seed(&library).unwrap();
    let root = edit_groups(
        &library,
        "private",
        GroupEditOp::CreateRoot {
            name: "Synthetic private directory".into(),
            sensitivity: Sensitivity::Private,
        },
        Sensitivity::Private,
    )
    .unwrap();
    let q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    let p = preview(
        &q,
        &library,
        "local_owner",
        directory(root.group_id),
        Sensitivity::Private,
    )
    .unwrap();
    assert!(preview_status(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        Sensitivity::Public
    )
    .is_err());
    assert!(targets(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        0,
        10,
        Sensitivity::Public
    )
    .is_err());
    prepare_one(&q, &library, |_, _| {
        panic!("empty directory must not resolve credentials")
    })
    .unwrap();
    let p = preview_status(
        &q,
        &library,
        "local_owner",
        &p.preview_id,
        Sensitivity::Private,
    )
    .unwrap();
    assert_eq!(p.quote.unwrap().counts["eligible"], 0);
}
