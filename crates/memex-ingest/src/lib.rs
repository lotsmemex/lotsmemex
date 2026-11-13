use anyhow::{bail, Context, Result};
use chrono::DateTime;
use memex_core::{CaptureEnvelope, SourceProfile, VerifiedBlob, VerifiedCapture, SCHEMA_VERSION};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub mod legacy;

const MAX_HTML_BYTES: u64 = 8 * 1024 * 1024;

fn is_html_media_type(value: &str) -> bool {
    matches!(
        value
            .split_once(';')
            .map_or(value, |(base, _)| base)
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "text/html" | "application/xhtml+xml"
    )
}

pub fn read_verified_capture(
    source_path: impl AsRef<Path>,
    capture_path: impl AsRef<Path>,
) -> Result<VerifiedCapture> {
    let source_path = source_path.as_ref();
    let capture_path = capture_path.as_ref();
    let root = capture_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .with_context(|| "canonicalize capture bundle root")?;
    let capture_path = capture_path
        .canonicalize()
        .with_context(|| "canonicalize capture")?;
    if !capture_path.starts_with(&root) {
        bail!("capture path escapes capture bundle root");
    }
    let source_path = source_path
        .canonicalize()
        .with_context(|| "canonicalize source")?;
    if !source_path.starts_with(&root) {
        bail!("source path escapes capture bundle root");
    }
    let source: SourceProfile =
        serde_json::from_str(&fs::read_to_string(source_path).context("read source")?)
            .context("parse source")?;
    let capture: CaptureEnvelope =
        serde_json::from_str(&fs::read_to_string(capture_path).context("read capture")?)
            .context("parse capture")?;
    if source.schema_version != SCHEMA_VERSION || capture.schema_version != SCHEMA_VERSION {
        bail!("unsupported schema version");
    }
    if source.source_id != capture.source_id {
        bail!("source_id does not match capture");
    }
    if DateTime::parse_from_rfc3339(&capture.captured_at).is_err() {
        bail!("captured_at must be RFC3339");
    }
    if capture.artifacts.is_empty() {
        bail!("capture must contain at least one artifact");
    }
    let mut blobs = Vec::with_capacity(capture.artifacts.len());
    for descriptor in &capture.artifacts {
        if descriptor.schema_version != SCHEMA_VERSION {
            bail!("unsupported blob schema version");
        }
        let locator = Path::new(&descriptor.locator);
        if locator.is_absolute()
            || locator
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            bail!("unsafe artifact locator");
        }
        let path = root
            .join(locator)
            .canonicalize()
            .with_context(|| "resolve artifact locator")?;
        if !path.starts_with(&root) {
            bail!("artifact locator escapes bundle root");
        }
        let is_html = is_html_media_type(&descriptor.media_type);
        let bytes = if is_html {
            if descriptor.byte_size > MAX_HTML_BYTES {
                bail!("HTML artifact exceeds ingest size limit");
            }
            let artifact = fs::File::open(&path).context("open verified HTML artifact")?;
            if artifact
                .metadata()
                .context("inspect verified HTML artifact")?
                .len()
                > MAX_HTML_BYTES
            {
                bail!("HTML artifact exceeds ingest size limit");
            }
            let mut bytes = Vec::with_capacity(descriptor.byte_size as usize);
            artifact
                .take(MAX_HTML_BYTES + 1)
                .read_to_end(&mut bytes)
                .context("read verified HTML artifact")?;
            if bytes.len() as u64 > MAX_HTML_BYTES {
                bail!("HTML artifact exceeds ingest size limit");
            }
            bytes
        } else {
            fs::read(&path).context("read verified artifact")?
        };
        if bytes.len() as u64 != descriptor.byte_size {
            bail!("artifact byte size mismatch");
        }
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if digest != descriptor.sha256 {
            bail!("artifact sha256 mismatch");
        }
        blobs.push(VerifiedBlob {
            descriptor: descriptor.clone(),
            blob_id: format!("sha256:{digest}"),
            source_backend_ref: Some(format!(
                "capture-bundle:{}",
                hex::encode(Sha256::digest(serde_json::to_vec(&(
                    capture.adapter_id.as_str(),
                    capture.external_capture_id.as_str()
                ))?))
            )),
            bytes,
        });
    }
    Ok(VerifiedCapture {
        source,
        capture,
        blobs,
    })
}

pub fn bundle_root(path: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(path
        .as_ref()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    fn files(locator: &str, sha: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("artifacts")).unwrap();
        let mut f = fs::File::create(d.path().join("artifacts/body.txt")).unwrap();
        write!(f, "hello").unwrap();
        let s = d.path().join("source.json");
        let c = d.path().join("capture.json");
        fs::write(&s,r#"{"schema_version":1,"source_id":"src","name":"Synthetic","source_kind":"web","canonical_uris":[],"evidence_roles":[],"default_sensitivity":"public","preferred_extractor":null}"#).unwrap();
        fs::write(&c,format!(r#"{{"schema_version":1,"adapter_id":"test","adapter_version":"1","external_capture_id":"cap","source_id":"src","captured_at":"2026-01-01T00:00:00Z","capture_method":"fixture","sensitivity":"public","route_summary":null,"title":"Title","source_url":"https://example.test","canonical_url":null,"artifacts":[{{"schema_version":1,"locator":"{locator}","media_type":"text/plain","byte_size":5,"sha256":"{sha}"}}]}}"#)).unwrap();
        (d, s, c)
    }
    #[test]
    fn rejects_parent_locator() {
        let (_d, s, c) = files(
            "../x",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        assert!(read_verified_capture(s, c).is_err());
    }
    #[test]
    fn rejects_absolute_locator() {
        let (_d, s, c) = files(
            "/tmp/not-a-bundle-artifact",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        assert!(read_verified_capture(s, c).is_err());
    }
    #[test]
    fn rejects_bad_checksum() {
        let (_d, s, c) = files("artifacts/body.txt", "bad");
        assert!(read_verified_capture(s, c).is_err());
    }
    #[test]
    fn rejects_declared_oversized_html_before_read() {
        let (_d, s, c) = files("artifacts/body.txt", "bad");
        let capture = fs::read_to_string(&c)
            .unwrap()
            .replace(
                "\"media_type\":\"text/plain\"",
                "\"media_type\":\"Text/HTML; charset=utf-8\"",
            )
            .replace(
                "\"byte_size\":5",
                &format!("\"byte_size\":{}", MAX_HTML_BYTES + 1),
            );
        fs::write(&c, capture).unwrap();
        assert_eq!(
            read_verified_capture(s, c).unwrap_err().to_string(),
            "HTML artifact exceeds ingest size limit"
        );
    }

    #[test]
    fn rejects_filesystem_oversized_html_before_read() {
        let (d, s, c) = files("artifacts/body.txt", "bad");
        let capture = fs::read_to_string(&c).unwrap().replace(
            "\"media_type\":\"text/plain\"",
            "\"media_type\":\"APPLICATION/XHTML+XML; q=x\"",
        );
        fs::write(&c, capture).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(d.path().join("artifacts/body.txt"))
            .unwrap()
            .set_len(MAX_HTML_BYTES + 1)
            .unwrap();
        assert_eq!(
            read_verified_capture(s, c).unwrap_err().to_string(),
            "HTML artifact exceeds ingest size limit"
        );
    }
    #[test]
    fn rejects_source_outside_bundle() {
        let (_d, _s, c) = files(
            "artifacts/body.txt",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        let external = tempfile::NamedTempFile::new().unwrap();
        fs::write(external.path(), r#"{"schema_version":1,"source_id":"src","name":"x","source_kind":"x","canonical_uris":[],"evidence_roles":[],"default_sensitivity":"public","preferred_extractor":null}"#).unwrap();
        assert!(read_verified_capture(external.path(), c).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_capture_symlink_outside_bundle() {
        use std::os::unix::fs::symlink;
        let (_d, s, c) = files(
            "artifacts/body.txt",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::copy(&c, outside.path()).unwrap();
        fs::remove_file(&c).unwrap();
        symlink(outside.path(), &c).unwrap();
        assert!(read_verified_capture(s, c).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let (d, s, c) = files(
            "artifacts/escape.txt",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "hello").unwrap();
        symlink(outside.path(), d.path().join("artifacts/escape.txt")).unwrap();
        assert!(read_verified_capture(s, c).is_err());
    }
}
