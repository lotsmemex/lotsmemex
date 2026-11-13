//! Shared date policy. Inputs must come from verified, currently authorized
//! source records; neither an import clock nor a persisted inheritance snapshot
//! is an original acquisition or current parent publication proof.

use anyhow::{Context, Result};
use memex_core::{
    bibliographic::{EffectiveCaptureOrigin, EffectiveDateBasis, EffectiveDates},
    PartialDate,
};

pub const DATE_POLICY_TIMEZONE: &str = "Asia/Taipei";

/// Private-source identity for dependency resolution, not a client DTO. Do not
/// log this record: even IDs and raw date evidence belong to the authorized view.
pub struct ZoteroDateSource {
    pub source_id: String,
    pub native_item_id: u64,
    pub native_library_id: u64,
    pub parent_native_item_id: Option<u64>,
    pub publication: Option<PartialDate>,
    pub acquisition_at: Option<String>,
    pub evidence_id: String,
    pub evidence_sha256: String,
}

/// Call within a consistent Store snapshot. Current item/trash and original
/// source sensitivity are checked before reading any external JSON bytes.
pub fn read_zotero_date_source(
    root: &std::path::Path, item_id: &str, ceiling: memex_core::Sensitivity,
) -> Result<Option<ZoteroDateSource>> {
    use super::*;
    let result = (|| {
        if read_item_at_sensitivity(root, item_id, ceiling)?.is_none() { return Ok(None) }
        let Some((capture, evidence)) = origin::creating_source(root, item_id)? else { return Ok(None) };
        if capture.sensitivity > ceiling { return Ok(None) }
        if capture.adapter_id != "memex.zotero-native-import"
            || capture.capture_method != "owner_authorized_offline_import"
        { return Ok(None) }
        let group = capture.source_id.strip_prefix("source_zotero_group_")
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .context("invalid_date_source")?;
        let prefix = format!("zotero:group:{group}:item:");
        let key = capture.external_capture_id.strip_prefix(&prefix).context("invalid_date_source")?;
        let candidates: Vec<_> = evidence.iter().filter(|e|
            e.descriptor.as_ref().is_some_and(|d| d.media_type == "application/json")
        ).collect();
        anyhow::ensure!(candidates.len() == 1, "ambiguous_date_source");
        let source = candidates[0];
        if source.sensitivity > ceiling { return Ok(None) }
        // Native imports have durable creation journals. Verify canonical source
        // records against those hashes, not just their mutually matching IDs.
        let key_hash = format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&(
            capture.adapter_id.as_str(), capture.external_capture_id.as_str()))?));
        let journal = validate_journal(root, &root.join(COMMITTED_JOURNALS).join(&key_hash), &key_hash)?;
        anyhow::ensure!(journal.state == JournalState::Committed && journal.item_id == item_id
            && journal.capture_id == capture.capture_id, "invalid_date_source");
        for relative in [format!("captures/{}/capture.json", capture.capture_id),
            format!("evidence/{}/evidence.json", source.evidence_id),
            format!("receipts/{}.json", capture.capture_id)] {
            let entry = journal.entries.iter().find(|e| e.path == relative).context("invalid_date_source")?;
            let canonical = contained_existing(root, &root.join(relative))?;
            let mut bytes = Vec::new();
            open_regular_nofollow(&canonical)?.take(entry.byte_size + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() as u64 == entry.byte_size
                && format!("sha256:{:x}", sha2::Sha256::digest(&bytes)) == entry.sha256, "invalid_date_source");
        }
        let descriptor = source.descriptor.as_ref().context("invalid_date_source")?;
        anyhow::ensure!(descriptor.schema_version == SCHEMA_VERSION
            && valid_lower_hex_64(&descriptor.sha256)
            && source.blob_id == format!("sha256:{}", descriptor.sha256)
            && source.logical_locator == descriptor.locator, "invalid_date_source");
        let path = source.backend_ref.strip_prefix("memex-native-import:")
            .map(std::path::Path::new).context("invalid_date_source")?;
        let relative = std::path::Path::new(&descriptor.locator);
        anyhow::ensure!(path.is_absolute() && !relative.as_os_str().is_empty()
            && relative.components().all(|c| matches!(c, std::path::Component::Normal(_)))
            && path.ends_with(relative), "invalid_date_source");
        let mut checked = std::path::PathBuf::new();
        for part in path.components() {
            anyhow::ensure!(matches!(part, std::path::Component::RootDir | std::path::Component::Normal(_)), "invalid_date_source");
            checked.push(part);
            anyhow::ensure!(!std::fs::symlink_metadata(&checked)?.file_type().is_symlink(), "invalid_date_source");
        }
        // Bounded JSON source only; PDF/HTML bodies are never read to find a date.
        const MAX_DATE_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
        anyhow::ensure!(descriptor.byte_size <= MAX_DATE_SOURCE_BYTES, "date_source_too_large");
        let file = open_regular_nofollow(path)?;
        anyhow::ensure!(file.metadata()?.len() == descriptor.byte_size, "invalid_date_source");
        let mut bytes = Vec::new();
        file.take(MAX_DATE_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() as u64 == descriptor.byte_size
            && format!("{:x}", sha2::Sha256::digest(&bytes)) == descriptor.sha256, "invalid_date_source");
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(raw.get("key").and_then(|v| v.as_str()) == Some(key), "invalid_date_source");
        let native_item_id = raw.get("itemID").and_then(|v| v.as_u64()).context("invalid_date_source")?;
        let native_library_id = raw.get("libraryID").and_then(|v| v.as_u64()).context("invalid_date_source")?;
        let mut parents = Vec::new();
        for kind in ["note", "attachment"] {
            if let Some(parent) = raw.get(kind).and_then(|v| v.get("parentItemID")).filter(|v| !v.is_null()) {
                parents.push(parent.as_u64().context("invalid_date_relationship")?);
            }
        }
        parents.sort_unstable();
        parents.dedup();
        anyhow::ensure!(parents.len() <= 1 && !parents.contains(&native_item_id), "invalid_date_relationship");
        let publication = raw.get("fields").and_then(|v| v.get("date")).filter(|v| !v.is_null())
            .map(|value| value.as_str().context("invalid_date_source").map(PartialDate::parse_zotero_multipart)).transpose()?;
        // Malformed timestamps are unresolved, not permission to use import time.
        let acquisition_at = raw.get("dateAdded").and_then(|v| v.as_str()).and_then(zotero_acquisition_instant);
        Ok(Some(ZoteroDateSource { source_id: capture.source_id, native_item_id, native_library_id,
            parent_native_item_id: parents.first().copied(), publication, acquisition_at,
            evidence_id: source.evidence_id.clone(), evidence_sha256: descriptor.sha256.clone() }))
    })();
    // Never disclose an external source path, JSON excerpt or invalid raw date.
    result.map_err(|_: anyhow::Error| anyhow::anyhow!("date_source_validation_failed"))
}

/// Zotero's SQL dateAdded is UTC. Offset-bearing ISO values are instants;
/// timezone-less ISO, date-only values and malformed SQL are unresolved.
pub fn zotero_acquisition_instant(raw: &str) -> Option<String> {
    let instant = if raw.len() == 19 {
        let time = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S").ok()?;
        if time.format("%Y-%m-%d %H:%M:%S").to_string() != raw {
            return None;
        }
        time.and_utc().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    } else {
        super::added_at::from_capture_time(raw)?
    };
    // Reject leap seconds unsupported by the shared instant/day projection.
    instant.parse::<jiff::Timestamp>().ok()?;
    Some(instant)
}

/// Resolve the same effective value for metadata and indexed query rows.
/// `parent_publication` is a current authorized publication, never a parent's
/// effective date (which could itself be an acquisition fallback).
pub fn project_dates(
    own_publication: Option<&PartialDate>,
    parent_publication: Option<&PartialDate>,
    acquisition: Option<(&str, EffectiveCaptureOrigin)>,
) -> Result<EffectiveDates> {
    for date in [own_publication, parent_publication].into_iter().flatten() {
        date.validate().map_err(|_| anyhow::anyhow!("invalid_date_projection"))?;
    }
    let mut result = EffectiveDates::default();
    let capture_day = if let Some((raw, origin)) = acquisition {
        let canonical = super::added_at::from_capture_time(raw)
            .context("invalid_acquisition_timestamp")?;
        let timestamp: jiff::Timestamp = canonical.parse()
            .map_err(|_| anyhow::anyhow!("invalid_acquisition_timestamp"))?;
        let day = timestamp.in_tz(DATE_POLICY_TIMEZONE)
            .context("date_policy_timezone_unavailable")?.date().to_string();
        let date = PartialDate::parse_legacy(&day);
        anyhow::ensure!(date.value.is_some(), "invalid_acquisition_date");
        result.effective_capture_at = Some(canonical);
        result.effective_capture_origin = Some(origin);
        Some(date)
    } else {
        None
    };
    if let Some(date) = parent_publication.filter(|date| date.value.is_some()) {
        result.effective_date = Some(date.clone());
        result.effective_date_basis = EffectiveDateBasis::InheritedFromParent;
    } else if let Some(date) = own_publication.filter(|date| date.value.is_some()) {
        result.effective_date = Some(date.clone());
        result.effective_date_basis = EffectiveDateBasis::Published;
    } else if let Some(date) = capture_day {
        result.effective_date = Some(date);
        result.effective_date_basis = EffectiveDateBasis::Captured;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_source_is_receipt_bound_and_corruption_is_not_missing() {
        use memex_core::Sensitivity;
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let library = root.join("Library");
        let objects = root.join("objects");
        std::fs::create_dir(&objects).unwrap();
        let bytes = br#"{"itemID":17,"libraryID":8,"key":"TESTKEY1","dateAdded":"2001-01-01 16:00:00","dateModified":"2099-01-01 00:00:00","fields":{"date":"1999-00-00 1999"},"note":{"parentItemID":9}}"#;
        let hash = format!("{:x}", Sha256::digest(bytes));
        let path = objects.join(&hash);
        std::fs::write(&path, bytes).unwrap();
        let mut capture = crate::tests::capture();
        capture.source.source_id = "source_zotero_group_42".into();
        capture.capture.source_id = capture.source.source_id.clone();
        capture.capture.adapter_id = "memex.zotero-native-import".into();
        capture.capture.capture_method = "owner_authorized_offline_import".into();
        capture.capture.external_capture_id = "zotero:group:42:item:TESTKEY1".into();
        capture.capture.captured_at = "2026-11-08T12:00:00Z".into();
        capture.capture.sensitivity = Sensitivity::Private;
        let blob = &mut capture.blobs[0];
        blob.bytes = bytes.to_vec();
        blob.blob_id = format!("sha256:{hash}");
        blob.descriptor.locator = format!("objects/{hash}");
        blob.descriptor.media_type = "application/json".into();
        blob.descriptor.byte_size = bytes.len() as u64;
        blob.descriptor.sha256 = hash;
        blob.source_backend_ref = Some(format!("memex-native-import:{}", path.display()));
        capture.capture.artifacts = vec![blob.descriptor.clone()];
        let receipt = crate::ingest(&library, &capture, crate::tests::draft()).unwrap();
        let id = &receipt.item_ids[0];
        let source = read_zotero_date_source(&library, id, Sensitivity::Private).unwrap().unwrap();
        assert_eq!(source.acquisition_at.as_deref(), Some("2001-01-01T16:00:00Z"));
        assert_eq!(source.publication.unwrap().value.as_deref(), Some("1999"));
        assert_eq!(source.parent_native_item_id, Some(9));
        assert_eq!((source.native_item_id, source.native_library_id), (17, 8));
        // Even with a corrupt external source, hidden items do not reveal errors.
        let mut changed = bytes.to_vec();
        changed[0] = b'[';
        std::fs::write(&path, changed).unwrap();
        assert!(read_zotero_date_source(&library, id, Sensitivity::Public).unwrap().is_none());
        assert_eq!(read_zotero_date_source(&library, id, Sensitivity::Private).err().unwrap().to_string(), "date_source_validation_failed");
        std::fs::write(&path, bytes).unwrap();
        assert!(read_zotero_date_source(&library, id, Sensitivity::Private).unwrap().is_some());
        let canonical = library.join("evidence").join(&source.evidence_id).join("evidence.json");
        let original = std::fs::read(&canonical).unwrap();
        let mut evidence: serde_json::Value = serde_json::from_slice(&original).unwrap();
        evidence["backend_ref"] = format!("memex-native-import:{}/different/objects/{}", root.display(), source.evidence_sha256).into();
        std::fs::write(&canonical, serde_json::to_vec(&evidence).unwrap()).unwrap();
        assert!(read_zotero_date_source(&library, id, Sensitivity::Private).is_err());
        std::fs::write(&canonical, original).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(read_zotero_date_source(&library, id, Sensitivity::Private).is_err());
        let target = root.join("elsewhere");
        std::fs::write(&target, bytes).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_zotero_date_source(&library, id, Sensitivity::Private).is_err());
    }

    #[test]
    fn acquisition_is_utc_and_display_uses_taipei_not_import_clock() {
        let sql = zotero_acquisition_instant("2020-12-31 16:00:00").unwrap();
        assert_eq!(sql, "2020-12-31T16:00:00Z");
        assert_eq!(zotero_acquisition_instant("2021-01-01T00:00:00+08:00"), Some(sql.clone()));
        let result = project_dates(None, None, Some((&sql, EffectiveCaptureOrigin::ZoteroDateAdded))).unwrap();
        assert_eq!(result.effective_date.unwrap().value.as_deref(), Some("2021-01-01"));
        assert_eq!(result.effective_date_basis, EffectiveDateBasis::Captured);
        let before = project_dates(None, None, Some(("2020-12-31T15:59:59.999999999Z", EffectiveCaptureOrigin::ZoteroDateAdded))).unwrap();
        assert_eq!(before.effective_date.unwrap().value.as_deref(), Some("2020-12-31"));
        for raw in ["2021-01-01", "2021-01-01T00:00:00", "2021-02-29 00:00:00", "2021-1-01 00:00:00", "2021-01-01 24:00:00"] {
            assert_eq!(zotero_acquisition_instant(raw), None);
        }
    }

    #[test]
    fn current_parent_publication_wins_without_changing_own_evidence() {
        let own = PartialDate::parse_legacy("2024-04");
        let parent = PartialDate::parse_legacy("1999");
        let unknown = PartialDate::parse_legacy("undated");
        let acquisition = Some(("2020-01-01T00:00:00Z", EffectiveCaptureOrigin::ZoteroDateAdded));
        let result = project_dates(Some(&own), Some(&parent), acquisition).unwrap();
        assert_eq!(result.effective_date, Some(parent));
        assert_eq!(result.effective_date_basis, EffectiveDateBasis::InheritedFromParent);
        assert_eq!(own.value.as_deref(), Some("2024-04"));
        let result = project_dates(Some(&own), Some(&unknown), acquisition).unwrap();
        assert_eq!(result.effective_date, Some(own));
        assert_eq!(result.effective_date_basis, EffectiveDateBasis::Published);
        let result = project_dates(Some(&unknown), None, acquisition).unwrap();
        assert_eq!(result.effective_date_basis, EffectiveDateBasis::Captured);
        assert_eq!(project_dates(Some(&unknown), None, None).unwrap(), EffectiveDates::default());
        let mut corrupt = unknown;
        corrupt.value = Some("2024-99".into());
        assert!(project_dates(Some(&corrupt), None, acquisition).is_err());
    }
}
