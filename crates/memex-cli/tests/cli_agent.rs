#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    process::{Command, Stdio},
};

#[test]
fn capture_sentence_confirms_redacted_shape_and_sends_only_the_url() {
    let home = tempfile::tempdir().unwrap();
    let app = home.path().join("Library/Application Support").join(memex_agent::config::DATA_DIR_NAME);
    fs::create_dir_all(&app).unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = app.join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut body = vec![0; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut body).unwrap();
        let command: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(command["command"], "enqueue");
        assert_eq!(
            command["url"],
            "https://article.test/nested/path?access_token=synthetic-secret"
        );
        assert!(command["producer_job_id"]
            .as_str()
            .unwrap()
            .starts_with("cli_"));
        let response = serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "job": {
                "job_id": "job_syntheticopaqueid",
                "state": "queued",
                "stage": "extract",
                "reason_code": null,
                "item_id": null,
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z",
                "attempt_count": 0,
                "preview": null
            },
            "jobs": null,
            "reason_code": null
        }))
        .unwrap();
        stream
            .write_all(&(response.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&response).unwrap();
    });

    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .env("HOME", home.path())
        .args([
            "capture",
            "Please capture https://article.test/nested/path?access_token=synthetic-secret, thanks.",
            "--yes",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("article.test"));
    assert!(stdout.contains("nested"));
    assert!(stdout.contains("job_syntheticopaqueid"));
    assert!(!stdout.contains("access_token"));
    assert!(!stdout.contains("synthetic-secret"));
    assert!(!stdout.contains("/nested/path"));
}

#[test]
fn capture_without_yes_requires_affirmative_confirmation() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_memex"))
        .env("HOME", home.path())
        .args(["capture", "请收藏：https://article.test/文章。谢谢"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"n\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("article.test"));
    assert!(!stdout.contains("/文章"));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("cancelled_by_user"));
}

#[test]
fn capture_reuses_one_request_id_after_an_ambiguous_response_loss() {
    let home = tempfile::tempdir().unwrap();
    let app = home.path().join("Library/Application Support").join(memex_agent::config::DATA_DIR_NAME);
    fs::create_dir_all(&app).unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = app.join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let server = std::thread::spawn(move || {
        let mut commands = Vec::new();
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0u8; 4];
            stream.read_exact(&mut length).unwrap();
            let mut body = vec![0; u32::from_be_bytes(length) as usize];
            stream.read_exact(&mut body).unwrap();
            commands.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
            if attempt == 0 {
                continue;
            }
            let response = serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "job": {
                    "job_id": "job_replayedopaqueid",
                    "state": "queued",
                    "stage": "extract",
                    "reason_code": null,
                    "item_id": null,
                    "created_at": "2026-08-11T00:00:00Z",
                    "updated_at": "2026-08-11T00:00:00Z",
                    "attempt_count": 0,
                    "preview": null
                },
                "jobs": null,
                "reason_code": null
            }))
            .unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        }
        commands
    });

    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .env("HOME", home.path())
        .args([
            "capture",
            "Capture https://article.test/one now.",
            "--yes",
            "--request-id",
            "cli_stable_synthetic",
        ])
        .output()
        .unwrap();
    let commands = server.join().unwrap();
    assert!(output.status.success());
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["producer_job_id"], "cli_stable_synthetic");
    assert_eq!(commands[1]["producer_job_id"], "cli_stable_synthetic");
    assert_eq!(commands[0]["url"], commands[1]["url"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("cli_stable_synthetic"));
    assert!(stdout.contains("job_replayedopaqueid"));
    assert!(!stdout.contains("/one"));
}

#[test]
fn library_query_cli_uses_real_isolated_daemon_and_expires_views_after_store_edit() {
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let library = root.join("library");
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/phase1-evidence");
    let ingest = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(fixture.join("source.json"))
        .arg("--capture")
        .arg(fixture.join("capture.json"))
        .arg("--library")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&ingest.stdout).unwrap();
    let socket = root.join("agent.sock");
    let config = root.join("config.json");
    fs::write(
        &config,
        serde_json::to_vec(&memex_agent::AgentConfig {
            library: library.clone(),
            spool: root.join("spool"),
            database: root.join("agent.db"),
            socket: socket.clone(),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .env("HOME", &root)
            .args(["daemon", "run", "--config"])
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let start = std::time::Instant::now();
    while !socket.exists() {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "isolated daemon exited"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "daemon did not bind"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let query = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .env("HOME", &root)
            .args(["library-query", "--socket"])
            .arg(&socket)
            .args([
                "--query",
                "synthetic",
                "--max-sensitivity",
                "public",
                "--limit",
                "1",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let first = query(&["--sort", "source", "--source", "https://example.test"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["library_query"]["total"], 1);
    assert_eq!(
        first["library_query"]["items"][0]["item_id"],
        receipt["item_ids"][0]
    );
    assert_eq!(
        first["library_query"]["items"][0]["title"],
        "Phase 1 Synthetic Evidence"
    );
    let view = first["library_query"]["view"].as_str().unwrap();
    memex_store::revise_item_metadata(
        &library,
        receipt["item_ids"][0].as_str().unwrap(),
        memex_store::ItemMetadataUpdate {
            expected_revision_id: receipt["revision_ids"][0].as_str().unwrap().into(),
            title: "Renamed synthetic".into(),
            manual_tags: vec![],
        },
    )
    .unwrap();
    let stale = query(&[
        "--sort",
        "source",
        "--source",
        "https://example.test",
        "--view",
        view,
    ]);
    assert!(!stale.status.success());
    let stale: serde_json::Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["reason_code"], "stale_query_view");
    assert!(stale.get("library_query").is_none());
    let refreshed = query(&[
        "--sort",
        "captured_at",
        "--descending",
        "--item-type",
        "webpage",
    ]);
    assert!(refreshed.status.success());
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["library_query"]["total"], 1);
    assert_eq!(
        refreshed["library_query"]["items"][0]["title"],
        "Renamed synthetic"
    );
    let missing = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["library-query", "--socket"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
}

#[test]
fn real_daemon_delivers_large_utf8_context_without_truncation() {
    use sha2::{Digest, Sha256};
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/phase1-evidence");
    let body = "中文🧭\\\n".repeat(1000);
    assert!(body.len() <= 16 * 1024);
    fs::create_dir(root.join("artifacts")).unwrap();
    fs::copy(fixture.join("source.json"), root.join("source.json")).unwrap();
    fs::write(root.join("artifacts/body.txt"), &body).unwrap();
    let mut capture: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.join("capture.json")).unwrap()).unwrap();
    capture["artifacts"][0]["byte_size"] = body.len().into();
    capture["artifacts"][0]["sha256"] = format!("{:x}", Sha256::digest(body.as_bytes())).into();
    fs::write(
        root.join("capture.json"),
        serde_json::to_vec(&capture).unwrap(),
    )
    .unwrap();
    let library = root.join("library");
    let result = Command::new(env!("CARGO_BIN_EXE_memex"))
        .arg("ingest")
        .arg("--library")
        .arg(&library)
        .arg("--source")
        .arg(root.join("source.json"))
        .arg("--capture")
        .arg(root.join("capture.json"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let (_, revision) =
        memex_store::read_item(&library, receipt["item_ids"][0].as_str().unwrap()).unwrap();
    assert!(
        revision.content_markdown.len() > 8 * 1024 && revision.content_markdown.len() <= 16 * 1024
    );
    let socket = root.join("agent.sock");
    let config = root.join("config.json");
    fs::write(
        &config,
        serde_json::to_vec(&memex_agent::AgentConfig {
            library,
            spool: root.join("spool"),
            database: root.join("agent.db"),
            socket: socket.clone(),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .env("HOME", &root)
            .args(["daemon", "run", "--config"])
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() {
        assert!(daemon.0.try_wait().unwrap().is_none());
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let response = memex_agent::send_command(
        &socket,
        &serde_json::from_value(serde_json::json!({
            "command":"context_read", "item_id":revision.item_id,
            "expected_revision_id":revision.revision_id, "start_byte":0,
            "end_byte":revision.content_markdown.len(), "max_sensitivity":"public"
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(response.ok);
    assert_eq!(
        response.context.unwrap().content_markdown,
        revision.content_markdown
    );
}

#[test]
fn production_daemon_wires_authenticated_chrome_url_jobs() {
    for destination in [None, Some("inbox"), Some("selected_group")] {
        production_daemon_destination(destination);
    }
}

fn production_daemon_destination(destination: Option<&str>) {
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let secret = "b".repeat(64);
    let secret_file = root.join("secret");
    fs::write(&secret_file, &secret).unwrap();
    fs::set_permissions(&secret_file, fs::Permissions::from_mode(0o600)).unwrap();
    let config = root.join("config.json");
    let socket = root.join("agent.sock");
    fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "library":root.join("library"), "spool":root.join("spool"),
        "database":root.join("agent.db"), "socket":socket,
        "intake":{"snapshots":root.join("snapshots"),"secret_file":secret_file,
                  "origin":origin,"port":port}
    })).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let mut daemon = Daemon(Command::new(env!("CARGO_BIN_EXE_memex"))
        .env("HOME", &root).args(["daemon", "run", "--config"]).arg(&config)
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() { break; }
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");
        assert!(Instant::now() < deadline, "intake did not bind");
        std::thread::sleep(Duration::from_millis(20));
    }
    let post = |route: &str, body: &str, request_origin: &str| {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let nonce = format!("{}{}", uuid::Uuid::now_v7().simple(), uuid::Uuid::now_v7().simple());
        write!(stream, "POST /api/v1/{route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {request_origin}\r\nContent-Type: application/json\r\nX-MemeX-Intake-Secret: {secret}\r\nX-MemeX-Intake-Nonce: {nonce}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new(); stream.read_to_string(&mut response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        (headers.lines().next().unwrap().to_owned(), serde_json::from_str::<serde_json::Value>(body).unwrap())
    };
    let (status, caps) = post("capabilities", r#"{"schema_version":1}"#, origin);
    assert!(status.contains("200"));
    for feature in ["url-jobs-v1", "url-jobs-no-generation-v1", "url-jobs-enrich-v1", "url-jobs-group-picker-v1", "url-jobs-active-group-v1", "url-jobs-destination-v1"] {
        assert!(caps["features"].as_array().unwrap().contains(&feature.into()), "missing {feature}");
    }
    assert!(post("capabilities", r#"{"schema_version":1}"#, "https://foreign.invalid").0.contains("403"));
    let (_, missing) = post("url-jobs/status", r#"{"schema_version":1,"job_id":"job_missing"}"#, origin);
    assert_eq!(missing["error"], "resource_not_found");
    let response = memex_agent::send_command(&socket, &serde_json::from_value(serde_json::json!({"command":"capabilities"})).unwrap()).unwrap();
    assert!(response.ok);
    assert!(response.capabilities.unwrap().features.contains(&"item_cover_v1".into()));
    // Exercise HTTP enqueue and this exact daemon's worker without external
    // networking: restore already-fetched synthetic artifacts while origin
    // pacing holds the job, then restart the production entry point.
    use memex_core::Sensitivity;
    use sha2::{Digest, Sha256};
    let library = root.join("library");
    let group = memex_store::edit_groups(&library, "fixture-root", memex_store::GroupEditOp::CreateRoot {
        name: "Main".into(), sensitivity: Sensitivity::Internal,
    }, Sensitivity::Restricted).unwrap().group_id;
    let version = memex_store::with_consistent_snapshot(&library, |root| {
        Ok(memex_store::read_visible_groups(root, Sensitivity::Restricted)?.nodes.into_iter().find(|n| n.group_id == group).unwrap().version_token())
    }).unwrap();
    let target = memex_store::edit_groups(&library, "fixture-target", memex_store::GroupEditOp::CreateFolder {
        parent_id: group, expected_parent_version: version, name: "AmpCode".into(),
    }, Sensitivity::Restricted).unwrap().group_id;
    assert!(post("active-group", r#"{"schema_version":1}"#, origin).1["group_id"].is_null());
    let select = serde_json::from_value(serde_json::json!({"command":"ui_active_group_set",
        "request_id":"fixture-ui-selection", "group_id":target, "max_sensitivity":"restricted"})).unwrap();
    let selected_receipt = memex_agent::send_command(&socket, &select).unwrap();
    assert!(selected_receipt.ok, "{selected_receipt:?}");
    let (status, active) = post("active-group", r#"{"schema_version":1}"#, origin);
    assert!(status.contains("200"));
    assert_eq!(active["group_id"], target);
    assert_eq!(active["path"], serde_json::json!(["Main", "AmpCode"]));
    assert_eq!(active["writable"], true);
    assert_eq!(active.as_object().unwrap().len(), 6);
    assert!(post("active-group", r#"{"schema_version":1}"#, "https://foreign.invalid").0.contains("403"));
    assert!(post("active-group", r#"{"schema_version":1,"extra":true}"#, origin).0.contains("400"));
    let db = rusqlite::Connection::open(root.join("agent.db")).unwrap();
    if destination == Some("inbox") {
        // Longer than Queue's five-second busy timeout. A competing writer
        // cannot take down the production IPC/Intake process.
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        std::thread::sleep(Duration::from_secs(6));
        let exited = daemon.0.try_wait().unwrap();
        db.execute_batch("ROLLBACK").unwrap();
        assert!(exited.is_none(), "daemon exited during queue contention: {exited:?}");
        assert!(post("capabilities", r#"{"schema_version":1}"#, origin).0.contains("200"));
    }
    db.execute("INSERT INTO capture_origins(origin,next_fetch_at) VALUES('https://ampcode.com',4102444800)", []).unwrap();
    let mut payload = serde_json::json!({"schema_version":1,"request_key":format!("chrome_capture_{}", "c".repeat(32)),
        "url":"https://ampcode.com/podcast/episode-1", "include_comments":false,"quality_check":"off",
        "generation_policy":"none","target_group_id":target,"remove_from_group_ids":[]});
    let (status, picker) = post("groups", r#"{"schema_version":1}"#, origin);
    assert!(status.contains("200"));
    let nodes = picker["nodes"].as_array().unwrap();
    let selected = nodes.iter().find(|node| node["group_id"] == target).unwrap();
    assert_eq!(selected["writable"], true);
    assert_eq!(selected["kind"], "collection");
    assert_eq!(selected.as_object().unwrap().len(), 6);
    assert_eq!(picker["groups_version"], post("groups", r#"{"schema_version":1}"#, origin).1["groups_version"]);
    assert!(post("groups", r#"{"schema_version":1}"#, "https://foreign.invalid").0.contains("403"));
    assert!(post("groups", r#"{"schema_version":1,"extra":true}"#, origin).0.contains("400"));
    let mut invalid = payload.clone();
    for bad_target in [format!("group_{}", "a".repeat(32)), selected["root_group_id"].as_str().unwrap().to_owned()] {
        invalid["target_group_id"] = bad_target.into();
        for _ in 0..2 {
            let (status, rejection) = post("url-jobs/enqueue", &invalid.to_string(), origin);
            assert!(status.contains("400"), "{rejection}");
            assert_eq!(rejection["error"], "invalid_request");
        }
    }
    assert_eq!(db.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT count(*) FROM chrome_episode_requests", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    if let Some(mode) = destination {
        payload["destination_mode"] = mode.into();
        if mode == "inbox" { payload.as_object_mut().unwrap().remove("target_group_id"); }
        else { payload["target_group_id"] = selected["root_group_id"].clone(); }
    }
    let payload = payload.to_string();
    let (status, receipt) = post("url-jobs/enqueue", &payload, origin);
    assert!(status.contains("200"), "{receipt}");
    let job = receipt["job_id"].as_str().unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM jobs WHERE job_id=?1 AND state='queued'", [job], |r| r.get::<_,u64>(0)).unwrap(), 1);
    daemon.0.kill().unwrap(); daemon.0.wait().unwrap();
    let artifact = |bytes: &[u8], media: &str| {
        let digest = format!("{:x}", Sha256::digest(bytes));
        let dir = root.join("spool/cas").join(&digest[..2]); fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(&digest), bytes).unwrap();
        fs::set_permissions(dir.join(&digest), fs::Permissions::from_mode(0o600)).unwrap();
        serde_json::json!({"digest":digest,"size":bytes.len(),"media":media})
    };
    let rss = br#"<rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><item><guid>synthetic-episode</guid><link>https://ampcode.com/podcast/episode-1</link><itunes:season>1</itunes:season><itunes:episode>1</itunes:episode><pubDate>Sun, 08 Nov 2026 23:30:00 -0800</pubDate><itunes:duration>601</itunes:duration></item></channel></rss>"#;
    let html = b"<article><h1>Synthetic engineering podcast</h1><p>This synthetic transcript contains enough deterministic prose for a stable capture. Engineers discuss careful experiments and reproducible testing. This source contains no private data and needs no external network or language model to enter the isolated store.</p></article>";
    let work = serde_json::json!({"rss":artifact(rss,"application/rss+xml"),"page":artifact(html,"text/html"),
        "captured_at":"2026-11-09T08:00:00Z","cover":null,"cover_source":null,"cover_checked":true,
        "item_id":null,"metadata_request":null,"metadata_done":false,"conflicts":[],"cover_edit":null,"cover_status":null,"archive_request":null});
    db.execute("INSERT INTO chrome_episode_work(job_id,payload) VALUES(?1,?2)", rusqlite::params![job,work.to_string()]).unwrap();
    db.execute("UPDATE capture_origins SET next_fetch_at=0", []).unwrap();
    db.execute("UPDATE jobs SET next_attempt_at=0 WHERE job_id=?1", [job]).unwrap();
    for _ in 0..2 {
        daemon.0 = Command::new(env!("CARGO_BIN_EXE_memex")).env("HOME", &root)
            .args(["daemon", "run", "--config"]).arg(&config).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let state: String = db.query_row("SELECT state FROM jobs WHERE job_id=?1", [job], |r| r.get(0)).unwrap();
            if state == "succeeded" && TcpStream::connect(("127.0.0.1", port)).is_ok() { break; }
            assert!(daemon.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "worker did not finish: {state}");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(post("url-jobs/enqueue", &payload, origin).1["job_id"], job);
        let items = memex_store::list_items(&library).unwrap(); assert_eq!(items.len(),1);
        assert_eq!(post("active-group", r#"{"schema_version":1}"#, origin).1["group_id"], target);
        let groups = memex_store::read_visible_groups(&library, Sensitivity::Restricted).unwrap();
        if destination == Some("inbox") { assert!(groups.memberships.is_empty()); }
        else {
            let expected = if destination.is_some() { selected["root_group_id"].as_str().unwrap() } else { target.as_str() };
            assert_eq!(groups.memberships.len(), 1);
            assert_eq!(groups.memberships[0].group_id, expected);
        }
        assert_eq!(items[0].1.metadata.duration_seconds.as_ref().unwrap().value,601);
        assert_eq!(items[0].1.metadata.published_partial_date.as_ref().unwrap().value.value.as_deref(),Some("2026-11-08"));
        let analysis = rusqlite::Connection::open(library.join("analysis/translation-jobs.sqlite3")).unwrap();
        for table in ["translation_jobs","summary_jobs"] {
            assert_eq!(analysis.query_row(&format!("SELECT coalesce(sum(attempt_count),0)+coalesce(sum(provider_authorized),0) FROM {table}"), [], |r| r.get::<_,u64>(0)).unwrap(),0);
        }
        assert_eq!(db.query_row("SELECT translation_hook_completed FROM jobs WHERE job_id=?1", [job], |r| r.get::<_,u64>(0)).unwrap(),1);
        daemon.0.kill().unwrap(); daemon.0.wait().unwrap();
    }
}

#[test]
fn jobs_recent_reads_capture_progress_and_prints_labels_without_content() {
    let home = tempfile::tempdir().unwrap();
    let app = home.path().join("Library/Application Support").join(memex_agent::config::DATA_DIR_NAME);
    fs::create_dir_all(&app).unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = app.join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut body = vec![0; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut body).unwrap();
        let command: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            command,
            serde_json::json!({"command": "capture_jobs_recent", "max_sensitivity": "restricted", "limit": 7})
        );
        let response = serde_json::to_vec(&serde_json::json!({
            "ok": true, "job": null, "jobs": null, "next_cursor": null, "reason_code": null,
            "capture_jobs": {
                "schema_version": 1,
                "jobs": [{
                    "job_id": "job_synthetic_run", "url": "https://podcast.test/episodes/ep-7",
                    "state": "leased", "stage": "fetch", "reason_code": null,
                    "created_at": "2026-11-09T00:00:00+00:00", "updated_at": "2026-11-09T00:00:05+00:00",
                    "attempt_count": 1, "result_item_id": null, "next_attempt_at": null, "producer": "chrome"
                }],
                "counts": {"queued": 9, "leased": 1, "artifact_ready": 0, "needs_user": 0,
                    "retry_wait": 0, "succeeded": 3, "failed": 0, "cancelled": 0}
            }
        }))
        .unwrap();
        stream.write_all(&(response.len() as u32).to_be_bytes()).unwrap();
        stream.write_all(&response).unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .env("HOME", home.path())
        .args(["jobs", "recent", "--limit", "7"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["capture_jobs"]["counts"]["queued"], 9);
    let progress = &value["progress"];
    assert_eq!(progress["summary_line"], "进行中 1 · 排队 9");
    assert_eq!(progress["busy"], true);
    assert_eq!(progress["jobs"][0]["title"], "ep-7");
    assert_eq!(progress["jobs"][0]["state_label"], "抓取中");
    assert_eq!(progress["jobs"][0]["category"], "running");
}
