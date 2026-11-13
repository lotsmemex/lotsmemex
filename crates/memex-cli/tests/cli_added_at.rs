use std::{fs, path::Path, process::Command};

fn memex(args: &[&str], paths: &[&Path]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memex"));
    command.args(args).args(paths);
    command.output().unwrap()
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(base, &entry.path(), out);
            } else if !entry.file_name().to_string_lossy().ends_with(".lock") {
                let relative = entry
                    .path()
                    .strip_prefix(base)
                    .unwrap()
                    .display()
                    .to_string();
                out.push((relative, fs::read(entry.path()).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn added_at_backfill_dry_run_is_read_only_and_apply_is_replay_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    fs::create_dir_all(&library).unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/phase1-evidence");
    let ingest = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["ingest", "--source"])
        .arg(examples.join("source.json"))
        .arg("--capture")
        .arg(examples.join("capture.json"))
        .arg("--library")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );

    let preview = tmp.path().join("preview.json");
    let missing_out = memex(&["added-at", "backfill", "--dry-run"], &[&library]);
    assert!(!missing_out.status.success());
    let before = snapshot(&library);
    let dry = memex(
        &["added-at", "backfill", "--dry-run", "--out"],
        &[&preview, &library],
    );
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(
        snapshot(&library),
        before,
        "dry run must not touch the library"
    );
    let summary: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(summary["status_counts"]["already_present"], 1);
    assert_eq!(summary["status_counts"]["set_from_capture"], 0);
    let again = memex(
        &["added-at", "backfill", "--dry-run", "--out"],
        &[&preview, &library],
    );
    assert!(!again.status.success(), "preview file is never overwritten");

    let apply = memex(
        &["added-at", "backfill", "--apply", "--preview"],
        &[&preview, &library],
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
    assert_eq!(applied["apply_counts"]["no_op"], 1);
    assert_eq!(applied["apply_counts"]["applied"], 0);
    assert_eq!(applied["replayed"], false);
    assert!(library
        .join(applied["library_receipt"].as_str().unwrap())
        .is_file());
    let replay = memex(
        &["added-at", "backfill", "--apply", "--preview"],
        &[&preview, &library],
    );
    let replayed: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replayed["replayed"], true);
    let apply_without_preview = memex(&["added-at", "backfill", "--apply"], &[&library]);
    assert!(!apply_without_preview.status.success());
}
