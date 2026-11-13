use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Command, Stdio},
};

#[test]
fn serve_emits_private_safe_receipt_and_serves_health() {
    let library = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["serve", "--library"])
        .arg(library.path())
        .args(["--max-sensitivity", "public"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(!line.contains(library.path().to_str().unwrap()));
    let receipt: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(receipt["read_only"], true);
    let address = receipt["url"]
        .as_str()
        .unwrap()
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "GET /api/v1/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        address
    )
    .unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"read_only\":true"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(TcpStream::connect(address).is_err());
}

#[test]
fn serve_requires_explicit_sensitivity() {
    let output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["serve", "--library", "."])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("--bind"));
}
