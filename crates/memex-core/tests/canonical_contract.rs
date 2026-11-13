use memex_core::{
    new_id, BlobDescriptor, CaptureRecord, Evidence, ItemMetadata, ItemRevision, Sensitivity,
    SourceProfile, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};

#[test]
fn canonical_source_profile_roundtrips_with_schema_version() {
    let source = SourceProfile {
        schema_version: SCHEMA_VERSION,
        source_id: new_id("src"),
        name: "Synthetic Example".to_string(),
        source_kind: "web".to_string(),
        canonical_uris: vec!["https://example.invalid".to_string()],
        evidence_roles: vec!["primary".to_string()],
        default_sensitivity: Sensitivity::Public,
        preferred_extractor: Some("generic.webpage".to_string()),
    };

    let json = serde_json::to_string(&source).unwrap();
    let decoded: SourceProfile = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded.schema_version, 1);
    assert!(decoded.source_id.starts_with("src_"));
    assert_eq!(decoded, source);
}

#[test]
fn opaque_ids_are_namespaced_and_do_not_accept_metadata() {
    let first = new_id("itm");
    let second = new_id("itm");

    assert!(first.starts_with("itm_"));
    assert!(second.starts_with("itm_"));
    assert_ne!(first, second);
    assert!(!first.contains("example"));
}

#[test]
fn historical_v1_revision_keeps_compact_json_and_integrity_digest() {
    let historical = r#"{"schema_version":1,"revision_id":"revision_legacy","item_id":"item_legacy","revision_number":1,"title":"Legacy","source_url":"https://example.test","canonical_url":null,"content_markdown":"body\n","sensitivity":"public","evidence_ids":["evidence_legacy"],"provenance":[]}"#;
    let revision: ItemRevision = serde_json::from_str(historical).unwrap();

    assert!(revision.metadata.is_empty());
    assert!(revision.attachments.is_empty());
    assert_eq!(serde_json::to_string(&revision).unwrap(), historical);
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(historical.as_bytes())),
        "sha256:fd28240c92f7765d616c0b14e592a7fa91595aaa213e7174143172385bcba1fe"
    );
    assert_eq!(revision.metadata, ItemMetadata::default());
}

#[test]
fn historical_v1_capture_evidence_and_blob_descriptor_remain_compact() {
    let descriptor_json = r#"{"schema_version":1,"locator":"source.html","media_type":"text/html","byte_size":4,"sha256":"abcd"}"#;
    let descriptor: BlobDescriptor = serde_json::from_str(descriptor_json).unwrap();
    assert!(descriptor.source_url.is_none());
    assert_eq!(serde_json::to_string(&descriptor).unwrap(), descriptor_json);

    let capture_json = r#"{"schema_version":1,"capture_id":"capture_legacy","adapter_id":"legacy.adapter","adapter_version":"1","external_capture_id":"external-legacy","source_id":"source_legacy","captured_at":"2026-01-01T00:00:00Z","capture_method":"legacy","route_summary":null,"source_url":"https://example.test/source","canonical_url":null,"sensitivity":"public","blob_ids":["sha256:abcd"]}"#;
    let capture: CaptureRecord = serde_json::from_str(capture_json).unwrap();
    assert!(capture.ingest_input_sha256.is_none());
    assert!(capture.evidence_descriptor_version.is_none());
    assert_eq!(serde_json::to_string(&capture).unwrap(), capture_json);

    let evidence_json = r#"{"schema_version":1,"evidence_id":"evidence_legacy","source_id":"source_legacy","capture_id":"capture_legacy","blob_id":"sha256:abcd","logical_locator":"source.html","backend_ref":"capture-bundle:source.html","sensitivity":"public","supersedes":null}"#;
    let evidence: Evidence = serde_json::from_str(evidence_json).unwrap();
    assert!(evidence.descriptor.is_none());
    assert_eq!(serde_json::to_string(&evidence).unwrap(), evidence_json);
}
