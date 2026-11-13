use memex_analysis::{
    ActorKind, AnalysisKind, AnalysisRequest, ApplyStatus, LinkRelation, LinkRequest,
    LinkTargetKind, LinkTargetRequest, RecallQuery,
};
use memex_core::Sensitivity;
use memex_publication::{
    PublicField, PublicReleaseRequest, PublicTarget, PublicationInputKind, PublicationInputRequest,
    PublicationKind, PublicationRequest,
};
use lots_scout_extract::{Extractor, GenericWebpageExtractor};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};

const LEGACY_SOURCE: &str = "phase7.synthetic.legacy";
const LEGACY_TITLE: &str = "P7_SYNTHETIC_LEGACY_TITLE";
const LEGACY_BODY: &str = "P7_REHEARSAL_FIXED_POINT P7_SYNTHETIC_LEGACY_BODY";
const LEGACY_AUTHOR: &str = "P7_SYNTHETIC_LEGACY_AUTHOR";
const LEGACY_NOTE: &str = "P7_SYNTHETIC_LEGACY_NOTE";
const LEGACY_URL: &str = "https://example.invalid/phase7-legacy";
const ANALYSIS_BODY: &str = "P7_REHEARSAL_FIXED_POINT P7_SYNTHETIC_ANALYSIS_BODY";
const LINK_RATIONALE: &str = "P7_SYNTHETIC_LINK_RATIONALE";
const INTERNAL_PUBLICATION_BODY: &str = "P7_SYNTHETIC_INTERNAL_PUBLICATION_BODY";
const PUBLIC_CONTROL_BODY: &str = "P7_SYNTHETIC_PUBLIC_CONTROL_BODY";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn typed_digest<T>(value: &T) -> String
where
    T: serde::Serialize,
{
    sha256(&serde_json::to_vec(value).unwrap())
}

fn canonical_pretty_json<T>(value: &T) -> Vec<u8>
where
    T: serde::Serialize,
{
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn assert_sha256(value: &str) {
    assert_eq!(value.len(), 71);
    assert!(value.starts_with("sha256:"));
    assert!(value[7..].bytes().all(
        |byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
    ));
}

fn assert_receipt_private<T>(receipt: &T, forbidden: &[&str])
where
    T: serde::Serialize,
{
    let receipt = serde_json::to_string(receipt).unwrap();
    for value in forbidden {
        assert!(
            !receipt.contains(value),
            "privacy-safe receipt leaked {value}"
        );
    }
}

fn write_legacy_package(root: &std::path::Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("run.meta.json"),
        serde_json::json!({
            "runId": "phase7-synthetic-run",
            "expectedSources": [LEGACY_SOURCE],
            "ok": [{"source": LEGACY_SOURCE}],
            "fail": [],
            "createdAt": "2026-08-09T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("items.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "source": LEGACY_SOURCE,
                "item_id": "phase7-item",
                "title": LEGACY_TITLE,
                "content": LEGACY_BODY,
                "url": LEGACY_URL,
                "captured_at": "2026-08-09T00:01:00Z",
                "author": LEGACY_AUTHOR,
                "legacy_note": LEGACY_NOTE
            })
        ),
    )
    .unwrap();
}

fn write_legacy_profile(root: &std::path::Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("source.json"),
        serde_json::json!({
            "source_id": LEGACY_SOURCE,
            "name": "Phase 7 Synthetic Legacy Source",
            "url": "https://example.invalid/phase7-source",
            "category": "synthetic",
            "topic_tags": ["phase7"],
            "region": "global",
            "language": "en",
            "access_type": "public_web",
            "capture_mode": "synthetic_fixture",
            "evidence_role": ["primary"],
            "notes": LEGACY_NOTE
        })
        .to_string(),
    )
    .unwrap();
}

fn release_request(publication_id: String, published_at: &str) -> PublicReleaseRequest {
    PublicReleaseRequest {
        schema_version: 1,
        target: PublicTarget::YuanfangPublicV1,
        publication_id,
        published_at: published_at.into(),
        field_allowlist: vec![PublicField::Citations, PublicField::Title],
        actor_kind: ActorKind::Agent,
        actor_id: "phase7-release-agent".into(),
        tool_id: "phase7-rehearsal".into(),
        tool_version: "1".into(),
        config_digest: format!("sha256:{}", "4".repeat(64)),
    }
}

#[test]
fn synthetic_p2_to_p5_rehearsal_reaches_fixed_points_without_declassifying_legacy() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("legacy-package");
    let profiles = temporary.path().join("legacy-profiles");
    let library = temporary.path().join("library");
    write_legacy_package(&package);
    write_legacy_profile(&profiles);

    // P2: every selected input is preflighted before the first owner write.
    let preflight = memex_ingest::legacy::preflight_package(
        &package,
        "run.meta.json",
        &[PathBuf::from("items.jsonl")],
    )
    .unwrap();
    assert_eq!(preflight.input_files.len(), 2);
    assert!(preflight.input_files.iter().all(|input| {
        assert_sha256(&input.sha256);
        matches!(input.path.as_str(), "run.meta.json" | "items.jsonl")
    }));
    assert_sha256(&preflight.run_ref_sha256);
    assert_eq!(preflight.items.len(), 1);
    let prepared = preflight.items.into_iter().next().unwrap();
    assert_eq!(
        prepared.capture.capture.adapter_id,
        memex_ingest::legacy::ADAPTER_ID
    );
    assert_eq!(
        prepared.capture.capture.external_capture_id,
        prepared.legacy_ref_sha256
    );
    assert_eq!(
        prepared.capture.source.source_id,
        memex_registry::legacy_source_id(LEGACY_SOURCE)
    );
    assert_eq!(
        prepared.capture.capture.source_id,
        prepared.capture.source.source_id
    );
    assert_eq!(
        prepared.capture.source.default_sensitivity,
        Sensitivity::Internal
    );
    assert_eq!(prepared.capture.capture.sensitivity, Sensitivity::Internal);
    assert!(prepared.capture.capture.route_summary.is_none());
    assert!(prepared.capture.source.preferred_extractor.is_none());
    assert!(prepared.unmapped_fields.contains(&"author".into()));
    assert!(prepared.unmapped_fields.contains(&"legacy_note".into()));
    let unmapped = serde_json::to_string(&prepared.unmapped_fields).unwrap();
    assert!(!unmapped.contains(LEGACY_AUTHOR) && !unmapped.contains(LEGACY_NOTE));

    let draft = GenericWebpageExtractor
        .extract(&prepared.capture)
        .unwrap();
    let first_ingest = memex_store::ingest(&library, &prepared.capture, draft.clone()).unwrap();
    assert!(!first_ingest.idempotent_replay);
    assert_eq!(first_ingest.item_ids.len(), 1);
    assert_eq!(first_ingest.revision_ids.len(), 1);
    assert!(!first_ingest.evidence_ids.is_empty());
    let legacy_item_id = first_ingest.item_ids[0].clone();
    let (legacy_item, legacy_revision) =
        memex_store::read_item(&library, &legacy_item_id).unwrap();
    assert_eq!(legacy_item.current_revision, first_ingest.revision_ids[0]);
    assert_eq!(legacy_revision.item_id, legacy_item_id);
    assert_eq!(legacy_revision.revision_id, first_ingest.revision_ids[0]);
    assert_eq!(legacy_revision.evidence_ids, first_ingest.evidence_ids);
    assert_eq!(legacy_item.sensitivity, Sensitivity::Internal);
    assert_eq!(legacy_revision.sensitivity, Sensitivity::Internal);
    assert_eq!(legacy_revision.title, LEGACY_TITLE);
    assert_eq!(legacy_revision.content_markdown, LEGACY_BODY);
    assert_eq!(legacy_revision.source_url, LEGACY_URL);

    // P3: the registry reconciles to the P2 identity without equating the two profiles.
    let mut registry_preflight =
        memex_registry::preflight(&profiles, &[PathBuf::from("source.json")]).unwrap();
    assert_eq!(registry_preflight.len(), 1);
    let registry_digest = registry_preflight[0].digest.clone();
    assert_sha256(&registry_digest);
    let first_registry = memex_registry::apply(&library, registry_preflight.remove(0)).unwrap();
    assert_eq!(first_registry.status, memex_registry::ApplyStatus::Created);
    assert_eq!(first_registry.record.external_source_id, LEGACY_SOURCE);
    assert_eq!(
        first_registry.record.source_id,
        prepared.capture.source.source_id
    );
    assert_eq!(first_registry.record.revision_number, 1);
    assert_eq!(
        first_registry.record.provenance.input_sha256,
        registry_digest
    );
    assert_eq!(
        first_registry.record.source_profile.default_sensitivity,
        Sensitivity::Internal
    );
    assert_eq!(
        memex_registry::read_current(&library, &first_registry.record.source_id).unwrap(),
        first_registry.record
    );

    // Registry ApplyReceipt is full owner readback. The existing CLI reconciliation report is the
    // separate privacy-safe surface and exposes only identity, digest, field names, and status.
    let registry_report_output = Command::new(env!("CARGO_BIN_EXE_memex"))
        .args(["source", "import-legacy", "--package-root"])
        .arg(&profiles)
        .args(["--profile", "source.json", "--library"])
        .arg(&library)
        .output()
        .unwrap();
    assert!(registry_report_output.status.success());
    let registry_report_text = String::from_utf8(registry_report_output.stdout).unwrap();
    let registry_report: serde_json::Value = serde_json::from_str(&registry_report_text).unwrap();
    assert_eq!(registry_report["summary"]["replayed"], 1);
    assert_eq!(
        registry_report["sources"][0]["source_id"],
        first_registry.record.source_id
    );
    assert_eq!(
        registry_report["sources"][0]["revision_id"],
        first_registry.record.revision_id
    );
    for private in [
        "Phase 7 Synthetic Legacy Source",
        "https://example.invalid/phase7-source",
        LEGACY_NOTE,
        temporary.path().to_str().unwrap(),
    ] {
        assert!(
            !registry_report_text.contains(private),
            "registry reconciliation report leaked {private}"
        );
    }

    // P4: authored derivatives preserve typed revision identity and cannot lower sensitivity.
    let analysis_request = AnalysisRequest {
        schema_version: 1,
        analysis_kind: AnalysisKind::Summary,
        title: "Phase 7 Synthetic Analysis".into(),
        body_markdown: ANALYSIS_BODY.into(),
        item_ids: vec![legacy_item_id.clone()],
        topics: vec!["phase7".into()],
        requested_sensitivity: Some(Sensitivity::Public),
        created_at: "2026-08-09T00:02:00Z".into(),
        actor_kind: ActorKind::Agent,
        actor_id: "phase7-analysis-agent".into(),
        tool_id: "phase7-rehearsal".into(),
        tool_version: "1".into(),
        config_digest: format!("sha256:{}", "1".repeat(64)),
        generated_text: None,
    };
    let analysis = memex_analysis::prepare_record(&library, &analysis_request).unwrap();
    assert_eq!(analysis.sensitivity, Sensitivity::Internal);
    assert_eq!(analysis.inputs.len(), 1);
    assert_eq!(analysis.inputs[0].item_id, legacy_item_id);
    assert_eq!(analysis.inputs[0].revision_id, legacy_revision.revision_id);
    assert_eq!(
        analysis.inputs[0].evidence_ids,
        legacy_revision.evidence_ids
    );
    assert_eq!(
        analysis.inputs[0].revision_sha256,
        typed_digest(&legacy_revision)
    );
    let first_analysis = memex_analysis::apply_record(&library, &analysis).unwrap();
    assert_eq!(first_analysis.status, ApplyStatus::Created);
    assert!(first_analysis.warning_codes.is_empty());
    assert_eq!(
        memex_analysis::read_record(&library, &analysis.analysis_id).unwrap(),
        analysis
    );

    let link_request = LinkRequest {
        schema_version: 1,
        from_analysis_id: analysis.analysis_id.clone(),
        target: LinkTargetRequest {
            kind: LinkTargetKind::ItemRevision,
            id: legacy_item_id.clone(),
            revision_id: Some(legacy_revision.revision_id.clone()),
        },
        relation: LinkRelation::Supports,
        rationale_markdown: LINK_RATIONALE.into(),
        created_at: "2026-08-09T00:03:00Z".into(),
        actor_kind: ActorKind::Agent,
        actor_id: "phase7-link-agent".into(),
        tool_id: "phase7-rehearsal".into(),
        tool_version: "1".into(),
        config_digest: format!("sha256:{}", "2".repeat(64)),
    };
    let link = memex_analysis::prepare_link(&library, &link_request).unwrap();
    assert_eq!(link.sensitivity, Sensitivity::Internal);
    let first_link = memex_analysis::apply_link(&library, &link).unwrap();
    assert_eq!(first_link.status, ApplyStatus::Created);
    assert!(first_link.warning_codes.is_empty());
    assert_eq!(
        memex_analysis::read_link(&library, &link.link_id).unwrap(),
        link
    );

    let public_recall = memex_analysis::recall(
        &library,
        &RecallQuery {
            query: "P7_REHEARSAL_FIXED_POINT".into(),
            max_sensitivity: Sensitivity::Public,
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(public_recall.hit_count, 0);
    let internal_recall = memex_analysis::recall(
        &library,
        &RecallQuery {
            query: "P7_REHEARSAL_FIXED_POINT".into(),
            max_sensitivity: Sensitivity::Internal,
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(internal_recall.retrieval_mode, "lexical_v0");
    assert_eq!(internal_recall.hit_count, 2);
    assert!(internal_recall
        .hits
        .iter()
        .any(|hit| hit.id == legacy_item_id
            && hit.revision_id.as_deref() == Some(legacy_revision.revision_id.as_str())));
    assert!(internal_recall
        .hits
        .iter()
        .any(|hit| hit.id == analysis.analysis_id && hit.revision_id.is_none()));
    assert!(internal_recall
        .hits
        .iter()
        .all(|hit| hit.sensitivity == Sensitivity::Internal));

    // P5: the legacy-derived publication converges, but PublicRelease fails closed.
    let internal_publication_request = PublicationRequest {
        schema_version: 1,
        publication_kind: PublicationKind::Briefing,
        product_id: "phase7-synthetic-legacy".into(),
        title: "Phase 7 Synthetic Internal Briefing".into(),
        summary: "Synthetic internal rehearsal summary".into(),
        body_markdown: INTERNAL_PUBLICATION_BODY.into(),
        inputs: vec![PublicationInputRequest {
            kind: PublicationInputKind::Analysis,
            id: analysis.analysis_id.clone(),
            revision_id: None,
        }],
        topics: vec!["phase7".into()],
        requested_sensitivity: Some(Sensitivity::Public),
        created_at: "2026-08-09T00:04:00Z".into(),
        actor_kind: ActorKind::Agent,
        actor_id: "phase7-publication-agent".into(),
        tool_id: "phase7-rehearsal".into(),
        tool_version: "1".into(),
        config_digest: format!("sha256:{}", "3".repeat(64)),
    };
    let internal_publication =
        memex_publication::prepare_publication(&library, &internal_publication_request).unwrap();
    assert_eq!(internal_publication.sensitivity, Sensitivity::Internal);
    assert_eq!(internal_publication.inputs.len(), 1);
    assert_eq!(internal_publication.inputs[0].id, analysis.analysis_id);
    assert_eq!(
        internal_publication.inputs[0].content_sha256,
        typed_digest(&analysis)
    );
    assert_eq!(
        internal_publication.inputs[0].sensitivity,
        Sensitivity::Internal
    );
    let first_publication =
        memex_publication::apply_publication(&library, &internal_publication).unwrap();
    assert_eq!(first_publication.status, ApplyStatus::Created);
    assert!(first_publication.warning_codes.is_empty());
    assert_eq!(
        memex_publication::read_publication(&library, &internal_publication.publication_id)
            .unwrap(),
        internal_publication
    );

    let blocked_release = release_request(
        internal_publication.publication_id.clone(),
        "2026-08-09T00:05:00Z",
    );
    let owner_items_before = memex_store::list_items(&library).unwrap();
    let owner_sources_before = memex_registry::list_current(&library).unwrap();
    let owner_analyses_before = memex_analysis::list_records(&library).unwrap();
    let owner_links_before = memex_analysis::list_links(&library).unwrap();
    let owner_publications_before = memex_publication::list_publications(&library).unwrap();
    let first_release_error =
        memex_publication::release_public(&library, &blocked_release).unwrap_err();
    assert_eq!(first_release_error.to_string(), "publication is not public");
    assert_eq!(
        memex_store::list_items(&library).unwrap(),
        owner_items_before
    );
    assert_eq!(
        memex_registry::list_current(&library).unwrap(),
        owner_sources_before
    );
    assert_eq!(
        memex_analysis::list_records(&library).unwrap(),
        owner_analyses_before
    );
    assert_eq!(
        memex_analysis::list_links(&library).unwrap(),
        owner_links_before
    );
    assert_eq!(
        memex_publication::list_publications(&library).unwrap(),
        owner_publications_before
    );

    // Replay every legacy owner operation and prove that identities and owner counts converge.
    let replay_preflight = memex_ingest::legacy::preflight_package(
        &package,
        "run.meta.json",
        &[PathBuf::from("items.jsonl")],
    )
    .unwrap();
    let replay_prepared = replay_preflight.items.into_iter().next().unwrap();
    let replay_ingest = memex_store::ingest(
        &library,
        &replay_prepared.capture,
        GenericWebpageExtractor
            .extract(&replay_prepared.capture)
            .unwrap(),
    )
    .unwrap();
    assert!(replay_ingest.idempotent_replay);
    assert_eq!(replay_ingest.capture_id, first_ingest.capture_id);
    assert_eq!(replay_ingest.item_ids, first_ingest.item_ids);
    assert_eq!(replay_ingest.revision_ids, first_ingest.revision_ids);
    assert_eq!(replay_ingest.evidence_ids, first_ingest.evidence_ids);

    let replay_registry_preflight =
        memex_registry::preflight(&profiles, &[PathBuf::from("source.json")]).unwrap();
    let replay_registry = memex_registry::apply(
        &library,
        replay_registry_preflight.into_iter().next().unwrap(),
    )
    .unwrap();
    assert_eq!(
        replay_registry.status,
        memex_registry::ApplyStatus::Replayed
    );
    assert_eq!(replay_registry.record, first_registry.record);

    let replay_analysis = memex_analysis::prepare_record(&library, &analysis_request).unwrap();
    assert_eq!(replay_analysis, analysis);
    let replay_analysis_receipt =
        memex_analysis::apply_record(&library, &replay_analysis).unwrap();
    assert_eq!(replay_analysis_receipt.status, ApplyStatus::Replayed);
    assert_eq!(
        replay_analysis_receipt.analysis_id,
        first_analysis.analysis_id
    );
    let replay_link = memex_analysis::prepare_link(&library, &link_request).unwrap();
    assert_eq!(replay_link, link);
    let replay_link_receipt = memex_analysis::apply_link(&library, &replay_link).unwrap();
    assert_eq!(replay_link_receipt.status, ApplyStatus::Replayed);
    assert_eq!(replay_link_receipt.link_id, first_link.link_id);
    let replay_publication =
        memex_publication::prepare_publication(&library, &internal_publication_request).unwrap();
    assert_eq!(replay_publication, internal_publication);
    let replay_publication_receipt =
        memex_publication::apply_publication(&library, &replay_publication).unwrap();
    assert_eq!(replay_publication_receipt.status, ApplyStatus::Replayed);
    assert_eq!(
        replay_publication_receipt.publication_id,
        first_publication.publication_id
    );
    assert_eq!(
        memex_analysis::recall(
            &library,
            &RecallQuery {
                query: "P7_REHEARSAL_FIXED_POINT".into(),
                max_sensitivity: Sensitivity::Internal,
                limit: 10,
            }
        )
        .unwrap(),
        internal_recall
    );
    let replay_owner_items_before = memex_store::list_items(&library).unwrap();
    let replay_owner_sources_before = memex_registry::list_current(&library).unwrap();
    let replay_owner_analyses_before = memex_analysis::list_records(&library).unwrap();
    let replay_owner_links_before = memex_analysis::list_links(&library).unwrap();
    let replay_owner_publications_before = memex_publication::list_publications(&library).unwrap();
    let replay_release_error =
        memex_publication::release_public(&library, &blocked_release).unwrap_err();
    assert_eq!(
        replay_release_error.to_string(),
        first_release_error.to_string()
    );
    assert_eq!(
        memex_store::list_items(&library).unwrap(),
        replay_owner_items_before
    );
    assert_eq!(
        memex_registry::list_current(&library).unwrap(),
        replay_owner_sources_before
    );
    assert_eq!(
        memex_analysis::list_records(&library).unwrap(),
        replay_owner_analyses_before
    );
    assert_eq!(
        memex_analysis::list_links(&library).unwrap(),
        replay_owner_links_before
    );
    assert_eq!(
        memex_publication::list_publications(&library).unwrap(),
        replay_owner_publications_before
    );
    assert!(!library.join("publications/public").exists());

    let receipt_forbidden = [
        LEGACY_TITLE,
        LEGACY_BODY,
        LEGACY_AUTHOR,
        LEGACY_NOTE,
        LEGACY_URL,
        ANALYSIS_BODY,
        LINK_RATIONALE,
        INTERNAL_PUBLICATION_BODY,
        "phase7-analysis-agent",
        "phase7-link-agent",
        "phase7-publication-agent",
        temporary.path().to_str().unwrap(),
    ];
    for receipt in [&first_ingest, &replay_ingest] {
        assert_receipt_private(receipt, &receipt_forbidden);
    }
    for receipt in [&first_analysis, &replay_analysis_receipt] {
        assert_receipt_private(receipt, &receipt_forbidden);
    }
    for receipt in [&first_link, &replay_link_receipt] {
        assert_receipt_private(receipt, &receipt_forbidden);
    }
    for receipt in [&first_publication, &replay_publication_receipt] {
        assert_receipt_private(receipt, &receipt_forbidden);
    }

    // A separately classified native Public fixture is the positive release control. It is not a
    // declassification or continuation of the legacy lineage above.
    let public_fixture = workspace_root().join("examples/phase1-evidence");
    let public_capture = memex_ingest::read_verified_capture(
        public_fixture.join("source.json"),
        public_fixture.join("capture.json"),
    )
    .unwrap();
    assert_eq!(
        public_capture.source.default_sensitivity,
        Sensitivity::Public
    );
    assert_eq!(public_capture.capture.sensitivity, Sensitivity::Public);
    let public_draft = GenericWebpageExtractor.extract(&public_capture).unwrap();
    let public_ingest =
        memex_store::ingest(&library, &public_capture, public_draft.clone()).unwrap();
    assert!(!public_ingest.idempotent_replay);
    let public_ingest_replay =
        memex_store::ingest(&library, &public_capture, public_draft).unwrap();
    assert!(public_ingest_replay.idempotent_replay);
    assert_eq!(public_ingest_replay.capture_id, public_ingest.capture_id);
    assert_eq!(public_ingest_replay.item_ids, public_ingest.item_ids);
    assert_eq!(
        public_ingest_replay.revision_ids,
        public_ingest.revision_ids
    );
    assert_eq!(
        public_ingest_replay.evidence_ids,
        public_ingest.evidence_ids
    );
    assert_ne!(public_ingest.item_ids[0], legacy_item_id);
    let (public_item, public_revision) =
        memex_store::read_item(&library, &public_ingest.item_ids[0]).unwrap();
    assert_eq!(public_item.sensitivity, Sensitivity::Public);
    assert_eq!(public_revision.sensitivity, Sensitivity::Public);

    let public_publication_request = PublicationRequest {
        schema_version: 1,
        publication_kind: PublicationKind::Publication,
        product_id: "phase7-public-control".into(),
        title: "Phase 7 Public Control".into(),
        summary: "P7_SYNTHETIC_PUBLIC_CONTROL_SUMMARY".into(),
        body_markdown: PUBLIC_CONTROL_BODY.into(),
        inputs: vec![PublicationInputRequest {
            kind: PublicationInputKind::ItemRevision,
            id: public_item.item_id.clone(),
            revision_id: Some(public_revision.revision_id.clone()),
        }],
        topics: vec!["phase7-control".into()],
        requested_sensitivity: Some(Sensitivity::Public),
        created_at: "2026-08-09T00:06:00Z".into(),
        actor_kind: ActorKind::Agent,
        actor_id: "phase7-public-control-agent".into(),
        tool_id: "phase7-rehearsal".into(),
        tool_version: "1".into(),
        config_digest: format!("sha256:{}", "5".repeat(64)),
    };
    let public_publication =
        memex_publication::prepare_publication(&library, &public_publication_request).unwrap();
    assert_eq!(public_publication.sensitivity, Sensitivity::Public);
    assert_eq!(public_publication.inputs[0].id, public_item.item_id);
    assert_eq!(
        public_publication.inputs[0].revision_id.as_deref(),
        Some(public_revision.revision_id.as_str())
    );
    assert_eq!(
        public_publication.inputs[0].content_sha256,
        typed_digest(&public_revision)
    );
    assert_eq!(
        public_publication.inputs[0].sensitivity,
        Sensitivity::Public
    );
    let public_publication_created =
        memex_publication::apply_publication(&library, &public_publication).unwrap();
    assert_eq!(public_publication_created.status, ApplyStatus::Created);
    let public_publication_replayed =
        memex_publication::apply_publication(&library, &public_publication).unwrap();
    assert_eq!(public_publication_replayed.status, ApplyStatus::Replayed);
    assert_eq!(
        memex_publication::read_publication(&library, &public_publication.publication_id).unwrap(),
        public_publication
    );

    let public_release_request = release_request(
        public_publication.publication_id.clone(),
        "2026-08-09T00:07:00Z",
    );
    let public_release_created =
        memex_publication::release_public(&library, &public_release_request).unwrap();
    assert_eq!(public_release_created.status, ApplyStatus::Created);
    assert_eq!(public_release_created.citation_count, 1);
    assert!(public_release_created.warning_codes.is_empty());
    assert_eq!(
        public_release_created.field_allowlist,
        vec![PublicField::Title, PublicField::Citations]
    );
    let public_release_replayed =
        memex_publication::release_public(&library, &public_release_request).unwrap();
    assert_eq!(public_release_replayed.status, ApplyStatus::Replayed);
    assert_eq!(
        public_release_replayed.release_id,
        public_release_created.release_id
    );
    assert_eq!(
        public_release_replayed.publication_id,
        public_release_created.publication_id
    );
    assert!(public_release_replayed.warning_codes.is_empty());

    let release =
        memex_publication::read_public_release(&library, &public_release_created.release_id)
            .unwrap();
    memex_publication::validate_public_release_record(&release).unwrap();
    assert_eq!(
        release.publication_sha256,
        typed_digest(&public_publication)
    );
    assert_eq!(
        memex_publication::show_public_projection(&library, &release.release_id).unwrap(),
        release.projection
    );
    assert_eq!(release.projection.citations.len(), 1);
    let citation = &release.projection.citations[0];
    assert_eq!(citation.title, public_revision.title);
    assert_eq!(citation.source_url, public_revision.source_url);
    assert_eq!(citation.canonical_url, public_revision.canonical_url);
    assert_eq!(
        citation.citation_id,
        format!(
            "citation_{:x}",
            Sha256::digest(
                format!("{}\0{}", public_item.item_id, public_revision.revision_id).as_bytes()
            )
        )
    );

    // The P5 public directory is the documented export boundary; owner state above was read only
    // through owner APIs rather than reconstructed from internal storage.
    let bundle = library
        .join("publications/public")
        .join(&public_release_created.release_id);
    let manifest_bytes = fs::read(bundle.join("manifest.json")).unwrap();
    let projection_bytes = fs::read(bundle.join("publication.json")).unwrap();
    let manifest: memex_publication::PublicBundleManifest =
        serde_json::from_slice(&manifest_bytes).unwrap();
    let projection: memex_publication::PublicProjection =
        serde_json::from_slice(&projection_bytes).unwrap();
    let markdown = fs::read_to_string(bundle.join("publication.md")).unwrap();
    assert_eq!(manifest.release_id, release.release_id);
    assert_eq!(manifest.publication_id, release.publication_id);
    assert_eq!(manifest.publication_sha256, release.publication_sha256);
    assert_eq!(manifest.projection_sha256, release.projection_sha256);
    assert_eq!(manifest.outputs.json, "publication.json");
    assert_eq!(manifest.outputs.markdown, "publication.md");
    assert_eq!(projection, release.projection);
    assert_eq!(manifest_bytes, canonical_pretty_json(&manifest));
    assert_eq!(projection_bytes, canonical_pretty_json(&projection));
    assert_eq!(
        markdown,
        memex_publication::render_public_projection_markdown(&projection)
    );
    let manifest_json = String::from_utf8(manifest_bytes).unwrap();
    let projection_json = String::from_utf8(projection_bytes).unwrap();
    assert!(!projection_json.contains("\"summary\""));
    assert!(!projection_json.contains("\"body_markdown\""));
    assert!(!projection_json.contains("\"topics\""));
    for private in [
        legacy_item_id.as_str(),
        legacy_revision.revision_id.as_str(),
        analysis.analysis_id.as_str(),
        public_item.item_id.as_str(),
        public_revision.revision_id.as_str(),
        ANALYSIS_BODY,
        LINK_RATIONALE,
        INTERNAL_PUBLICATION_BODY,
        PUBLIC_CONTROL_BODY,
        "provenance",
        temporary.path().to_str().unwrap(),
    ]
    .into_iter()
    .chain(
        legacy_revision
            .evidence_ids
            .iter()
            .chain(public_revision.evidence_ids.iter())
            .map(String::as_str),
    ) {
        assert!(
            !manifest_json.contains(private)
                && !projection_json.contains(private)
                && !markdown.contains(private),
            "public bundle leaked {private}"
        );
    }
    assert!(!library.join("publications/latest").exists());

    assert_eq!(memex_store::list_items(&library).unwrap().len(), 2);
    assert_eq!(memex_registry::list_current(&library).unwrap().len(), 1);
    assert_eq!(memex_analysis::list_records(&library).unwrap().len(), 1);
    assert_eq!(memex_analysis::list_links(&library).unwrap().len(), 1);
    assert_eq!(
        memex_publication::list_publications(&library)
            .unwrap()
            .publications
            .len(),
        2
    );

    let public_receipt_forbidden = [
        public_revision.title.as_str(),
        public_revision.source_url.as_str(),
        PUBLIC_CONTROL_BODY,
        "P7_SYNTHETIC_PUBLIC_CONTROL_SUMMARY",
        "phase7-public-control-agent",
        "phase7-release-agent",
        temporary.path().to_str().unwrap(),
    ];
    for receipt in [&public_ingest, &public_ingest_replay] {
        assert_receipt_private(receipt, &public_receipt_forbidden);
    }
    for receipt in [&public_publication_created, &public_publication_replayed] {
        assert_receipt_private(receipt, &public_receipt_forbidden);
    }
    for receipt in [&public_release_created, &public_release_replayed] {
        assert_receipt_private(receipt, &public_receipt_forbidden);
    }
}
