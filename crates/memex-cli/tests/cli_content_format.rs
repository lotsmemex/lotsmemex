#![cfg(unix)]

use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

fn cli(socket: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["content-format", "--socket"])
        .arg(socket)
        .args(["--max-sensitivity", "private"])
        .args(args)
        .output()
        .unwrap()
}

// Synthetic framed owner socket verifies the public CLI wire contract, not model execution.
fn exchange(root: &Path, args: &[&str], response: Value) -> (Output, Value) {
    let socket = root.join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("synthetic socket did not receive request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut size = [0; 4];
        stream.read_exact(&mut size).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(size) as usize];
        stream.read_exact(&mut bytes).unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let bytes = serde_json::to_vec(&response).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
        request
    });
    let output = cli(&socket, args);
    let request = server.join().unwrap();
    fs::remove_file(socket).unwrap();
    assert_eq!(request["command"], "reader");
    assert_eq!(request["max_sensitivity"], "private");
    assert!(request["principal"].is_null());
    (output, request["request"].clone())
}

#[test]
fn commands_use_owner_contract_without_implicit_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let selection = root.path().join("selection.json");
    let selected = json!({"kind":"group","group_id":"group_fixture","include_descendants":false});
    fs::write(&selection, serde_json::to_vec(&selected).unwrap()).unwrap();
    let cases = vec![
        (
            vec!["preview", "--selection", selection.to_str().unwrap()],
            json!({"op":"format_preview","selection":selected}),
        ),
        (
            vec!["preview", "--preview-id", "preview_fixture"],
            json!({"op":"format_preview","preview_id":"preview_fixture"}),
        ),
        (
            vec!["targets", "--preview-id", "p"],
            json!({"op":"format_targets","preview_id":"p","offset":0,"limit":100}),
        ),
        (
            vec![
                "request",
                "--quote-id",
                "q",
                "--request-id",
                "r",
                "--allow-model",
                "--accept-cost",
            ],
            json!({"op":"format_request","quote_id":"q","request_id":"r","accept_cost":true,"accept_model_processing":true}),
        ),
        (
            vec!["status", "--batch-id", "b"],
            json!({"op":"format_status","batch_id":"b"}),
        ),
        (
            vec![
                "results",
                "--batch-id",
                "b",
                "--offset",
                "4",
                "--limit",
                "2",
            ],
            json!({"op":"format_results","batch_id":"b","offset":4,"limit":2}),
        ),
        (
            vec![
                "changes",
                "--batch-id",
                "b",
                "--target-id",
                "t",
                "--expected-version",
                "v",
            ],
            json!({"op":"format_changes","batch_id":"b","target_id":"t","expected_version":"v","view":"list","offset":0,"limit":64}),
        ),
        (
            vec![
                "restore",
                "--batch-id",
                "b",
                "--target-id",
                "t",
                "--expected-revision-id",
                "rev",
                "--request-id",
                "r",
            ],
            json!({"op":"format_restore","batch_id":"b","target_id":"t","expected_revision_id":"rev","request_id":"r"}),
        ),
        (
            vec!["resume", "--batch-id", "b", "--allow-model"],
            json!({"op":"format_resume","batch_id":"b"}),
        ),
    ];
    for (args, expected) in cases {
        let (output, request) = exchange(
            root.path(),
            &args,
            json!({"ok":true,"reader":{"state":"paused"}}),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(request, expected);
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"state":"paused"})
        );
    }
}

#[test]
fn missing_consent_invalid_selection_and_paging_fail_before_ipc() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("absent.sock");
    let cases: &[&[&str]] = &[
        &[
            "request",
            "--quote-id",
            "q",
            "--request-id",
            "r",
            "--accept-cost",
        ],
        &[
            "request",
            "--quote-id",
            "q",
            "--request-id",
            "r",
            "--allow-model",
        ],
        &[
            "request",
            "--request-id",
            "r",
            "--allow-model",
            "--accept-cost",
        ],
        &["resume", "--batch-id", "b"],
        &["preview"],
        &["status", "--batch-id", "b", "--allow-model"],
        &["results", "--batch-id", "b", "--limit", "101"],
        &[
            "changes",
            "--batch-id",
            "b",
            "--target-id",
            "t",
            "--expected-version",
            "v",
            "--limit",
            "0",
        ],
    ];
    for args in cases {
        let output = cli(&socket, args);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("daemon_unavailable"));
    }
    let path = root.path().join("selection.json");
    for selection in [
        json!({"kind":"group","group_id":"g"}),
        json!({"kind":"group","group_id":"g","include_descendants":false,"body":"synthetic-private-body"}),
    ] {
        fs::write(&path, serde_json::to_vec(&selection).unwrap()).unwrap();
        let output = cli(&socket, &["preview", "--selection", path.to_str().unwrap()]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("invalid_content_format_request"));
        assert!(!error.contains("synthetic-private-body"));
    }
}

#[test]
fn change_text_is_explicit_private_local_export_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("change.json");
    let args = [
        "change-text",
        "--batch-id",
        "b",
        "--target-id",
        "t",
        "--expected-version",
        "v",
        "--change-id",
        "c",
        "--part",
        "before",
        "--out",
        out.to_str().unwrap(),
    ];
    let body = json!({"version":"v","change_id":"c","part":"before","total_bytes":22,"start_byte":0,"end_byte":22,"next_offset":null,"text":"synthetic-private-body"});
    let (output, request) = exchange(root.path(), &args, json!({"ok":true,"reader":body}));
    assert!(output.status.success());
    assert_eq!(request["view"], "text");
    assert_eq!(request["limit_bytes"], 8192);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{\"exported\":true}"
    );
    assert_eq!(
        fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&out).unwrap()).unwrap(),
        body
    );
    let (output, _) = exchange(
        root.path(),
        &args,
        json!({"ok":true,"reader":{"text":"different"}}),
    );
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&out).unwrap()).unwrap(),
        body
    );
}

#[test]
fn errors_never_echo_daemon_prose() {
    let root = tempfile::tempdir().unwrap();
    for (code, expected) in [
        ("synthetic-private-body", "content_format_request_failed"),
        (
            "content_format_quote_consumed",
            "content_format_quote_consumed",
        ),
    ] {
        let (output, _) = exchange(
            root.path(),
            &["status", "--batch-id", "b"],
            json!({"ok":false,"reason_code":code}),
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected));
        assert!(!error.contains("synthetic-private-body"));
    }
}
