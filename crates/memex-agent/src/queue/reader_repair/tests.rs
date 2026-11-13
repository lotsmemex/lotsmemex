use super::*;
#[path = "../../../examples/fixtures/reader_book.rs"]
pub(super) mod book;

pub(super) fn setup() -> (tempfile::TempDir, Queue, PathBuf, Request) {
    setup_with_book(book::seed)
}

pub(super) fn setup_with_book(
    seed: fn(&Path) -> Result<reader::ReaderDocument>,
) -> (tempfile::TempDir, Queue, PathBuf, Request) {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = seed(&library).unwrap();
    // Remove only this disposable fixture's prebuilt index to exercise inference.
    if library.join("catalog/chapter-alignments").exists() {
        std::fs::remove_dir_all(library.join("catalog/chapter-alignments")).unwrap();
    }
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
    let request = Request {
        item_id: doc.item_id,
        revision_id: doc.revision_id,
        chapter: "chapter-1".into(),
        request_id: "synthetic-job".into(),
        expected_alignment_version: "missing".into(),
    };
    (tmp, q, library, request)
}

const ANSWER: &str = r#"{"groups":[
    {"o":[0],"t":[0],"relation":"matched"},
    {"o":[1],"t":[1,2],"relation":"matched"},
    {"o":[2,3],"t":[3],"relation":"matched"},
    {"o":[],"t":[4],"relation":"translation_only"}
]}"#;

#[test]
fn usage_receipts_preserve_unknown_splits_and_historical_absence() {
    let (_tmp, mut q, library, req) = setup();
    let admitted = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    assert_eq!(admitted.usage.input_tokens, Some(0));
    assert_eq!(admitted.usage.output_tokens, Some(0));
    assert_eq!(admitted.usage.dispatches.as_ref().unwrap().len(), 0);
    step_with_usage(&mut q, &library, None, |_, _, _, _, dispatch| {
        dispatch()?;
        Ok((ANSWER.into(), DispatchUsage { input_tokens: None, output_tokens: Some(7), total_tokens: Some(37) }))
    }).unwrap();
    let observed = status(&q, &library, &req.item_id, &admitted.job_id, Sensitivity::Public).unwrap();
    assert_eq!(observed.operation, "repair");
    assert_eq!(observed.failure_stage, None);
    assert_eq!(observed.failure_reason, None);
    assert_eq!(observed.usage.input_tokens, None);
    assert_eq!(observed.usage.output_tokens, Some(7));
    assert_eq!(observed.usage.reported_total_tokens, 37);
    assert_eq!(observed.usage.unknown_usage_requests, 0);
    assert_eq!(serde_json::to_value(request(&mut q, &library, req, Sensitivity::Public, true).unwrap().usage).unwrap(), serde_json::to_value(&observed.usage).unwrap());
    let mut usage = observed.usage.clone();
    usage.dispatched();
    usage.observed(DispatchUsage { input_tokens: Some(11), output_tokens: Some(2), total_tokens: Some(13) });
    assert_eq!((usage.input_tokens, usage.output_tokens, usage.reported_total_tokens), (None, Some(9), 50));
    assert_eq!(usage.dispatches.unwrap().len(), 2);
    let historical: Usage = serde_json::from_value(serde_json::json!({"requests_dispatched":1,"reported_total_tokens":1374,"unknown_usage_requests":0})).unwrap();
    assert!(historical.dispatches.is_none());
    assert_eq!((historical.input_tokens, historical.output_tokens, historical.reported_total_tokens), (None, None, 1374));
}

#[test]
fn chapter_edit_commit_excludes_active_jobs_but_upload_and_receipt_replay_do_not() {
    use reader::chapter_edit::{begin, chunk, Begin};
    let (_tmp, mut q, library, req) = setup();
    let plan = reader::repair::plan(&library, &req.item_id, &req.revision_id, &req.chapter, Sensitivity::Public).unwrap();
    let edit = Begin { item_id: req.item_id.clone(), revision_id: req.revision_id.clone(),
        side: "original".into(), chapter: req.chapter.clone(), request_id: "edit".into(),
        expected_chapter_sha256: plan.identity.original.sha256.clone(), bytes: 6,
        sha256: format!("{:x}", sha2::Sha256::digest(b"Edited")) };
    let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    let upload = begin(&library, edit, "local_owner", Sensitivity::Public).unwrap();
    chunk(&library, &req.item_id, &upload.edit_id, 0, "Edited", "local_owner", Sensitivity::Public).unwrap();
    assert_eq!(commit_edit(&mut q, &library, &req.item_id, &upload.edit_id, "local_owner", Sensitivity::Public).unwrap_err().to_string(), "reader_job_in_progress");
    step_with(&mut q, &library, Some(&job.job_id), |_, _, _, _, _| anyhow::bail!("provider_failure")).unwrap();
    let receipt = commit_edit(&mut q, &library, &req.item_id, &upload.edit_id, "local_owner", Sensitivity::Public).unwrap();
    let newer = Request { revision_id: receipt.revision_id.clone(), request_id: "new-job".into(), ..req.clone() };
    request(&mut q, &library, newer, Sensitivity::Public, true).unwrap();
    assert_eq!(commit_edit(&mut q, &library, &req.item_id, &upload.edit_id, "local_owner", Sensitivity::Public).unwrap(), receipt);
}

#[test]
fn malformed_group_does_not_discard_independent_valid_matches() {
    let (_tmp, mut q, library, req) = setup_with_book(book::fragmented);
    let admitted = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    step_with(&mut q, &library, None, |_, _, _, _, dispatch| {
        dispatch()?;
        // An extra heading is incorrectly combined with body prose. A second
        // proposal incorrectly matches a body block to a note. Neither may be
        // accepted, but the independent, valid body matches remain usable.
        Ok((
            r#"{"groups":[
          {"o":[0,1],"t":[0],"relation":"matched"},
          {"o":[2,3],"t":[1],"relation":"matched"},
          {"o":[4],"t":[2],"relation":"matched"},
          {"o":[4],"t":[3],"relation":"matched"}
        ]}"#
            .into(),
            Some(23),
        ))
    })
    .unwrap();
    let observed = status(
        &q,
        &library,
        &req.item_id,
        &admitted.job_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(observed.state, "succeeded");
    assert_eq!(observed.alignment_status.as_deref(), Some("partial"));
    assert_eq!(
        observed.result_revision_id.as_deref(),
        Some(req.revision_id.as_str())
    );
    assert_eq!(observed.usage.reported_total_tokens, 23);
    assert_eq!(observed.usage.requests_dispatched, 1);
    let page = reader::paragraphs::read(
        &library,
        &req.item_id,
        &req.revision_id,
        "chapter-1",
        0,
        128,
        "any",
        Sensitivity::Public,
    )
    .unwrap();
    let matched: Vec<_> = page
        .pairs
        .iter()
        .filter(|p| p.relation == reader::paragraphs::Relation::Matched)
        .collect();
    assert_eq!(matched.len(), 2);
    assert!(matched
        .iter()
        .all(|p| p.kind == reader::paragraphs::Kind::Paragraph));
    assert!(page
        .pairs
        .iter()
        .any(|p| p.kind == reader::paragraphs::Kind::Note
            && p.relation == reader::paragraphs::Relation::Unresolved
            && p.o.is_none()));
}

#[test]
fn recovery_cannot_steal_a_live_cli_executor() {
    let (_tmp, mut q, library, req) = setup();
    let mut other = Queue::open(q.database_path()).unwrap();
    let job = request(&mut q, &library, req.clone(), Sensitivity::Public, false).unwrap();
    step_with(
        &mut q,
        &library,
        Some(&job.job_id),
        |_, _, _, _, dispatch| {
            recover(&mut other, &library)?;
            assert!(!step_with(
                &mut other,
                &library,
                Some(&job.job_id),
                |_, _, _, _, _| panic!("live task stolen")
            )?);
            dispatch()?;
            Ok((ANSWER.into(), Some(7)))
        },
    )
    .unwrap();
    let observed = status(
        &other,
        &library,
        &req.item_id,
        &job.job_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(observed.state, "succeeded");
    assert_eq!(observed.usage.requests_dispatched, 1);
    assert_eq!(observed.usage.reported_total_tokens, 7);
}

#[test]
fn selected_job_commits_complete_partition_and_replay_does_not_dispatch() {
    let (_tmp, mut q, library, req) = setup();
    let admitted = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    let mut other = req.clone();
    other.request_id = "another".into();
    assert_eq!(
        request(&mut q, &library, other, Sensitivity::Public, true)
            .unwrap_err()
            .to_string(),
        "reader_job_in_progress"
    );
    step_with(
        &mut q,
        &library,
        None,
        |_, snapshot, profile, input, dispatch| {
            assert_eq!(
                snapshot
                    .configuration
                    .target(Purpose::CaptureQuality)
                    .unwrap()
                    .1
                    .model_id,
                "synthetic"
            );
            assert!(input.len() + matching::PROMPT.len() <= profile.max_input_bytes);
            dispatch()?;
            Ok((ANSWER.into(), Some(37)))
        },
    )
    .unwrap();
    let job = status(
        &q,
        &library,
        &req.item_id,
        &admitted.job_id,
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(job.state, "succeeded");
    assert_eq!(
        job.result_revision_id.as_deref(),
        Some(req.revision_id.as_str())
    );
    assert_eq!(job.alignment_status.as_deref(), Some("partial"));
    assert_eq!(
        (
            job.usage.requests_dispatched,
            job.usage.reported_total_tokens,
            job.usage.unknown_usage_requests
        ),
        (1, 37, 0)
    );
    let page = reader::paragraphs::read(
        &library,
        &req.item_id,
        &req.revision_id,
        "chapter-1",
        0,
        128,
        "any",
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(page.pairs.len(), 6);
    assert_eq!(page.pairs[3].kind, reader::paragraphs::Kind::Note);
    assert_eq!(
        page.pairs[4].relation,
        reader::paragraphs::Relation::Unresolved
    );
    assert_eq!(
        page.pairs[5].relation,
        reader::paragraphs::Relation::Unresolved
    );
    assert_eq!(
        request(&mut q, &library, req.clone(), Sensitivity::Public, true)
            .unwrap()
            .job_id,
        job.job_id
    );
    assert!(!step_with(&mut q, &library, None, |_, _, _, _, _| panic!(
        "replay dispatched"
    ))
    .unwrap());
    let mut conflicting = req.clone();
    conflicting.chapter = "appendix".into();
    assert_eq!(
        request(&mut q, &library, conflicting, Sensitivity::Public, true)
            .unwrap_err()
            .to_string(),
        "request_identity_conflict"
    );
    assert!(status(
        &q,
        &library,
        "item_missing",
        &job.job_id,
        Sensitivity::Public
    )
    .is_err());
    assert_eq!(
        reader::paragraphs::read(
            &library,
            &req.item_id,
            &req.revision_id,
            "appendix",
            0,
            128,
            "any",
            Sensitivity::Public
        )
        .unwrap()
        .version,
        "missing"
    );
    let renamed = memex_store::revise_item_metadata(&library, &req.item_id, memex_store::ItemMetadataUpdate {
        expected_revision_id: req.revision_id.clone(), title: "Renamed synthetic".into(), manual_tags: vec![],
    }).unwrap();
    let inherited = reader::paragraphs::read(&library, &req.item_id, &renamed.revision_id, "chapter-1", 0, 128, "any", Sensitivity::Public).unwrap();
    assert_eq!(inherited.pairs, page.pairs);
    let mut next = req;
    next.request_id = "metadata-inherited-admission".into();
    next.revision_id = renamed.revision_id;
    assert!(request(&mut q, &library, next.clone(), Sensitivity::Public, true).is_err());
    next.expected_alignment_version = inherited.version;
    let accepted = request(&mut q, &library, next, Sensitivity::Public, true).unwrap();
    assert_eq!(accepted.usage.requests_dispatched, 0);
}

#[test]
fn failed_and_unknown_work_accounts_usage_and_retries_with_a_bound() {
    crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
    let attempts = 1 + crate::queue::AUTO_RETRIES as usize;
    for (answer, tokens, unknown) in [
        ("not json", Some(19), false),
        (
            r#"{"groups":[{"o":[0,1],"t":[0],"relation":"matched"}]}"#,
            Some(21),
            false,
        ),
        (
            r#"{"groups":[{"o":[999],"t":[0],"relation":"matched"}]}"#,
            Some(22),
            false,
        ),
        ("", None, true),
    ] {
        let (_tmp, mut q, library, req) = setup();
        let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
        let mut calls = 0;
        for _ in 0..8 {
            step_with(&mut q, &library, None, |_, _, _, _, dispatch| {
                dispatch()?;
                calls += 1;
                if unknown {
                    return Err(memex_provider::generation::GenerationError::OutcomeUnknown.into());
                }
                Ok((answer.into(), tokens))
            })
            .unwrap();
        }
        assert_eq!(calls, attempts, "bounded automatic retries");
        let observed =
            status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(
            observed.state,
            if unknown { "outcome_unknown" } else { "failed" }
        );
        assert_eq!(observed.usage.requests_dispatched as usize, attempts);
        assert_eq!(observed.usage.reported_total_tokens, tokens.unwrap_or(0) * attempts as u64);
        assert_eq!(observed.usage.unknown_usage_requests as usize, usize::from(unknown) * attempts);
        let dispatches = observed.usage.dispatches.as_ref().unwrap();
        assert_eq!(dispatches.len(), attempts);
        assert_eq!(dispatches[0].total_tokens, tokens);
        assert_eq!(dispatches[0].input_tokens, None);
        assert_eq!(dispatches[0].output_tokens, None);
        assert_eq!(observed.failure_stage.as_deref(), Some("matching"));
        assert!(observed.failure_reason.is_some());
        recover(&mut q, &library).unwrap();
        assert!(!step_with(&mut q, &library, None, |_, _, _, _, _| panic!(
            "failure retried"
        ))
        .unwrap());
        assert_eq!(
            request(&mut q, &library, req.clone(), Sensitivity::Public, true)
                .unwrap()
                .state,
            observed.state
        );
        assert_eq!(
            reader::paragraphs::read(
                &library,
                &req.item_id,
                &req.revision_id,
                "chapter-1",
                0,
                128,
                "any",
                Sensitivity::Public
            )
            .unwrap()
            .version,
            "missing"
        );
    }
}

#[test]
fn stale_index_at_dispatch_is_rejected_without_charging() {
    let (_tmp, mut q, library, req) = setup();
    let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    step_with(&mut q, &library, None, |_, _, _, _, dispatch| {
        let plan = reader::repair::plan(
            &library,
            &req.item_id,
            &req.revision_id,
            &req.chapter,
            Sensitivity::Public,
        )?;
        reader::repair::apply(&library, plan, Sensitivity::Public)?;
        dispatch()?;
        panic!("stale input dispatched");
    })
    .unwrap();
    let job = status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
    assert_eq!(job.error_code.as_deref(), Some("stale_chapter_alignment"));
    assert_eq!(job.usage.requests_dispatched, 0);
}

#[test]
fn removed_access_blocks_dispatch_commit_and_observation() {
    for after_dispatch in [false, true] {
        let (_tmp, mut q, library, req) = setup();
        let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
        step_with(&mut q, &library, None, |_, _, _, _, dispatch| {
            if after_dispatch {
                dispatch()?;
            }
            memex_store::trash_item(
                &library,
                &req.item_id,
                &req.revision_id,
                "synthetic_test",
                None,
                None,
            )?;
            if !after_dispatch {
                dispatch()?;
                panic!("inaccessible input dispatched");
            }
            Ok((ANSWER.into(), Some(11)))
        })
        .unwrap();
        assert!(status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).is_err());
        assert!(request(&mut q, &library, req.clone(), Sensitivity::Public, true).is_err());
        memex_store::restore_item(&library, &req.item_id).unwrap();
        let observed =
            status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(observed.state, "failed");
        assert_eq!(observed.usage.requests_dispatched, u8::from(after_dispatch));
        assert_eq!(
            reader::paragraphs::read(
                &library,
                &req.item_id,
                &req.revision_id,
                &req.chapter,
                0,
                128,
                "any",
                Sensitivity::Public
            )
            .unwrap()
            .version,
            "missing"
        );
    }
}

#[test]
fn restart_distinguishes_uncertain_dispatch_from_durable_commit_plan() {
    for committing in [false, true] {
        let (_tmp, mut q, library, req) = setup();
        let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
        let mut record = load(&q, &job.job_id).unwrap();
        record.job.state = "running".into();
        record.job.stage = if committing { "committing" } else { "matching" }.into();
        record.job.usage.requests_dispatched = 1;
        record.job.usage.unknown_usage_requests = 1;
        if committing {
            // A crash after Store publication but before the SQLite success receipt.
            reader::repair::apply(&library, record.plan.clone(), Sensitivity::Public).unwrap();
        }
        put(&q, &record).unwrap();
        recover(&mut q, &library).unwrap();
        assert_eq!(
            step_with(&mut q, &library, None, |_, _, _, _, _| panic!(
                "restart dispatched"
            ))
            .unwrap(),
            committing
        );
        let observed =
            status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(
            observed.state,
            if committing {
                "succeeded"
            } else {
                "outcome_unknown"
            }
        );
        assert_eq!(observed.usage.requests_dispatched, 1);
    }
}

#[test]
fn cli_model_plan_does_not_publish_library_index() {
    let (_tmp, mut q, library, mut req) = setup();
    let offline = reader::repair::plan(
        &library,
        &req.item_id,
        &req.revision_id,
        &req.chapter,
        Sensitivity::Public,
    )
    .unwrap();
    req.expected_alignment_version = reader::repair::apply(&library, offline, Sensitivity::Public)
        .unwrap()
        .alignment_version;
    let job = request(&mut q, &library, req.clone(), Sensitivity::Public, false).unwrap();
    step_with(
        &mut q,
        &library,
        Some(&job.job_id),
        |_, _, _, _, dispatch| {
            dispatch()?;
            Ok((ANSWER.into(), None))
        },
    )
    .unwrap();
    let plan = planned(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
    assert_eq!(plan.pairs.len(), 6);
    assert_eq!(
        reader::paragraphs::read(
            &library,
            &req.item_id,
            &req.revision_id,
            "chapter-1",
            0,
            128,
            "any",
            Sensitivity::Public
        )
        .unwrap()
        .version,
        req.expected_alignment_version
    );
    assert_eq!(
        status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public)
            .unwrap()
            .usage
            .unknown_usage_requests,
        1
    );
}


#[test]
fn matching_reply_envelope_is_normalized_but_known_fields_stay_strict() {
    use matching::{accept, from_texts};
    let (_, pools) = from_texts(["# A\n\nOne.\n\nTwo.", "# 甲\n\n一。\n\n二。"], 8192).unwrap().unwrap();
    let good = r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[1],"t":[1],"relation":"matched"}]}"#;
    let err = |reply: &str| accept(reply, &pools).err().map(|e| e.to_string());
    let matched = |reply: &str| {
        accept(reply, &pools).unwrap().0.iter()
            .filter(|p| p.relation == reader::paragraphs::Relation::Matched).count()
    };
    assert_eq!(matched(good), 2);
    assert_eq!(matched(&format!("```json\n{good}\n```")), 2);
    assert_eq!(matched(&format!("```\n{good}\n```")), 2);
    // Inert extras at either level grant nothing.
    assert_eq!(matched(r#"{"groups":[{"o":[0],"t":[0],"relation":"matched","note":"x"}],"source_verified":true}"#), 1);
    let json = Some("alignment_output_json");
    assert_eq!(err(&format!("Here you go:\n{good}")).as_deref(), json);
    assert_eq!(err(&format!("{good}{good}")).as_deref(), json);
    assert_eq!(err(&good[..good.len() - 3]).as_deref(), json);
    assert_eq!(err(&format!("```json\n{good}\n```\n```json\n{good}\n```")).as_deref(), json);
    assert_eq!(err(r#"{"groups":[],"groups":[]}"#).as_deref(), json);
    // Duplicates inside ignored extras, including escaped spellings, still reject.
    assert_eq!(err(r#"{"groups":[],"x":{"a":1,"\u0061":2}}"#).as_deref(), json);
    let schema = Some("alignment_output_schema");
    for bad in [
        r#"{"groups":[{"o":[0],"t":[0],"relation":"similar"}]}"#,
        r#"{"groups":[{"o":"0","t":[0],"relation":"matched"}]}"#,
        r#"{"groups":[{"o":[0],"relation":"matched"}]}"#,
        r#"{"groups":[{"o":null,"t":[0],"relation":"matched"}]}"#,
        r#"{"groups":null}"#,
        r#"{}"#,
        r#"[]"#,
        // Mixed schema-bad: the whole reply rejects.
        r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[1],"t":[1],"relation":5}]}"#,
    ] {
        assert_eq!(err(bad).as_deref(), schema, "{bad}");
    }
    // Mixed semantic-bad keeps the existing partial rule: the bad group alone is rejected.
    let (pairs, rejected) = accept(r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[9],"t":[1],"relation":"matched"}]}"#, &pools).unwrap();
    assert_eq!(rejected, vec!["invalid_block_ids".to_string()]);
    assert_eq!(pairs.iter().filter(|p| p.relation == reader::paragraphs::Relation::Matched).count(), 1);
    // Empty groups never claim alignment.
    assert_eq!(matching::status(&accept(r#"{"groups":[]}"#, &pools).unwrap().0), "none");
    // Size: nodes beyond the shared budget map to size, not json.
    let many = format!(r#"{{"groups":[],"x":[{}]}}"#, vec!["0"; 70_000].join(","));
    assert_eq!(err(&many).as_deref(), Some("alignment_output_size"));
}

fn strip_policy(q: &Queue, job_id: &str) {
    let payload: String = q.conn.query_row("SELECT payload FROM reader_repair_jobs WHERE job_id=?1", [job_id], |r| r.get(0)).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(v.as_object_mut().unwrap().remove("parser_policy").is_some());
    q.conn.execute("UPDATE reader_repair_jobs SET payload=?1 WHERE job_id=?2", params![v.to_string(), job_id]).unwrap();
}

#[test]
fn jobs_admitted_before_the_parser_policy_never_dispatch_under_it() {
    // Old queued job, persisted without a policy, after a daemon restart.
    let (tmp, mut q, library, req) = setup();
    let job_id = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap().job_id;
    strip_policy(&q, &job_id);
    drop(q);
    let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    recover(&mut q, &library).unwrap();
    step_with(&mut q, &library, None, |_, _, _, _, _| panic!("old job dispatched")).unwrap();
    let observed = status(&q, &library, &req.item_id, &job_id, Sensitivity::Public).unwrap();
    assert_eq!(observed.state, "failed");
    assert_eq!(observed.failure_reason.as_deref(), Some("reader_alignment_policy_stale"));
    assert_eq!(observed.usage.requests_dispatched, 0);
    // The same request id returns that job, never a new dispatch.
    let again = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
    assert_eq!(again.job_id, job_id);
    assert!(!step_with(&mut q, &library, None, |_, _, _, _, _| panic!("replay dispatched")).unwrap());

    // Old in-flight and old committing jobs reconcile by their frozen terms, with zero dispatch.
    for committing in [false, true] {
        let (tmp, mut q, library, req) = setup();
        let job = request(&mut q, &library, req.clone(), Sensitivity::Public, true).unwrap();
        let mut record = load(&q, &job.job_id).unwrap();
        record.job.state = "running".into();
        record.job.stage = if committing { "committing" } else { "matching" }.into();
        record.job.usage.requests_dispatched = 1;
        record.job.usage.unknown_usage_requests = 1;
        if committing {
            reader::repair::apply(&library, record.plan.clone(), Sensitivity::Public).unwrap();
        }
        put(&q, &record).unwrap();
        strip_policy(&q, &job.job_id);
        drop(q);
        let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
        recover(&mut q, &library).unwrap();
        step_with(&mut q, &library, None, |_, _, _, _, _| panic!("restart dispatched")).unwrap();
        let observed = status(&q, &library, &req.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(observed.state, if committing { "succeeded" } else { "outcome_unknown" });
        assert_eq!(observed.usage.requests_dispatched, 1);
        assert!(!step_with(&mut q, &library, None, |_, _, _, _, _| panic!("resent")).unwrap());
    }
}
