use memex_models::{Configuration, Model, Provider};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

#[path = "../../memex-agent/examples/fixtures/reader_book.rs"]
mod reader_book;

#[test]
fn chapter_cli_reuses_bound_work_without_provider_and_keeps_plan_explicit() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = reader_book::seed(&library).unwrap();
    let db = tmp.path().join("settings.db");
    // Queue owns permissions; this synthetic endpoint must never be contacted.
    drop(memex_agent::queue::Queue::open(&db).unwrap());
    seed(&db, "http://127.0.0.1:1/v1/".into());
    ok(&["low-dial", "configure", "--database", db.to_str().unwrap(), "--model", "cheap", "--expected-revision", "0"]);
    let out = tmp.path().join("plan.json");
    let args = ["book", "repair-chapter", "--library", library.to_str().unwrap(),
        "--item", &doc.item_id, "--revision", &doc.revision_id, "--chapter", "chapter-1",
        "--max-sensitivity", "public", "--out", out.to_str().unwrap(), "--allow-model",
        "--database", db.to_str().unwrap(), "--request-id", "cli-reuse"];
    let result = ok(&args);
    assert_eq!(result["job"]["state"], "succeeded");
    assert_eq!(result["model_requests"], 0);
    assert_eq!(result["library_written"], false);
    let bytes = fs::read(&out).unwrap();
    assert!(!cli(&args).status.success(), "existing output must fail before new work");
    assert_eq!(fs::read(&out).unwrap(), bytes);
    assert_eq!(memex_store::reader::reader_document(&library, &doc.item_id, &doc.revision_id,
        memex_core::Sensitivity::Public).unwrap().canonical_sha256, doc.canonical_sha256);
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(args)
        .output()
        .unwrap()
}
fn ok(args: &[&str]) -> Value {
    let r = cli(args);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    serde_json::from_slice(&r.stdout).unwrap()
}
fn seed(path: &Path, base: String) -> Configuration {
    let configuration = Configuration {
        providers: vec![Provider {
            id: "fixture".into(),
            label: "Synthetic".into(),
            api_base: base,
        }],
        models: vec![Model {
            id: "cheap".into(),
            label: "Synthetic cheap model".into(),
            provider_id: "fixture".into(),
            model_id: "synthetic-cheap".into(),
        }],
        selections: vec![],
    };
    let mut conn = rusqlite::Connection::open(path).unwrap();
    memex_models::replace(&mut conn, 0, configuration.clone()).unwrap();
    configuration
}

#[test]
fn offline_sparse_preview_and_route_configuration_never_rewrite_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("settings.db");
    let original = seed(&db, "http://127.0.0.1:1/v1/".into());
    let db = db.to_str().unwrap();
    let configured = ok(&[
        "low-dial",
        "configure",
        "--database",
        db,
        "--model",
        "cheap",
        "--expected-revision",
        "0",
        "--max-input-bytes",
        "1024",
    ]);
    assert_eq!(configured["profile"]["reasoning_effort"], Value::Null);
    assert_eq!(configured["inference_performed"], false);
    assert_eq!(
        ok(&["low-dial", "show", "--database", db])["profile"],
        configured["profile"]
    );
    assert!(!cli(&[
        "low-dial",
        "configure",
        "--database",
        db,
        "--model",
        "cheap",
        "--expected-revision",
        "0"
    ])
    .status
    .success());
    assert_eq!(
        memex_models::read(&rusqlite::Connection::open(db).unwrap())
            .unwrap()
            .configuration,
        original
    );
    let a = tmp.path().join("a.md");
    let b = tmp.path().join("b.md");
    let out = tmp.path().join("task");
    let en = (0..60)
        .map(|i| format!("Paragraph {i}: {}\n\n", "English é. ".repeat(150)))
        .collect::<String>();
    let zh = (0..83)
        .map(|i| format!("第{i}段：{}\n\n", "中文片段。".repeat(120)))
        .collect::<String>();
    fs::write(&a, &en).unwrap();
    fs::write(&b, &zh).unwrap();
    let args = [
        "file",
        "align-samples",
        "--original",
        a.to_str().unwrap(),
        "--translation",
        b.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    let plan = ok(&args);
    assert_eq!(plan["requests_this_run"], 0);
    assert_eq!(plan["sample_count"], 3);
    assert_eq!(plan["source_paragraphs"], 60);
    assert_eq!(plan["translation_paragraphs"], 83);
    assert!(plan["sample_text_bytes"].as_u64().unwrap() <= 30 * 512);
    let positions = plan["sample_positions"].as_array().unwrap();
    assert!(positions[0]["original"][0].as_u64().unwrap() < en.len() as u64 / 3);
    assert!(positions[2]["original"][0].as_u64().unwrap() > en.len() as u64 * 2 / 3);
    assert!(plan["anchors"].as_array().unwrap().is_empty());
    assert!(!out.exists());
    let mut run = args.to_vec();
    run.extend(["--allow-model", "--database", db]);
    let result = cli(&run);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("low_dial_input_budget_exceeded"));
    assert!(!out.exists()); // Rejected before credentials, dispatch or receipts.
    assert_eq!(fs::read_to_string(&a).unwrap(), en);
    assert_eq!(fs::read_to_string(&b).unwrap(), zh);
    fs::write(&a, "A0.\n\nA1.\n\nA2.\n\nA3.\n\nA4.").unwrap();
    fs::write(&b, "B0.\n\nB1.\n\nB2.\n\nB3.\n\nB4.\n\nB5.\n\nB6.").unwrap();
    let short = ok(&args);
    assert_eq!(short["sample_count"], 3);
    assert_eq!(short["sample_text_bytes"], 36); // Twelve distinct three-byte passages.
    assert!(short["repeated_text_bytes_avoided"].as_u64().unwrap() > 0);
    assert!(!out.exists());
}

/// Explicit native integration, using one disposable scoped credential only.
/// Ordinary cargo test never touches Keychain. The loopback server is not a model.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "native disposable Keychain integration; run explicitly"]
fn native_low_dial_cache_replay_and_invalid_response_never_resend() {
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("settings.db");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = seed(
        &db,
        format!("http://{}/v1/", listener.local_addr().unwrap()),
    );
    let canonical = db.canonicalize().unwrap();
    let vault = memex_provider::credentials::Credentials::for_database(&canonical, "fixture");
    let mut digest = Sha256::new();
    use std::os::unix::ffi::OsStrExt;
    let path = canonical.as_os_str().as_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    digest.update(b"fixture");
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Command::new("security")
                .args([
                    "delete-generic-password",
                    "-s",
                    "org.memex.provider-credentials.v1",
                    "-a",
                    &self.0,
                ])
                .output();
        }
    }
    let cleanup = Cleanup(format!("{:x}", digest.finalize()));
    // Save through the real host API, so the CLI executable owns its native
    // credential. Never relax an ACL to share another executable's credential.
    let root = tmp.path().canonicalize().unwrap();
    let socket = root.join("agent.sock");
    let daemon_config = root.join("config.json");
    fs::write(
        &daemon_config,
        serde_json::to_vec(&memex_agent::AgentConfig {
            library: root.join("library"),
            spool: root.join("spool"),
            database: canonical.clone(),
            socket: socket.clone(),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        })
        .unwrap(),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&daemon_config, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_memex"))
            .args(["daemon", "run", "--config"])
            .arg(&daemon_config)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while !socket.exists() {
        if daemon.0.try_wait().unwrap().is_some() {
            let mut error = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("fixture daemon exited: {error}");
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    let saved = memex_agent::send_command(
        &socket,
        &memex_agent::Command::ProviderCredentialReplace {
            expected_revision: 1,
            provider_id: "fixture".into(),
            expected_generation: 0,
            api_key: serde_json::from_value(json!("synthetic-only-token")).unwrap(),
        },
    )
    .unwrap();
    assert!(saved.ok, "{:?}", saved.reason_code);
    drop(daemon);
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(40);
        while observed.load(Ordering::SeqCst) < 9 {
            assert!(
                Instant::now() < deadline,
                "synthetic provider request missing"
            );
            let Ok((mut s, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            s.set_nonblocking(false).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                s.read_exact(&mut b).unwrap();
                head.push(b[0]);
                assert!(head.len() < 8192);
            }
            let head = String::from_utf8(head).unwrap().to_lowercase();
            let n: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            assert!(n < 32768);
            let mut body = vec![0; n];
            s.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert!(request.get("reasoning_effort").is_none());
            assert_eq!(request["model"], "synthetic-cheap");
            let i = observed.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                let input: Value =
                    serde_json::from_str(request["messages"][1]["content"].as_str().unwrap())
                        .unwrap();
                assert_eq!(input["samples"].as_array().unwrap().len(), 3);
                assert_eq!(input["samples"][0]["original_ids"], json!([0, 1]));
                assert_eq!(input["samples"][2]["original_ids"], json!([1, 2]));
                assert_eq!(
                    input["samples"][0]["original_options"],
                    json!([[0], [1], [0, 1]])
                );
                assert_eq!(
                    input["samples"][0]["translation_options"],
                    json!([[0], [1], [2], [0, 1], [1, 2], [0, 1, 2]])
                );
                for side in ["original_candidates", "translation_candidates"] {
                    let pool = input[side].as_array().unwrap();
                    assert_eq!(pool.len(), 3);
                    assert_eq!(
                        pool.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
                        vec![json!(0), json!(1), json!(2)]
                    );
                    assert!(pool
                        .iter()
                        .all(|p| p.get("start").is_none() && p.get("end").is_none()));
                }
            }
            let mut response = match i {
                0 => {
                    json!({"matches":[{"id":0,"status":"matched","original_option":2,"translation_option":1}]})
                }
                1 => {
                    json!({"samples":[{"id":0,"status":"uncertain","issues":[],"reference_lines":[]}]})
                }
                2 => {
                    json!({"matches":[{"id":0,"status":"matched","original_ids":[u64::MAX,0],"translation_ids":[1]}]})
                }
                3 => {
                    json!({"matches":[{"id":0,"status":"matched","original_ids":[0,1,2,3,4,5],"translation_ids":[1]}]})
                }
                4 => {
                    json!({"matches":[{"id":0,"status":"matched","original_ids":[1,0],"translation_ids":[1]}]})
                }
                5 => {
                    json!({"matches":[{"id":0,"status":"uncertain","original_ids":[0],"translation_ids":[1]}]})
                }
                7 => {
                    json!({"matches":[{"id":0,"status":"matched","original_option":999,"translation_option":1}]})
                }
                8 => {
                    json!({"matches":[{"id":0,"status":"uncertain","original_option":0,"translation_option":null}]})
                }
                _ => {
                    json!({"matches":[{"id":0,"status":"matched","original_ids":[0],"translation_ids":[1],"source_text":"SOURCE_ECHO_SENTINEL"}]})
                }
            };
            if i != 1 {
                response["matches"].as_array_mut().unwrap().extend((1..=2).map(|id|
                    if i == 0 || i >= 7 {
                        json!({"id":id,"status":"uncertain","original_option":null,"translation_option":null})
                    } else {
                        json!({"id":id,"status":"uncertain","original_ids":[],"translation_ids":[]})
                    }));
            }
            let result=json!({"model":"synthetic-cheap","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":response.to_string()}}],"usage":{"prompt_tokens":17,"completion_tokens":11,"total_tokens":28}}).to_string();
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                result.len(),
                result
            )
            .unwrap();
        }
    });
    let db = db.to_str().unwrap();
    ok(&[
        "low-dial",
        "configure",
        "--database",
        db,
        "--model",
        "cheap",
        "--expected-revision",
        "0",
    ]);
    let a = tmp.path().join("a.md");
    let b = tmp.path().join("b.md");
    fs::write(&a, "Alpha.\n\nBeta long.\n\nGamma.").unwrap();
    fs::write(&b, "译者说明。\n\n甲乙合段。\n\n丙。").unwrap();
    let out = tmp.path().join("alignment");
    let args = [
        "file",
        "align-samples",
        "--original",
        a.to_str().unwrap(),
        "--translation",
        b.to_str().unwrap(),
        "--samples",
        "3",
        "--out",
        out.to_str().unwrap(),
        "--allow-model",
        "--database",
        db,
    ];
    // Diagnostics are fixture-only metadata; never print the request body.
    let initial = cli(&args);
    assert!(
        initial.status.success(),
        "requests={}, dispatched={}, failure={}",
        count.load(Ordering::SeqCst),
        out.join("dispatch.json").exists(),
        fs::read_to_string(out.join("failure.json")).unwrap_or_default()
    );
    let first: Value = serde_json::from_slice(&initial.stdout).unwrap();
    let again = ok(&args);
    assert_eq!(first["anchors"][0]["original"], json!([0, 18]));
    assert_eq!(first["anchors"][0]["translation"], json!([17, 32]));
    assert_eq!(again["anchors"], first["anchors"]);
    assert_eq!(again["requests_this_run"], 0);
    assert_eq!(again["model_receipt"]["cache_hit"], true);
    assert_eq!(first["model_receipt"]["usage"]["total_tokens"], 28);
    assert!(out.join("alignment.json").exists());
    let receipt_path = out.join("result.json");
    let clean = fs::read(&receipt_path).unwrap();
    for (field, value) in [
        ("source_text", json!("SOURCE_ECHO_SENTINEL")),
        ("model", json!("different-model")),
        ("reported_model", json!("different-model")),
        ("effort", json!("low")),
        ("usage", json!({"quote":"SOURCE_ECHO_SENTINEL"})),
        (
            "usage",
            json!({"prompt_tokens":17,"completion_tokens":11,"total_tokens":28,"quote":"SOURCE_ECHO_SENTINEL"}),
        ),
    ] {
        let mut contaminated: Value = serde_json::from_slice(&clean).unwrap();
        contaminated[field] = value;
        fs::write(&receipt_path, serde_json::to_vec(&contaminated).unwrap()).unwrap();
        let rejected = cli(&args);
        assert!(!rejected.status.success());
        assert!(!String::from_utf8_lossy(&rejected.stdout).contains("SOURCE_ECHO_SENTINEL"));
        assert!(!String::from_utf8_lossy(&rejected.stderr).contains("SOURCE_ECHO_SENTINEL"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    fs::write(&receipt_path, &clean).unwrap();
    let audit = tmp.path().join("audit");
    let args_audit = [
        "file",
        "audit-format",
        a.to_str().unwrap(),
        "--out",
        audit.to_str().unwrap(),
        "--allow-model",
        "--low-dial",
        "--database",
        db,
    ];
    assert_eq!(
        ok(&args_audit)["findings"]["samples"][0]["status"],
        "uncertain"
    );
    assert_eq!(ok(&args_audit)["model_receipt"]["requests_this_run"], 0);
    fs::write(&a, "Changed.\n\nSecond.\n\nThird.").unwrap();
    assert!(!cli(&args).status.success()); // Changed source cannot reuse an index or dispatch.
    for (i, reason) in [
        "outside_candidates",
        "count",
        "non_contiguous",
        "unmatched_has_ids",
        "schema",
        "schema",
        "schema",
    ]
    .iter()
    .enumerate()
    {
        let bad = tmp.path().join(format!("bad-{i}"));
        let mut bad_args = args.to_vec();
        bad_args[9] = bad.to_str().unwrap();
        for _ in 0..2 {
            let rejected = cli(&bad_args);
            assert!(!rejected.status.success());
            assert!(!String::from_utf8_lossy(&rejected.stdout).contains("SOURCE_ECHO_SENTINEL"));
            assert!(!String::from_utf8_lossy(&rejected.stderr).contains("SOURCE_ECHO_SENTINEL"));
            assert_eq!(count.load(Ordering::SeqCst), i + 3);
        }
        assert!(!bad.join("result.json").exists());
        assert!(!bad.join("alignment.json").exists());
        let response: Value =
            serde_json::from_slice(&fs::read(bad.join("response.json")).unwrap()).unwrap();
        assert_eq!(response["usage"]["total_tokens"], 28);
        assert_eq!(response["json_syntax_valid"], true);
        let bytes = fs::read(bad.join("failure.json")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("SOURCE_ECHO_SENTINEL"));
        let failure: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(failure["stage"], "task_validation");
        if *reason == "schema" {
            assert_eq!(failure["code"], "task_response_invalid_schema");
            assert_eq!(failure["task_error"], json!({"kind":"schema"}));
        } else {
            assert_eq!(failure["code"], "task_response_invalid_candidate_ids");
            assert_eq!(failure["task_error"]["reason"], *reason);
            assert_eq!(failure["task_error"]["original_allowed"], json!([0, 1]));
            assert_eq!(
                failure["task_error"]["translation_allowed"],
                json!([0, 1, 2])
            );
            assert!(
                failure["task_error"]["original_ids_prefix"]
                    .as_array()
                    .unwrap()
                    .len()
                    <= 4
            );
            if i == 0 {
                assert_eq!(
                    failure["task_error"]["original_ids_prefix"],
                    json!([u64::MAX, 0])
                );
            }
        }
    }
    handle.join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 9);
    drop(cleanup);
    assert_eq!(
        vault.status(&config.providers[0], 1).unwrap().state,
        memex_provider::credentials::CredentialState::Missing
    );
}
