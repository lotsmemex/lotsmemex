use super::*;
use jobs::{recover_with, status_with, stop_with};
#[path = "../../../examples/fixtures/reader_book.rs"]
mod book;

const OWNER: Authority = Authority::LocalOwner;

fn setup() -> (tempfile::TempDir, Queue, PathBuf, Selection) {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = book::seed(&library).unwrap();
    let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    memex_models::replace(
        &mut q.conn,
        0,
        memex_models::Configuration {
            providers: vec![memex_models::Provider {
                id: "fixture".into(),
                label: "Synthetic".into(),
                api_base: "https://model.example.invalid/v1/".into(),
            }],
            models: vec![memex_models::Model {
                id: "cheap".into(),
                label: "Synthetic".into(),
                provider_id: "fixture".into(),
                model_id: "synthetic".into(),
            }],
            selections: vec![],
        },
    )
    .unwrap();
    memex_models::low_dial::replace(
        &mut q.conn,
        0,
        Profile {
            revision: 1,
            model: "cheap".into(),
            reasoning_effort: None,
            max_input_bytes: 8192,
            max_completion_tokens: 1024,
        },
    )
    .unwrap();
    let source = reader::chapter_translation::source(
        &library,
        &doc.item_id,
        &doc.revision_id,
        "appendix",
        Sensitivity::Public,
    )
    .unwrap();
    let metadata = reader::chapter_translation::publish(
        &library,
        source.identity,
        reader::chapter_translation::Provenance {
            route: reader::chapter_translation::Route {
                provider_id: "fixture".into(),
                model_id: "synthetic".into(),
                reasoning_effort: None,
            },
            prompt_version: "synthetic-v1".into(),
            prompt_sha256: "a".repeat(64),
            plan_sha256: "b".repeat(64),
        },
        "# 合成附录\n\n合成旧AI译文。\n".into(),
        Sensitivity::Public,
    )
    .unwrap();
    (
        tmp,
        q,
        library,
        Selection {
            item_id: doc.item_id,
            revision_id: doc.revision_id,
            chapter: "appendix".into(),
            translation_version: metadata.version,
        },
    )
}
fn quote(q: &Queue, library: &Path, s: &Selection) -> Quote {
    preview_with(
        q,
        library,
        &OWNER,
        s.clone(),
        Sensitivity::Public,
        100,
        |_, _| Ok(7),
        local_only,
    )
    .unwrap()
    .quote
    .unwrap()
}
fn req(s: &Selection, quote: &Quote, id: &str) -> Request {
    Request {
        item_id: s.item_id.clone(),
        revision_id: s.revision_id.clone(),
        chapter: s.chapter.clone(),
        translation_version: s.translation_version.clone(),
        quote_id: quote.quote_id.clone(),
        request_id: id.into(),
        accept_cost: true,
        accept_model_processing: true,
        expected_map_version: "missing".into(),
    }
}
fn admit(q: &mut Queue, library: &Path, r: Request) -> Job {
    request_with(
        q,
        library,
        &OWNER,
        r,
        Sensitivity::Public,
        101,
        |_, _| Ok(7),
        local_only,
    )
    .unwrap()
}
fn get(q: &Queue, library: &Path, s: &Selection, job: &Job) -> Job {
    status(
        q,
        library,
        &OWNER,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public,
    )
    .unwrap()
}
fn answer() -> String {
    r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[1],"t":[1],"relation":"matched"}]}"#
        .into()
}

#[test]
fn confirmations_quote_replay_stop_and_old_executor_isolation() {
    let (_tmp, mut q, library, s) = setup();
    let quote = quote(&q, &library, &s);
    let mut r = req(&s, &quote, "one");
    r.accept_model_processing = false;
    assert!(request_with(
        &mut q,
        &library,
        &OWNER,
        r.clone(),
        Sensitivity::Public,
        101,
        |_, _| Ok(7),
        local_only
    )
    .is_err());
    r.accept_model_processing = true;
    let job = admit(&mut q, &library, r.clone());
    let replay = request_with(
        &mut q,
        &library,
        &OWNER,
        r,
        Sensitivity::Public,
        i64::MAX,
        |_, _| panic!("replay probes credentials"),
        local_only,
    )
    .unwrap();
    assert_eq!(replay.job_id, job.job_id);
    let stopped = stop(
        &mut q,
        &library,
        &OWNER,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(stopped.state, "stopped");
    assert_eq!(stopped.usage.requests_dispatched, 0);
    assert!(!step_with(
        &mut q,
        &library,
        |_, _| panic!(),
        local_only,
        |_, _, _, _, _| panic!()
    )
    .unwrap());
    assert!(request_with(
        &mut q,
        &library,
        &OWNER,
        req(&s, &quote, "two"),
        Sensitivity::Public,
        102,
        |_, _| Ok(7),
        local_only
    )
    .unwrap_err()
    .to_string()
    .contains("quote_consumed"));
    for table in ["reader_repair_jobs", "reader_translation_jobs"] {
        assert_eq!(
            q.conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
    }
    recover(&mut q, &library).unwrap();
    assert_eq!(get(&q, &library, &s, &job).state, "stopped");
}

#[test]
fn unknown_never_resends_or_bypasses_fresh_quote_and_preserves_usage() {
    let (_tmp, mut q, library, s) = setup();
    let first = quote(&q, &library, &s);
    let job = admit(&mut q, &library, req(&s, &first, "unknown"));
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, dispatch| {
            dispatch()?;
            Err(anyhow::anyhow!("SYNTHETIC provider secret prose"))
        },
    )
    .unwrap();
    recover(&mut q, &library).unwrap();
    assert!(!step_with(
        &mut q,
        &library,
        |_, _| panic!(),
        local_only,
        |_, _, _, _, _| panic!()
    )
    .unwrap());
    let current = get(&q, &library, &s, &job);
    assert_eq!(current.state, "outcome_unknown");
    assert_eq!(current.usage.requests_dispatched, 1);
    assert_eq!(current.usage.unknown_usage_requests, 1);
    assert!(!serde_json::to_string(&current).unwrap().contains("secret"));
    let fresh = quote(&q, &library, &s);
    assert!(request_with(
        &mut q,
        &library,
        &OWNER,
        req(&s, &fresh, "again"),
        Sensitivity::Public,
        102,
        |_, _| Ok(7),
        local_only
    )
    .is_err());
    assert_eq!(
        stop(
            &mut q,
            &library,
            &OWNER,
            &s.item_id,
            &job.job_id,
            Sensitivity::Public
        )
        .unwrap()
        .state,
        "outcome_unknown"
    );
}

#[test]
fn durable_commit_receipt_recovery_and_projection_unchanged() {
    let (_tmp, mut q, library, s) = setup();
    let before = source(&library, &s, Sensitivity::Public).unwrap();
    let quote = quote(&q, &library, &s);
    let job = admit(&mut q, &library, req(&s, &quote, "commit"));
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |settings, _, _, _, dispatch| {
            assert!(settings
                .configuration
                .target(Purpose::Translation)
                .is_none());
            assert!(settings
                .configuration
                .target(Purpose::CaptureQuality)
                .is_some());
            dispatch()?;
            Ok((
                answer(),
                DispatchUsage {
                    input_tokens: Some(20),
                    output_tokens: Some(10),
                    total_tokens: Some(30),
                },
            ))
        },
    )
    .unwrap();
    assert_eq!(get(&q, &library, &s, &job).stage, "committing");
    let pending: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_translation_map_jobs WHERE job_id=?1",
            [&job.job_id],
            |r| r.get(0),
        )
        .unwrap();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!("commit sent a model call"),
    )
    .unwrap();
    assert_eq!(get(&q, &library, &s, &job).state, "succeeded");
    q.conn
        .execute(
            "UPDATE reader_translation_map_jobs SET state='running',payload=?1 WHERE job_id=?2",
            params![pending, job.job_id],
        )
        .unwrap();
    recover(&mut q, &library).unwrap();
    let done = get(&q, &library, &s, &job);
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.usage.reported_total_tokens, 30);
    assert_eq!(done.usage.requests_dispatched, 1);
    let after = source(&library, &s, Sensitivity::Public).unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.source_markdown, after.source_markdown);
    assert_eq!(before.target_markdown, after.target_markdown);
    let reused = preview_with(
        &q,
        &library,
        &OWNER,
        s,
        Sensitivity::Public,
        i64::MAX,
        |_, _| panic!("reuse credential lookup"),
        local_only,
    )
    .unwrap();
    assert_eq!(reused.map_version, done.map_version);
    assert!(reused.quote.is_none());
}

#[test]
fn generation_revocation_and_pre_dispatch_failure_send_nothing() {
    for mode in [
        "generation",
        "settings",
        "profile",
        "grant",
        "not_sent",
        "malformed",
    ] {
        let (_tmp, mut q, library, s) = setup();
        let quote = quote(&q, &library, &s);
        let job = admit(&mut q, &library, req(&s, &quote, mode));
        if mode == "settings" {
            let settings = memex_models::read(&q.conn).unwrap();
            memex_models::replace(&mut q.conn, settings.revision, settings.configuration).unwrap();
        }
        if mode == "profile" {
            let profile = memex_models::low_dial::read(&q.conn).unwrap().unwrap();
            memex_models::low_dial::replace(&mut q.conn, profile.revision, profile).unwrap();
        }
        step_with(
            &mut q,
            &library,
            |_, _| Ok(if mode == "generation" { 8 } else { 7 }),
            |c| {
                if mode == "grant" {
                    anyhow::bail!("revoked")
                } else {
                    Ok(c.ceiling)
                }
            },
            |_, _, _, _, dispatch| {
                assert!(mode == "not_sent" || mode == "malformed");
                if mode == "not_sent" {
                    anyhow::bail!("credential unavailable before dispatch");
                }
                dispatch()?;
                Ok((
                    "invalid synthetic response".into(),
                    DispatchUsage::default(),
                ))
            },
        )
        .unwrap();
        recover(&mut q, &library).unwrap();
        let result = get(&q, &library, &s, &job);
        assert_eq!(result.state, "failed");
        assert_eq!(
            result.usage.requests_dispatched,
            u8::from(mode == "malformed")
        );
        assert!(result.map_version.is_none());
    }
}

#[test]
fn remote_origin_epoch_target_and_each_phase_remain_bound_after_restart() {
    let (tmp, mut q, library, s) = setup();
    let authority = Authority::Remote {
        principal: "a".repeat(64),
        session_id: "fixture-session".into(),
        grant_epoch: 7,
    };
    let phases = std::cell::RefCell::new(Vec::new());
    let denied = std::cell::Cell::new(None);
    let verifier = |check: Check<'_>| {
        assert_eq!(check.authority, &authority);
        assert_eq!(check.library, library.canonicalize().unwrap());
        assert_eq!(check.item_id, s.item_id);
        assert_eq!(check.ceiling, Sensitivity::Public);
        phases.borrow_mut().push(check.phase);
        anyhow::ensure!(denied.get() != Some(check.phase), "revoked");
        Ok(check.ceiling)
    };
    assert!(preview(&q, &library, &authority, s.clone(), Sensitivity::Public).is_err());
    let quote = preview_with(
        &q,
        &library,
        &authority,
        s.clone(),
        Sensitivity::Public,
        100,
        |_, _| Ok(7),
        &verifier,
    )
    .unwrap()
    .quote
    .unwrap();
    denied.set(Some(Phase::Admission));
    assert!(request_with(
        &mut q,
        &library,
        &authority,
        req(&s, &quote, "remote"),
        Sensitivity::Public,
        101,
        |_, _| Ok(7),
        &verifier
    )
    .is_err());
    denied.set(None);
    let job = request_with(
        &mut q,
        &library,
        &authority,
        req(&s, &quote, "remote"),
        Sensitivity::Public,
        101,
        |_, _| Ok(7),
        &verifier,
    )
    .unwrap();
    drop(q);
    let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        &verifier,
        |_, _, _, _, dispatch| {
            dispatch()?;
            Ok((answer(), DispatchUsage::default()))
        },
    )
    .unwrap();
    denied.set(Some(Phase::Commit));
    assert!(step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        &verifier,
        |_, _, _, _, _| panic!()
    )
    .unwrap());
    assert!(store::receipt(
        &library,
        &source(&library, &s, Sensitivity::Public).unwrap().identity,
        &job.job_id,
        Sensitivity::Public
    )
    .unwrap()
    .is_none());
    denied.set(Some(Phase::Read));
    assert!(status_with(
        &q,
        &library,
        &authority,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public,
        &verifier
    )
    .is_err());
    denied.set(Some(Phase::Stop));
    assert!(stop_with(
        &mut q,
        &library,
        &authority,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public,
        &verifier
    )
    .is_err());
    denied.set(None);
    recover_with(&mut q, &library, &verifier).unwrap();
    stop_with(
        &mut q,
        &library,
        &authority,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public,
        &verifier,
    )
    .unwrap();
    for phase in [
        Phase::Preview,
        Phase::Admission,
        Phase::Dispatch,
        Phase::Commit,
        Phase::Read,
        Phase::Stop,
        Phase::Reconcile,
    ] {
        assert!(phases.borrow().contains(&phase));
    }
    assert!(status(
        &q,
        &library,
        &OWNER,
        &s.item_id,
        &job.job_id,
        Sensitivity::Public
    )
    .is_err());
}

#[test]
fn revoked_at_dispatch_callback_and_unsent_blocks_stay_unresolved() {
    let (_tmp, mut q, library, s) = setup();
    let quote = quote(&q, &library, &s);
    let job = admit(&mut q, &library, req(&s, &quote, "revoke"));
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        |check| {
            anyhow::ensure!(check.phase != Phase::Dispatch, "revoked");
            Ok(check.ceiling)
        },
        |_, _, _, _, dispatch| {
            dispatch()?;
            panic!("revoked dispatch succeeded")
        },
    )
    .unwrap();
    assert_eq!(get(&q, &library, &s, &job).usage.requests_dispatched, 0);
    let large = format!("# Synthetic\n\n{}", "x".repeat(6000));
    let (input, pools) = matching::from_texts([&large, "# 合成\n\n合成正文。"], 2048)
        .unwrap()
        .unwrap();
    assert!(!input.contains(&"x".repeat(100)));
    let (pairs, _) = matching::accept(
        r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"}]}"#,
        &pools,
    )
    .unwrap();
    assert!(pairs
        .iter()
        .any(|p| p.relation == reader::paragraphs::Relation::Unresolved));
    assert_eq!(
        pairs
            .iter()
            .filter_map(|p| p.o)
            .map(|s| s[1] - s[0])
            .sum::<usize>(),
        large.len()
    );
}

#[test]
fn stop_races_dispatch_and_crash_after_dispatch_has_no_automatic_retry() {
    for mode in ["stop_before", "stop_after", "crash"] {
        let (tmp, mut q, library, s) = setup();
        let quote = quote(&q, &library, &s);
        let job = admit(&mut q, &library, req(&s, &quote, mode));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            step_with(
                &mut q,
                &library,
                |_, _| Ok(7),
                local_only,
                |_, _, _, _, dispatch| {
                    if mode == "stop_before" {
                        let mut other = Queue::open(&tmp.path().join("queue.db"))?;
                        stop(
                            &mut other,
                            &library,
                            &OWNER,
                            &s.item_id,
                            &job.job_id,
                            Sensitivity::Public,
                        )?;
                        dispatch()?;
                        panic!("stopped job dispatched");
                    }
                    dispatch()?;
                    if mode == "crash" {
                        panic!("synthetic process exit boundary");
                    }
                    let mut other = Queue::open(&tmp.path().join("queue.db"))?;
                    let stopped = stop(
                        &mut other,
                        &library,
                        &OWNER,
                        &s.item_id,
                        &job.job_id,
                        Sensitivity::Public,
                    )?;
                    assert_eq!(stopped.state, "stopping");
                    Ok((
                        answer(),
                        DispatchUsage {
                            input_tokens: Some(3),
                            output_tokens: Some(2),
                            total_tokens: Some(5),
                        },
                    ))
                },
            )
        }));
        if mode == "crash" {
            assert!(result.is_err());
        } else {
            result.unwrap().unwrap();
        }
        recover(&mut q, &library).unwrap();
        let current = get(&q, &library, &s, &job);
        assert_eq!(
            current.state,
            if mode == "crash" {
                "outcome_unknown"
            } else {
                "stopped"
            }
        );
        assert_eq!(
            current.usage.requests_dispatched,
            u8::from(mode != "stop_before")
        );
        if mode == "stop_after" {
            assert_eq!(current.usage.reported_total_tokens, 5);
        }
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| panic!(),
            local_only,
            |_, _, _, _, _| panic!()
        )
        .unwrap());
    }
}

#[test]
fn receipt_and_exact_replay_survive_current_revision_drift() {
    let (_tmp, mut q, library, s) = setup();
    let quote = quote(&q, &library, &s);
    let request = req(&s, &quote, "historical");
    let job = admit(&mut q, &library, request.clone());
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, dispatch| {
            dispatch()?;
            Ok((answer(), DispatchUsage::default()))
        },
    )
    .unwrap();
    let pending: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_translation_map_jobs WHERE job_id=?1",
            [&job.job_id],
            |r| r.get(0),
        )
        .unwrap();
    step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        local_only,
        |_, _, _, _, _| panic!(),
    )
    .unwrap();
    q.conn
        .execute(
            "UPDATE reader_translation_map_jobs SET state='running',payload=?1 WHERE job_id=?2",
            params![pending, job.job_id],
        )
        .unwrap();
    memex_store::revise_item_metadata(
        &library,
        &s.item_id,
        memex_store::ItemMetadataUpdate {
            expected_revision_id: s.revision_id.clone(),
            title: "Synthetic metadata change".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    recover(&mut q, &library).unwrap();
    assert_eq!(get(&q, &library, &s, &job).state, "succeeded");
    let replay = request_with(
        &mut q,
        &library,
        &OWNER,
        request,
        Sensitivity::Public,
        i64::MAX,
        |_, _| panic!("historical replay credentials"),
        local_only,
    )
    .unwrap();
    assert_eq!(replay.job_id, job.job_id);
    assert_eq!(replay.state, "succeeded");
    assert_eq!(
        stop(
            &mut q,
            &library,
            &OWNER,
            &s.item_id,
            &job.job_id,
            Sensitivity::Public
        )
        .unwrap()
        .state,
        "succeeded"
    );
}

#[test]
fn restart_reconciles_without_live_grant_but_never_resumes_spending_or_commits() {
    for stage in ["queued", "responded", "committed"] {
        let (_tmp, mut q, library, s) = setup();
        let quote = quote(&q, &library, &s);
        let job = admit(&mut q, &library, req(&s, &quote, stage));
        if stage != "queued" {
            step_with(
                &mut q,
                &library,
                |_, _| Ok(7),
                local_only,
                |_, _, _, _, dispatch| {
                    dispatch()?;
                    Ok((answer(), DispatchUsage::default()))
                },
            )
            .unwrap();
        }
        let pending: String = q
            .conn
            .query_row(
                "SELECT payload FROM reader_translation_map_jobs WHERE job_id=?1",
                [&job.job_id],
                |r| r.get(0),
            )
            .unwrap();
        if stage == "committed" {
            step_with(
                &mut q,
                &library,
                |_, _| Ok(7),
                local_only,
                |_, _, _, _, _| panic!("second model request"),
            )
            .unwrap();
            q.conn.execute("UPDATE reader_translation_map_jobs SET state='running',payload=?1 WHERE job_id=?2",
                params![pending,job.job_id]).unwrap();
        }
        let mut q = Queue::open(q.database_path()).unwrap();
        recover_with(&mut q, &library, |_| anyhow::bail!("revoked")).unwrap();
        let observed = get(&q, &library, &s, &job);
        assert_eq!(
            observed.state,
            if stage == "committed" {
                "succeeded"
            } else {
                "stopped"
            }
        );
        assert_eq!(
            observed.usage.requests_dispatched,
            u8::from(stage != "queued")
        );
        assert_eq!(observed.map_version.is_some(), stage == "committed");
        assert!(
            !source(&library, &s, Sensitivity::Public)
                .unwrap()
                .recorded_map
        );
        assert!(!reader_item_busy(&q.conn, &library.canonicalize().unwrap(), &s.item_id).unwrap());
        assert!(status_with(
            &q,
            &library,
            &OWNER,
            &s.item_id,
            &job.job_id,
            Sensitivity::Public,
            |_| anyhow::bail!("revoked")
        )
        .is_err());
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| panic!("restart credential lookup"),
            local_only,
            |_, _, _, _, _| panic!("restart model dispatch")
        )
        .unwrap());
    }
}
