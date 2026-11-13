use anyhow::{bail, Context, Result};
use chrono::DateTime;
use memex_core::{
    BlobDescriptor, CaptureEnvelope, Sensitivity, SourceProfile, VerifiedBlob, VerifiedCapture,
    SCHEMA_VERSION,
};
use memex_registry::legacy_source_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub const ADAPTER_ID: &str = "ia-multi-source-capture.normalized-jsonl.v0";
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 10_000;
const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMeta {
    run_id: String,
    expected_sources: Vec<String>,
    ok: Vec<OkSource>,
    #[allow(dead_code)]
    fail: Vec<Value>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct OkSource {
    source: String,
}

#[derive(Debug, Deserialize)]
struct LegacyItem {
    source: String,
    item_id: String,
    title: String,
    content: String,
    url: String,
    captured_at: String,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InputFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug)]
pub struct PreparedItem {
    pub legacy_ref_sha256: String,
    pub capture: VerifiedCapture,
    pub unmapped_fields: Vec<String>,
}

#[derive(Debug)]
pub struct Preflight {
    pub run_ref_sha256: String,
    pub input_files: Vec<InputFile>,
    pub items: Vec<PreparedItem>,
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn read_input(root: &Path, relative: &Path) -> Result<(PathBuf, Vec<u8>)> {
    let path = resolve_package_file(root, relative)?;
    let meta = fs::metadata(&path)?;
    if meta.len() > MAX_FILE_BYTES {
        bail!("legacy package input exceeds 16 MiB");
    }
    let bytes = fs::read(&path).context("read legacy package input")?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("legacy package input exceeds 16 MiB");
    }
    Ok((path, bytes))
}

fn rejects_credential_key(value: &Value) -> bool {
    const KEYS: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "access_token",
        "refresh_token",
        "authorization",
        "cookie",
        "cookies",
        "set-cookie",
    ];
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            KEYS.iter().any(|x| key.eq_ignore_ascii_case(x)) || rejects_credential_key(value)
        }),
        Value::Array(values) => values.iter().any(rejects_credential_key),
        _ => false,
    }
}

fn checked_time(value: &str, name: &str) -> Result<()> {
    if DateTime::parse_from_rfc3339(value).is_err() {
        bail!("{name} must be RFC3339");
    }
    Ok(())
}

fn checked_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value).context("item url must be valid")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("item url must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("item url must not contain userinfo");
    }
    Ok(())
}

fn unknown_fields(value: &serde_json::Map<String, Value>) -> Vec<String> {
    const KNOWN: &[&str] = &[
        "source",
        "item_id",
        "title",
        "content",
        "url",
        "captured_at",
        "run_id",
    ];
    let mut result: Vec<_> = value
        .keys()
        .filter(|key| !KNOWN.contains(&key.as_str()))
        .cloned()
        .collect();
    result.sort();
    result
}

fn convert(meta: &RunMeta, item: LegacyItem, unmapped_fields: Vec<String>) -> PreparedItem {
    let legacy_ref_sha256 =
        digest(format!("{}\0{}\0{}", meta.run_id, item.source, item.item_id).as_bytes());
    let content_hash = digest(item.content.as_bytes());
    let source_id = legacy_source_id(&item.source);
    let descriptor = BlobDescriptor {
        schema_version: SCHEMA_VERSION,
        locator: format!("legacy-normalized-content:{content_hash}"),
        media_type: "text/plain; charset=utf-8".into(),
        byte_size: item.content.len() as u64,
        sha256: content_hash.clone(),
        source_url: None,
    };
    PreparedItem {
        legacy_ref_sha256: legacy_ref_sha256.clone(),
        capture: VerifiedCapture {
            source: SourceProfile {
                schema_version: SCHEMA_VERSION,
                source_id: source_id.clone(),
                name: item.source.clone(),
                source_kind: "legacy_normalized_jsonl".into(),
                canonical_uris: vec![],
                evidence_roles: vec!["normalized_evidence".into()],
                default_sensitivity: Sensitivity::Internal,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: SCHEMA_VERSION,
                adapter_id: ADAPTER_ID.into(),
                adapter_version: "0".into(),
                external_capture_id: legacy_ref_sha256,
                source_id,
                captured_at: item.captured_at,
                capture_method: "legacy_normalized_jsonl_v0".into(),
                sensitivity: Sensitivity::Internal,
                route_summary: None,
                title: Some(item.title),
                source_url: item.url,
                canonical_url: None,
                artifacts: vec![descriptor.clone()],
            },
            blobs: vec![VerifiedBlob {
                descriptor,
                blob_id: content_hash,
                source_backend_ref: Some("legacy-shadow-adapter".into()),
                bytes: item.content.into_bytes(),
            }],
        },
        unmapped_fields,
    }
}

/// Parse every caller-selected input and validate the complete package before a store call.
pub fn preflight_package(
    root: impl AsRef<Path>,
    meta_path: impl AsRef<Path>,
    item_paths: &[PathBuf],
) -> Result<Preflight> {
    if item_paths.is_empty() {
        bail!("at least one --items input is required");
    }
    let root = root.as_ref();
    let (_, meta_bytes) = read_input(root, meta_path.as_ref())?;
    let meta_value: Value = serde_json::from_slice(&meta_bytes).context("parse legacy run meta")?;
    if rejects_credential_key(&meta_value) {
        bail!("credential-like key in legacy run meta");
    }
    let meta: RunMeta = serde_json::from_value(meta_value).context("validate legacy run meta")?;
    if meta.run_id.trim().is_empty() || meta.expected_sources.is_empty() {
        bail!("legacy run meta has empty run_id or expected_sources");
    }
    checked_time(&meta.created_at, "createdAt")?;
    let expected: std::collections::BTreeSet<_> = meta.expected_sources.iter().collect();
    if expected.len() != meta.expected_sources.len()
        || meta.expected_sources.iter().any(|x| x.is_empty())
    {
        bail!("expectedSources must be unique non-empty values");
    }
    let ok: std::collections::BTreeSet<_> = meta.ok.iter().map(|x| &x.source).collect();
    if ok.len() != meta.ok.len() || ok.iter().any(|source| !expected.contains(source)) {
        bail!("meta ok source is not expected");
    }
    let mut input_files = vec![InputFile {
        path: meta_path.as_ref().to_string_lossy().into_owned(),
        sha256: digest(&meta_bytes),
    }];
    let mut items = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    for relative in item_paths {
        let (_, bytes) = read_input(root, relative)?;
        input_files.push(InputFile {
            path: relative.to_string_lossy().into_owned(),
            sha256: digest(&bytes),
        });
        for line in bytes.split(|x| *x == b'\n').filter(|line| !line.is_empty()) {
            if items.len() >= MAX_ITEMS {
                bail!("legacy package exceeds 10000 items");
            }
            let value: Value = serde_json::from_slice(line).context("parse legacy JSONL item")?;
            if rejects_credential_key(&value) {
                bail!("credential-like key in legacy item");
            }
            let object = value
                .as_object()
                .context("legacy JSONL item must be object")?;
            let unmapped = unknown_fields(object);
            let item: LegacyItem =
                serde_json::from_value(value).context("validate legacy JSONL item")?;
            if !expected.contains(&item.source) || !ok.contains(&item.source) {
                bail!("item source is not expected and successful");
            }
            if item.item_id.trim().is_empty() {
                bail!("item_id must not be empty");
            }
            if !identities.insert((item.source.clone(), item.item_id.clone())) {
                bail!("duplicate legacy item identity");
            }
            if item.content.len() > MAX_CONTENT_BYTES {
                bail!("legacy item content exceeds 2 MiB");
            }
            checked_url(&item.url)?;
            checked_time(&item.captured_at, "captured_at")?;
            if let Some(published) = &item.published_at {
                checked_time(published, "published_at")?;
            }
            if item.run_id.as_deref().is_some_and(|run| run != meta.run_id) {
                bail!("item run_id does not match meta runId");
            }
            items.push(convert(&meta, item, unmapped));
        }
    }
    Ok(Preflight {
        run_ref_sha256: digest(meta.run_id.as_bytes()),
        input_files,
        items,
    })
}

/// Resolve one caller-selected legacy package file without trusting paths
/// embedded in producer metadata.
pub fn resolve_package_file(
    package_root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
) -> Result<PathBuf> {
    let package_root = package_root.as_ref();
    let relative_path = relative_path.as_ref();
    if fs::symlink_metadata(package_root)
        .with_context(|| "inspect legacy package root")?
        .file_type()
        .is_symlink()
    {
        bail!("legacy package root must not be a symlink");
    }
    let root = package_root
        .canonicalize()
        .with_context(|| "canonicalize legacy package root")?;
    if !root.is_dir() {
        bail!("legacy package root must be a directory");
    }
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("legacy package path must be a normalized relative path");
    }

    let mut candidate = root.clone();
    for component in relative_path.components() {
        let Component::Normal(segment) = component else {
            unreachable!("non-normal components rejected above");
        };
        candidate.push(segment);
        if fs::symlink_metadata(&candidate)
            .with_context(|| "inspect legacy package path")?
            .file_type()
            .is_symlink()
        {
            bail!("legacy package path must not contain symlinks");
        }
    }
    let candidate = candidate
        .canonicalize()
        .with_context(|| "canonicalize legacy package file")?;
    if !candidate.starts_with(&root) {
        bail!("legacy package path escapes package root");
    }
    if !candidate.is_file() {
        bail!("legacy package input must be a regular file");
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_explicit_regular_file_inside_package() {
        let package = tempfile::tempdir().unwrap();
        fs::create_dir(package.path().join("nested")).unwrap();
        fs::write(package.path().join("nested/items.jsonl"), "{}\n").unwrap();

        let resolved = resolve_package_file(package.path(), "nested/items.jsonl").unwrap();

        assert_eq!(
            resolved,
            package
                .path()
                .join("nested/items.jsonl")
                .canonicalize()
                .unwrap()
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let package = tempfile::tempdir().unwrap();
        assert!(resolve_package_file(package.path(), "../outside.json").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_input() {
        use std::os::unix::fs::symlink;

        let package = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), package.path().join("items.jsonl")).unwrap();

        assert!(resolve_package_file(package.path(), "items.jsonl").is_err());
    }

    fn fixture(root: &Path, item: &str) {
        fs::write(root.join("meta.json"), r#"{"runId":"run-1","expectedSources":["source-a"],"ok":[{"source":"source-a"}],"fail":[],"createdAt":"2026-07-30T00:00:00Z"}"#).unwrap();
        fs::write(root.join("items.jsonl"), item).unwrap();
    }
    fn valid_item() -> &'static str {
        r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"https://example.invalid/a","captured_at":"2026-07-30T00:00:00Z"}"#
    }

    #[test]
    fn preflight_rejects_invalid_items_before_conversion() {
        let cases = [
            ("bad-json", "{", "parse legacy JSONL item"),
            (
                "bad-url",
                r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"ftp://example.invalid","captured_at":"2026-07-30T00:00:00Z"}"#,
                "http or https",
            ),
            (
                "bad-time",
                r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"https://example.invalid","captured_at":"never"}"#,
                "RFC3339",
            ),
            (
                "run-mismatch",
                r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"https://example.invalid","captured_at":"2026-07-30T00:00:00Z","run_id":"wrong"}"#,
                "run_id",
            ),
            (
                "credential",
                r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"https://example.invalid","captured_at":"2026-07-30T00:00:00Z","nested":{"ToKeN":"x"}}"#,
                "credential-like",
            ),
        ];
        for (name, item, expected) in cases {
            let package = tempfile::tempdir().unwrap();
            fixture(package.path(), item);
            let error =
                preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                    .unwrap_err()
                    .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
    }

    #[test]
    fn preflight_rejects_duplicate_identity_and_reports_unknown_names_only() {
        let package = tempfile::tempdir().unwrap();
        fixture(
            package.path(),
            &format!("{}\n{}", valid_item(), valid_item()),
        );
        assert!(
            preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                .is_err()
        );
        fixture(
            package.path(),
            r#"{"source":"source-a","item_id":"one","title":"t","content":"c","url":"https://example.invalid/a","captured_at":"2026-07-30T00:00:00Z","unknown":"private-value"}"#,
        );
        let preflight =
            preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                .unwrap();
        assert_eq!(preflight.items[0].unmapped_fields, vec!["unknown"]);
    }

    #[test]
    fn read_input_accepts_exact_size_limit_and_rejects_one_byte_over() {
        let package = tempfile::tempdir().unwrap();
        fs::write(
            package.path().join("exact.bin"),
            vec![b' '; MAX_FILE_BYTES as usize],
        )
        .unwrap();
        assert_eq!(
            read_input(package.path(), Path::new("exact.bin"))
                .unwrap()
                .1
                .len(),
            MAX_FILE_BYTES as usize
        );
        fs::write(
            package.path().join("too-large.bin"),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_input(package.path(), Path::new("too-large.bin"))
            .unwrap_err()
            .to_string()
            .contains("exceeds 16 MiB"));
    }

    fn items(count: usize) -> String {
        (0..count)
            .map(|id| format!(r#"{{"source":"source-a","item_id":"{id}","title":"t","content":"x","url":"https://example.invalid/{id}","captured_at":"2026-07-30T00:00:00Z"}}"#))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn preflight_accepts_10000_items_and_rejects_10001() {
        let package = tempfile::tempdir().unwrap();
        fixture(package.path(), &items(10_000));
        assert_eq!(
            preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                .unwrap()
                .items
                .len(),
            10_000
        );
        fixture(package.path(), &items(10_001));
        assert!(
            preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                .unwrap_err()
                .to_string()
                .contains("exceeds 10000 items")
        );
    }
    #[test]
    fn legacy_capture_and_registry_share_source_identity() {
        let package = tempfile::tempdir().unwrap();
        fixture(package.path(), valid_item());
        let prepared =
            preflight_package(package.path(), "meta.json", &[PathBuf::from("items.jsonl")])
                .unwrap();
        let source_id = memex_registry::legacy_source_id("source-a");
        assert_eq!(prepared.items[0].capture.source.source_id, source_id);
        assert_eq!(prepared.items[0].capture.capture.source_id, source_id);
    }
}
