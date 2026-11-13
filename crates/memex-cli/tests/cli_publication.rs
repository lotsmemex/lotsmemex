use std::{fs, path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn publication_empty_list_is_read_only_in_json_and_markdown() {
    let temp = tempfile::tempdir().unwrap();
    for format in ["json", "markdown"] {
        let output = run(
            &workspace_root(),
            &[
                "publication",
                "list",
                "--library",
                temp.path().to_str().unwrap(),
                "--format",
                format,
            ],
        );
        assert!(output.status.success());
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert!(value["publications"].as_array().unwrap().is_empty());
        } else {
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "# Publications\n"
            );
        }
    }
    assert!(!temp.path().join("publications").exists());
}

#[test]
fn publication_cli_create_show_release_and_public_readback() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = workspace_root();
    let ingest = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(workspace.join("examples/phase1-evidence/source.json"))
        .arg("--capture")
        .arg(workspace.join("examples/phase1-evidence/capture.json"))
        .arg("--library")
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(ingest.status.success());
    let item_id = serde_json::from_slice::<serde_json::Value>(&ingest.stdout).unwrap()["item_ids"]
        [0]
    .as_str()
    .unwrap()
    .to_owned();
    let analysis_request = temp.path().join("analysis.json");
    fs::write(&analysis_request, serde_json::json!({
        "schema_version":1,"analysis_kind":"summary","title":"Analysis","body_markdown":"ANALYSIS_PRIVATE_SENTINEL",
        "item_ids":[item_id.clone()],"topics":[],"requested_sensitivity":"public","created_at":"2026-07-31T00:00:00Z",
        "actor_kind":"agent","actor_id":"analysis_actor","tool_id":"tool","tool_version":"1",
        "config_digest":format!("sha256:{}", "0".repeat(64))
    }).to_string()).unwrap();
    let analysis = run(
        &workspace,
        &[
            "analysis",
            "add",
            "--library",
            temp.path().to_str().unwrap(),
            "--request",
            analysis_request.to_str().unwrap(),
        ],
    );
    assert!(analysis.status.success());
    let analysis_id = serde_json::from_slice::<serde_json::Value>(&analysis.stdout).unwrap()
        ["analysis_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let publication_request = temp.path().join("publication.json");
    fs::write(&publication_request, serde_json::json!({
        "schema_version":1,"publication_kind":"briefing","product_id":"synthetic","title":"Public title",
        "summary":"PUBLIC_SUMMARY","body_markdown":"PUBLIC_BODY","inputs":[{"kind":"analysis","id":analysis_id,"revision_id":null}],
        "topics":["topic"],"requested_sensitivity":"public","created_at":"2026-07-31T00:00:00Z",
        "actor_kind":"agent","actor_id":"publication_actor","tool_id":"tool","tool_version":"1",
        "config_digest":format!("sha256:{}", "1".repeat(64))
    }).to_string()).unwrap();
    let create_args = [
        "publication",
        "create",
        "--library",
        temp.path().to_str().unwrap(),
        "--request",
        publication_request.to_str().unwrap(),
    ];
    let created = run(&workspace, &create_args);
    assert!(created.status.success());
    assert!(!String::from_utf8_lossy(&created.stdout).contains("PUBLIC_BODY"));
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(created["status"], "created");
    let publication_id = created["publication_id"].as_str().unwrap().to_owned();
    let replay = run(&workspace, &create_args);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&replay.stdout).unwrap()["status"],
        "replayed"
    );
    for (command, id) in [("list", None), ("show", Some(publication_id.as_str()))] {
        let mut args = vec![
            "publication",
            command,
            "--library",
            temp.path().to_str().unwrap(),
        ];
        if let Some(id) = id {
            args.extend(["--publication", id]);
        }
        args.extend(["--format", "json"]);
        assert!(run(&workspace, &args).status.success());
        *args.last_mut().unwrap() = "markdown";
        assert!(run(&workspace, &args).status.success());
    }
    let release_request = temp.path().join("release.json");
    fs::write(
        &release_request,
        serde_json::json!({
            "schema_version":1,"publication_id":publication_id,"target":"yuanfang_public_v1",
            "field_allowlist":["title","citations"],"published_at":"2026-07-31T01:00:00Z",
            "actor_kind":"agent","actor_id":"release_actor","tool_id":"tool","tool_version":"1",
            "config_digest":format!("sha256:{}", "2".repeat(64))
        })
        .to_string(),
    )
    .unwrap();
    let release_args = [
        "publication",
        "release-public",
        "--library",
        temp.path().to_str().unwrap(),
        "--request",
        release_request.to_str().unwrap(),
    ];
    let released = run(&workspace, &release_args);
    assert!(released.status.success());
    let released: serde_json::Value = serde_json::from_slice(&released.stdout).unwrap();
    assert_eq!(released["status"], "created");
    let release_id = released["release_id"].as_str().unwrap();
    let replay = run(&workspace, &release_args);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&replay.stdout).unwrap()["status"],
        "replayed"
    );
    for format in ["json", "markdown"] {
        let output = run(
            &workspace,
            &[
                "publication",
                "show-public",
                "--library",
                temp.path().to_str().unwrap(),
                "--release",
                release_id,
                "--format",
                format,
            ],
        );
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            !text.contains("ANALYSIS_PRIVATE_SENTINEL")
                && !text.contains(&item_id)
                && !text.contains("provenance")
        );
    }
    let projection: serde_json::Value = serde_json::from_slice(
        &run(
            &workspace,
            &[
                "publication",
                "show-public",
                "--library",
                temp.path().to_str().unwrap(),
                "--release",
                release_id,
                "--format",
                "json",
            ],
        )
        .stdout,
    )
    .unwrap();
    assert!(projection.get("summary").is_none() && projection.get("body_markdown").is_none());
    assert!(projection["citations"].as_array().unwrap().len() == 1);
    assert!(temp
        .path()
        .join("publications/public")
        .join(release_id)
        .join("manifest.json")
        .exists());
    assert!(!temp.path().join("publications/latest").exists());
}

#[test]
fn invalid_publication_and_release_requests_create_no_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = workspace_root();
    let request = temp.path().join("bad.json");
    fs::write(&request, b"{}").unwrap();
    for command in ["create", "release-public"] {
        let output = run(
            &workspace,
            &[
                "publication",
                command,
                "--library",
                temp.path().to_str().unwrap(),
                "--request",
                request.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success());
    }
    assert!(!temp.path().join("publications").exists());
}
