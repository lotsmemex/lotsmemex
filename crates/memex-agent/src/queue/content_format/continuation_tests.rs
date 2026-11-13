use super::*;
use memex_store::reader::{self, paragraphs};
use serde_json::{json, Value};

#[test]
fn durable_continuation_preserves_evidence_aligns_fresh_and_restores() {
    let (_tmp, mut q, library, selection) = setup_with_book(book::fragmented);
    let Selection::Item {
        item_id,
        revision_id,
        target,
    } = &selection
    else {
        panic!()
    };
    let before = store::source(
        &library,
        item_id,
        revision_id,
        target.clone(),
        Sensitivity::Public,
    )
    .unwrap();
    let read = |revision: &str, side: &str, chapter: &str| {
        reader::reader_chunk(
            &library,
            item_id,
            revision,
            side,
            chapter,
            0,
            reader::READER_CHUNK_BYTES,
            Sensitivity::Public,
        )
        .unwrap()
    };
    let appendix = read(revision_id, "original", "appendix");
    let translation = read(revision_id, "translation", "chapter-1");
    let quoted = quote(&q, &library, selection.clone());
    let batch = request_with(
        &mut q,
        &library,
        "local_owner",
        &quoted.quote_id,
        "join",
        true,
        true,
        Sensitivity::Public,
        |_, _| Ok(7),
    )
    .unwrap();
    let id = batch["batch"]["batch_id"].as_str().unwrap();
    let mut calls = 0;
    while step_with(&mut q, &library, |_, _| Ok(7), |_, _, _, w, dispatch| {
        dispatch()?;
        calls += 1;
        let input: Value = serde_json::from_str(w.input())?;
        let parts = input["parts"].as_array().unwrap();
        let index = parts.iter().position(|p| p["text"] == "They called her Mira\n\n").unwrap();
        assert_eq!(parts[index + 1]["text"], "at the station.\n\n");
        let part = &parts[index];
        let end = part["source_range"][1].as_u64().unwrap();
        assert_eq!(parts[index + 1]["source_range"][0], end);
        let mut reply: Value = serde_json::from_str(&unchanged_reply(w))?;
        reply["edits"] = json!([{"range_id":part["id"],"old_range":[end-2,end],"replacement":" ","kind":"paragraph_boundary","reason_code":"paragraph_layout"}]);
        Ok((reply.to_string(), Default::default()))
    }, store::receipt, store::publish).unwrap() {}
    assert_eq!(calls, 1);
    let rows = results(&q, &library, "local_owner", id, 0, 10, Sensitivity::Public).unwrap();
    let row = &rows["results"][0];
    assert_eq!(row["state"], "succeeded");
    assert_eq!(row["formatting"]["applied_count"], 1);
    let revision = row["job"]["result_revision_id"].as_str().unwrap();
    let after = store::source(
        &library,
        item_id,
        revision,
        target.clone(),
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(
        after.markdown,
        before.markdown.replace("Mira\n\nat", "Mira at")
    );
    let facts = |s: &str| {
        s.bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<_>>()
    };
    assert_eq!(facts(&before.markdown), facts(&after.markdown));
    let count = |s: &str| {
        paragraphs::blocks(s)
            .iter()
            .filter(|(_, k)| *k == paragraphs::Kind::Paragraph)
            .count()
    };
    assert_eq!(count(&after.markdown) + 1, count(&before.markdown));
    for (side, chapter, old) in [
        ("original", "appendix", appendix),
        ("translation", "chapter-1", translation),
    ] {
        let new = read(revision, side, chapter);
        assert_eq!(new.markdown, old.markdown);
        // Original edition gets new Evidence because chapter-1 changed; the
        // untouched translation retains its existing Evidence identity.
        if side == "translation" {
            assert_eq!(new.evidence_id, old.evidence_id);
        }
        assert!(after
            .identity
            .source_evidence_ids
            .contains(old.evidence_id.as_ref().unwrap()));
    }
    for evidence in &before.identity.source_evidence_ids {
        assert!(after.identity.source_evidence_ids.contains(evidence));
    }

    // A fresh real repair job validates semantic fixture correspondences against
    // the newly published source. Never derive matches by zipping block counts.
    use crate::queue::reader_repair as repair;
    let plan = reader::repair::plan(
        &library,
        item_id,
        revision,
        "chapter-1",
        Sensitivity::Public,
    )
    .unwrap();
    assert!(plan
        .pairs
        .iter()
        .all(|p| p.relation != paragraphs::Relation::Matched));
    let job = repair::request(
        &mut q,
        &library,
        repair::Request {
            item_id: item_id.clone(),
            revision_id: revision.into(),
            chapter: "chapter-1".into(),
            request_id: "fresh-after-format".into(),
            expected_alignment_version: plan.expected_alignment_version,
        },
        Sensitivity::Public,
        true,
    )
    .unwrap();
    repair::step_with(&mut q, &library, Some(&job.job_id), |_, _, _, input, dispatch| {
        dispatch()?;
        let input: Value = serde_json::from_str(input)?;
        let find = |side: &str, text: &str| input[side].as_array().unwrap().iter().find(|b| b["text"].as_str().unwrap().trim() == text).unwrap()["id"].clone();
        let groups = [
            ("# Opening", "# 开篇"),
            ("They called her Mira at the station.", "车站有人叫她米拉。"),
            ("A bright morning.", "明亮的清晨。"),
        ].into_iter().map(|(o, t)| json!({"o":[find("original",o)],"t":[find("translation",t)],"relation":"matched"})).collect::<Vec<_>>();
        Ok((json!({"groups":groups}).to_string(), Some(12)))
    }).unwrap();
    let job = repair::status(&q, &library, item_id, &job.job_id, Sensitivity::Public).unwrap();
    assert_eq!(job.state, "succeeded");
    let page = paragraphs::read(
        &library,
        item_id,
        revision,
        "chapter-1",
        0,
        128,
        "any",
        Sensitivity::Public,
    )
    .unwrap();
    let translated = read(revision, "translation", "chapter-1").markdown;
    let matched = page
        .pairs
        .iter()
        .filter(|p| {
            p.kind == paragraphs::Kind::Paragraph && p.relation == paragraphs::Relation::Matched
        })
        .collect::<Vec<_>>();
    assert_eq!(matched.len(), 2);
    for pair in matched {
        let [a, b] = pair.o.unwrap();
        let [c, d] = pair.t.unwrap();
        assert_eq!(count(&after.markdown[a..b]), 1);
        assert_eq!(count(&translated[c..d]), 1);
    }
    assert!(page.pairs.iter().any(
        |p| p.kind == paragraphs::Kind::Note && p.relation == paragraphs::Relation::Unresolved
    ));
    let restored = restore(
        &mut q,
        &library,
        "local_owner",
        id,
        row["target_id"].as_str().unwrap(),
        revision,
        "undo-join",
        Sensitivity::Public,
    )
    .unwrap();
    let source = store::source(
        &library,
        item_id,
        &restored.revision_id,
        target.clone(),
        Sensitivity::Public,
    )
    .unwrap();
    assert_eq!(source.markdown, before.markdown);
}

#[test]
fn continuation_boundaries_reject_wrong_ranges_and_protected_structure() {
    let limits = planner::SavedLowLimits {
        max_input_bytes: 8192,
        max_completion_tokens: 1024,
    };
    for source in [
        "They called her Mira\n\n# At the station\n",
        "They called her Mira\n\n[^1]: at the station.\n",
        "# They called her Mira\n\nat the station.\n",
        "[^1]: They called her Mira\n\nat the station.\n",
    ] {
        let plan = planner::plan(source, limits, &[]).unwrap();
        let w = &plan.windows()[0];
        let input: Value = serde_json::from_str(w.input()).unwrap();
        let end = input["parts"][0]["source_range"][1].as_u64().unwrap();
        let mut reply: Value = serde_json::from_str(&unchanged_reply(w)).unwrap();
        reply["edits"] = json!([{"range_id":input["parts"][0]["id"],"old_range":[end-2,end],"replacement":" ","kind":"paragraph_boundary","reason_code":"paragraph_layout"}]);
        assert!(
            plan.apply(source, &[reply.to_string()]).is_err(),
            "{source}"
        );
    }
    let source = "They called her Mira\n\nat the station.";
    let plan = planner::plan(source, limits, &[]).unwrap();
    let w = &plan.windows()[0];
    let input: Value = serde_json::from_str(w.input()).unwrap();
    let boundary = source.find("\n\n").unwrap();
    for (part, range) in [(1, [boundary, boundary + 2]), (0, [boundary, source.len()])] {
        let mut reply: Value = serde_json::from_str(&unchanged_reply(w)).unwrap();
        reply["edits"] = json!([{"range_id":input["parts"][part]["id"],"old_range":range,"replacement":" at the station.","kind":"paragraph_boundary","reason_code":"paragraph_layout"}]);
        assert!(w.validate_output(&reply.to_string()).is_err());
    }
    // Uncertain semantics are model responsibility, but unresolved ranges cannot
    // be edited even when the whitespace edit would otherwise be structurally valid.
    let source = "She waited\n\nperhaps.";
    let plan = planner::plan(source, limits, &[]).unwrap();
    let w = &plan.windows()[0];
    let input: Value = serde_json::from_str(w.input()).unwrap();
    let mut reply: Value = serde_json::from_str(&unchanged_reply(w)).unwrap();
    reply["classifications"][0]["role"] = json!("unresolved");
    reply["unresolved_ids"] = json!([input["parts"][0]["id"]]);
    assert_eq!(
        plan.apply(source, &[reply.to_string()]).unwrap().output(),
        source
    );
    let boundary = source.find("\n\n").unwrap();
    reply["edits"] = json!([{"range_id":input["parts"][0]["id"],"old_range":[boundary,boundary+2],"replacement":" ","kind":"paragraph_boundary","reason_code":"paragraph_layout"}]);
    assert!(plan.apply(source, &[reply.to_string()]).is_err());
}
