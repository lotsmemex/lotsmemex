use super::jobs::*;
use super::*;
#[path = "../../../examples/fixtures/reader_book.rs"]
mod book;
#[path = "continuation_tests.rs"]
mod continuation;

fn setup() -> (tempfile::TempDir, Queue, PathBuf, Selection) {
    setup_with_book(book::seed)
}

fn setup_with_book(
    seed: fn(&Path) -> anyhow::Result<memex_store::reader::ReaderDocument>,
) -> (tempfile::TempDir, Queue, PathBuf, Selection) {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = seed(&library).unwrap();
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
    let selection = Selection::Item {
        item_id: doc.item_id,
        revision_id: doc.revision_id,
        target: store::Target::Chapter {
            side: "original".into(),
            chapter: "chapter-1".into(),
        },
    };
    (tmp, q, library, selection)
}

fn quote(q: &Queue, library: &Path, selection: Selection) -> Quote {
    let preview = preview(q, library, "local_owner", selection, Sensitivity::Public).unwrap();
    assert_eq!(preview.state, "preparing");
    prepare_one(q, library, |_, _| Ok(7)).unwrap();
    preview_status(
        q,
        library,
        "local_owner",
        &preview.preview_id,
        Sensitivity::Public,
    )
    .unwrap()
    .quote
    .unwrap()
}

fn unchanged_reply(w: &planner::Window) -> String {
    let input: serde_json::Value = serde_json::from_str(w.input()).unwrap();
    let parts = input["parts"].as_array().unwrap();
    serde_json::json!({"source_sha256":input["source_sha256"],"window_id":w.id(),"edits":[],
        "classifications":parts.iter().filter(|p|p["protected_unexamined"]!=true).map(|p|serde_json::json!({"range_id":p["id"],"role":"body"})).collect::<Vec<_>>(),"unresolved_ids":[]}).to_string()
}

#[test]
fn one_step_preview_is_admitted_and_formatted_without_a_cost_confirmation() {
    let (_tmp, mut q, library, selection) = setup();
    let preview = preview_auto(&q, &library, "local_owner", selection, Some("one-click".into()), Sensitivity::Public).unwrap();
    let mut calls = 0;
    // One click: preparation, admission and formatting run with no quote/accept round trip.
    while step_with(&mut q, &library, |_, _| Ok(7), |_, _, _, w, dispatch| {
        dispatch()?;
        calls += 1;
        Ok((unchanged_reply(w), crate::queue::reader_repair::DispatchUsage { input_tokens: Some(1), output_tokens: Some(1), total_tokens: Some(2) }))
    }, store::receipt, store::publish).unwrap() {}
    let read = preview_status(&q, &library, "local_owner", &preview.preview_id, Sensitivity::Public).unwrap();
    let batch = read.batch_id.expect("one-step preview was admitted as a batch");
    assert!(calls >= 1);
    let rows = results(&q, &library, "local_owner", &batch, 0, 10, Sensitivity::Public).unwrap();
    assert_eq!(rows["results"][0]["state"], "succeeded");
    // Replaying the same one-step request admits nothing new.
    assert!(!step_with(&mut q, &library, |_, _| Ok(7), |_, _, _, _, _| panic!("replayed"), store::receipt, store::publish).unwrap());
}

#[test]
fn durable_quote_once_only_noop_proof_and_replay() {
    let (_tmp, mut q, library, selection) = setup();
    let quote = quote(&q, &library, selection.clone());
    let admitted = request_with(
        &mut q,
        &library,
        "local_owner",
        &quote.quote_id,
        "one",
        true,
        true,
        Sensitivity::Public,
        |_, _| Ok(7),
    )
    .unwrap();
    let id = admitted["batch"]["batch_id"].as_str().unwrap();
    assert_eq!(
        request_with(
            &mut q,
            &library,
            "local_owner",
            &quote.quote_id,
            "two",
            true,
            true,
            Sensitivity::Public,
            |_, _| Ok(7)
        )
        .unwrap_err()
        .to_string(),
        "content_format_quote_consumed"
    );
    assert_eq!(
        request_with(
            &mut q,
            &library,
            "local_owner",
            &quote.quote_id,
            "one",
            true,
            true,
            Sensitivity::Public,
            |_, _| panic!()
        )
        .unwrap(),
        admitted
    );
    let mut calls = 0;
    while step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        |_, _, _, w, dispatch| {
            dispatch()?;
            calls += 1;
            Ok((
                unchanged_reply(w),
                crate::queue::reader_repair::DispatchUsage {
                    input_tokens: Some(11),
                    output_tokens: Some(3),
                    total_tokens: Some(17),
                },
            ))
        },
        store::receipt,
        store::publish,
    )
    .unwrap()
    {}
    assert_eq!(calls, 1);
    let result = results(&q, &library, "local_owner", id, 0, 10, Sensitivity::Public).unwrap();
    let row = &result["results"][0];
    assert_eq!(row["state"], "succeeded");
    assert_eq!(row["formatting"]["changed"], false);
    assert_eq!(row["formatting"]["status"], "complete");
    let Selection::Item { revision_id, .. } = selection else {
        panic!()
    };
    assert_eq!(row["job"]["result_revision_id"], revision_id);
    let proof = store::changes_list(
        &library,
        row["job"]["job_id"].as_str().unwrap(),
        "local_owner",
        row["formatting"]["changes_version"].as_str().unwrap(),
        0,
        64,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(proof.total, 0);
    assert!(status(&q, &library, "other", id, Sensitivity::Public).is_err());
    assert_eq!(
        status(&q, &library, "local_owner", id, Sensitivity::Public).unwrap()["batch"]["usage"]
            ["reported_total_tokens"],
        17
    );
}

#[test]
fn unknown_retries_with_a_bound_and_generation_drift_sends_nothing() {
    crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
    for unknown in [false, true] {
        let (_tmp, mut q, library, selection) = setup();
        let quote = quote(&q, &library, selection);
        let batch = request_with(
            &mut q,
            &library,
            "local_owner",
            &quote.quote_id,
            "one",
            true,
            true,
            Sensitivity::Public,
            |_, _| Ok(7),
        )
        .unwrap();
        let id = batch["batch"]["batch_id"].as_str().unwrap();
        let mut calls = 0;
        for _ in 0..6 {
            step_with(
                &mut q,
                &library,
                |_, _| Ok(if unknown { 7 } else { 8 }),
                |_, _, _, _, dispatch| {
                    dispatch()?;
                    calls += 1;
                    Err(memex_provider::generation::GenerationError::OutcomeUnknown.into())
                },
                store::receipt,
                store::publish,
            )
            .unwrap();
        }
        // Unknown outcomes are retried automatically, at most AUTO_RETRIES times.
        let expected = if unknown { 1 + crate::queue::AUTO_RETRIES as usize } else { 0 };
        assert_eq!(calls, expected);
        let s = status(&q, &library, "local_owner", id, Sensitivity::Public).unwrap();
        let rows = results(&q, &library, "local_owner", id, 0, 10, Sensitivity::Public).unwrap();
        let row = &rows["results"][0];
        let item = row["item_id"].as_str().unwrap();
        memex_store::trash_item(
            &library,
            item,
            row["revision_id"].as_str().unwrap(),
            "synthetic",
            None,
            None,
        )
        .unwrap();
        assert!(resume_with(
            &q,
            &library,
            "local_owner",
            id,
            Sensitivity::Public,
            |_, _| panic!("denied resume must not resolve credentials")
        )
        .is_err());
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| panic!("denied resume must not enqueue"),
            |_, _, _, _, _| panic!("denied resume must not dispatch"),
            store::receipt,
            store::publish
        )
        .unwrap());
        // Explicit synthetic owner restoration, not formatter-driven authorization repair.
        memex_store::restore_item(&library, item).unwrap();
        assert_eq!(
            status(&q, &library, "local_owner", id, Sensitivity::Public).unwrap(),
            s
        );
        if unknown {
            assert_eq!(s["batch"]["counts"]["outcome_unknown"], 1);
            resume_with(
                &q,
                &library,
                "local_owner",
                id,
                Sensitivity::Public,
                |_, _| Ok(7),
            )
            .unwrap();
            assert!(!step_with(
                &mut q,
                &library,
                |_, _| Ok(7),
                |_, _, _, _, _| panic!(),
                store::receipt,
                store::publish
            )
            .unwrap());
        } else {
            assert_eq!(s["batch"]["state"], "paused");
            assert!(resume_with(
                &q,
                &library,
                "local_owner",
                id,
                Sensitivity::Public,
                |_, _| Ok(8)
            )
            .is_err());
        }
    }
}

#[test]
fn changed_publication_ack_loss_recovers_without_dispatch_and_restores() {
    for interrupt in [true, false] {
        publication_ack_loss(interrupt);
    }
}

#[test]
fn later_windows_recheck_generation_and_source_before_dispatch() {
    for drift in ["none", "generation", "source"] {
        let (_tmp, mut q, library, mut selection) = setup();
        let initial = selection.clone();
        let Selection::Item {
            item_id,
            revision_id,
            target,
        } = &mut selection
        else {
            panic!()
        };
        let rewrite = |revision: &str, text: &str, request: &str| {
            use memex_store::reader::{chapter_edit, reader_sha256};
            let source = store::source(
                &library,
                item_id,
                revision,
                target.clone(),
                Sensitivity::Public,
            )
            .unwrap();
            let upload = chapter_edit::begin(
                &library,
                chapter_edit::Begin {
                    item_id: item_id.clone(),
                    revision_id: revision.into(),
                    side: "original".into(),
                    chapter: "chapter-1".into(),
                    expected_chapter_sha256: source.identity.source_sha256,
                    request_id: request.into(),
                    bytes: text.len(),
                    sha256: reader_sha256(text.as_bytes()),
                },
                "local_owner",
                Sensitivity::Public,
            )
            .unwrap();
            for (i, chunk) in text.as_bytes().chunks(8192).enumerate() {
                chapter_edit::chunk(
                    &library,
                    item_id,
                    &upload.edit_id,
                    i * 8192,
                    std::str::from_utf8(chunk).unwrap(),
                    "local_owner",
                    Sensitivity::Public,
                )
                .unwrap();
            }
            chapter_edit::commit(
                &library,
                item_id,
                &upload.edit_id,
                "local_owner",
                Sensitivity::Public,
            )
            .unwrap()
            .revision_id
        };
        *revision_id = rewrite(
            revision_id,
            &"Retained synthetic prose. ".repeat(600),
            "large-source",
        );
        let current = revision_id.clone();
        let mut selected = initial;
        if let Selection::Item { revision_id, .. } = &mut selected {
            *revision_id = current.clone();
        }
        let quoted = quote(&q, &library, selected);
        assert!(quoted.budget["max_requests"].as_u64().unwrap() > 1);
        let admitted = request_with(
            &mut q,
            &library,
            "local_owner",
            &quoted.quote_id,
            "multi",
            true,
            true,
            Sensitivity::Public,
            |_, _| Ok(7),
        )
        .unwrap();
        let id = admitted["batch"]["batch_id"].as_str().unwrap();
        let mut calls = 0;
        step_with(
            &mut q,
            &library,
            |_, _| Ok(7),
            |_, _, _, window, dispatch| {
                dispatch()?;
                calls += 1;
                Ok((unchanged_reply(window), Default::default()))
            },
            store::receipt,
            store::publish,
        )
        .unwrap();
        assert_eq!(calls, 1);
        if drift == "source" {
            rewrite(
                &current,
                "Changed content after first window.",
                "concurrent-edit",
            );
        }
        while step_with(
            &mut q,
            &library,
            |_, _| Ok(if drift == "generation" { 8 } else { 7 }),
            |_, _, _, window, dispatch| {
                dispatch()?;
                calls += 1;
                Ok((unchanged_reply(window), Default::default()))
            },
            store::receipt,
            store::publish,
        )
        .unwrap()
        {}
        let state = status(&q, &library, "local_owner", id, Sensitivity::Public).unwrap();
        if drift == "none" {
            assert_eq!(state["batch"]["counts"]["succeeded"], 1);
            assert_eq!(
                calls as u64,
                quoted.budget["max_requests"].as_u64().unwrap()
            );
        } else {
            assert_eq!(calls, 1, "drift must stop the second paid request");
            assert_eq!(
                state["batch"]["state"],
                if drift == "generation" {
                    "paused"
                } else {
                    "done"
                }
            );
            assert_eq!(state["batch"]["counts"]["succeeded"], 0);
        }
    }
}

fn publication_ack_loss(interrupt: bool) {
    let (_tmp, mut q, library, selection) = setup();
    let selected = selection.clone();
    let quote = quote(&q, &library, selection);
    let batch = request_with(
        &mut q,
        &library,
        "local_owner",
        &quote.quote_id,
        "crash",
        true,
        true,
        Sensitivity::Public,
        |_, _| Ok(7),
    )
    .unwrap();
    let id = batch["batch"]["batch_id"].as_str().unwrap();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        step_with(&mut q,&library,|_,_|Ok(7),|_,_,_,w,dispatch| {
            dispatch()?;
            let input:serde_json::Value=serde_json::from_str(w.input())?;
            let part=input["parts"].as_array().unwrap().iter().find(|p|p["text"].as_str().unwrap_or_default().contains("One morning.")).unwrap();
            let start=part["source_range"][0].as_u64().unwrap() as usize+3;
            let mut reply:serde_json::Value=serde_json::from_str(&unchanged_reply(w))?;
            reply["edits"]=serde_json::json!([{"range_id":part["id"],"old_range":[start,start+1],"replacement":"  ","kind":"whitespace","reason_code":"paragraph_layout"}]);
            Ok((reply.to_string(),Default::default()))
        },store::receipt,|lib,identity,request,principal,applied,provenance,ceiling| {
            store::publish(lib,identity,request,principal,applied,provenance,ceiling)?;
            if interrupt {
                panic!("synthetic lost acknowledgement after actual publication")
            }
            anyhow::bail!("synthetic lost acknowledgement without process interruption")
        }).unwrap();
    }));
    assert_eq!(crashed.is_err(), interrupt);
    if interrupt {
        // A settled receipt wins over new busy/configuration gates after restart.
        q = Queue::open(q.database_path()).unwrap();
        let Selection::Item { item_id, .. } = &selected else {
            panic!()
        };
        q.conn.execute("INSERT OR REPLACE INTO content_format_active(library,item_id,batch_id) VALUES(?1,?2,'busy-after-publication')",
            params![library.canonicalize().unwrap().to_string_lossy(), item_id]).unwrap();
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| panic!(),
            |_, _, _, _, _| panic!(),
            store::receipt,
            store::publish
        )
        .unwrap());
        assert_eq!(
            status(&q, &library, "local_owner", id, Sensitivity::Public).unwrap()["batch"]["state"],
            "done"
        );
        resume_with(
            &q,
            &library,
            "local_owner",
            id,
            Sensitivity::Public,
            |_, _| panic!("settled result does not require original credentials"),
        )
        .unwrap();
        assert!(!step_with(
            &mut q,
            &library,
            |_, _| panic!("receipt recovery must precede credentials"),
            |_, _, _, _, _| panic!("redispatched"),
            store::receipt,
            store::publish
        )
        .unwrap());
    }
    let rows = results(&q, &library, "local_owner", id, 0, 10, Sensitivity::Public).unwrap();
    let row = &rows["results"][0];
    assert_eq!(row["state"], "succeeded");
    assert_eq!(row["job"]["usage"]["requests_dispatched"], 1);
    assert_eq!(row["formatting"]["applied_count"], 1);
    let target = row["target_id"].as_str().unwrap();
    let revision = row["job"]["result_revision_id"].as_str().unwrap();
    let version = row["formatting"]["changes_version"].as_str().unwrap();
    let changes = changes(
        &q,
        &library,
        "local_owner",
        id,
        target,
        version,
        0,
        64,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(changes.changes[0].before_bytes, 1);
    assert_eq!(changes.changes[0].after_bytes, 2);
    let restored = restore(
        &mut q,
        &library,
        "local_owner",
        id,
        target,
        revision,
        "restore",
        Sensitivity::Public,
    )
    .unwrap();
    assert!(restored.changed);
    q.conn.execute("INSERT INTO content_format_active(library,item_id,batch_id) VALUES(?1,?2,'another-batch')",
        params![library.canonicalize().unwrap().to_string_lossy(), restored.item_id]).unwrap();
    assert_eq!(
        restore(
            &mut q,
            &library,
            "local_owner",
            id,
            target,
            revision,
            "another-restore",
            Sensitivity::Public
        )
        .unwrap_err()
        .to_string(),
        "reader_job_in_progress"
    );
    assert_eq!(
        restore(
            &mut q,
            &library,
            "local_owner",
            id,
            target,
            revision,
            "restore",
            Sensitivity::Public
        )
        .unwrap(),
        restored
    );
    let Selection::Item {
        item_id,
        revision_id,
        target,
    } = selected
    else {
        panic!()
    };
    assert_ne!(restored.revision_id, revision_id);
    let source = store::source(
        &library,
        &item_id,
        &restored.revision_id,
        target,
        Sensitivity::Public,
    )
    .unwrap();
    assert!(source.markdown.contains("One morning."));
    assert!(!source.markdown.contains("One  morning."));
}

#[test]
fn invalid_reply_is_retried_with_a_bound_recorded_without_text_and_publishes_nothing() {
    let secret = "sk-synthetic-reply-secret";
    crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
    // (reply builder, expected state, expected failure_reason)
    let cases: [(fn(&planner::Window, &str) -> String, &str, Option<&str>); 3] = [
        // Fenced, no echoed binding fields, no unresolved_ids: accepted.
        (|w, _| {
            let mut v: serde_json::Value = serde_json::from_str(&unchanged_reply(w)).unwrap();
            for k in ["source_sha256", "window_id", "unresolved_ids"] { v.as_object_mut().unwrap().remove(k); }
            v["note"] = "inert".into();
            format!("```json\n{v}\n```")
        }, "succeeded", None),
        // One valid typo plus one invalid edit: whole target fails, nothing published.
        (|w, secret| {
            let mut v: serde_json::Value = serde_json::from_str(&unchanged_reply(w)).unwrap();
            let input: serde_json::Value = serde_json::from_str(w.input()).unwrap();
            let part = input["parts"].as_array().unwrap().iter().find(|p| p["text"].as_str().is_some_and(|t| t.is_ascii() && t.len() > 4)).unwrap();
            let s = part["source_range"][0].as_u64().unwrap();
            let first = &part["text"].as_str().unwrap()[..1];
            v["edits"] = serde_json::json!([
                {"range_id":part["id"],"old_range":[s,s+1],"replacement":if first == "Q" {"R"} else {"Q"},"kind":"typo","reason_code":"short_correction"},
                {"range_id":part["id"],"old_range":[s+2,s+3],"replacement":format!("{secret}\u{0}"),"kind":"typo","reason_code":"short_correction"}]);
            v.to_string()
        }, "failed", Some("format_output_edit_policy")),
        (|w, secret| format!("Sure! {secret}\n{}", unchanged_reply(w)), "failed", Some("format_output_json")),
    ];
    for (reply, state, reason) in cases {
        let (_tmp, mut q, library, selection) = setup();
        let before = memex_store::read_item(&library, match &selection { Selection::Item { item_id, .. } => item_id, _ => unreachable!() }).unwrap().1;
        let quote = quote(&q, &library, selection);
        let batch = request_with(&mut q, &library, "local_owner", &quote.quote_id, "one", true, true, Sensitivity::Public, |_, _| Ok(7)).unwrap();
        let id = batch["batch"]["batch_id"].as_str().unwrap().to_owned();
        let mut calls = 0;
        while step_with(&mut q, &library, |_, _| Ok(7), |_, _, _, w, dispatch| {
            dispatch()?;
            calls += 1;
            Ok((reply(w, secret), crate::queue::reader_repair::DispatchUsage { input_tokens: Some(11), output_tokens: Some(3), total_tokens: Some(17) }))
        }, store::receipt, store::publish).unwrap() {}
        let rows = results(&q, &library, "local_owner", &id, 0, 10, Sensitivity::Public).unwrap();
        let row = &rows["results"][0];
        assert_eq!(row["state"], state);
        assert!(calls >= 1);
        assert_eq!(row["job"]["usage"]["reported_total_tokens"].as_u64().unwrap(), 17 * calls as u64, "observed usage kept");
        assert_eq!(row["job"]["failure_reason"].as_str(), reason);
        if state == "failed" {
            assert_eq!(calls, 1 + crate::queue::AUTO_RETRIES as usize, "bounded automatic retries of an invalid reply");
            assert_eq!(row["job"]["error_code"], "content_format_invalid_result");
            assert_eq!(row["reason"], "content_format_invalid_result");
            assert!(row["job"]["result_revision_id"].is_null());
            assert_eq!(memex_store::read_item(&library, &before.item_id).unwrap().1, before, "nothing published");
        }
        let status = status(&q, &library, "local_owner", &id, Sensitivity::Public).unwrap().to_string();
        assert!(!status.contains(secret) && !rows.to_string().contains(secret));
    }
}

#[test]
fn old_plan_batches_neither_reparse_recorded_output_nor_redispatch() {
    for scenario in ["queued", "recorded", "unknown"] {
        let (_tmp, mut q, library, selection) = setup();
        let quote = quote(&q, &library, selection.clone());
        let admitted = request_with(&mut q, &library, "local_owner", &quote.quote_id,
            "old-policy", true, true, Sensitivity::Public, |_, _| Ok(7)).unwrap();
        let id = admitted["batch"]["batch_id"].as_str().unwrap();
        let raw: String = q.conn.query_row("SELECT payload FROM content_format_batches WHERE batch_id=?1", [id], |r| r.get(0)).unwrap();
        let mut old: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // A stored plan from another prompt/parser policy cannot be reinterpreted.
        old["frozen"]["targets"][0]["plan_sha256"] = "0".repeat(64).into();
        if scenario == "recorded" {
            old["children"][0]["outputs"] = serde_json::json!(["OLD_REPLY_MUST_NOT_BE_PARSED"]);
        }
        if scenario != "queued" {
            old["children"][0]["job"]["usage"]["requests_dispatched"] = 1.into();
            old["children"][0]["job"]["usage"]["reported_total_tokens"] = 17.into();
        }
        if scenario == "unknown" {
            old["state"] = "running".into();
            old["children"][0]["state"] = "running".into();
            old["children"][0]["job"]["state"] = "running".into();
            old["children"][0]["in_flight"] = true.into();
        }
        q.conn.execute("UPDATE content_format_batches SET state=?1,payload=?2 WHERE batch_id=?3",
            params![old["state"].as_str().unwrap(), old.to_string(), id]).unwrap();
        for _ in 0..2 {
            step_with(&mut q, &library, |_, _| Ok(7), |_, _, _, _, _| panic!("old policy redispatched"),
                store::receipt, |_, _, _, _, _, _, _| panic!("old policy published")).unwrap();
        }
        let after: String = q.conn.query_row("SELECT payload FROM content_format_batches WHERE batch_id=?1", [id], |r| r.get(0)).unwrap();
        let after: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(after["children"][0]["outputs"], old["children"][0]["outputs"]);
        assert_eq!(after["children"][0]["job"]["usage"]["requests_dispatched"], old["children"][0]["job"]["usage"]["requests_dispatched"]);
        assert_eq!(after["children"][0]["job"]["usage"]["reported_total_tokens"], old["children"][0]["job"]["usage"]["reported_total_tokens"]);
        if scenario == "unknown" {
            assert_eq!(after["children"][0]["state"], "outcome_unknown");
        } else {
            assert_eq!(after["state"], "paused");
            assert_eq!(after["error_code"], "content_format_quote_stale");
        }
        let Selection::Item { item_id, revision_id, .. } = selection else { panic!() };
        assert_eq!(memex_store::read_item(&library, &item_id).unwrap().1.revision_id, revision_id);
    }
}
