use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

fn write_html_bundle(root: &Path, bodies: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
    let artifacts = root.join("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let descriptors = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            let locator = format!("artifacts/body-{index}.html");
            fs::write(root.join(&locator), body).unwrap();
            serde_json::json!({
                "schema_version": 1,
                "locator": locator,
                "media_type": "text/html; charset=utf-8",
                "byte_size": body.len(),
                "sha256": format!("{:x}", Sha256::digest(body.as_bytes())),
            })
        })
        .collect::<Vec<_>>();
    let source = root.join("source.json");
    let capture = root.join("capture.json");
    fs::write(
        &source,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "source_id": "source_web_markdown_test",
            "name": "Synthetic Web Markdown",
            "source_kind": "web",
            "canonical_uris": ["https://example.test/articles/one"],
            "evidence_roles": ["primary"],
            "default_sensitivity": "internal",
            "preferred_extractor": "generic.webpage"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        &capture,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "adapter_id": "synthetic.web.capture",
            "adapter_version": "1",
            "external_capture_id": "web-markdown-capture-1",
            "source_id": "source_web_markdown_test",
            "captured_at": "2026-08-09T00:00:00Z",
            "capture_method": "fixture",
            "sensitivity": "internal",
            "route_summary": null,
            "title": null,
            "source_url": "https://example.test/articles/one",
            "canonical_url": "https://example.test/articles/one",
            "artifacts": descriptors
        }))
        .unwrap(),
    )
    .unwrap();
    (source, capture)
}

#[test]
fn cli_ingest_writes_library_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(root.join("examples/phase1-evidence/source.json"))
        .arg("--capture")
        .arg(root.join("examples/phase1-evidence/capture.json"))
        .arg("--library")
        .arg(tmp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let item_id = receipt["item_ids"][0].as_str().unwrap();
    assert!(tmp
        .path()
        .join("items")
        .join(item_id)
        .join("index.md")
        .exists());
    assert!(tmp
        .path()
        .join("items")
        .join(item_id)
        .join("index.json")
        .exists());
    assert!(tmp
        .path()
        .join("items")
        .join(item_id)
        .join("attachments")
        .is_dir());
    let replay = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(root.join("examples/phase1-evidence/source.json"))
        .arg("--capture")
        .arg(root.join("examples/phase1-evidence/capture.json"))
        .arg("--library")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(receipt["item_ids"], replay["item_ids"]);
    assert_eq!(replay["idempotent_replay"], true);
    for format in ["json", "markdown", "portable-json"] {
        let show = Command::new(env!("CARGO_BIN_EXE_memex"))
            .args(["show", "--library"])
            .arg(tmp.path())
            .args(["--item", item_id, "--format", format])
            .output()
            .unwrap();
        assert!(show.status.success());
        assert!(String::from_utf8_lossy(&show.stdout).contains(item_id));
        if format == "json" {
            let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
            assert!(value.is_object());
            assert_eq!(value["item"]["item_id"], item_id);
            assert_eq!(value["current_revision"]["item_id"], item_id);
        } else if format == "portable-json" {
            let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
            assert_eq!(value["projection"], "memex.portable-item");
            assert_eq!(value["projection_version"], 1);
            assert_eq!(value["item_id"], item_id);
            assert_eq!(value["content"]["path"], "index.md");
            assert!(value.get("content_markdown").is_none());
            assert_eq!(
                show.stdout,
                fs::read(tmp.path().join("items").join(item_id).join("index.json")).unwrap()
            );
            assert!(show.stdout.ends_with(b"\n"));
            assert!(!show.stdout.ends_with(b"\n\n"));
        }
    }
}

#[test]
fn concurrent_ingests_keep_both_catalog_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let one_dir = tmp.path().join("one");
    let two_dir = tmp.path().join("two");
    std::fs::create_dir_all(&one_dir).unwrap();
    std::fs::create_dir_all(&two_dir).unwrap();
    let one = one_dir.join("capture.json");
    let two = two_dir.join("capture.json");
    let capture =
        std::fs::read_to_string(root.join("examples/phase1-evidence/capture.json")).unwrap();
    std::fs::write(
        &one,
        capture.replace("fixture-capture-001", "concurrent-one"),
    )
    .unwrap();
    let source =
        std::fs::read_to_string(root.join("examples/phase1-evidence/source.json")).unwrap();
    std::fs::write(one_dir.join("source.json"), &source).unwrap();
    std::fs::write(two_dir.join("source.json"), source).unwrap();
    std::fs::create_dir_all(one_dir.join("artifacts")).unwrap();
    std::fs::create_dir_all(two_dir.join("artifacts")).unwrap();
    let body = std::fs::read(root.join("examples/phase1-evidence/artifacts/body.txt")).unwrap();
    std::fs::write(one_dir.join("artifacts/body.txt"), &body).unwrap();
    std::fs::write(two_dir.join("artifacts/body.txt"), body).unwrap();
    std::fs::write(
        &two,
        capture.replace("fixture-capture-001", "concurrent-two"),
    )
    .unwrap();
    let command = |capture: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .args(["ingest", "--source"])
            .arg(capture.parent().unwrap().join("source.json"))
            .arg("--capture")
            .arg(capture)
            .arg("--library")
            .arg(tmp.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first_child = command(&one);
    let second_child = command(&two);
    let first = first_child.wait_with_output().unwrap();
    let second = second_child.wait_with_output().unwrap();
    assert!(first.status.success());
    assert!(second.status.success());
    let first_receipt: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let second_receipt: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first_receipt["capture_id"], second_receipt["capture_id"]);
    assert_ne!(first_receipt["item_ids"], second_receipt["item_ids"]);
    assert_ne!(
        first_receipt["revision_ids"],
        second_receipt["revision_ids"]
    );
    let catalog: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("catalog/ingest-keys.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog.as_object().unwrap().len(), 2);
    for (capture, receipt) in [(&one, first_receipt), (&two, second_receipt)] {
        let replay = Command::new(env!("CARGO_BIN_EXE_memex"))
            .args(["ingest", "--source"])
            .arg(capture.parent().unwrap().join("source.json"))
            .arg("--capture")
            .arg(capture)
            .arg("--library")
            .arg(tmp.path())
            .output()
            .unwrap();
        assert!(replay.status.success());
        let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["capture_id"], receipt["capture_id"]);
        assert_eq!(replay["item_ids"], receipt["item_ids"]);
    }
}

#[test]
fn cli_ingest_extracts_verified_html_and_replays() {
    let temporary = tempfile::tempdir().unwrap();
    let bundle = temporary.path().join("bundle");
    let library = temporary.path().join("library");
    fs::create_dir_all(&bundle).unwrap();
    fs::create_dir_all(&library).unwrap();
    let html = r#"<!doctype html><html lang="zh-CN"><head>
<title>HTML 页面标题</title><meta property="og:title" content="移动阅读需要稳定 Markdown">
</head><body><nav>PRIVATE_NAV_SENTINEL</nav><article>
<h1>移动阅读需要稳定 Markdown</h1>
<p>移动设备需要快速载入干净的正文，并允许用户圈选一小部分内容交给 AI 分析追问。这个合成段落提供足够长度来稳定通过正文识别。</p>
<p><a href="../evidence">查看证据</a></p>
<figure><img src="/assets/reader.png" alt="移动阅读器"><figcaption>阅读界面</figcaption></figure>
</article><script>PRIVATE_SCRIPT_SENTINEL</script></body></html>"#;
    let (source, capture) = write_html_bundle(&bundle, &[html]);
    let run_ingest = || {
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .args(["ingest", "--source"])
            .arg(&source)
            .arg("--capture")
            .arg(&capture)
            .arg("--library")
            .arg(&library)
            .output()
            .unwrap()
    };

    let first = run_ingest();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["idempotent_replay"], false);
    let item_id = first["item_ids"][0].as_str().unwrap();

    let show_json = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["show", "--library"])
        .arg(&library)
        .args(["--item", item_id, "--format", "json"])
        .output()
        .unwrap();
    assert!(show_json.status.success());
    let item: serde_json::Value = serde_json::from_slice(&show_json.stdout).unwrap();
    let revision = &item["current_revision"];
    assert_eq!(revision["title"], "移动阅读需要稳定 Markdown");
    assert_eq!(revision["provenance"][0]["tool_id"], "generic.webpage");
    assert_eq!(revision["provenance"][0]["tool_version"], "2");
    let markdown = revision["content_markdown"].as_str().unwrap();
    assert!(markdown.contains("移动设备需要快速载入干净的正文"));
    assert!(markdown.contains("https://example.test/evidence"));
    assert!(markdown.contains("https://example.test/assets/reader.png"));
    assert!(!markdown.contains("PRIVATE_NAV_SENTINEL"));
    assert!(!markdown.contains("PRIVATE_SCRIPT_SENTINEL"));
    assert!(!markdown.contains(temporary.path().to_str().unwrap()));

    // A separate CLI process and clean library must produce byte-identical Markdown.
    let second_library = temporary.path().join("second-library");
    fs::create_dir_all(&second_library).unwrap();
    let second_ingest = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(&source)
        .arg("--capture")
        .arg(&capture)
        .arg("--library")
        .arg(&second_library)
        .output()
        .unwrap();
    assert!(second_ingest.status.success());
    let second_receipt: serde_json::Value = serde_json::from_slice(&second_ingest.stdout).unwrap();
    let second_item_id = second_receipt["item_ids"][0].as_str().unwrap();
    let second_show = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["show", "--library"])
        .arg(&second_library)
        .args(["--item", second_item_id, "--format", "json"])
        .output()
        .unwrap();
    assert!(second_show.status.success());
    let second_item: serde_json::Value = serde_json::from_slice(&second_show.stdout).unwrap();
    assert_eq!(
        second_item["current_revision"]["content_markdown"],
        revision["content_markdown"]
    );

    let replay = run_ingest();
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["capture_id"], first["capture_id"]);
    assert_eq!(replay["item_ids"], first["item_ids"]);
    assert_eq!(replay["revision_ids"], first["revision_ids"]);
}

#[test]
fn cli_ingest_rejects_ambiguous_html_before_owner_write() {
    let temporary = tempfile::tempdir().unwrap();
    let bundle = temporary.path().join("bundle");
    let library = temporary.path().join("library");
    fs::create_dir_all(&bundle).unwrap();
    fs::create_dir_all(&library).unwrap();
    let (source, capture) = write_html_bundle(
        &bundle,
        &[
            "<article><p>First ambiguous HTML artifact with enough readable content for extraction.</p></article>",
            "<article><p>Second ambiguous HTML artifact with enough readable content for extraction.</p></article>",
        ],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(source)
        .arg("--capture")
        .arg(capture)
        .arg("--library")
        .arg(&library)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("capture contains multiple HTML artifacts"));
    assert!(output.stdout.is_empty());
    assert!(!library.join("items").exists());
    assert!(!library.join("catalog").exists());
}
