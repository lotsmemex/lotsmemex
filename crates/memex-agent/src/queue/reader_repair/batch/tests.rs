use super::*;
use crate::queue::reader_authority::local_only;

fn setup() -> (tempfile::TempDir, Queue, PathBuf, Request, String) {
    let (tmp, q, library, req) = super::super::tests::setup();
    let p = preview(
        &q,
        &library,
        Authority::LocalOwner,
        &req.item_id,
        &req.revision_id,
        Sensitivity::Public,
        local_only,
    )
    .unwrap();
    assert_eq!(p["state"], "preparing");
    assert!(p["quote"].is_null());
    assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
    let id = p["batch_id"].as_str().unwrap().to_owned();
    prepare_one(&q, &library, |_, _| Ok(7), local_only).unwrap();
    let ready = preview_status(
        &q,
        &library,
        "local_owner",
        &req.item_id,
        &id,
        Sensitivity::Public,
        local_only,
    )
    .unwrap();
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["quote"]["chapters"].as_array().unwrap().len(), 1);
    assert_eq!(ready["quote"]["budget"]["max_requests"], 2);
    assert_eq!(ready["quote"]["budget"]["max_total_input_bytes"], 16384);
    (tmp, q, library, req, id)
}

fn start(q: &mut Queue, library: &Path, req: &Request, id: &str) -> Result<Value> {
    start_with(
        q,
        library,
        "local_owner",
        &req.item_id,
        id,
        &"r".repeat(128),
        true,
        true,
        Sensitivity::Public,
        |_, _| Ok(7),
        local_only,
    )
}

fn observed(q: &Queue, library: &Path, req: &Request, id: &str) -> Value {
    status(
        q,
        library,
        "local_owner",
        &req.item_id,
        id,
        0,
        100,
        Sensitivity::Public,
        local_only,
    )
    .unwrap()
}

const ANSWER: &str = r#"{"groups":[{"o":[1],"t":[1],"relation":"matched"}]}"#;

// Exercise Store's formatting transaction, not the matching engine's ability to
// produce edits. Partition the actual resulting chapter bytes on both sides.
fn whitespace_plan(library: &Path, mut plan: Plan) -> Plan {
    use reader::paragraphs::{blocks, Pair, Relation};
    let chapter = &plan.identity.original.chapter;
    let read = |side| {
        reader::reader_chunk(
            library,
            &plan.identity.item_id,
            &plan.identity.revision_id,
            side,
            chapter,
            0,
            4096,
            Sensitivity::Public,
        )
        .unwrap()
        .markdown
    };
    let mut original = read("original");
    let translation = read("translation");
    assert_eq!(original.len(), plan.identity.original.bytes);
    assert_eq!(
        translation.len(),
        plan.identity.translation.as_ref().unwrap().bytes
    );
    let end = original.len();
    let start = original.trim_end().len();
    assert!(start < end);
    let replacement = if end - start == 1 { "\n\n" } else { "\n" };
    plan.original_edits = vec![reader::repair::Edit {
        range: [start, end],
        replacement: replacement.into(),
    }];
    original.replace_range(start..end, replacement);
    plan.pairs = blocks(&original)
        .into_iter()
        .map(|(range, kind)| Pair {
            o: Some(range),
            t: None,
            kind,
            relation: Relation::Unresolved,
        })
        .chain(blocks(&translation).into_iter().map(|(range, kind)| Pair {
            o: None,
            t: Some(range),
            kind,
            relation: Relation::Unresolved,
        }))
        .collect();
    plan
}

#[test]
fn own_content_revision_refreshes_inherited_cas_but_external_revision_dispatches_nothing() {
    for external_drift in [false, true] {
        let (_tmp, mut q, library, req) =
            super::super::tests::setup_with_book(super::super::tests::book::paired_chapters);
        // Give the untouched second chapter a real immutable artifact so this
        // tests inherited effective CAS, not merely missing == missing.
        let appendix = reader::repair::plan(
            &library,
            &req.item_id,
            &req.revision_id,
            "appendix",
            Sensitivity::Public,
        )
        .unwrap();
        let initial = reader::repair::apply(&library, appendix, Sensitivity::Public).unwrap();
        assert_eq!(initial.revision_id, req.revision_id);
        let p = preview(
            &q,
            &library,
            Authority::LocalOwner,
            &req.item_id,
            &req.revision_id,
            Sensitivity::Public,
            local_only,
        )
        .unwrap();
        let id = p["batch_id"].as_str().unwrap().to_owned();
        prepare_one(&q, &library, |_, _| Ok(7), local_only).unwrap();
        start(&mut q, &library, &req, &id).unwrap();
        let mut b = load(&q.conn, &id).unwrap();
        assert_eq!(b.chapters.len(), 2);
        assert_eq!(b.chapters[1].plan.identity.original.chapter, "appendix");
        let frozen = b.chapters[1].clone();
        assert!(frozen.artifact.is_some());
        assert_eq!(
            frozen.plan.expected_alignment_version,
            initial.alignment_version
        );

        let plan = whitespace_plan(&library, current_plan(&b, 0, Sensitivity::Public).unwrap());
        let mut child = new_child(&b, 0, plan);
        let receipt =
            reader::repair::apply(&library, child.plan.clone(), Sensitivity::Public).unwrap();
        assert_ne!(receipt.revision_id, req.revision_id);
        assert_eq!(
            reader::repair::committed(&library, &child.plan, Sensitivity::Public)
                .unwrap()
                .unwrap()
                .revision_id,
            receipt.revision_id
        );
        // Explicitly synthesize the settled queue seam from a REAL Store receipt.
        // No claim that the legacy chapter matcher generated this whitespace edit.
        child.job.state = "succeeded".into();
        child.job.stage = "done".into();
        child.job.result_revision_id = Some(receipt.revision_id.clone());
        child.job.alignment_version = Some(receipt.alignment_version.clone());
        child.job.alignment_status = Some(matching::status(&child.plan.pairs).into());
        b.state = "running".into();
        save(&q.conn, &b).unwrap();
        q.conn.execute("INSERT INTO reader_repair_batch_children(job_id,batch_id,ordinal,state,payload) VALUES(?1,?2,0,'succeeded',?3)",
            params![child.job.job_id,id,serde_json::to_string(&child).unwrap()]).unwrap();
        finish(&q, &id).unwrap();
        let mut q = Queue::open(q.database_path()).unwrap();
        let b = load(&q.conn, &id).unwrap();
        assert_eq!(b.current_revision, receipt.revision_id);
        assert_eq!(b.state, "queued");
        assert!(reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
        let fresh = current_plan(&b, 1, Sensitivity::Public).unwrap();
        assert_eq!(fresh.identity.revision_id, receipt.revision_id);
        assert_eq!(
            fresh.identity.original.sha256,
            frozen.plan.identity.original.sha256
        );
        assert_ne!(
            fresh.identity.original.chapter_start,
            frozen.plan.identity.original.chapter_start
        );
        assert_ne!(
            fresh.expected_alignment_version,
            frozen.plan.expected_alignment_version
        );
        assert_eq!(
            reader::paragraphs::artifact_binding(
                &library,
                &req.item_id,
                &b.current_revision,
                "appendix",
                Sensitivity::Public,
            )
            .unwrap(),
            frozen.artifact
        );
        let mut stale_cas = fresh.clone();
        stale_cas.expected_alignment_version = frozen.plan.expected_alignment_version.clone();
        assert_eq!(
            reader::repair::apply(&library, stale_cas, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "stale_chapter_alignment"
        );

        if external_drift {
            let plan = reader::repair::plan(
                &library,
                &req.item_id,
                &b.current_revision,
                "chapter-1",
                Sensitivity::Public,
            )
            .unwrap();
            let external = reader::repair::apply(
                &library,
                whitespace_plan(&library, plan),
                Sensitivity::Public,
            )
            .unwrap();
            assert_ne!(external.revision_id, b.current_revision);
            assert!(current_plan(&b, 1, Sensitivity::Public).is_err());
        }
        let sent = std::cell::Cell::new(0);
        assert!(step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            local_only,
            |_, _, _, _, dispatch| {
                dispatch()?;
                sent.set(sent.get() + 1);
                Ok((ANSWER.into(), DispatchUsage::default()))
            },
        )
        .unwrap());
        let end = observed(&q, &library, &req, &id);
        let jobs = children(&q.conn, &id).unwrap();
        assert_eq!(sent.get(), usize::from(!external_drift));
        assert_eq!(end["batch"]["usage"]["requests_dispatched"], sent.get());
        assert_eq!(end["batch"]["done"], if external_drift { 1 } else { 2 });
        assert_eq!(
            end["batch"]["state"],
            if external_drift {
                "failed"
            } else {
                "succeeded"
            }
        );
        assert_eq!(jobs.len(), if external_drift { 1 } else { 2 });
        if external_drift {
            assert_eq!(end["batch"]["stop_reason"], "stale_item_revision");
        } else {
            assert_eq!(jobs[1].job.request.revision_id, receipt.revision_id);
            assert_eq!(
                jobs[1].job.request.expected_alignment_version,
                fresh.expected_alignment_version
            );
            assert_eq!(
                jobs[1].job.result_revision_id.as_deref(),
                Some(receipt.revision_id.as_str())
            );
        }
        assert_eq!(
            load(&q.conn, &id).unwrap().current_revision,
            receipt.revision_id
        );
        assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
    }
}

#[test]
fn consent_replay_reservation_and_partial_completion_are_durable() {
    let (_tmp, mut q, library, req, id) = setup();
    assert!(start_with(
        &mut q,
        &library,
        "local_owner",
        &req.item_id,
        &id,
        "a",
        true,
        false,
        Sensitivity::Public,
        |_, _| Ok(7),
        local_only
    )
    .is_err());
    start(&mut q, &library, &req, &id).unwrap();
    assert!(reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
    assert_eq!(
        super::super::request(&mut q, &library, req.clone(), Sensitivity::Public, true)
            .unwrap_err()
            .to_string(),
        "reader_job_in_progress"
    );
    assert_eq!(
        start_with(
            &mut q,
            &library,
            "local_owner",
            &req.item_id,
            &id,
            "other",
            true,
            true,
            Sensitivity::Public,
            |_, _| Ok(7),
            local_only
        )
        .unwrap_err()
        .to_string(),
        "reader_batch_quote_consumed"
    );
    let mut expired = load(&q.conn, &id).unwrap();
    expired.expires_at = Some(0);
    save(&q.conn, &expired).unwrap();
    assert_eq!(
        start(&mut q, &library, &req, &id).unwrap()["batch"]["state"],
        "queued"
    );
    assert!(
        !super::super::step_with(&mut q, &library, None, |_, _, _, _, _| panic!(
            "legacy dispatch"
        ))
        .unwrap()
    );
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, dispatch| {
            dispatch()?;
            Ok((
                ANSWER.into(),
                DispatchUsage {
                    input_tokens: Some(11),
                    output_tokens: Some(6),
                    total_tokens: Some(17),
                },
            ))
        },
    )
    .unwrap();
    let v = observed(&q, &library, &req, &id);
    assert_eq!(v["batch"]["state"], "succeeded");
    assert_eq!(v["batch"]["done"], 1);
    assert_eq!(v["chapters"][0]["alignment_status"], "partial");
    assert_eq!(v["batch"]["usage"]["reported_total_tokens"], 17);
    assert!(
        children(&q.conn, &id).unwrap()[0]
            .job
            .request
            .request_id
            .len()
            <= 128
    );
    let reopened = Queue::open(q.database_path()).unwrap();
    assert_eq!(observed(&reopened, &library, &req, &id), v);
    assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
}

#[test]
fn stop_serializes_with_admission_and_keeps_already_sent_cost() {
    let (_tmp, mut q, library, req, id) = setup();
    start(&mut q, &library, &req, &id).unwrap();
    let database = q.database_path().to_owned();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, dispatch| {
            dispatch()?;
            let mut other = Queue::open(&database)?;
            let response = stop(
                &mut other,
                &library,
                "local_owner",
                &req.item_id,
                &id,
                Sensitivity::Public,
                local_only,
            )?;
            assert_eq!(response["batch"]["state"], "stopping");
            Ok((
                ANSWER.into(),
                DispatchUsage {
                    total_tokens: Some(19),
                    ..DispatchUsage::default()
                },
            ))
        },
    )
    .unwrap();
    let v = observed(&q, &library, &req, &id);
    assert_eq!(v["batch"]["state"], "stopped");
    assert_eq!(v["batch"]["done"], 1);
    assert_eq!(v["batch"]["usage"]["reported_total_tokens"], 19);
    assert!(!step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!("after stop")
    )
    .unwrap());

    let (_tmp, mut q, library, req, id) = setup();
    start(&mut q, &library, &req, &id).unwrap();
    assert_eq!(
        stop(
            &mut q,
            &library,
            "local_owner",
            &req.item_id,
            &id,
            Sensitivity::Public,
            local_only
        )
        .unwrap()["batch"]["state"],
        "stopped"
    );
    assert!(!step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!("stopped before admission")
    )
    .unwrap());
}

#[test]
fn restart_never_dispatches_pending_or_uncertain_children() {
    let (_tmp, mut q, library, req, id) = setup();
    start(&mut q, &library, &req, &id).unwrap();
    recover(&q, &library, local_only).unwrap();
    assert_eq!(
        observed(&q, &library, &req, &id)["batch"]["state"],
        "stopped"
    );
    assert_eq!(
        observed(&q, &library, &req, &id)["batch"]["usage"]["requests_dispatched"],
        0
    );

    let (_tmp, mut q, library, req, id) = setup();
    start(&mut q, &library, &req, &id).unwrap();
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            local_only,
            |_, _, _, _, dispatch| {
                dispatch()?;
                panic!("synthetic crash after intent")
            },
        );
    }));
    assert!(crash.is_err());
    let mut q = Queue::open(q.database_path()).unwrap();
    recover(&q, &library, local_only).unwrap();
    let v = observed(&q, &library, &req, &id);
    assert_eq!(v["batch"]["state"], "outcome_unknown");
    assert_eq!(v["batch"]["usage"]["unknown_usage_requests"], 1);
    assert!(preview(
        &q,
        &library,
        Authority::LocalOwner,
        &req.item_id,
        &req.revision_id,
        Sensitivity::Public,
        local_only
    )
    .is_err());
    assert!(
        !super::super::step_with(&mut q, &library, None, |_, _, _, _, _| panic!(
            "legacy after restart"
        ))
        .unwrap()
    );
    assert!(!step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!("resend")
    )
    .unwrap());
}

#[test]
fn live_authority_and_credential_drift_gate_dispatch_and_commit_independently() {
    for deny in [Phase::Admission, Phase::Dispatch, Phase::Commit] {
        let (_tmp, mut q, library, req, id) = setup();
        start(&mut q, &library, &req, &id).unwrap();
        let calls = std::cell::Cell::new(0);
        step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            |check| {
                assert_eq!(check.item_id, req.item_id);
                assert_eq!(check.library, library);
                anyhow::ensure!(check.phase != deny, "permission_denied");
                Ok(check.ceiling)
            },
            |_, _, _, _, dispatch| {
                dispatch()?;
                calls.set(calls.get() + 1);
                Ok((
                    ANSWER.into(),
                    DispatchUsage {
                        total_tokens: Some(23),
                        ..DispatchUsage::default()
                    },
                ))
            },
        )
        .unwrap();
        assert_eq!(calls.get(), usize::from(deny == Phase::Commit));
        assert_eq!(
            reader::paragraphs::read(
                &library,
                &req.item_id,
                &req.revision_id,
                &req.chapter,
                0,
                1,
                "any",
                Sensitivity::Public
            )
            .unwrap()
            .version,
            "missing"
        );
        assert_eq!(
            observed(&q, &library, &req, &id)["batch"]["usage"]["requests_dispatched"],
            calls.get()
        );
    }
    let (_tmp, mut q, library, req, id) = setup();
    start(&mut q, &library, &req, &id).unwrap();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(8),
        local_only,
        |_, _, _, _, _| panic!("credential drift"),
    )
    .unwrap();
    assert_eq!(
        observed(&q, &library, &req, &id)["batch"]["stop_reason"],
        "reader_batch_quote_stale"
    );
}

#[test]
fn committed_receipt_wins_after_restart_without_new_authority_or_model_work() {
    for installed in [false, true] {
        let (_tmp, mut q, library, req, id) = setup();
        start(&mut q, &library, &req, &id).unwrap();
        let mut b = load(&q.conn, &id).unwrap();
        let mut child = new_child(&b, 0, current_plan(&b, 0, Sensitivity::Public).unwrap());
        child.job.stage = "committing".into();
        b.state = "running".into();
        save(&q.conn, &b).unwrap();
        q.conn.execute("INSERT INTO reader_repair_batch_children(job_id,batch_id,ordinal,state,payload) VALUES(?1,?2,0,'running',?3)",
            params![child.job.job_id,id,serde_json::to_string(&child).unwrap()]).unwrap();
        assert!(
            reader::repair::committed(&library, &child.plan, Sensitivity::Public)
                .unwrap()
                .is_none()
        );
        if installed {
            reader::repair::apply(&library, child.plan.clone(), Sensitivity::Public).unwrap();
        }
        let before = reader::paragraphs::read(
            &library,
            &req.item_id,
            &req.revision_id,
            &req.chapter,
            0,
            1,
            "any",
            Sensitivity::Public,
        )
        .unwrap()
        .version;
        let q = Queue::open(q.database_path()).unwrap();
        recover(&q, &library, |_| anyhow::bail!("permission_denied")).unwrap();
        let v = observed(&q, &library, &req, &id);
        assert_eq!(v["batch"]["done"], usize::from(installed));
        assert_eq!(v["batch"]["state"], "stopped");
        assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
        assert_eq!(v["batch"]["usage"]["requests_dispatched"], 0);
        assert_eq!(
            reader::paragraphs::read(
                &library,
                &req.item_id,
                &req.revision_id,
                &req.chapter,
                0,
                1,
                "any",
                Sensitivity::Public
            )
            .unwrap()
            .version,
            before
        );
    }
}

#[test]
fn remote_origin_epoch_survives_restart_without_owner_fallback() {
    let (_tmp, mut q, library, req) = super::super::tests::setup();
    let origin = Authority::Remote {
        principal: "a".repeat(64),
        session_id: "fixture-session".into(),
        grant_epoch: 4,
    };
    let allow = |check: Check<'_>| {
        assert_eq!(check.authority, &origin);
        assert_eq!(check.required_scope, "edit");
        Ok(check.ceiling)
    };
    let p = preview(
        &q,
        &library,
        origin.clone(),
        &req.item_id,
        &req.revision_id,
        Sensitivity::Public,
        &allow,
    )
    .unwrap();
    let id = p["batch_id"].as_str().unwrap();
    prepare_one(&q, &library, |_, _| Ok(7), &allow).unwrap();
    start_with(
        &mut q,
        &library,
        origin.principal(),
        &req.item_id,
        id,
        "remote-request",
        true,
        true,
        Sensitivity::Public,
        |_, _| Ok(7),
        &allow,
    )
    .unwrap();
    let mut q = Queue::open(q.database_path()).unwrap();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!("remote became owner"),
    )
    .unwrap();
    assert!(status(
        &q,
        &library,
        origin.principal(),
        &req.item_id,
        id,
        0,
        1,
        Sensitivity::Public,
        local_only
    )
    .is_err());
    let state = status(
        &q,
        &library,
        origin.principal(),
        &req.item_id,
        id,
        0,
        1,
        Sensitivity::Public,
        |c| {
            assert_eq!(c.authority, &origin);
            assert_eq!(c.required_scope, "read");
            Ok(c.ceiling)
        },
    )
    .unwrap();
    assert_eq!(state["batch"]["state"], "failed");
    assert_eq!(state["batch"]["usage"]["requests_dispatched"], 0);
}

#[test]
fn ordinary_queue_open_does_not_install_unapproved_feature_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    let count:i64=q.conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('reader_repair_batches','reader_repair_batch_children','reader_translation_map_jobs','reader_translation_map_quotes')",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn recorded_response_is_not_an_uncertain_call_and_recovery_never_finishes_new_commits() {
    for stage in ["matching", "committing"] {
        let (_tmp, mut q, library, req, id) = setup();
        start(&mut q, &library, &req, &id).unwrap();
        let mut b = load(&q.conn, &id).unwrap();
        let mut child = new_child(&b, 0, current_plan(&b, 0, Sensitivity::Public).unwrap());
        child.job.stage = stage.into();
        child.job.usage.dispatched();
        // The response arrived without token counts. Unknown cost is not an
        // unknown provider result: this marker is persisted independently.
        child.job.usage.observed(DispatchUsage::default());
        child.dispatch_in_flight = false;
        b.state = "running".into();
        save(&q.conn, &b).unwrap();
        q.conn.execute("INSERT INTO reader_repair_batch_children(job_id,batch_id,ordinal,state,payload) VALUES(?1,?2,0,'running',?3)",
            params![child.job.job_id,id,serde_json::to_string(&child).unwrap()]).unwrap();
        let settings = memex_models::read(&q.conn).unwrap();
        memex_models::replace(&mut q.conn, settings.revision, settings.configuration).unwrap();
        let mut q = Queue::open(q.database_path()).unwrap();
        recover(&q, &library, |_| {
            panic!("recovery must not attempt new publication")
        })
        .unwrap();
        let v = observed(&q, &library, &req, &id);
        assert_eq!(v["batch"]["state"], "stopped");
        assert_eq!(v["batch"]["usage"]["requests_dispatched"], 1);
        assert_eq!(v["batch"]["usage"]["unknown_usage_requests"], 1);
        assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
        assert!(
            reader::repair::committed(&library, &child.plan, Sensitivity::Public)
                .unwrap()
                .is_none()
        );
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| Ok(8),
            local_only,
            |_, _, _, _, _| panic!("recovery resend")
        )
        .unwrap());
    }
}

#[test]
fn multi_chapter_progress_retains_reservation_and_rejects_external_alignment_drift() {
    for drift in [false, true] {
        let (_tmp, mut q, library, req) =
            super::super::tests::setup_with_book(super::super::tests::book::paired_chapters);
        let p = preview(
            &q,
            &library,
            Authority::LocalOwner,
            &req.item_id,
            &req.revision_id,
            Sensitivity::Public,
            local_only,
        )
        .unwrap();
        let id = p["batch_id"].as_str().unwrap().to_owned();
        prepare_one(&q, &library, |_, _| Ok(7), local_only).unwrap();
        let ready = preview_status(
            &q,
            &library,
            "local_owner",
            &req.item_id,
            &id,
            Sensitivity::Public,
            local_only,
        )
        .unwrap();
        assert_eq!(ready["quote"]["chapters"].as_array().unwrap().len(), 2);
        assert_eq!(ready["quote"]["budget"]["max_requests"], 4);
        start(&mut q, &library, &req, &id).unwrap();
        step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            local_only,
            |_, _, _, _, dispatch| {
                dispatch()?;
                Ok((ANSWER.into(), DispatchUsage::default()))
            },
        )
        .unwrap();
        let mid = observed(&q, &library, &req, &id);
        assert_eq!(mid["batch"]["state"], "queued");
        assert_eq!(mid["batch"]["done"], 1);
        assert!(reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
        let first = status(
            &q,
            &library,
            "local_owner",
            &req.item_id,
            &id,
            0,
            1,
            Sensitivity::Public,
            local_only,
        )
        .unwrap();
        let second = status(
            &q,
            &library,
            "local_owner",
            &req.item_id,
            &id,
            1,
            1,
            Sensitivity::Public,
            local_only,
        )
        .unwrap();
        assert_eq!(first["next_offset"], 1);
        assert_eq!(second["chapters"][0]["chapter"], "appendix");
        assert_eq!(second["chapters"][0]["state"], "pending");
        if drift {
            let b = load(&q.conn, &id).unwrap();
            let plan = reader::repair::plan(
                &library,
                &req.item_id,
                &b.current_revision,
                "appendix",
                Sensitivity::Public,
            )
            .unwrap();
            reader::repair::apply(&library, plan, Sensitivity::Public).unwrap();
        }
        let sent = std::cell::Cell::new(0);
        step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            local_only,
            |_, _, _, _, dispatch| {
                dispatch()?;
                sent.set(sent.get() + 1);
                Ok((ANSWER.into(), DispatchUsage::default()))
            },
        )
        .unwrap();
        let end = observed(&q, &library, &req, &id);
        assert_eq!(sent.get(), usize::from(!drift));
        assert_eq!(
            end["batch"]["state"],
            if drift { "failed" } else { "succeeded" }
        );
        assert_eq!(end["batch"]["done"], if drift { 1 } else { 2 });
        assert!(!reader_item_busy(&q.conn, &library, &req.item_id).unwrap());
    }
}
