use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Command, Stdio},
};

#[test]
#[cfg(unix)]
fn intake_serve_emits_private_safe_receipt_and_accepts_binary_capture() {
    let library = tempfile::tempdir().unwrap();
    let snapshots = tempfile::tempdir().unwrap();
    let secret_dir = tempfile::tempdir().unwrap();
    let secret_file = secret_dir.path().join("pairing-secret");
    fs::write(&secret_file, format!("{}\n", "a".repeat(64))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(snapshots.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&secret_file, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut child = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["intake", "serve", "--library"])
        .arg(library.path())
        .arg("--snapshots")
        .arg(snapshots.path())
        .arg("--secret-file")
        .arg(&secret_file)
        .args(["--origin", origin])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    for private in [
        library.path().to_str().unwrap(),
        snapshots.path().to_str().unwrap(),
        secret_file.to_str().unwrap(),
        &"a".repeat(64),
    ] {
        assert!(!line.contains(private));
    }
    let receipt: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(receipt["loopback_only"], true);
    assert_eq!(receipt["write_capable"], true);
    assert_eq!(receipt["nonce_scope"], "process_lifetime_no_eviction");
    let address = receipt["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap()
        .split_once('/')
        .unwrap()
        .0;
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "OPTIONS /api/v1/captures HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type,x-memex-intake-secret,x-memex-intake-nonce\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    assert!(response.contains(&format!("Access-Control-Allow-Origin: {origin}")));

    let html = "<!doctype html><html><body><article><h1>Binary Capture</h1><p>Bounded raw HTML for the CLI transport test.</p></article></body></html>";
    let digest = format!("{:x}", Sha256::digest(html.as_bytes()));
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "external_capture_id": "cli-binary-capture-1",
        "captured_at": "2026-08-10T12:00:00Z",
        "source_url": "https://example.invalid/cli-binary",
        "canonical_url": "https://example.invalid/cli-binary",
        "title": "Binary Capture",
        "sensitivity": "internal",
        "html": {
            "media_type": "text/html",
            "byte_size": html.len(),
            "sha256": digest
        }
    }))
    .unwrap();
    let mut body = b"MEMEX\0\0\x01".to_vec();
    body.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
    body.extend_from_slice(&manifest);
    body.extend_from_slice(html.as_bytes());
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "POST /api/v1/captures HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nContent-Type: application/vnd.memex.capture-v1\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        "a".repeat(64),
        "b".repeat(64)
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    let response_body: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(response_body["status"], "created");
    assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);
    assert!(library
        .path()
        .join("items")
        .join(response_body["item_id"].as_str().unwrap())
        .join("index.json")
        .is_file());

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(TcpStream::connect(address).is_err());
}
