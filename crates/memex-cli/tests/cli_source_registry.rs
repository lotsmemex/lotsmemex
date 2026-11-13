use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const PROFILE: &str = r#"{
  "source_id": "fixture.source",
  "name": "SYNTHETIC_SOURCE_NAME",
  "url": "https://example.invalid/source",
  "category": "technology",
  "topic_tags": ["artificial-intelligence"],
  "region": "global",
  "language": "en",
  "access_type": "public",
  "capture_mode": "browser",
  "evidence_role": ["primary"],
  "status": {"state": "legacy-unknown"},
  "notes": "SYNTHETIC_PRIVATE_NOTE",
  "rules": {"archive_path": "/Users/SYNTHETIC_PRIVATE/archive"}
}"#;

fn import(package: &Path, library: &Path) -> Output {
    import_profiles(package, library, &["source.json"])
}
fn import_profiles(package: &Path, library: &Path, profiles: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memex"));
    command
        .args(["source", "import-legacy", "--package-root"])
        .arg(package);
    for profile in profiles {
        command.args(["--profile", profile]);
    }
    command.arg("--library").arg(library).output().unwrap()
}

fn query(library: &Path, command: &str, extra: &[&str]) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_memex"));
    process
        .args(["source", command, "--library"])
        .arg(library)
        .args(extra);
    process.output().unwrap()
}

fn registry_text(root: &Path) -> String {
    fn visit(path: &Path, output: &mut String) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                visit(&path, output);
            } else {
                output.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    let mut output = String::new();
    visit(root, &mut output);
    output
}

#[test]
fn imports_replays_revises_and_queries_one_canonical_source() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    let library = tmp.path().join("library");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("source.json"), PROFILE).unwrap();

    let first = import(&package, &library);
    assert!(first.status.success(), "{first:?}");
    let first_report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_report["summary"]["created"], 1);
    assert_eq!(first_report["summary"]["revised"], 0);
    assert_eq!(first_report["summary"]["replayed"], 0);
    assert_eq!(
        first_report["adapter_id"],
        "intelligence-source-registry.legacy-profile.v0"
    );
    assert_eq!(first_report["adapter_version"], "0");
    assert_eq!(
        first_report["input_files"][0]["relative_path"],
        "source.json"
    );
    assert!(first_report["input_files"][0]["sha256"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(first_report["summary"]["lossy"], 1);
    for field in [
        "source_id",
        "name",
        "url",
        "category",
        "region",
        "language",
        "access_type",
        "capture_mode",
        "topic_tags",
        "evidence_role",
    ] {
        assert!(first_report["sources"][0]["mapped_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == field));
    }
    assert!(first_report["sources"][0]["warning_codes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "absolute_path_value_omitted"));
    let source_id = first_report["sources"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(source_id.starts_with("legacy_source_"));
    let report_text = String::from_utf8(first.stdout).unwrap();
    for value in [
        "SYNTHETIC_SOURCE_NAME",
        "https://example.invalid/source",
        "SYNTHETIC_PRIVATE_NOTE",
        "/Users/SYNTHETIC_PRIVATE/archive",
    ] {
        assert!(!report_text.contains(value), "import report leaked {value}");
    }

    let replay = import(&package, &library);
    assert!(replay.status.success(), "{replay:?}");
    let replay_report: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay_report["summary"]["replayed"], 1);
    assert_eq!(replay_report["sources"][0]["source_id"], source_id);

    fs::write(
        package.join("source.json"),
        PROFILE.replace(
            r#""topic_tags": ["artificial-intelligence"]"#,
            r#""topic_tags": ["artificial-intelligence", "revision-two"]"#,
        ),
    )
    .unwrap();
    let revised = import(&package, &library);
    assert!(revised.status.success(), "{revised:?}");
    let revised_report: serde_json::Value = serde_json::from_slice(&revised.stdout).unwrap();
    assert_eq!(revised_report["summary"]["revised"], 1);
    assert_eq!(revised_report["sources"][0]["revision_number"], 2);
    assert_eq!(revised_report["sources"][0]["source_id"], source_id);

    let list = query(&library, "list", &["--format", "json"]);
    assert!(list.status.success(), "{list:?}");
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list_json["sources"].as_array().unwrap().len(), 1);
    assert_eq!(list_json["sources"][0]["source_id"], source_id);
    assert_eq!(list_json["schema_version"], 1);

    let show = query(
        &library,
        "show",
        &["--source", &source_id, "--format", "json"],
    );
    assert!(show.status.success(), "{show:?}");
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["record"]["source_id"], source_id);
    assert_eq!(show_json["record"]["revision_number"], 2);
    assert_eq!(show_json["schema_version"], 1);
    assert_eq!(
        show_json["record"]["source_profile"]["name"],
        "SYNTHETIC_SOURCE_NAME"
    );

    let markdown = query(
        &library,
        "show",
        &["--source", &source_id, "--format", "markdown"],
    );
    assert!(markdown.status.success(), "{markdown:?}");
    assert!(String::from_utf8(markdown.stdout)
        .unwrap()
        .contains("SYNTHETIC_SOURCE_NAME"));

    let persisted = registry_text(&library.join("registry"));
    assert!(!persisted.contains("SYNTHETIC_PRIVATE_NOTE"));
    assert!(!persisted.contains("/Users/SYNTHETIC_PRIVATE/archive"));
    assert_eq!(
        fs::read_dir(
            library
                .join("registry/sources")
                .join(source_id)
                .join("revisions")
        )
        .unwrap()
        .count(),
        2
    );
}

#[test]
fn invalid_profile_fails_before_registry_write() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    let library = tmp.path().join("library");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("source.json"),
        PROFILE.replace(
            r#""notes": "SYNTHETIC_PRIVATE_NOTE""#,
            r#""nested": {"ToKeN": "SYNTHETIC_SECRET"}"#,
        ),
    )
    .unwrap();

    let output = import(&package, &library);
    assert!(!output.status.success(), "credential fixture was accepted");
    assert!(
        !library.exists(),
        "invalid preflight created the target library"
    );
}
#[test]
fn store_failure_reports_committed_failed_and_unattempted() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("package");
    let library = tmp.path().join("library");
    fs::create_dir_all(&package).unwrap();
    for (file, id, name) in [
        ("a.json", "fixture.a", "PRIVATE_A"),
        ("b.json", "fixture.b", "PRIVATE_B"),
        ("c.json", "fixture.c", "PRIVATE_C"),
    ] {
        fs::write(
            package.join(file),
            PROFILE
                .replace("fixture.source", id)
                .replace("SYNTHETIC_SOURCE_NAME", name)
                .replace(
                    "https://example.invalid/source",
                    &format!("https://example.invalid/{id}"),
                ),
        )
        .unwrap();
    }
    fs::write(
        package.join("b.json"),
        fs::read_to_string(package.join("b.json"))
            .unwrap()
            .replace(r#""category": "technology""#, r#""category": "seed""#),
    )
    .unwrap();
    let seed = memex_registry::preflight(&package, &[std::path::PathBuf::from("b.json")])
        .unwrap()
        .pop()
        .unwrap();
    memex_registry::apply(&library, seed).unwrap();
    fs::write(
        package.join("b.json"),
        fs::read_to_string(package.join("b.json"))
            .unwrap()
            .replace(r#""category": "seed""#, r#""category": "changed""#),
    )
    .unwrap();
    let blocked = memex_registry::preflight(&package, &[std::path::PathBuf::from("b.json")])
        .unwrap()
        .pop()
        .unwrap();
    let blocked_revision_id =
        memex_registry::revision_id_for(&blocked.record.source_id, &blocked.digest, 2).unwrap();
    let tampered = library
        .join("registry/sources")
        .join(&blocked.record.source_id)
        .join("revisions")
        .join(format!("{}.json", blocked_revision_id));
    fs::write(&tampered, "tampered").unwrap();
    let out = import_profiles(&package, &library, &["a.json", "b.json", "c.json"]);
    assert!(!out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["summary"]["created"], 1);
    assert_eq!(report["summary"]["failed"], 1);
    assert_eq!(report["summary"]["unattempted"], 1);
    assert_eq!(report["sources"][0]["status"], "created");
    assert_eq!(report["sources"][1]["status"], "failed");
    assert_eq!(report["sources"][2]["status"], "unattempted");
    assert_eq!(report["sources"][1]["revision_id"], serde_json::Value::Null);
    assert_eq!(
        report["sources"][2]["revision_number"],
        serde_json::Value::Null
    );
    assert_eq!(report["sources"][1]["error_code"], "registry_apply_failed");
    assert_eq!(
        report["sources"][2]["error_code"],
        "not_attempted_after_registry_failure"
    );
    assert!(library
        .join("registry/sources")
        .join(memex_registry::legacy_source_id("fixture.a"))
        .join("current.json")
        .exists());
    assert!(!library
        .join("registry/sources")
        .join(memex_registry::legacy_source_id("fixture.c"))
        .exists());
    assert_eq!(fs::read(&tampered).unwrap(), b"tampered");
    assert!(
        !text.contains("PRIVATE_A") && !text.contains("PRIVATE_B") && !text.contains("PRIVATE_C")
    );
}
#[cfg(unix)]
#[test]
fn post_current_index_failure_reports_committed_revision() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("p");
    let library = tmp.path().join("l");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("source.json"), PROFILE).unwrap();
    let first = import(&package, &library);
    assert!(first.status.success());
    let source_id = serde_json::from_slice::<serde_json::Value>(&first.stdout).unwrap()["sources"]
        [0]["source_id"]
        .as_str()
        .unwrap()
        .to_string();
    let sentinel = tmp.path().join("sentinel");
    fs::write(&sentinel, "outside").unwrap();
    let index = library
        .join("registry/sources")
        .join(&source_id)
        .join("index.md");
    fs::remove_file(&index).unwrap();
    symlink(&sentinel, &index).unwrap();
    fs::write(
        package.join("source.json"),
        PROFILE.replace(r#""category": "technology""#, r#""category": "changed""#),
    )
    .unwrap();
    let second = import(&package, &library);
    assert!(second.status.success());
    let report: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(report["summary"]["revised"], 1);
    assert_eq!(report["summary"]["failed"], 0);
    assert_eq!(report["summary"]["unattempted"], 0);
    assert_eq!(report["sources"][0]["status"], "revised");
    assert_eq!(report["sources"][0]["revision_number"], 2);
    assert!(report["sources"][0]["revision_id"].as_str().is_some());
    assert!(report["sources"][0]["warning_codes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "source_index_projection_pending"));
    assert_eq!(
        memex_registry::read_current(&library, &source_id)
            .unwrap()
            .revision_number,
        2
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
}
