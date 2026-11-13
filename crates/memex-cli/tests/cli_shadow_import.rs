use std::{
    path::{Path, PathBuf},
    process::Command,
};

const A: &str = r#"{"source":"fixture.a","item_id":"a","title":"PRIVATE_TITLE_A","content":"PRIVATE_BODY_A","url":"https://example.invalid/a","author":"PRIVATE_AUTHOR_A","captured_at":"2026-07-30T08:01:00Z","published_at":"2026-07-30T07:59:00Z","section":"PRIVATE_SECTION","legacy_extra":"PRIVATE_EXTRA"}"#;
const B: &str = r#"{"source":"fixture.b","item_id":"b","title":"PRIVATE_TITLE_B","content":"PRIVATE_BODY_B","url":"https://example.invalid/b","captured_at":"2026-07-30T08:01:00Z"}"#;

fn write_package(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("run.meta.json"), r#"{"runId":"synthetic-run-001","expectedSources":["fixture.a","fixture.b"],"ok":[{"source":"fixture.a","file":"/ignored/a"},{"source":"fixture.b","file":"/ignored/b"}],"fail":[],"createdAt":"2026-07-30T08:00:00Z"}"#).unwrap();
    std::fs::write(root.join("a.jsonl"), format!("{A}\n")).unwrap();
    std::fs::write(root.join("b.jsonl"), format!("{B}\n")).unwrap();
}

fn run(package: &Path, library: &Path, items: &[PathBuf]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memex"));
    command
        .args(["shadow-import-legacy", "--package-root"])
        .arg(package)
        .args(["--meta", "run.meta.json"]);
    for item in items {
        command.arg("--items").arg(item);
    }
    command.arg("--library").arg(library).output().unwrap()
}

#[test]
fn imports_multiple_files_replays_and_never_leaks_values() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    let library = tmp.path().join("library");
    write_package(&package);
    let items = vec!["a.jsonl".into(), "b.jsonl".into()];
    let first = run(&package, &library, &items);
    assert!(first.status.success(), "{:?}", first);
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["summary"]["total"], 2);
    assert_eq!(report["summary"]["created"], 2);
    let output = String::from_utf8(first.stdout).unwrap();
    for value in [
        "PRIVATE_TITLE_A",
        "PRIVATE_BODY_A",
        "PRIVATE_AUTHOR_A",
        "PRIVATE_SECTION",
        "PRIVATE_EXTRA",
        "https://example.invalid/a",
    ] {
        assert!(!output.contains(value));
    }
    for field in ["author", "published_at", "section", "legacy_extra"] {
        assert!(report["items"][0]["unmapped_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == field));
    }
    let replay = run(&package, &library, &items);
    assert!(replay.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&replay.stdout).unwrap()["summary"]["replayed"],
        2
    );
}

#[test]
fn invalid_preflight_never_creates_library() {
    let cases = [
        ("bad-json", "{\n".to_string()),
        ("duplicate", format!("{A}\n{A}\n")),
        (
            "bad-scheme",
            A.replace("https://example.invalid/a", "ftp://example.invalid/a"),
        ),
        (
            "userinfo",
            A.replace("https://example.invalid/a", "https://u:p@example.invalid/a"),
        ),
        ("bad-captured", A.replace("2026-07-30T08:01:00Z", "nope")),
        ("bad-published", A.replace("2026-07-30T07:59:00Z", "nope")),
        (
            "run-mismatch",
            A.replace("\"section\"", "\"run_id\":\"wrong\",\"section\""),
        ),
        (
            "credential",
            A.replace(
                "\"section\"",
                "\"nested\":{\"ToKeN\":\"secret\"},\"section\"",
            ),
        ),
        (
            "empty-id",
            A.replace("\"item_id\":\"a\"", "\"item_id\":\"\""),
        ),
        (
            "oversize",
            format!(
                r#"{{"source":"fixture.a","item_id":"x","title":"x","content":"{}","url":"https://example.invalid/x","captured_at":"2026-07-30T08:01:00Z"}}"#,
                "x".repeat(2 * 1024 * 1024 + 1)
            ),
        ),
    ];
    for (name, body) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let package = tmp.path().join("package");
        let library = tmp.path().join("library");
        write_package(&package);
        std::fs::write(package.join("b.jsonl"), body).unwrap();
        let output = run(&package, &library, &["a.jsonl".into(), "b.jsonl".into()]);
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        assert!(
            !library.exists(),
            "{name} wrote library before full preflight"
        );
    }
}

#[test]
fn unsafe_paths_and_store_failure_are_reported_without_writes_or_leaks() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    write_package(&package);
    for item in [
        PathBuf::from("../escape.jsonl"),
        tmp.path().join("absolute.jsonl"),
    ] {
        let library = tmp.path().join(format!("library-{}", item.display()));
        let output = run(&package, &library, &[item]);
        assert!(!output.status.success());
        assert!(!library.exists());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(package.join("a.jsonl"), package.join("link.jsonl")).unwrap();
        let library = tmp.path().join("symlink-library");
        assert!(!run(&package, &library, &["link.jsonl".into()])
            .status
            .success());
        assert!(!library.exists());
    }
    let library = tmp.path().join("not-a-library");
    std::fs::write(&library, "ordinary file").unwrap();
    let output = run(&package, &library, &["a.jsonl".into(), "b.jsonl".into()]);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["items"][0]["status"], "failed");
    assert_eq!(report["items"][0]["error_code"], "store_ingest_failed");
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains("PRIVATE_BODY_A"));
}

#[test]
fn post_commit_replay_mismatch_reports_partial_failure_and_unattempted_row() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    let library = tmp.path().join("library");
    write_package(&package);
    assert!(run(&package, &library, &["b.jsonl".into()])
        .status
        .success());
    std::fs::write(
        package.join("b.jsonl"),
        B.replace("PRIVATE_BODY_B", "PRIVATE_BODY_B_CHANGED"),
    )
    .unwrap();
    std::fs::write(
        package.join("c.jsonl"),
        B.replace("fixture.b", "fixture.c")
            .replace(r#""item_id":"b""#, r#""item_id":"c""#),
    )
    .unwrap();
    std::fs::write(package.join("run.meta.json"), r#"{"runId":"synthetic-run-001","expectedSources":["fixture.a","fixture.b","fixture.c"],"ok":[{"source":"fixture.a"},{"source":"fixture.b"},{"source":"fixture.c"}],"fail":[],"createdAt":"2026-07-30T08:00:00Z"}"#).unwrap();
    let output = run(
        &package,
        &library,
        &["a.jsonl".into(), "b.jsonl".into(), "c.jsonl".into()],
    );
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["summary"]["created"], 1);
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["items"][0]["status"], "created");
    assert_eq!(report["items"][1]["status"], "failed");
    assert_eq!(report["items"][2]["status"], "unattempted");
    for item in report["items"].as_array().unwrap() {
        for field in [
            "legacy_ref_sha256",
            "item_id",
            "capture_id",
            "status",
            "mapped_fields",
            "unmapped_fields",
            "defaulted_fields",
            "provenance_gaps",
        ] {
            assert!(item.get(field).is_some());
        }
    }
    assert_eq!(report["items"][1]["error_code"], "store_ingest_failed");
    assert_eq!(
        report["items"][2]["error_code"],
        "not_attempted_after_store_failure"
    );
    assert_eq!(report["items"][1]["item_id"], serde_json::Value::Null);
    assert_eq!(std::fs::read_dir(library.join("items")).unwrap().count(), 2);
    assert!(!stdout.contains("PRIVATE_BODY_B_CHANGED"));
}
