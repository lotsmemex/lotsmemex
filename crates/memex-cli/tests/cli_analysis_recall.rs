use std::process::Command;

#[test]
fn analysis_list_returns_empty_contract() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args([
            "analysis",
            "list",
            "--library",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(value["analyses"].as_array().unwrap().is_empty());
    assert!(!temp.path().join("analysis").exists());
}

#[test]
fn recall_returns_empty_lexical_contract() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args([
            "recall",
            "--library",
            temp.path().to_str().unwrap(),
            "--query",
            "synthetic",
            "--max-sensitivity",
            "public",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["retrieval_mode"], "lexical_v0");
    assert_eq!(value["hit_count"], 0);
    assert!(value["hits"].as_array().unwrap().is_empty());
    assert!(!temp.path().join("analysis").exists());
    assert!(!temp.path().join("items").exists());
}

#[test]
fn analysis_add_replays_without_body_in_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let ingest = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(root.join("examples/phase1-evidence/source.json"))
        .arg("--capture")
        .arg(root.join("examples/phase1-evidence/capture.json"))
        .arg("--library")
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(ingest.status.success());
    let ingest_receipt: serde_json::Value = serde_json::from_slice(&ingest.stdout).unwrap();
    let item_id = ingest_receipt["item_ids"][0].as_str().unwrap().to_owned();
    let revision_id = ingest_receipt["revision_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!revision_id.is_empty());
    let request = temp.path().join("analysis-request.json");
    std::fs::write(&request,serde_json::json!({"schema_version":1,"analysis_kind":"summary","title":"Synthetic Analysis","body_markdown":"ANALYSIS_BODY_SENTINEL","item_ids":[item_id.clone()],"topics":["synthetic"],"requested_sensitivity":"public","created_at":"2026-07-31T00:00:00Z","actor_kind":"agent","actor_id":"agent","tool_id":"tool","tool_version":"1","config_digest":format!("sha256:{}","0".repeat(64))}).to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "add", "--library"])
        .arg(temp.path())
        .arg("--request")
        .arg(&request)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ANALYSIS_BODY_SENTINEL"));
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "created");
    let analysis_id = receipt["analysis_id"].as_str().unwrap().to_owned();
    let replay = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "add", "--library"])
        .arg(temp.path())
        .arg("--request")
        .arg(request)
        .output()
        .unwrap();
    assert!(replay.status.success());
    assert!(!String::from_utf8_lossy(&replay.stdout).contains("ANALYSIS_BODY_SENTINEL"));
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["status"], "replayed");
    assert_eq!(replay["analysis_id"], analysis_id);
    assert!(temp
        .path()
        .join("analysis/records")
        .join(format!("{analysis_id}.json"))
        .exists());
    let listed = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "list", "--library"])
        .arg(temp.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["analyses"].as_array().unwrap().len(), 1);
    assert_eq!(listed["analyses"][0]["analysis_id"], analysis_id);
    assert_eq!(listed["analyses"][0]["title"], "Synthetic Analysis");
    let markdown = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "list", "--library"])
        .arg(temp.path())
        .args(["--format", "markdown"])
        .output()
        .unwrap();
    assert!(markdown.status.success());
    let markdown = String::from_utf8_lossy(&markdown.stdout);
    assert!(markdown.contains(&analysis_id));
    assert!(markdown.contains("Synthetic Analysis"));
    assert!(!markdown.contains("ANALYSIS_BODY_SENTINEL"));
    let shown = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "show", "--library"])
        .arg(temp.path())
        .args(["--analysis", &analysis_id, "--format", "json"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["record"]["analysis_id"], analysis_id);
    assert_eq!(shown["record"]["title"], "Synthetic Analysis");
    assert_eq!(shown["record"]["body_markdown"], "ANALYSIS_BODY_SENTINEL");
    let shown = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "show", "--library"])
        .arg(temp.path())
        .args(["--analysis", &analysis_id, "--format", "markdown"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains(&analysis_id));
    assert!(shown.contains("Synthetic Analysis"));
    assert!(shown.contains("ANALYSIS_BODY_SENTINEL"));
    assert!(shown.contains("## Analysis"));
    let link_request = temp.path().join("link-request.json");
    std::fs::write(&link_request,serde_json::json!({"schema_version":1,"from_analysis_id":analysis_id,"target":{"kind":"item_revision","id":item_id,"revision_id":revision_id},"relation":"supports","rationale_markdown":"LINK_RATIONALE_SENTINEL","created_at":"2026-07-31T00:00:00Z","actor_kind":"agent","actor_id":"agent","tool_id":"tool","tool_version":"1","config_digest":format!("sha256:{}","0".repeat(64))}).to_string()).unwrap();
    let linked = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "link", "--library"])
        .arg(temp.path())
        .arg("--request")
        .arg(&link_request)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(!String::from_utf8_lossy(&linked.stdout).contains("LINK_RATIONALE_SENTINEL"));
    let linked: serde_json::Value = serde_json::from_slice(&linked.stdout).unwrap();
    assert_eq!(linked["status"], "created");
    let link_id = linked["link_id"].as_str().unwrap();
    assert!(link_id.starts_with("analysis_link_"));
    assert!(temp
        .path()
        .join("analysis/links")
        .join(format!("{link_id}.json"))
        .exists());
    let replay_link = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["analysis", "link", "--library"])
        .arg(temp.path())
        .arg("--request")
        .arg(&link_request)
        .output()
        .unwrap();
    assert!(replay_link.status.success());
    assert!(!String::from_utf8_lossy(&replay_link.stdout).contains("LINK_RATIONALE_SENTINEL"));
    let replay_link: serde_json::Value = serde_json::from_slice(&replay_link.stdout).unwrap();
    assert_eq!(replay_link["status"], "replayed");
    assert_eq!(replay_link["link_id"], link_id);
    let recalled = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["recall", "--library"])
        .arg(temp.path())
        .args([
            "--query",
            "Synthetic",
            "--max-sensitivity",
            "public",
            "--limit",
            "10",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(recalled.status.success());
    assert!(!String::from_utf8_lossy(&recalled.stdout).contains("LINK_RATIONALE_SENTINEL"));
    let recalled: serde_json::Value = serde_json::from_slice(&recalled.stdout).unwrap();
    assert_eq!(recalled["schema_version"], 1);
    assert_eq!(recalled["retrieval_mode"], "lexical_v0");
    assert!(recalled["hit_count"].as_u64().unwrap() >= 2);
    let hits = recalled["hits"].as_array().unwrap();
    assert!(hits.iter().any(|h| h["id"] == item_id));
    assert!(hits.iter().any(|h| h["id"] == analysis_id));
    assert!(hits
        .iter()
        .all(|h| h["score"].as_u64().unwrap() > 0 && h["score_breakdown"].is_object()));
    let recalled = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["recall", "--library"])
        .arg(temp.path())
        .args([
            "--query",
            "Synthetic",
            "--max-sensitivity",
            "public",
            "--limit",
            "10",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(recalled.status.success());
    let recalled = String::from_utf8_lossy(&recalled.stdout);
    assert!(recalled.contains("lexical_v0"));
    assert!(recalled.contains(&item_id));
    assert!(recalled.contains(&analysis_id));
    assert!(recalled.contains("Synthetic Analysis"));
    assert!(recalled.contains("score:"));
    assert!(recalled.contains("breakdown:"));
    assert!(!recalled.contains("LINK_RATIONALE_SENTINEL"));
}

#[test]
fn context_read_preserves_utf8_citations_and_rejects_stale_or_unavailable_content() {
    use memex_core::Sensitivity;
    use lots_scout_extract::Extractor;
    use sha2::{Digest, Sha256};

    let temp = tempfile::tempdir().unwrap();
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/phase1-evidence");
    let mut capture = memex_ingest::read_verified_capture(
        fixtures.join("source.json"),
        fixtures.join("capture.json"),
    )
    .unwrap();
    capture.source.default_sensitivity = Sensitivity::Private;
    capture.capture.sensitivity = Sensitivity::Private;
    let mut draft = lots_scout_extract::GenericWebpageExtractor
        .extract(&capture)
        .unwrap();
    // Unequal byte/character counts, combining marks, escaping and a long tail.
    let body = format!("A中🙂e\u{301}\n\\\"{}", "x".repeat(16384));
    draft.content_markdown = body.clone();
    let receipt = memex_store::ingest(temp.path(), &capture, draft).unwrap();
    let item = &receipt.item_ids[0];
    let revision = &receipt.revision_ids[0];
    let run = |id: &str, rev: &str, start: usize, end: usize, ceiling: &str| {
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .arg("context-read")
            .arg("--library")
            .arg(temp.path())
            .args(["--item", id, "--expected-revision-id", rev])
            .args([
                "--start-byte",
                &start.to_string(),
                "--end-byte",
                &end.to_string(),
            ])
            .args(["--max-sensitivity", ceiling])
            .output()
            .unwrap()
    };
    let output = run(item, revision, 1, 8, "private");
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["content_markdown"], "中🙂");
    assert_eq!(value["start_byte"], 1);
    assert_eq!(value["end_byte"], 8);
    assert_eq!(value["total_bytes"], body.len());
    assert_eq!(
        value["content_sha256"],
        format!("sha256:{:x}", Sha256::digest(&body))
    );
    assert_eq!(value["input"]["item_id"], *item);
    assert_eq!(value["input"]["revision_id"], *revision);
    assert_eq!(
        value["input"]["evidence_ids"],
        serde_json::json!(receipt.evidence_ids)
    );
    assert_eq!(value["source_url"], "https://example.test/phase1");
    assert_eq!(value["origin"], "machine_projection");
    assert_eq!(value["coordinate_space"], "revision_markdown_utf8_v1");
    assert_eq!(value["sensitivity"], "private");
    let prefix = run(item, revision, 0, 14, "private");
    let tail = run(item, revision, 14, 14 + 16384, "private");
    assert!(prefix.status.success() && tail.status.success());
    let prefix: serde_json::Value = serde_json::from_slice(&prefix.stdout).unwrap();
    let tail: serde_json::Value = serde_json::from_slice(&tail.stdout).unwrap();
    assert_eq!(tail["content_markdown"].as_str().unwrap().len(), 16384);
    assert_eq!(
        format!(
            "{}{}",
            prefix["content_markdown"].as_str().unwrap(),
            tail["content_markdown"].as_str().unwrap()
        ),
        body
    );

    for (id, rev, start, end, ceiling, code) in [
        (
            item.as_str(),
            revision.as_str(),
            2,
            8,
            "private",
            "invalid_context_range",
        ),
        (
            item.as_str(),
            revision.as_str(),
            1,
            6,
            "private",
            "invalid_context_range",
        ),
        (
            item.as_str(),
            revision.as_str(),
            0,
            body.len() + 1,
            "private",
            "context_range_too_large",
        ),
        (
            item.as_str(),
            revision.as_str(),
            body.len(),
            body.len() + 1,
            "private",
            "invalid_context_range",
        ),
        (
            item.as_str(),
            revision.as_str(),
            0,
            0,
            "private",
            "invalid_context_request",
        ),
        (
            item.as_str(),
            revision.as_str(),
            8,
            1,
            "private",
            "invalid_context_request",
        ),
        (
            item.as_str(),
            revision.as_str(),
            0,
            16385,
            "private",
            "context_range_too_large",
        ),
        (
            item.as_str(),
            "rev_stale",
            1,
            8,
            "private",
            "stale_item_revision",
        ),
        (
            item.as_str(),
            "rev_stale",
            1,
            8,
            "public",
            "context_resource_not_found",
        ),
        (
            "item_missing",
            revision.as_str(),
            1,
            8,
            "public",
            "context_resource_not_found",
        ),
    ] {
        let failed = run(id, rev, start, end, ceiling);
        assert!(!failed.status.success(), "{code}");
        assert!(failed.stdout.is_empty(), "no partial data on error");
        assert_eq!(
            String::from_utf8_lossy(&failed.stderr).trim(),
            format!("Error: {code}")
        );
    }
    let update = memex_store::revise_item_metadata(
        temp.path(),
        item,
        memex_store::ItemMetadataUpdate {
            expected_revision_id: revision.clone(),
            title: "Revised title".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let stale = run(item, revision, 1, 8, "private");
    assert!(!stale.status.success());
    assert_eq!(
        String::from_utf8_lossy(&stale.stderr).trim(),
        "Error: stale_item_revision"
    );
    assert!(run(item, &update.revision_id, 1, 8, "private")
        .status
        .success());
    // Canonical integrity failures must not return unverified text or filesystem paths.
    let path = temp
        .path()
        .join("items")
        .join(item)
        .join("revisions")
        .join(format!("{}.json", update.revision_id));
    std::fs::write(path, b"tampered").unwrap();
    let corrupt = run(item, &update.revision_id, 1, 8, "private");
    assert!(!corrupt.status.success());
    assert!(corrupt.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&corrupt.stderr).trim(),
        "Error: context_read_failed"
    );
}
