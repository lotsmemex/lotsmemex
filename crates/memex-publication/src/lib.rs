use anyhow::Result;
use chrono::DateTime;
use fs2::FileExt;
use memex_analysis::{ActorKind, ApplyStatus};
use memex_core::Sensitivity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use url::Url;

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_INPUT_COMPONENT_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationKind {
    Briefing,
    Dossier,
    Publication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationInputKind {
    Analysis,
    ItemRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationInputRequest {
    pub kind: PublicationInputKind,
    pub id: String,
    pub revision_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRequest {
    pub schema_version: u32,
    pub publication_kind: PublicationKind,
    pub product_id: String,
    pub title: String,
    pub summary: String,
    pub body_markdown: String,
    pub inputs: Vec<PublicationInputRequest>,
    pub topics: Vec<String>,
    pub requested_sensitivity: Option<Sensitivity>,
    pub created_at: String,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationInputRef {
    pub kind: PublicationInputKind,
    pub id: String,
    pub revision_id: Option<String>,
    pub content_sha256: String,
    pub sensitivity: Sensitivity,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationProvenance {
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRecord {
    pub schema_version: u32,
    pub publication_id: String,
    pub publication_kind: PublicationKind,
    pub product_id: String,
    pub title: String,
    pub summary: String,
    pub body_markdown: String,
    pub inputs: Vec<PublicationInputRef>,
    pub topics: Vec<String>,
    pub sensitivity: Sensitivity,
    pub created_at: String,
    pub provenance: PublicationProvenance,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub publication_id: String,
    pub status: ApplyStatus,
    pub publication_kind: PublicationKind,
    pub sensitivity: Sensitivity,
    pub input_count: usize,
    pub warning_codes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationIndexEntry {
    pub publication_id: String,
    pub publication_kind: PublicationKind,
    pub product_id: String,
    pub title: String,
    pub sensitivity: Sensitivity,
    pub created_at: String,
    pub input_count: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationListReport {
    pub schema_version: u32,
    pub publications: Vec<PublicationIndexEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationShowReport {
    pub schema_version: u32,
    pub record: PublicationRecord,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicTarget {
    YuanfangPublicV1,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PublicField {
    Title,
    Summary,
    BodyMarkdown,
    Topics,
    Citations,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicReleaseRequest {
    pub schema_version: u32,
    pub target: PublicTarget,
    pub publication_id: String,
    pub published_at: String,
    pub field_allowlist: Vec<PublicField>,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicCitation {
    pub citation_id: String,
    pub title: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicProjection {
    pub schema_version: u32,
    pub release_id: String,
    pub target: PublicTarget,
    pub publication_id: String,
    pub publication_kind: PublicationKind,
    pub product_id: String,
    pub published_at: String,
    pub field_allowlist: Vec<PublicField>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<String>>,
    pub citations: Vec<PublicCitation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicReleaseProvenance {
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicReleaseRecord {
    pub schema_version: u32,
    pub release_id: String,
    pub publication_id: String,
    pub publication_sha256: String,
    pub target: PublicTarget,
    pub field_allowlist: Vec<PublicField>,
    pub published_at: String,
    pub projection_sha256: String,
    pub projection: PublicProjection,
    pub provenance: PublicReleaseProvenance,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicReleaseReceipt {
    pub release_id: String,
    pub status: ApplyStatus,
    pub publication_id: String,
    pub target: PublicTarget,
    pub field_allowlist: Vec<PublicField>,
    pub citation_count: usize,
    pub warning_codes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicBundleOutputs {
    pub json: String,
    pub markdown: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicBundleManifest {
    pub schema_version: u32,
    pub release_id: String,
    pub publication_id: String,
    pub target: PublicTarget,
    pub publication_sha256: String,
    pub projection_sha256: String,
    pub outputs: PublicBundleOutputs,
}

pub fn read_publication_request(path: impl AsRef<Path>) -> Result<PublicationRequest> {
    let bytes = read_bounded_regular(path.as_ref(), MAX_REQUEST_BYTES, "publication request")?;
    let request = serde_json::from_slice(&bytes)?;
    validate_publication_request(&request)?;
    Ok(request)
}
pub fn read_public_release_request(path: impl AsRef<Path>) -> Result<PublicReleaseRequest> {
    let bytes = read_bounded_regular(path.as_ref(), 1024 * 1024, "public release request")?;
    let mut request: PublicReleaseRequest = serde_json::from_slice(&bytes)?;
    validate_public_release_request(&mut request)?;
    Ok(request)
}
pub fn validate_public_release_request(request: &mut PublicReleaseRequest) -> Result<()> {
    if request.schema_version != 1 || !valid_publication_id(&request.publication_id) {
        anyhow::bail!("invalid public release request");
    }
    DateTime::parse_from_rfc3339(&request.published_at)
        .map_err(|_| anyhow::anyhow!("invalid published_at"))?;
    for value in [&request.actor_id, &request.tool_id, &request.tool_version] {
        text(value, 256, true, "release provenance")?;
    }
    if !valid_sha256(&request.config_digest) {
        anyhow::bail!("invalid config digest");
    }
    if serde_json::to_vec(request)?.len() > 1024 * 1024 {
        anyhow::bail!("oversized public release request");
    }
    request.field_allowlist.sort();
    if request.field_allowlist.windows(2).any(|v| v[0] == v[1]) {
        anyhow::bail!("duplicate public fields");
    }
    if !request.field_allowlist.contains(&PublicField::Title)
        || !request.field_allowlist.contains(&PublicField::Citations)
    {
        anyhow::bail!("required public fields missing");
    }
    Ok(())
}
pub fn prepare_public_release(
    library: impl AsRef<Path>,
    request: &PublicReleaseRequest,
) -> Result<PublicReleaseRecord> {
    let mut request = request.clone();
    validate_public_release_request(&mut request)?;
    let publication = read_publication(library.as_ref(), &request.publication_id)?;
    verify_publication_inputs(library.as_ref(), &publication)?;
    if publication.sensitivity != Sensitivity::Public {
        anyhow::bail!("publication is not public");
    }
    let mut citation_revisions = BTreeMap::new();
    for input in &publication.inputs {
        match input.kind {
            PublicationInputKind::ItemRevision => {
                let (_, revision) = memex_store::read_item(library.as_ref(), &input.id)?;
                citation_revisions.insert(
                    (revision.item_id.clone(), revision.revision_id.clone()),
                    revision,
                );
            }
            PublicationInputKind::Analysis => {
                let analysis = memex_analysis::read_record(library.as_ref(), &input.id)?;
                if analysis.sensitivity != Sensitivity::Public {
                    anyhow::bail!("analysis is not public");
                }
                for reference in analysis.inputs {
                    let (item, revision) =
                        memex_store::read_item(library.as_ref(), &reference.item_id)?;
                    if item.current_revision != reference.revision_id
                        || revision.revision_id != reference.revision_id
                        || item.sensitivity != Sensitivity::Public
                        || revision.sensitivity != Sensitivity::Public
                        || digest(&revision)? != reference.revision_sha256
                    {
                        anyhow::bail!("transitive input drift or non-public");
                    }
                    citation_revisions.insert(
                        (revision.item_id.clone(), revision.revision_id.clone()),
                        revision,
                    );
                }
            }
        }
    }
    let citations = citation_revisions
        .values()
        .map(citation)
        .collect::<Result<Vec<_>>>()?;
    let mut projection = PublicProjection {
        schema_version: 1,
        release_id: String::new(),
        target: request.target.clone(),
        publication_id: publication.publication_id.clone(),
        publication_kind: publication.publication_kind.clone(),
        product_id: publication.product_id.clone(),
        published_at: request.published_at.clone(),
        field_allowlist: request.field_allowlist.clone(),
        title: publication.title.clone(),
        summary: request
            .field_allowlist
            .contains(&PublicField::Summary)
            .then(|| publication.summary.clone()),
        body_markdown: request
            .field_allowlist
            .contains(&PublicField::BodyMarkdown)
            .then(|| publication.body_markdown.clone()),
        topics: request
            .field_allowlist
            .contains(&PublicField::Topics)
            .then(|| publication.topics.clone()),
        citations,
    };
    let provenance = PublicReleaseProvenance {
        actor_kind: request.actor_kind.clone(),
        actor_id: request.actor_id.clone(),
        tool_id: request.tool_id.clone(),
        tool_version: request.tool_version.clone(),
        config_digest: request.config_digest.clone(),
    };
    let publication_sha256 = digest(&publication)?;
    let projection_sha256 = projection_digest(&projection)?;
    let release_id = format!(
        "release_{:x}",
        Sha256::digest(serde_json::to_vec(&PublicReleaseIdPayload {
            publication_id: publication.publication_id.clone(),
            publication_sha256: publication_sha256.clone(),
            target: request.target.clone(),
            field_allowlist: request.field_allowlist.clone(),
            published_at: request.published_at.clone(),
            projection_sha256: projection_sha256.clone(),
            provenance: provenance.clone()
        })?)
    );
    projection.release_id = release_id.clone();
    Ok(PublicReleaseRecord {
        schema_version: 1,
        release_id,
        publication_id: publication.publication_id.clone(),
        publication_sha256,
        target: request.target.clone(),
        field_allowlist: request.field_allowlist.clone(),
        published_at: request.published_at.clone(),
        projection_sha256,
        projection,
        provenance,
    })
}
fn citation(revision: &memex_core::ItemRevision) -> Result<PublicCitation> {
    validate_public_web_url(&revision.source_url)?;
    if let Some(canonical_url) = &revision.canonical_url {
        validate_public_web_url(canonical_url)?;
    }
    Ok(PublicCitation {
        citation_id: format!(
            "citation_{:x}",
            Sha256::digest(format!("{}\0{}", revision.item_id, revision.revision_id).as_bytes())
        ),
        title: revision.title.clone(),
        source_url: revision.source_url.clone(),
        canonical_url: revision.canonical_url.clone(),
    })
}

fn validate_public_web_url(value: &str) -> Result<()> {
    let parsed = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid public web url"))?;
    if !parsed.has_host()
        || !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        anyhow::bail!("invalid public web url");
    }
    Ok(())
}
#[derive(Serialize)]
struct PublicProjectionHashPayload {
    schema_version: u32,
    publication_id: String,
    target: PublicTarget,
    publication_kind: PublicationKind,
    product_id: String,
    published_at: String,
    field_allowlist: Vec<PublicField>,
    title: String,
    summary: Option<String>,
    body_markdown: Option<String>,
    topics: Option<Vec<String>>,
    citations: Vec<PublicCitation>,
}
#[derive(Serialize)]
struct PublicReleaseIdPayload {
    publication_id: String,
    publication_sha256: String,
    target: PublicTarget,
    field_allowlist: Vec<PublicField>,
    published_at: String,
    projection_sha256: String,
    provenance: PublicReleaseProvenance,
}
fn projection_digest(value: &PublicProjection) -> Result<String> {
    let payload = PublicProjectionHashPayload {
        schema_version: value.schema_version,
        publication_id: value.publication_id.clone(),
        target: value.target.clone(),
        publication_kind: value.publication_kind.clone(),
        product_id: value.product_id.clone(),
        published_at: value.published_at.clone(),
        field_allowlist: value.field_allowlist.clone(),
        title: value.title.clone(),
        summary: value.summary.clone(),
        body_markdown: value.body_markdown.clone(),
        topics: value.topics.clone(),
        citations: value.citations.clone(),
    };
    digest(&payload)
}

pub fn validate_public_release_record(record: &PublicReleaseRecord) -> Result<()> {
    if record.schema_version != 1
        || !valid_release_id(&record.release_id)
        || !valid_publication_id(&record.publication_id)
        || !valid_sha256(&record.publication_sha256)
        || !valid_sha256(&record.projection_sha256)
    {
        anyhow::bail!("invalid public release identity");
    }
    DateTime::parse_from_rfc3339(&record.published_at)
        .map_err(|_| anyhow::anyhow!("invalid release published_at"))?;
    let mut fields = record.field_allowlist.clone();
    validate_public_fields(&mut fields)?;
    if fields != record.field_allowlist {
        anyhow::bail!("noncanonical release allowlist");
    }
    for value in [
        &record.provenance.actor_id,
        &record.provenance.tool_id,
        &record.provenance.tool_version,
    ] {
        text(value, 256, true, "release provenance")?;
    }
    if !valid_sha256(&record.provenance.config_digest) {
        anyhow::bail!("invalid release config digest");
    }
    validate_public_projection(&record.projection, &record.release_id, &fields)?;
    if record.projection.publication_id != record.publication_id
        || record.projection.target != record.target
        || record.projection.published_at != record.published_at
    {
        anyhow::bail!("public release projection parity mismatch");
    }
    if record.projection_sha256 != projection_digest(&record.projection)? {
        anyhow::bail!("public projection digest mismatch");
    }
    let expected_id = format!(
        "release_{:x}",
        Sha256::digest(serde_json::to_vec(&PublicReleaseIdPayload {
            publication_id: record.publication_id.clone(),
            publication_sha256: record.publication_sha256.clone(),
            target: record.target.clone(),
            field_allowlist: record.field_allowlist.clone(),
            published_at: record.published_at.clone(),
            projection_sha256: record.projection_sha256.clone(),
            provenance: record.provenance.clone(),
        })?)
    );
    if expected_id != record.release_id {
        anyhow::bail!("public release payload identity mismatch");
    }
    Ok(())
}

fn validate_public_fields(fields: &mut [PublicField]) -> Result<()> {
    fields.sort();
    if fields.windows(2).any(|v| v[0] == v[1])
        || !fields.contains(&PublicField::Title)
        || !fields.contains(&PublicField::Citations)
    {
        anyhow::bail!("invalid public field allowlist");
    }
    Ok(())
}

fn validate_public_projection(
    projection: &PublicProjection,
    release_id: &str,
    fields: &[PublicField],
) -> Result<()> {
    if projection.schema_version != 1
        || projection.release_id != release_id
        || !valid_publication_id(&projection.publication_id)
        || projection.field_allowlist != fields
        || projection.title.is_empty()
        || projection.title.len() > 512
        || projection.title.chars().any(char::is_control)
    {
        anyhow::bail!("invalid public projection");
    }
    DateTime::parse_from_rfc3339(&projection.published_at)
        .map_err(|_| anyhow::anyhow!("invalid projection published_at"))?;
    let optional = [
        (PublicField::Summary, projection.summary.as_ref()),
        (PublicField::BodyMarkdown, projection.body_markdown.as_ref()),
    ];
    for (field, value) in optional {
        if fields.contains(&field) != value.is_some() {
            anyhow::bail!("public projection optional field mismatch");
        }
        if let Some(value) = value {
            text(
                value,
                if field == PublicField::Summary {
                    8 * 1024
                } else {
                    512 * 1024
                },
                false,
                "projection",
            )?;
        }
    }
    if fields.contains(&PublicField::Topics) != projection.topics.is_some()
        || projection.topics.as_ref().is_some_and(|topics| {
            topics.len() > 64
                || !unique(topics)
                || topics.iter().any(|v| text(v, 256, true, "topic").is_err())
        })
    {
        anyhow::bail!("public projection topics mismatch");
    }
    if projection.citations.is_empty()
        || projection.citations.iter().any(|citation| {
            !valid_citation_id(&citation.citation_id)
                || text(&citation.title, 512, true, "citation").is_err()
                || validate_public_web_url(&citation.source_url).is_err()
                || citation
                    .canonical_url
                    .as_ref()
                    .is_some_and(|v| validate_public_web_url(v).is_err())
        })
    {
        anyhow::bail!("invalid public citations");
    }
    Ok(())
}

fn revalidate_prepared_release(root: &Path, record: &PublicReleaseRecord) -> Result<()> {
    validate_public_release_record(record)?;
    let request = PublicReleaseRequest {
        schema_version: 1,
        target: record.target.clone(),
        publication_id: record.publication_id.clone(),
        published_at: record.published_at.clone(),
        field_allowlist: record.field_allowlist.clone(),
        actor_kind: record.provenance.actor_kind.clone(),
        actor_id: record.provenance.actor_id.clone(),
        tool_id: record.provenance.tool_id.clone(),
        tool_version: record.provenance.tool_version.clone(),
        config_digest: record.provenance.config_digest.clone(),
    };
    if prepare_public_release(root, &request)? != *record {
        anyhow::bail!("prepared public release no longer matches current refs");
    }
    Ok(())
}

pub fn apply_public_release(
    library: impl AsRef<Path>,
    record: &PublicReleaseRecord,
) -> Result<PublicReleaseReceipt> {
    let root = safe_library_root(library.as_ref())?;
    revalidate_prepared_release(&root, record)?;
    let publications = safe_create_dir(&root, "publications")?;
    let lock = lock_file(&publications)?;
    revalidate_prepared_release(&root, record)?;
    let releases = safe_create_dir(&publications, "releases")?;
    let path = releases.join(format!("{}.json", record.release_id));
    let bytes = canonical_release_bytes(record)?;
    let status = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = create_regular_new(&path)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            File::open(&releases)?.sync_all()?;
            ApplyStatus::Created
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            if read_public_release_file(&path)? != *record {
                anyhow::bail!("public release immutable bytes differ");
            }
            ApplyStatus::Replayed
        }
        Ok(_) => anyhow::bail!("unsafe public release record"),
        Err(error) => return Err(error.into()),
    };
    lock.unlock()?;
    let mut receipt = PublicReleaseReceipt {
        release_id: record.release_id.clone(),
        status,
        publication_id: record.publication_id.clone(),
        target: record.target.clone(),
        field_allowlist: record.field_allowlist.clone(),
        citation_count: record.projection.citations.len(),
        warning_codes: vec![],
    };
    receipt.warning_codes = write_public_bundle(&root, record);
    Ok(receipt)
}

pub fn release_public(
    library: impl AsRef<Path>,
    request: &PublicReleaseRequest,
) -> Result<PublicReleaseReceipt> {
    let mut request = request.clone();
    validate_public_release_request(&mut request)?;
    let root = safe_library_root(library.as_ref())?;
    if let Some(record) = find_matching_public_release(&root, &request)? {
        verify_historical_public_release(&root, &record)?;
        return Ok(PublicReleaseReceipt {
            release_id: record.release_id.clone(),
            status: ApplyStatus::Replayed,
            publication_id: record.publication_id.clone(),
            target: record.target.clone(),
            field_allowlist: record.field_allowlist.clone(),
            citation_count: record.projection.citations.len(),
            warning_codes: write_public_bundle(&root, &record),
        });
    }
    let record = prepare_public_release(&root, &request)?;
    apply_public_release(&root, &record)
}

fn find_matching_public_release(
    root: &Path,
    request: &PublicReleaseRequest,
) -> Result<Option<PublicReleaseRecord>> {
    let publications = root.join("publications");
    let releases = publications.join("releases");
    match fs::symlink_metadata(&publications) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication directory"),
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(&releases) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe public release directory"),
        Err(error) => return Err(error.into()),
    }
    let mut matched = None;
    for entry in fs::read_dir(&releases)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("unsafe release filename"))?;
        if !name.ends_with(".json") {
            anyhow::bail!("unexpected public release artifact");
        }
        let release_id = &name[..name.len() - ".json".len()];
        if !valid_release_id(release_id) {
            anyhow::bail!("invalid public release filename");
        }
        let record = read_public_release_file(&entry.path())?;
        if record.release_id != release_id {
            anyhow::bail!("public release filename identity mismatch");
        }
        let same = record.publication_id == request.publication_id
            && record.target == request.target
            && record.field_allowlist == request.field_allowlist
            && record.published_at == request.published_at
            && record.provenance.actor_kind == request.actor_kind
            && record.provenance.actor_id == request.actor_id
            && record.provenance.tool_id == request.tool_id
            && record.provenance.tool_version == request.tool_version
            && record.provenance.config_digest == request.config_digest;
        if same && matched.replace(record).is_some() {
            anyhow::bail!("multiple matching public releases");
        }
    }
    Ok(matched)
}

fn verify_historical_public_release(root: &Path, record: &PublicReleaseRecord) -> Result<()> {
    validate_public_release_record(record)?;
    let publication = read_publication(root, &record.publication_id)?;
    if publication.sensitivity != Sensitivity::Public
        || record.publication_sha256 != digest(&publication)?
        || record.projection.publication_kind != publication.publication_kind
        || record.projection.product_id != publication.product_id
        || record.projection.title != publication.title
        || record.projection.summary
            != record
                .field_allowlist
                .contains(&PublicField::Summary)
                .then(|| publication.summary.clone())
        || record.projection.body_markdown
            != record
                .field_allowlist
                .contains(&PublicField::BodyMarkdown)
                .then(|| publication.body_markdown.clone())
        || record.projection.topics
            != record
                .field_allowlist
                .contains(&PublicField::Topics)
                .then(|| publication.topics.clone())
    {
        anyhow::bail!("historical public release publication parity mismatch");
    }
    Ok(())
}

fn write_public_bundle(root: &Path, record: &PublicReleaseRecord) -> Vec<String> {
    let warning_codes = [
        "public_manifest_projection_pending",
        "public_json_projection_pending",
        "public_markdown_projection_pending",
    ];
    let stored = match read_public_release(root, &record.release_id) {
        Ok(stored) => stored,
        Err(_) => return warning_codes.into_iter().map(String::from).collect(),
    };
    let publications = root.join("publications");
    let public = match safe_create_dir(&publications, "public") {
        Ok(path) => path,
        Err(_) => return warning_codes.into_iter().map(String::from).collect(),
    };
    let directory = match safe_create_dir(&public, &stored.release_id) {
        Ok(path) => path,
        Err(_) => return warning_codes.into_iter().map(String::from).collect(),
    };
    let manifest = PublicBundleManifest {
        schema_version: 1,
        release_id: stored.release_id.clone(),
        publication_id: stored.publication_id.clone(),
        target: stored.target.clone(),
        publication_sha256: stored.publication_sha256.clone(),
        projection_sha256: stored.projection_sha256.clone(),
        outputs: PublicBundleOutputs {
            json: "publication.json".into(),
            markdown: "publication.md".into(),
        },
    };
    let attempts = [
        (
            warning_codes[0],
            directory.join("manifest.json"),
            canonical_json(&manifest),
        ),
        (
            warning_codes[1],
            directory.join("publication.json"),
            canonical_json(&stored.projection),
        ),
        (
            warning_codes[2],
            directory.join("publication.md"),
            Ok(render_public_projection_markdown(&stored.projection).into_bytes()),
        ),
    ];
    attempts
        .into_iter()
        .filter_map(|(warning, path, bytes)| {
            match bytes.and_then(|bytes| write_projection(&path, &bytes)) {
                Ok(_) => None,
                Err(_) => Some(warning.into()),
            }
        })
        .collect()
}

pub fn read_public_release(
    library: impl AsRef<Path>,
    release_id: &str,
) -> Result<PublicReleaseRecord> {
    if !valid_release_id(release_id) {
        anyhow::bail!("invalid public release id");
    }
    let root = safe_library_root(library.as_ref())?;
    let publications = root.join("publications");
    let releases = publications.join("releases");
    for path in [&publications, &releases] {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
            Ok(_) => anyhow::bail!("unsafe public release directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("public release not found")
            }
            Err(error) => return Err(error.into()),
        }
    }
    let record = read_public_release_file(&releases.join(format!("{release_id}.json")))?;
    if record.release_id != release_id {
        anyhow::bail!("public release path identity mismatch");
    }
    Ok(record)
}

pub fn show_public_projection(
    library: impl AsRef<Path>,
    release_id: &str,
) -> Result<PublicProjection> {
    Ok(read_public_release(library, release_id)?.projection)
}

pub fn read_public_projection_optional(
    library: impl AsRef<Path>,
    release_id: &str,
) -> Result<Option<PublicProjection>> {
    if !valid_release_id(release_id) {
        return Ok(None);
    }
    let root = safe_library_root(library.as_ref())?;
    let publications = root.join("publications");
    match fs::symlink_metadata(&publications) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication directory"),
        Err(e) => return Err(e.into()),
    }
    let releases = publications.join("releases");
    match fs::symlink_metadata(&releases) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe public release directory"),
        Err(e) => return Err(e.into()),
    }
    let path = releases.join(format!("{release_id}.json"));
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_file() => {
            show_public_projection(&root, release_id).map(Some)
        }
        Ok(_) => anyhow::bail!("unsafe public release record"),
        Err(e) => Err(e.into()),
    }
}

pub fn render_public_projection_markdown(projection: &PublicProjection) -> String {
    let mut output = format!("# {}\n", projection.title);
    if let Some(summary) = &projection.summary {
        output.push_str(&format!("\n{}\n", summary));
    }
    if let Some(topics) = &projection.topics {
        output.push_str(&format!("\nTopics: {}\n", topics.join(", ")));
    }
    if let Some(body) = &projection.body_markdown {
        output.push_str(&format!("\n{}\n", body));
    }
    output.push_str("\n## Citations\n");
    for citation in &projection.citations {
        output.push_str(&format!(
            "\n- [{}]({})\n",
            citation.title,
            citation
                .canonical_url
                .as_deref()
                .unwrap_or(&citation.source_url)
        ));
    }
    output
}

pub fn apply_publication(
    library: impl AsRef<Path>,
    record: &PublicationRecord,
) -> Result<PublicationReceipt> {
    validate_publication_record(record)?;
    let root = safe_library_root(library.as_ref())?;
    verify_publication_inputs(&root, record)?;
    let publications = safe_create_dir(&root, "publications")?;
    let records = safe_create_dir(&publications, "records")?;
    let lock = lock_file(&publications)?;
    verify_publication_inputs(&root, record)?;
    let path = records.join(format!("{}.json", record.publication_id));
    let bytes = canonical_record_bytes(record)?;
    let status = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = create_regular_new(&path)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            File::open(&records)?.sync_all()?;
            ApplyStatus::Created
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            if read_publication_file(&path)? != *record {
                anyhow::bail!("publication record immutable bytes differ");
            }
            ApplyStatus::Replayed
        }
        Ok(_) => anyhow::bail!("unsafe publication record"),
        Err(error) => return Err(error.into()),
    };
    lock.unlock()?;
    let mut receipt = PublicationReceipt {
        publication_id: record.publication_id.clone(),
        status,
        publication_kind: record.publication_kind.clone(),
        sensitivity: record.sensitivity,
        input_count: record.inputs.len(),
        warning_codes: vec![],
    };
    if write_projection(
        &records.join(format!("{}.md", record.publication_id)),
        render_publication_markdown(record).as_bytes(),
    )
    .is_err()
    {
        receipt
            .warning_codes
            .push("publication_markdown_projection_pending".into());
    }
    match list_publications(&root).and_then(|report| {
        write_projection(
            &publications.join("index.json"),
            canonical_json(&report)?.as_slice(),
        )
    }) {
        Ok(_) => {}
        Err(_) => receipt
            .warning_codes
            .push("publication_json_index_projection_pending".into()),
    }
    match list_publications(&root).and_then(|report| {
        write_projection(
            &publications.join("index.md"),
            render_publication_list_markdown(&report).as_bytes(),
        )
    }) {
        Ok(_) => {}
        Err(_) => receipt
            .warning_codes
            .push("publication_markdown_index_projection_pending".into()),
    }
    Ok(receipt)
}

pub fn read_publication(
    library: impl AsRef<Path>,
    publication_id: &str,
) -> Result<PublicationRecord> {
    if !valid_publication_id(publication_id) {
        anyhow::bail!("invalid publication id");
    }
    let root = safe_library_root(library.as_ref())?;
    let publications = root.join("publications");
    let records = publications.join("records");
    for path in [&publications, &records] {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
            Ok(_) => anyhow::bail!("unsafe publication directory"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("publication not found")
            }
            Err(e) => return Err(e.into()),
        }
    }
    let record = read_publication_file(&records.join(format!("{publication_id}.json")))?;
    if record.publication_id != publication_id {
        anyhow::bail!("publication record path identity mismatch");
    }
    Ok(record)
}

fn read_owned_publication_index(root: &Path) -> Result<Vec<PublicationIndexEntry>> {
    let publications = root.join("publications");
    match fs::symlink_metadata(&publications) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication directory"),
        Err(e) => return Err(e.into()),
    }
    let records = publications.join("records");
    let mut record_ids = Vec::new();
    match fs::symlink_metadata(&records) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(publications.join("index.json")) {
                Err(index_error) if index_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Vec::new())
                }
                Ok(_) => anyhow::bail!("publication index exists without records"),
                Err(index_error) => Err(index_error.into()),
            };
        }
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication records"),
        Err(e) => return Err(e.into()),
    }
    for entry in fs::read_dir(&records)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid record filename"))?;
        if let Some(id) = name.strip_suffix(".json") {
            if entry.file_type()?.is_symlink()
                || !entry.file_type()?.is_file()
                || !valid_publication_id(id)
            {
                anyhow::bail!("invalid record filename");
            }
            record_ids.push(id.to_owned());
        } else {
            let Some(id) = name.strip_suffix(".md") else {
                anyhow::bail!("invalid record filename");
            };
            let metadata = entry.file_type()?;
            if metadata.is_symlink() || !metadata.is_file() || !valid_publication_id(id) {
                anyhow::bail!("invalid record filename");
            }
        }
    }
    record_ids.sort();
    let index = publications.join("index.json");
    if record_ids.is_empty() {
        match fs::symlink_metadata(&index) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Ok(m) if !m.file_type().is_symlink() && m.is_file() => {}
            Ok(_) => anyhow::bail!("unsafe publication index"),
            Err(e) => return Err(e.into()),
        }
    }
    let bytes = read_bounded_regular(&index, 8 * 1024 * 1024, "publication index")?;
    let report: PublicationListReport = serde_json::from_slice(&bytes)?;
    if report.schema_version != 1 {
        anyhow::bail!("invalid publication index");
    }
    let mut index_ids = report
        .publications
        .iter()
        .map(|e| e.publication_id.clone())
        .collect::<Vec<_>>();
    if report
        .publications
        .iter()
        .any(|e| !valid_publication_id(&e.publication_id))
    {
        anyhow::bail!("invalid publication index");
    }
    index_ids.sort();
    if index_ids.windows(2).any(|v| v[0] == v[1]) || index_ids != record_ids {
        anyhow::bail!("publication index/records mismatch");
    }
    Ok(report.publications)
}

fn publication_index_entry(record: &PublicationRecord) -> PublicationIndexEntry {
    PublicationIndexEntry {
        publication_id: record.publication_id.clone(),
        publication_kind: record.publication_kind.clone(),
        product_id: record.product_id.clone(),
        title: record.title.clone(),
        sensitivity: record.sensitivity,
        created_at: record.created_at.clone(),
        input_count: record.inputs.len(),
    }
}

pub fn read_publication_at_sensitivity(
    library: impl AsRef<Path>,
    publication_id: &str,
    max: Sensitivity,
) -> Result<Option<PublicationRecord>> {
    if !valid_publication_id(publication_id) {
        return Ok(None);
    }
    let root = safe_library_root(library.as_ref())?;
    let Some(entry) = read_owned_publication_index(&root)?
        .into_iter()
        .find(|e| e.publication_id == publication_id)
    else {
        return Ok(None);
    };
    if entry.sensitivity > max {
        return Ok(None);
    }
    let record = read_publication(&root, publication_id)?;
    if publication_index_entry(&record) != entry {
        anyhow::bail!("publication index entry mismatch");
    }
    Ok(Some(record))
}

pub fn list_publications_at_sensitivity(
    library: impl AsRef<Path>,
    max: Sensitivity,
) -> Result<Vec<PublicationRecord>> {
    let root = safe_library_root(library.as_ref())?;
    let mut out = Vec::new();
    for entry in read_owned_publication_index(&root)? {
        if entry.sensitivity <= max {
            let record = read_publication(&root, &entry.publication_id)?;
            if publication_index_entry(&record) != entry {
                anyhow::bail!("publication index entry mismatch");
            }
            out.push(record);
        }
    }
    Ok(out)
}

pub fn show_publication(
    library: impl AsRef<Path>,
    publication_id: &str,
) -> Result<PublicationShowReport> {
    Ok(PublicationShowReport {
        schema_version: 1,
        record: read_publication(library, publication_id)?,
    })
}
pub fn list_publications(library: impl AsRef<Path>) -> Result<PublicationListReport> {
    let root = safe_library_root(library.as_ref())?;
    let publications = root.join("publications");
    let meta = match fs::symlink_metadata(&publications) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PublicationListReport {
                schema_version: 1,
                publications: vec![],
            })
        }
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => m,
        Ok(_) => anyhow::bail!("unsafe publication directory"),
        Err(e) => return Err(e.into()),
    };
    let _ = meta;
    let records = publications.join("records");
    match fs::symlink_metadata(&records) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PublicationListReport {
                schema_version: 1,
                publications: vec![],
            })
        }
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication records"),
        Err(e) => return Err(e.into()),
    };
    let mut entries = Vec::new();
    for entry in fs::read_dir(&records)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid record filename"))?;
        if name.ends_with(".md") {
            continue;
        }
        if !name.ends_with(".json") || !valid_publication_id(&name[..name.len() - 5]) {
            anyhow::bail!("invalid record filename");
        }
        let record = read_publication_file(&entry.path())?;
        if name != format!("{}.json", record.publication_id) {
            anyhow::bail!("publication path identity mismatch");
        }
        entries.push(PublicationIndexEntry {
            publication_id: record.publication_id,
            publication_kind: record.publication_kind,
            product_id: record.product_id,
            title: record.title,
            sensitivity: record.sensitivity,
            created_at: record.created_at,
            input_count: record.inputs.len(),
        });
    }
    entries.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.publication_id.cmp(&b.publication_id))
    });
    Ok(PublicationListReport {
        schema_version: 1,
        publications: entries,
    })
}

pub fn validate_publication_request(request: &PublicationRequest) -> Result<()> {
    if request.schema_version != 1 || serde_json::to_vec(request)?.len() > MAX_REQUEST_BYTES {
        anyhow::bail!("unsupported or oversized publication request");
    }
    if !valid_product(&request.product_id) {
        anyhow::bail!("invalid product id");
    }
    text(&request.title, 512, true, "title")?;
    if request.summary.len() > 8 * 1024
        || request.summary.contains('\0')
        || request.body_markdown.len() > 512 * 1024
        || request.body_markdown.contains('\0')
    {
        anyhow::bail!("invalid publication text");
    }
    if !(1..=128).contains(&request.inputs.len()) {
        anyhow::bail!("invalid publication inputs");
    }
    let mut seen = HashSet::new();
    for input in &request.inputs {
        let kind = match input.kind {
            PublicationInputKind::Analysis => "analysis",
            PublicationInputKind::ItemRevision => "item_revision",
        };
        if !seen.insert((kind, &input.id, &input.revision_id)) || !valid_input(input) {
            anyhow::bail!("invalid or duplicate publication input");
        }
    }
    if request.topics.len() > 64
        || !unique(&request.topics)
        || request
            .topics
            .iter()
            .any(|v| text(v, 256, true, "topic").is_err())
    {
        anyhow::bail!("invalid publication topics");
    }
    DateTime::parse_from_rfc3339(&request.created_at)
        .map_err(|_| anyhow::anyhow!("invalid created_at"))?;
    for value in [&request.actor_id, &request.tool_id, &request.tool_version] {
        text(value, 256, true, "actor/tool field")?;
    }
    if !valid_sha256(&request.config_digest) {
        anyhow::bail!("invalid config digest");
    }
    Ok(())
}

pub fn prepare_publication(
    library: impl AsRef<Path>,
    request: &PublicationRequest,
) -> Result<PublicationRecord> {
    validate_publication_request(request)?;
    let mut sensitivity = request.requested_sensitivity.unwrap_or(Sensitivity::Public);
    let mut inputs = Vec::with_capacity(request.inputs.len());
    for input in &request.inputs {
        let (content_sha256, input_sensitivity) = match input.kind {
            PublicationInputKind::Analysis => {
                let record = memex_analysis::read_record(library.as_ref(), &input.id)?;
                (digest(&record)?, record.sensitivity)
            }
            PublicationInputKind::ItemRevision => {
                let (item, revision) = memex_store::read_item(library.as_ref(), &input.id)?;
                if input.revision_id.as_deref() != Some(item.current_revision.as_str())
                    || revision.revision_id != item.current_revision
                {
                    anyhow::bail!("item revision is not current");
                }
                (
                    digest(&revision)?,
                    item.sensitivity.max(revision.sensitivity),
                )
            }
        };
        sensitivity = sensitivity.max(input_sensitivity);
        inputs.push(PublicationInputRef {
            kind: input.kind.clone(),
            id: input.id.clone(),
            revision_id: input.revision_id.clone(),
            content_sha256,
            sensitivity: input_sensitivity,
        });
    }
    let provenance = PublicationProvenance {
        actor_kind: request.actor_kind.clone(),
        actor_id: request.actor_id.clone(),
        tool_id: request.tool_id.clone(),
        tool_version: request.tool_version.clone(),
        config_digest: request.config_digest.clone(),
    };
    let payload = PublicationPayload {
        schema_version: 1,
        publication_kind: request.publication_kind.clone(),
        product_id: request.product_id.clone(),
        title: request.title.clone(),
        summary: request.summary.clone(),
        body_markdown: request.body_markdown.clone(),
        inputs: inputs.clone(),
        topics: request.topics.clone(),
        sensitivity,
        created_at: request.created_at.clone(),
        provenance: provenance.clone(),
    };
    Ok(PublicationRecord {
        schema_version: 1,
        publication_id: format!(
            "publication_{:x}",
            Sha256::digest(serde_json::to_vec(&payload)?)
        ),
        publication_kind: payload.publication_kind,
        product_id: payload.product_id,
        title: payload.title,
        summary: payload.summary,
        body_markdown: payload.body_markdown,
        inputs,
        topics: payload.topics,
        sensitivity,
        created_at: payload.created_at,
        provenance,
    })
}

#[derive(Serialize)]
struct PublicationPayload {
    schema_version: u32,
    publication_kind: PublicationKind,
    product_id: String,
    title: String,
    summary: String,
    body_markdown: String,
    inputs: Vec<PublicationInputRef>,
    topics: Vec<String>,
    sensitivity: Sensitivity,
    created_at: String,
    provenance: PublicationProvenance,
}
fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value)?)
    ))
}
fn valid_product(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn component(v: &str, prefix: &str) -> bool {
    v.starts_with(prefix)
        && v.len() > prefix.len()
        && v.len() <= MAX_INPUT_COMPONENT_BYTES
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn valid_input(i: &PublicationInputRequest) -> bool {
    match i.kind {
        PublicationInputKind::Analysis => {
            i.revision_id.is_none()
                && i.id.len() == 73
                && i.id.starts_with("analysis_")
                && i.id[9..].bytes().all(|b| {
                    b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit())
                })
        }
        PublicationInputKind::ItemRevision => {
            component(&i.id, "item_")
                && i.revision_id
                    .as_deref()
                    .is_some_and(|v| component(v, "revision_"))
        }
    }
}
fn text(v: &str, max: usize, required: bool, _: &str) -> Result<()> {
    if (required && v.is_empty()) || v.len() > max || v.chars().any(char::is_control) {
        anyhow::bail!("invalid text");
    }
    Ok(())
}
fn unique(v: &[String]) -> bool {
    v.iter().collect::<HashSet<_>>().len() == v.len()
}
fn valid_sha256(v: &str) -> bool {
    v.len() == 71
        && v.starts_with("sha256:")
        && v[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn validate_publication_record(record: &PublicationRecord) -> Result<()> {
    if record.schema_version != 1
        || !valid_publication_id(&record.publication_id)
        || !valid_product(&record.product_id)
    {
        anyhow::bail!("invalid publication record identity");
    }
    text(&record.title, 512, true, "title")?;
    if record.summary.len() > 8 * 1024
        || record.summary.contains('\0')
        || record.body_markdown.len() > 512 * 1024
        || record.body_markdown.contains('\0')
        || !(1..=128).contains(&record.inputs.len())
    {
        anyhow::bail!("invalid publication record text or inputs");
    }
    let mut seen = HashSet::new();
    for input in &record.inputs {
        let kind = match input.kind {
            PublicationInputKind::Analysis => "analysis",
            PublicationInputKind::ItemRevision => "item_revision",
        };
        let request = PublicationInputRequest {
            kind: input.kind.clone(),
            id: input.id.clone(),
            revision_id: input.revision_id.clone(),
        };
        if !seen.insert((kind, &input.id, &input.revision_id))
            || !valid_input(&request)
            || !valid_sha256(&input.content_sha256)
        {
            anyhow::bail!("invalid publication record input");
        }
    }
    if record.topics.len() > 64
        || !unique(&record.topics)
        || record
            .topics
            .iter()
            .any(|v| text(v, 256, true, "topic").is_err())
    {
        anyhow::bail!("invalid publication record topics");
    }
    DateTime::parse_from_rfc3339(&record.created_at)
        .map_err(|_| anyhow::anyhow!("invalid created_at"))?;
    for value in [
        &record.provenance.actor_id,
        &record.provenance.tool_id,
        &record.provenance.tool_version,
    ] {
        text(value, 256, true, "provenance")?;
    }
    if !valid_sha256(&record.provenance.config_digest) {
        anyhow::bail!("invalid config digest");
    }
    let payload = PublicationPayload {
        schema_version: record.schema_version,
        publication_kind: record.publication_kind.clone(),
        product_id: record.product_id.clone(),
        title: record.title.clone(),
        summary: record.summary.clone(),
        body_markdown: record.body_markdown.clone(),
        inputs: record.inputs.clone(),
        topics: record.topics.clone(),
        sensitivity: record.sensitivity,
        created_at: record.created_at.clone(),
        provenance: record.provenance.clone(),
    };
    if record.publication_id
        != format!(
            "publication_{:x}",
            Sha256::digest(serde_json::to_vec(&payload)?)
        )
    {
        anyhow::bail!("invalid publication content identity");
    }
    Ok(())
}

fn verify_publication_inputs(root: &Path, record: &PublicationRecord) -> Result<()> {
    validate_publication_record(record)?;
    let mut effective = Sensitivity::Public;
    for input in &record.inputs {
        let sensitivity = match input.kind {
            PublicationInputKind::Analysis => {
                let analysis = memex_analysis::read_record(root, &input.id)?;
                if input.revision_id.is_some() || input.content_sha256 != digest(&analysis)? {
                    anyhow::bail!("publication analysis input drift");
                }
                analysis.sensitivity
            }
            PublicationInputKind::ItemRevision => {
                let (item, revision) = memex_store::read_item(root, &input.id)?;
                if input.revision_id.as_deref() != Some(item.current_revision.as_str())
                    || revision.revision_id != item.current_revision
                    || input.content_sha256 != digest(&revision)?
                {
                    anyhow::bail!("publication item input drift");
                }
                item.sensitivity.max(revision.sensitivity)
            }
        };
        if input.sensitivity != sensitivity {
            anyhow::bail!("publication input sensitivity drift");
        }
        effective = effective.max(sensitivity);
    }
    if record.sensitivity < effective {
        anyhow::bail!("publication sensitivity downgrade");
    }
    Ok(())
}

fn canonical_record_bytes(record: &PublicationRecord) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(record)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn canonical_release_bytes(record: &PublicReleaseRecord) -> Result<Vec<u8>> {
    canonical_json(record)
}
fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub fn render_publication_markdown(record: &PublicationRecord) -> String {
    format!("# {}\n\n- Kind: {:?}\n- Product: {}\n- Sensitivity: {:?}\n- Created: {}\n- Input count: {}\n- Topics: {}\n\n{}\n", record.title, record.publication_kind, record.product_id, record.sensitivity, record.created_at, record.inputs.len(), record.topics.join(", "), record.body_markdown)
}
pub fn render_publication_list_markdown(report: &PublicationListReport) -> String {
    let mut out = String::from("# Publications\n");
    for entry in &report.publications {
        out.push_str(&format!(
            "\n- {} | {:?} | {} | {:?} | {} inputs\n",
            entry.created_at,
            entry.publication_kind,
            entry.title,
            entry.sensitivity,
            entry.input_count
        ));
    }
    out
}
fn write_projection(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("projection has no parent"))?;
    match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_symlink() && m.is_file() => {}
        Ok(_) => anyhow::bail!("unsafe projection target"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn valid_publication_id(value: &str) -> bool {
    value.len() == 76
        && value.starts_with("publication_")
        && value[12..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}

fn valid_release_id(value: &str) -> bool {
    value.len() == 72
        && value.starts_with("release_")
        && value[8..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}

fn valid_citation_id(value: &str) -> bool {
    value.len() == 73
        && value.starts_with("citation_")
        && value[9..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}

fn read_publication_file(path: &Path) -> Result<PublicationRecord> {
    let bytes = read_bounded_regular(path, MAX_REQUEST_BYTES, "publication record")?;
    let record: PublicationRecord = serde_json::from_slice(&bytes)?;
    validate_publication_record(&record)?;
    if canonical_record_bytes(&record)? != bytes {
        anyhow::bail!("publication record bytes are noncanonical");
    }
    Ok(record)
}

fn read_public_release_file(path: &Path) -> Result<PublicReleaseRecord> {
    let bytes = read_bounded_regular(path, MAX_REQUEST_BYTES, "public release record")?;
    let record: PublicReleaseRecord = serde_json::from_slice(&bytes)?;
    validate_public_release_record(&record)?;
    if canonical_release_bytes(&record)? != bytes {
        anyhow::bail!("public release record bytes are noncanonical");
    }
    Ok(record)
}

fn safe_library_root(root: &Path) -> Result<PathBuf> {
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        anyhow::bail!("unsafe library root");
    }
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::RootDir | Component::CurDir) {
            continue;
        }
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() && !allowed_macos_system_alias(&current, &meta) {
            anyhow::bail!("unsafe library ancestor");
        }
    }
    let meta = fs::symlink_metadata(&absolute)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("unsafe library root");
    }
    Ok(absolute.canonicalize()?)
}
#[cfg(target_os = "macos")]
fn allowed_macos_system_alias(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let expected = match path.to_str() {
        Some("/var") => "private/var",
        Some("/tmp") => "private/tmp",
        Some("/etc") => "private/etc",
        _ => return false,
    };
    metadata.uid() == 0 && fs::read_link(path).ok().as_deref() == Some(Path::new(expected))
}
#[cfg(not(target_os = "macos"))]
fn allowed_macos_system_alias(_: &Path, _: &fs::Metadata) -> bool {
    false
}
fn safe_create_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe publication directory"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&path)?,
        Err(e) => return Err(e.into()),
    };
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("unsafe publication directory");
    }
    Ok(path)
}
fn lock_file(publications: &Path) -> Result<File> {
    let path = publications.join(".lock");
    let file = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_regular_new(&path)?,
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_file() => open_regular_lock(&path)?,
        Ok(_) => anyhow::bail!("unsafe publication lock"),
        Err(e) => return Err(e.into()),
    };
    file.lock_exclusive()?;
    Ok(file)
}
#[cfg(unix)]
fn open_regular_readonly(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("unsafe regular file");
    }
    Ok(file)
}
#[cfg(not(unix))]
fn open_regular_readonly(path: &Path) -> Result<File> {
    let file = fs::OpenOptions::new().read(true).open(path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("unsafe regular file");
    }
    Ok(file)
}
#[cfg(unix)]
fn open_regular_lock(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("unsafe regular file");
    }
    Ok(file)
}
#[cfg(not(unix))]
fn open_regular_lock(path: &Path) -> Result<File> {
    let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("unsafe regular file");
    }
    Ok(file)
}
#[cfg(unix)]
fn create_regular_new(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?)
}
#[cfg(not(unix))]
fn create_regular_new(path: &Path) -> Result<File> {
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?)
}
fn read_bounded_regular(path: &Path, max: usize, label: &str) -> Result<Vec<u8>> {
    let mut file = open_regular_readonly(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() as usize > max {
        anyhow::bail!("unsafe {label} file");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        anyhow::bail!("{label} is too large");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_analysis::{AnalysisKind, AnalysisRequest};
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ItemDraft, SourceProfile, VerifiedBlob, VerifiedCapture,
    };
    use tempfile::tempdir;
    fn request() -> PublicationRequest {
        PublicationRequest {
            schema_version: 1,
            publication_kind: PublicationKind::Briefing,
            product_id: "product.test".into(),
            title: "Title".into(),
            summary: "Summary".into(),
            body_markdown: "Body".into(),
            inputs: vec![PublicationInputRequest {
                kind: PublicationInputKind::Analysis,
                id: format!("analysis_{}", "0".repeat(64)),
                revision_id: None,
            }],
            topics: vec!["topic".into()],
            requested_sensitivity: None,
            created_at: "2026-07-31T00:00:00Z".into(),
            actor_kind: ActorKind::Agent,
            actor_id: "agent".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{}", "0".repeat(64)),
        }
    }
    fn ingest_one(root: &Path, sensitivity: Sensitivity) -> String {
        ingest_one_with_urls(root, sensitivity, "https://example.test", None)
    }
    fn ingest_one_with_urls(
        root: &Path,
        sensitivity: Sensitivity,
        source_url: &str,
        canonical_url: Option<&str>,
    ) -> String {
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: "source".into(),
                name: "source".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: sensitivity,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "adapter".into(),
                adapter_version: "1".into(),
                external_capture_id: "capture".into(),
                source_id: "source".into(),
                captured_at: "2026-07-31T00:00:00Z".into(),
                capture_method: "test".into(),
                sensitivity,
                route_summary: None,
                title: None,
                source_url: source_url.into(),
                canonical_url: canonical_url.map(str::to_owned),
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "body".into(),
                    media_type: "text/plain".into(),
                    byte_size: 4,
                    sha256: "sha256:body".into(),
                    source_url: None,
                },
                blob_id: "blob".into(),
                source_backend_ref: Some("test".into()),
                bytes: b"body".to_vec(),
            }],
        };
        memex_store::ingest(
            root,
            &capture,
            ItemDraft {
                item_type: "webpage".into(),
                title: "Title".into(),
                source_url: source_url.into(),
                canonical_url: canonical_url.map(str::to_owned),
                content_markdown: "body".into(),
                sensitivity,
                translator_id: "translator".into(),
                translator_version: "1".into(),
                metadata: Default::default(),
                attachments: Vec::new(),
            },
        )
        .unwrap()
        .item_ids
        .remove(0)
    }
    fn prepared_record(root: &Path) -> PublicationRecord {
        let item_id = ingest_one(root, Sensitivity::Private);
        let analysis = memex_analysis::prepare_record(
            root,
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind: AnalysisKind::Summary,
                title: "Analysis".into(),
                body_markdown: "body".into(),
                item_ids: vec![item_id.clone()],
                topics: vec![],
                requested_sensitivity: None,
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "agent".into(),
                tool_id: "tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "1".repeat(64)),
                generated_text: None,
            },
        )
        .unwrap();
        memex_analysis::apply_record(root, &analysis).unwrap();
        let mut publication = request();
        publication.inputs = vec![
            PublicationInputRequest {
                kind: PublicationInputKind::Analysis,
                id: analysis.analysis_id,
                revision_id: None,
            },
            PublicationInputRequest {
                kind: PublicationInputKind::ItemRevision,
                id: item_id.clone(),
                revision_id: Some(
                    memex_store::read_item(root, &item_id)
                        .unwrap()
                        .0
                        .current_revision,
                ),
            },
        ];
        prepare_publication(root, &publication).unwrap()
    }
    fn prepared_public_fixture(root: &Path) -> (PublicationRecord, String, String) {
        let item_id = ingest_one(root, Sensitivity::Public);
        let analysis = memex_analysis::prepare_record(
            root,
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind: AnalysisKind::Summary,
                title: "Analysis body private_sentinel".into(),
                body_markdown: "analysis_body_private_sentinel".into(),
                item_ids: vec![item_id.clone()],
                topics: vec![],
                requested_sensitivity: None,
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "analysis_actor".into(),
                tool_id: "tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "1".repeat(64)),
                generated_text: None,
            },
        )
        .unwrap();
        memex_analysis::apply_record(root, &analysis).unwrap();
        let mut publication = request();
        publication.inputs = vec![
            PublicationInputRequest {
                kind: PublicationInputKind::ItemRevision,
                id: item_id.clone(),
                revision_id: Some(
                    memex_store::read_item(root, &item_id)
                        .unwrap()
                        .0
                        .current_revision,
                ),
            },
            PublicationInputRequest {
                kind: PublicationInputKind::Analysis,
                id: analysis.analysis_id.clone(),
                revision_id: None,
            },
        ];
        let record = prepare_publication(root, &publication).unwrap();
        apply_publication(root, &record).unwrap();
        (record, item_id, analysis.analysis_id)
    }
    fn prepared_public_fixture_with_urls(
        root: &Path,
        source_url: &str,
        canonical_url: Option<&str>,
    ) -> PublicationRecord {
        let item_id = ingest_one_with_urls(root, Sensitivity::Public, source_url, canonical_url);
        let analysis = memex_analysis::prepare_record(
            root,
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind: AnalysisKind::Summary,
                title: "Analysis".into(),
                body_markdown: "body".into(),
                item_ids: vec![item_id],
                topics: vec![],
                requested_sensitivity: Some(Sensitivity::Public),
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "agent".into(),
                tool_id: "tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "1".repeat(64)),
                generated_text: None,
            },
        )
        .unwrap();
        memex_analysis::apply_record(root, &analysis).unwrap();
        let mut publication = request();
        publication.inputs = vec![PublicationInputRequest {
            kind: PublicationInputKind::Analysis,
            id: analysis.analysis_id,
            revision_id: None,
        }];
        publication.requested_sensitivity = Some(Sensitivity::Public);
        let record = prepare_publication(root, &publication).unwrap();
        apply_publication(root, &record).unwrap();
        record
    }
    #[test]
    fn prepare_all_kinds_is_deterministic_and_preserves_input_order() {
        let root = tempdir().unwrap();
        let item_id = ingest_one(root.path(), Sensitivity::Private);
        let analysis = memex_analysis::prepare_record(
            root.path(),
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind: AnalysisKind::Summary,
                title: "Analysis".into(),
                body_markdown: "body".into(),
                item_ids: vec![item_id.clone()],
                topics: vec![],
                requested_sensitivity: None,
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "agent".into(),
                tool_id: "tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "1".repeat(64)),
                generated_text: None,
            },
        )
        .unwrap();
        memex_analysis::apply_record(root.path(), &analysis).unwrap();
        let mut expected = Vec::new();
        for (kind, created_at) in [
            (PublicationKind::Briefing, "2026-07-31T03:00:00Z"),
            (PublicationKind::Dossier, "2026-07-31T01:00:00Z"),
            (PublicationKind::Publication, "2026-07-31T02:00:00Z"),
        ] {
            let mut publication = request();
            publication.publication_kind = kind;
            publication.created_at = created_at.into();
            publication.requested_sensitivity = Some(Sensitivity::Public);
            publication.inputs = vec![
                PublicationInputRequest {
                    kind: PublicationInputKind::Analysis,
                    id: analysis.analysis_id.clone(),
                    revision_id: None,
                },
                PublicationInputRequest {
                    kind: PublicationInputKind::ItemRevision,
                    id: item_id.clone(),
                    revision_id: Some(
                        memex_store::read_item(root.path(), &item_id)
                            .unwrap()
                            .0
                            .current_revision,
                    ),
                },
            ];
            let first = prepare_publication(root.path(), &publication).unwrap();
            assert_eq!(
                first,
                prepare_publication(root.path(), &publication).unwrap()
            );
            assert_eq!(
                apply_publication(root.path(), &first).unwrap().status,
                ApplyStatus::Created
            );
            assert_eq!(
                apply_publication(root.path(), &first).unwrap().status,
                ApplyStatus::Replayed
            );
            assert_eq!(
                read_publication(root.path(), &first.publication_id).unwrap(),
                first
            );
            assert_eq!(first.sensitivity, Sensitivity::Private);
            assert_eq!(first.inputs.len(), 2);
            assert!(first.publication_id.starts_with("publication_"));
            assert_eq!(
                show_publication(root.path(), &first.publication_id)
                    .unwrap()
                    .record,
                first
            );
            expected.push(first);
        }
        expected.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.publication_id.cmp(&b.publication_id))
        });
        let report = list_publications(root.path()).unwrap();
        assert_eq!(
            report
                .publications
                .iter()
                .map(|v| &v.publication_id)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|v| &v.publication_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            serde_json::from_slice::<PublicationListReport>(&canonical_json(&report).unwrap())
                .unwrap(),
            report
        );
        let markdown = render_publication_markdown(&expected[0]);
        assert!(
            markdown.contains("Kind:")
                && markdown.contains("Product:")
                && markdown.contains("Sensitivity:")
                && markdown.contains("Input count:")
                && markdown.contains("Topics:")
                && markdown.contains(&expected[0].body_markdown)
        );
        assert!(render_publication_list_markdown(&report).contains(&expected[0].title));
        let mut stale = request();
        stale.inputs = vec![PublicationInputRequest {
            kind: PublicationInputKind::ItemRevision,
            id: item_id,
            revision_id: Some("revision_stale".into()),
        }];
        assert!(prepare_publication(root.path(), &stale).is_err());
    }
    #[test]
    fn request_validation_rejects_invalid_and_duplicate_shapes() {
        let mut r = request();
        r.inputs.push(r.inputs[0].clone());
        assert!(validate_publication_request(&r).is_err());
        r.inputs.pop();
        r.inputs[0].revision_id = Some("revision_x".into());
        assert!(validate_publication_request(&r).is_err());
    }
    #[test]
    fn request_reader_rejects_oversize() {
        let root = tempdir().unwrap();
        let path = root.path().join("large.json");
        fs::write(&path, vec![b'x'; MAX_REQUEST_BYTES + 1]).unwrap();
        assert!(read_publication_request(path).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn request_reader_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"{}").unwrap();
        let link = root.path().join("link");
        symlink(target, &link).unwrap();
        assert!(read_publication_request(link).is_err());
    }
    #[test]
    fn missing_read_does_not_create_directories() {
        let root = tempdir().unwrap();
        let id = format!("publication_{}", "0".repeat(64));
        assert!(read_publication(root.path(), &id).is_err());
        assert!(!root.path().join("publications").exists());
    }
    #[cfg(unix)]
    #[test]
    fn records_symlink_is_rejected_without_touching_sentinel() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"unchanged").unwrap();
        fs::create_dir(root.path().join("publications")).unwrap();
        symlink(outside.path(), root.path().join("publications/records")).unwrap();
        let id = format!("publication_{}", "0".repeat(64));
        assert!(read_publication(root.path(), &id).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
    }
    #[test]
    fn upstream_tamper_before_apply_creates_no_publication_record() {
        let root = tempdir().unwrap();
        let record = prepared_record(root.path());
        let analysis_id = &record.inputs[0].id;
        fs::write(
            root.path()
                .join("analysis/records")
                .join(format!("{analysis_id}.json")),
            b"{}",
        )
        .unwrap();
        assert!(apply_publication(root.path(), &record).is_err());
        assert!(!root
            .path()
            .join("publications/records")
            .join(format!("{}.json", record.publication_id))
            .exists());
    }
    #[test]
    fn field_tamper_and_noncanonical_record_fail_read_and_replay() {
        let root = tempdir().unwrap();
        let record = prepared_record(root.path());
        apply_publication(root.path(), &record).unwrap();
        let path = root
            .path()
            .join("publications/records")
            .join(format!("{}.json", record.publication_id));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["title"] = serde_json::Value::String("Tampered".into());
        let mut tampered = serde_json::to_vec_pretty(&value).unwrap();
        tampered.push(b'\n');
        fs::write(&path, tampered).unwrap();
        assert!(read_publication(root.path(), &record.publication_id).is_err());
        assert!(show_publication(root.path(), &record.publication_id).is_err());
        assert!(list_publications(root.path()).is_err());
        assert!(apply_publication(root.path(), &record).is_err());
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(read_publication(root.path(), &record.publication_id).is_err());
        assert!(apply_publication(root.path(), &record).is_err());
    }
    #[test]
    fn wrong_filename_rejects_canonical_content() {
        let root = tempdir().unwrap();
        let record = prepared_record(root.path());
        apply_publication(root.path(), &record).unwrap();
        let other = format!("publication_{}", "f".repeat(64));
        let source = root
            .path()
            .join("publications/records")
            .join(format!("{}.json", record.publication_id));
        fs::copy(
            source,
            root.path()
                .join("publications/records")
                .join(format!("{other}.json")),
        )
        .unwrap();
        assert!(read_publication(root.path(), &other).is_err());
    }
    #[test]
    fn list_show_and_replay_repair_projections() {
        let root = tempdir().unwrap();
        let record = prepared_record(root.path());
        assert_eq!(
            apply_publication(root.path(), &record).unwrap().status,
            ApplyStatus::Created
        );
        let report = list_publications(root.path()).unwrap();
        assert_eq!(report.publications.len(), 1);
        assert_eq!(
            show_publication(root.path(), &record.publication_id)
                .unwrap()
                .record,
            record
        );
        let publications = root.path().join("publications");
        let markdown = publications
            .join("records")
            .join(format!("{}.md", record.publication_id));
        for path in [
            &markdown,
            &publications.join("index.json"),
            &publications.join("index.md"),
        ] {
            fs::remove_file(path).unwrap();
        }
        let replay = apply_publication(root.path(), &record).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert!(replay.warning_codes.is_empty());
        assert!(markdown.exists());
        assert!(publications.join("index.json").exists());
        assert!(publications.join("index.md").exists());
        let report = list_publications(root.path()).unwrap();
        assert_eq!(
            fs::read(&markdown).unwrap(),
            render_publication_markdown(&record).into_bytes()
        );
        assert_eq!(
            fs::read(publications.join("index.json")).unwrap(),
            canonical_json(&report).unwrap()
        );
        assert_eq!(
            fs::read(publications.join("index.md")).unwrap(),
            render_publication_list_markdown(&report).into_bytes()
        );
    }
    #[test]
    fn empty_list_is_read_only() {
        let root = tempdir().unwrap();
        assert!(list_publications(root.path())
            .unwrap()
            .publications
            .is_empty());
        assert!(!root.path().join("publications").exists());
    }
    #[cfg(unix)]
    #[test]
    fn combined_projection_blockers_have_ordered_warnings_and_preserve_sentinel() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"unchanged").unwrap();
        let record = prepared_record(root.path());
        let publications = root.path().join("publications");
        let records = publications.join("records");
        fs::create_dir(&publications).unwrap();
        fs::create_dir(&records).unwrap();
        symlink(
            outside.path(),
            records.join(format!("{}.md", record.publication_id)),
        )
        .unwrap();
        symlink(outside.path(), publications.join("index.json")).unwrap();
        symlink(outside.path(), publications.join("index.md")).unwrap();
        let receipt = apply_publication(root.path(), &record).unwrap();
        assert_eq!(
            receipt.warning_codes,
            vec![
                "publication_markdown_projection_pending",
                "publication_json_index_projection_pending",
                "publication_markdown_index_projection_pending"
            ]
        );
        assert_eq!(
            read_publication(root.path(), &record.publication_id).unwrap(),
            record
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
    }
    #[cfg(unix)]
    #[test]
    fn publication_lock_and_record_symlinks_fail_closed_and_readonly_files_read() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"unchanged").unwrap();
        let record = prepared_record(root.path());
        symlink(outside.path(), root.path().join("publications")).unwrap();
        assert!(apply_publication(root.path(), &record).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
        fs::remove_file(root.path().join("publications")).unwrap();
        fs::create_dir(root.path().join("publications")).unwrap();
        fs::create_dir(root.path().join("publications/records")).unwrap();
        symlink(outside.path(), root.path().join("publications/.lock")).unwrap();
        assert!(apply_publication(root.path(), &record).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
        fs::remove_file(root.path().join("publications/.lock")).unwrap();
        let path = root
            .path()
            .join("publications/records")
            .join(format!("{}.json", record.publication_id));
        symlink(outside.path(), &path).unwrap();
        assert!(apply_publication(root.path(), &record).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
        fs::remove_file(&path).unwrap();
        apply_publication(root.path(), &record).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(
            read_publication(root.path(), &record.publication_id).unwrap(),
            record
        );
        let request_path = root.path().join("request.json");
        fs::write(&request_path, serde_json::to_vec(&request()).unwrap()).unwrap();
        fs::set_permissions(&request_path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(read_publication_request(request_path).is_ok());
    }
    #[test]
    fn deterministic_direct_and_analysis_citations_dedup_sort_and_hashes() {
        let root = tempdir().unwrap();
        let (publication, item_id, _) = prepared_public_fixture(root.path());
        let request = PublicReleaseRequest {
            schema_version: 1,
            target: PublicTarget::YuanfangPublicV1,
            publication_id: publication.publication_id.clone(),
            published_at: "2026-07-31T00:00:00Z".into(),
            field_allowlist: vec![PublicField::Citations, PublicField::Title],
            actor_kind: ActorKind::Agent,
            actor_id: "release_actor".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{}", "2".repeat(64)),
        };
        let first = prepare_public_release(root.path(), &request).unwrap();
        let second = prepare_public_release(root.path(), &request).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.projection.citations.len(), 1);
        let revision = memex_store::read_item(root.path(), &item_id).unwrap().1;
        assert_eq!(
            first.projection.citations[0].citation_id,
            format!(
                "citation_{:x}",
                Sha256::digest(format!("{}\0{}", item_id, revision.revision_id).as_bytes())
            )
        );
        assert_eq!(
            first.field_allowlist,
            vec![PublicField::Title, PublicField::Citations]
        );
        assert_eq!(first.publication_sha256, digest(&publication).unwrap());
        assert_eq!(
            first.projection_sha256,
            projection_digest(&first.projection).unwrap()
        );
    }
    #[test]
    fn projection_omits_unallowed_fields_and_privacy_scan() {
        let root = tempdir().unwrap();
        let (publication, item_id, analysis_id) = prepared_public_fixture(root.path());
        let request = PublicReleaseRequest {
            schema_version: 1,
            target: PublicTarget::YuanfangPublicV1,
            publication_id: publication.publication_id.clone(),
            published_at: "2026-07-31T00:00:00Z".into(),
            field_allowlist: vec![PublicField::Title, PublicField::Citations],
            actor_kind: ActorKind::Agent,
            actor_id: "release_actor".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{}", "2".repeat(64)),
        };
        let projection = prepare_public_release(root.path(), &request)
            .unwrap()
            .projection;
        let bytes = serde_json::to_vec(&projection).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for key in ["summary", "body_markdown", "topics"] {
            assert!(value.get(key).is_none());
        }
        assert!(value.get("title").is_some() && value.get("citations").is_some());
        let text = String::from_utf8(bytes).unwrap();
        for secret in [
            item_id.as_str(),
            analysis_id.as_str(),
            "evidence_",
            "release_actor",
            "analysis_body_private_sentinel",
            root.path().to_str().unwrap(),
        ] {
            assert!(!text.contains(secret));
        }
    }
    fn release_request_for(publication_id: String) -> PublicReleaseRequest {
        PublicReleaseRequest {
            schema_version: 1,
            target: PublicTarget::YuanfangPublicV1,
            publication_id,
            published_at: "2026-07-31T00:00:00Z".into(),
            field_allowlist: vec![PublicField::Title, PublicField::Citations],
            actor_kind: ActorKind::Agent,
            actor_id: "release_actor".into(),
            tool_id: "tool".into(),
            tool_version: "1".into(),
            config_digest: format!("sha256:{}", "2".repeat(64)),
        }
    }
    fn assert_no_release_writes(root: &Path) {
        assert!(!root.join("releases").exists());
        assert!(!root.join("public").exists());
        assert!(!root.join("publications/releases").exists());
        assert!(!root.join("publications/public").exists());
    }
    fn forge_public_wrapper(
        root: &Path,
        mut record: PublicationRecord,
        input: PublicationInputRef,
    ) -> PublicationRecord {
        record.inputs = vec![input];
        record.sensitivity = Sensitivity::Public;
        let payload = PublicationPayload {
            schema_version: record.schema_version,
            publication_kind: record.publication_kind.clone(),
            product_id: record.product_id.clone(),
            title: record.title.clone(),
            summary: record.summary.clone(),
            body_markdown: record.body_markdown.clone(),
            inputs: record.inputs.clone(),
            topics: record.topics.clone(),
            sensitivity: record.sensitivity,
            created_at: record.created_at.clone(),
            provenance: record.provenance.clone(),
        };
        record.publication_id = format!(
            "publication_{:x}",
            Sha256::digest(serde_json::to_vec(&payload).unwrap())
        );
        fs::create_dir_all(root.join("publications/records")).unwrap();
        fs::write(
            root.join("publications/records")
                .join(format!("{}.json", record.publication_id)),
            canonical_record_bytes(&record).unwrap(),
        )
        .unwrap();
        record
    }
    fn forge_public_analysis(
        root: &Path,
        mut analysis: memex_analysis::AnalysisRecord,
        drift: bool,
    ) -> memex_analysis::AnalysisRecord {
        analysis.sensitivity = Sensitivity::Public;
        if drift {
            analysis.inputs[0].revision_sha256 = format!("sha256:{}", "f".repeat(64));
        }
        analysis.analysis_id.clear();
        analysis.analysis_id = format!(
            "analysis_{:x}",
            Sha256::digest(serde_json::to_vec(&analysis).unwrap())
        );
        fs::write(
            root.join("analysis/records")
                .join(format!("{}.json", analysis.analysis_id)),
            serde_json::to_vec_pretty(&analysis).unwrap(),
        )
        .unwrap();
        assert_eq!(
            memex_analysis::read_record(root, &analysis.analysis_id).unwrap(),
            analysis
        );
        analysis
    }
    fn public_analysis_publication(
        root: &Path,
        base: PublicationRecord,
        analysis: &memex_analysis::AnalysisRecord,
    ) -> PublicationRecord {
        forge_public_wrapper(
            root,
            base,
            PublicationInputRef {
                kind: PublicationInputKind::Analysis,
                id: analysis.analysis_id.clone(),
                revision_id: None,
                content_sha256: digest(analysis).unwrap(),
                sensitivity: Sensitivity::Public,
            },
        )
    }
    #[test]
    fn private_publication_rejected_without_release_writes() {
        let root = tempdir().unwrap();
        let record = prepared_record(root.path());
        assert!(
            prepare_public_release(root.path(), &release_request_for(record.publication_id))
                .is_err()
        );
        assert_no_release_writes(root.path());
    }
    #[test]
    fn direct_private_item_rejected_without_release_writes() {
        let root = tempdir().unwrap();
        let original = prepared_record(root.path());
        let record =
            forge_public_wrapper(root.path(), original.clone(), original.inputs[1].clone());
        assert_eq!(
            read_publication(root.path(), &record.publication_id)
                .unwrap()
                .sensitivity,
            Sensitivity::Public
        );
        assert_eq!(record.inputs[0].kind, PublicationInputKind::ItemRevision);
        assert_eq!(record.inputs[0].sensitivity, Sensitivity::Private);
        assert!(
            prepare_public_release(root.path(), &release_request_for(record.publication_id))
                .is_err()
        );
        assert_no_release_writes(root.path());
    }
    #[test]
    fn direct_private_analysis_rejected_without_release_writes() {
        let root = tempdir().unwrap();
        let original = prepared_record(root.path());
        let record =
            forge_public_wrapper(root.path(), original.clone(), original.inputs[0].clone());
        assert_eq!(
            read_publication(root.path(), &record.publication_id)
                .unwrap()
                .sensitivity,
            Sensitivity::Public
        );
        assert_eq!(record.inputs[0].kind, PublicationInputKind::Analysis);
        assert_eq!(record.inputs[0].sensitivity, Sensitivity::Private);
        assert!(
            prepare_public_release(root.path(), &release_request_for(record.publication_id))
                .is_err()
        );
        assert_no_release_writes(root.path());
    }
    #[test]
    fn transitive_private_item_rejected_without_release_writes() {
        let root = tempdir().unwrap();
        let base = prepared_record(root.path());
        let original = memex_analysis::read_record(root.path(), &base.inputs[0].id).unwrap();
        let analysis = forge_public_analysis(root.path(), original, false);
        let publication = public_analysis_publication(root.path(), base, &analysis);
        assert_eq!(analysis.sensitivity, Sensitivity::Public);
        assert_eq!(publication.sensitivity, Sensitivity::Public);
        assert_eq!(
            digest(&analysis).unwrap(),
            publication.inputs[0].content_sha256
        );
        assert!(prepare_public_release(
            root.path(),
            &release_request_for(publication.publication_id)
        )
        .is_err());
        assert_no_release_writes(root.path());
    }
    #[test]
    fn transitive_revision_drift_rejected_without_release_writes() {
        let root = tempdir().unwrap();
        let (base, _, analysis_id) = prepared_public_fixture(root.path());
        let original = memex_analysis::read_record(root.path(), &analysis_id).unwrap();
        let analysis = forge_public_analysis(root.path(), original, true);
        let publication = public_analysis_publication(root.path(), base, &analysis);
        assert_eq!(analysis.sensitivity, Sensitivity::Public);
        assert_eq!(publication.sensitivity, Sensitivity::Public);
        assert_eq!(
            digest(&analysis).unwrap(),
            publication.inputs[0].content_sha256
        );
        assert!(prepare_public_release(
            root.path(),
            &release_request_for(publication.publication_id)
        )
        .is_err());
        assert_no_release_writes(root.path());
    }
    #[test]
    fn release_request_rejects_duplicate_missing_bad_oversize_and_symlink_without_writes() {
        let root = tempdir().unwrap();
        let publication_id = format!("publication_{}", "0".repeat(64));
        let mut request = release_request_for(publication_id.clone());
        request.field_allowlist.push(PublicField::Title);
        assert!(validate_public_release_request(&mut request).is_err());
        for fields in [vec![PublicField::Title], vec![PublicField::Citations]] {
            let mut bad = release_request_for(publication_id.clone());
            bad.field_allowlist = fields;
            assert!(validate_public_release_request(&mut bad).is_err());
        }
        let mut bad = release_request_for("bad".into());
        assert!(validate_public_release_request(&mut bad).is_err());
        let mut bad = release_request_for(publication_id.clone());
        bad.published_at = "bad".into();
        assert!(validate_public_release_request(&mut bad).is_err());
        let mut bad = release_request_for(publication_id);
        bad.config_digest = "bad".into();
        assert!(validate_public_release_request(&mut bad).is_err());
        let path = root.path().join("oversize.json");
        fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(read_public_release_request(&path).is_err());
        let valid = release_request_for(format!("publication_{}", "1".repeat(64)));
        let target = root.path().join("valid.json");
        let mut invalid_target: serde_json::Value = serde_json::to_value(&valid).unwrap();
        invalid_target["target"] = serde_json::Value::String("not_a_target".into());
        let bad_target = root.path().join("bad-target.json");
        fs::write(&bad_target, serde_json::to_vec(&invalid_target).unwrap()).unwrap();
        assert!(read_public_release_request(&bad_target).is_err());
        fs::write(&target, serde_json::to_vec(&valid).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let outside = tempdir().unwrap();
            let sentinel = outside.path().join("sentinel");
            fs::write(&sentinel, b"unchanged").unwrap();
            let link = root.path().join("valid-link.json");
            symlink(&target, &link).unwrap();
            assert!(read_public_release_request(&link).is_err());
            assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
            fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
            assert_eq!(
                read_public_release_request(&target)
                    .unwrap()
                    .field_allowlist,
                vec![PublicField::Title, PublicField::Citations]
            );
        }
        assert_no_release_writes(root.path());
    }
    #[test]
    fn release_identity_changes_with_published_at_allowlist_and_provenance() {
        let root = tempdir().unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        let base_request = release_request_for(publication.publication_id);
        let base = prepare_public_release(root.path(), &base_request).unwrap();
        let mut time = base_request.clone();
        time.published_at = "2026-08-01T00:00:00Z".into();
        let time_record = prepare_public_release(root.path(), &time).unwrap();
        let mut fields = base_request.clone();
        fields.field_allowlist.push(PublicField::Summary);
        let fields_record = prepare_public_release(root.path(), &fields).unwrap();
        let mut provenance = base_request;
        provenance.actor_id = "other_actor".into();
        let provenance_record = prepare_public_release(root.path(), &provenance).unwrap();
        assert_ne!(base.release_id, time_record.release_id);
        assert_ne!(base.release_id, fields_record.release_id);
        assert_ne!(base.release_id, provenance_record.release_id);
        assert_ne!(base.projection_sha256, time_record.projection_sha256);
        assert_ne!(base.projection_sha256, fields_record.projection_sha256);
        assert_eq!(base.projection_sha256, provenance_record.projection_sha256);
        assert_eq!(base.publication_sha256, time_record.publication_sha256);
        for record in [base, time_record, fields_record, provenance_record] {
            assert!(record.release_id.starts_with("release_") && record.release_id.len() == 72);
            assert!(valid_sha256(&record.projection_sha256));
        }
    }
    #[test]
    fn public_release_created_replayed_read_shown_and_rendered_without_latest() {
        let root = tempdir().unwrap();
        let (publication, item_id, analysis_id) = prepared_public_fixture(root.path());
        let mut request = release_request_for(publication.publication_id.clone());
        request.field_allowlist = vec![
            PublicField::Title,
            PublicField::Summary,
            PublicField::BodyMarkdown,
            PublicField::Topics,
            PublicField::Citations,
        ];
        let release = prepare_public_release(root.path(), &request).unwrap();
        assert_eq!(
            apply_public_release(root.path(), &release).unwrap().status,
            ApplyStatus::Created
        );
        let replay = apply_public_release(root.path(), &release).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert!(replay.warning_codes.is_empty());
        assert_eq!(
            read_public_release(root.path(), &release.release_id).unwrap(),
            release
        );
        let projection = show_public_projection(root.path(), &release.release_id).unwrap();
        assert_eq!(projection, release.projection);
        let markdown = render_public_projection_markdown(&projection);
        assert!(markdown.contains(&publication.title));
        assert!(markdown.contains("## Citations"));
        let receipt = serde_json::to_string(&replay).unwrap();
        assert!(
            !receipt.contains("private_sentinel")
                && !receipt.contains(&item_id)
                && !receipt.contains(&analysis_id)
        );
        let bundle = root
            .path()
            .join("publications/public")
            .join(&release.release_id);
        let manifest = PublicBundleManifest {
            schema_version: 1,
            release_id: release.release_id.clone(),
            publication_id: release.publication_id.clone(),
            target: release.target.clone(),
            publication_sha256: release.publication_sha256.clone(),
            projection_sha256: release.projection_sha256.clone(),
            outputs: PublicBundleOutputs {
                json: "publication.json".into(),
                markdown: "publication.md".into(),
            },
        };
        assert_eq!(
            fs::read(bundle.join("manifest.json")).unwrap(),
            canonical_json(&manifest).unwrap()
        );
        assert_eq!(
            fs::read(bundle.join("publication.json")).unwrap(),
            canonical_json(&projection).unwrap()
        );
        assert_eq!(
            fs::read(bundle.join("publication.md")).unwrap(),
            render_public_projection_markdown(&projection).into_bytes()
        );
        let public = fs::read(bundle.join("publication.json")).unwrap();
        let public = String::from_utf8(public).unwrap();
        assert!(
            !public.contains(&item_id)
                && !public.contains(&analysis_id)
                && !public.contains("private_sentinel")
                && !public.contains("provenance")
                && !public.contains("evidence")
                && !public.contains("/tmp/")
        );
        assert!(!root.path().join("publications/latest").exists());
    }
    #[test]
    fn public_bundle_replay_repairs_missing_and_tampered_derived_files() {
        let root = tempdir().unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        let release = prepare_public_release(
            root.path(),
            &release_request_for(publication.publication_id),
        )
        .unwrap();
        assert_eq!(
            apply_public_release(root.path(), &release).unwrap().status,
            ApplyStatus::Created
        );
        let bundle = root
            .path()
            .join("publications/public")
            .join(&release.release_id);
        fs::remove_file(bundle.join("manifest.json")).unwrap();
        fs::write(bundle.join("publication.json"), b"tampered").unwrap();
        fs::write(bundle.join("publication.md"), b"tampered").unwrap();
        let replay = apply_public_release(root.path(), &release).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert!(replay.warning_codes.is_empty());
        assert_eq!(
            fs::read(bundle.join("publication.json")).unwrap(),
            canonical_json(&release.projection).unwrap()
        );
        assert_eq!(
            fs::read(bundle.join("publication.md")).unwrap(),
            render_public_projection_markdown(&release.projection).into_bytes()
        );
        assert!(fs::read(bundle.join("manifest.json"))
            .unwrap()
            .ends_with(b"\n"));
    }
    #[cfg(unix)]
    #[test]
    fn durable_release_survives_public_bundle_blocker_with_warnings() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        symlink(outside.path(), root.path().join("publications/public")).unwrap();
        let release = prepare_public_release(
            root.path(),
            &release_request_for(publication.publication_id),
        )
        .unwrap();
        let receipt = apply_public_release(root.path(), &release).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Created);
        assert_eq!(
            receipt.warning_codes,
            vec![
                "public_manifest_projection_pending",
                "public_json_projection_pending",
                "public_markdown_projection_pending"
            ]
        );
        assert_eq!(
            read_public_release(root.path(), &release.release_id).unwrap(),
            release
        );
    }
    #[test]
    fn exact_request_replay_repairs_bundle_after_upstream_drift() {
        let root = tempdir().unwrap();
        let (publication, _, analysis_id) = prepared_public_fixture(root.path());
        let request = release_request_for(publication.publication_id);
        let first = release_public(root.path(), &request).unwrap();
        assert_eq!(first.status, ApplyStatus::Created);
        let bundle = root
            .path()
            .join("publications/public")
            .join(&first.release_id);
        fs::remove_file(bundle.join("manifest.json")).unwrap();
        fs::write(bundle.join("publication.json"), b"tampered").unwrap();
        fs::write(bundle.join("publication.md"), b"tampered").unwrap();
        fs::write(
            root.path()
                .join("analysis/records")
                .join(format!("{analysis_id}.json")),
            b"{}",
        )
        .unwrap();
        let replay = release_public(root.path(), &request).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert!(replay.warning_codes.is_empty());
        let stored = read_public_release(root.path(), &first.release_id).unwrap();
        assert_eq!(
            fs::read(bundle.join("publication.json")).unwrap(),
            canonical_json(&stored.projection).unwrap()
        );
        assert_eq!(
            fs::read(bundle.join("publication.md")).unwrap(),
            render_public_projection_markdown(&stored.projection).into_bytes()
        );
        let mut changed_time = request.clone();
        changed_time.published_at = "2026-08-01T00:00:00Z".into();
        assert!(release_public(root.path(), &changed_time).is_err());
        let mut changed_fields = request;
        changed_fields.field_allowlist.push(PublicField::Summary);
        assert!(release_public(root.path(), &changed_fields).is_err());
        assert_eq!(
            fs::read_dir(root.path().join("publications/releases"))
                .unwrap()
                .count(),
            1
        );
    }
    #[cfg(unix)]
    #[test]
    fn stored_release_scan_and_bundle_symlinks_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"unchanged").unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        let request = release_request_for(publication.publication_id.clone());
        fs::create_dir(root.path().join("publications/releases")).unwrap();
        symlink(
            outside.path(),
            root.path().join("publications/releases/evil.json"),
        )
        .unwrap();
        assert!(release_public(root.path(), &request).is_err());
        assert!(!root
            .path()
            .join("publications/releases")
            .join("evil.json")
            .is_file());
        fs::remove_file(root.path().join("publications/releases/evil.json")).unwrap();
        let first = release_public(root.path(), &request).unwrap();
        assert_eq!(first.status, ApplyStatus::Created);
        let bundle = root
            .path()
            .join("publications/public")
            .join(&first.release_id);
        for (name, expected_warning) in [
            ("manifest.json", "public_manifest_projection_pending"),
            ("publication.json", "public_json_projection_pending"),
            ("publication.md", "public_markdown_projection_pending"),
        ] {
            fs::remove_file(bundle.join(name)).unwrap();
            symlink(outside.path(), bundle.join(name)).unwrap();
            let replay = release_public(root.path(), &request).unwrap();
            assert_eq!(replay.status, ApplyStatus::Replayed);
            assert_eq!(replay.warning_codes, vec![expected_warning]);
            assert!(bundle.join("manifest.json").exists() || name == "manifest.json");
            assert!(bundle.join("publication.json").exists() || name == "publication.json");
            assert!(bundle.join("publication.md").exists() || name == "publication.md");
            fs::remove_file(bundle.join(name)).unwrap();
        }
        fs::remove_dir_all(&bundle).unwrap();
        symlink(outside.path(), &bundle).unwrap();
        let replay = release_public(root.path(), &request).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert_eq!(
            replay.warning_codes,
            vec![
                "public_manifest_projection_pending",
                "public_json_projection_pending",
                "public_markdown_projection_pending"
            ]
        );
        assert!(read_public_release(root.path(), &first.release_id).is_ok());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
    }
    #[test]
    fn malformed_stored_release_blocks_request_replay() {
        let root = tempdir().unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        let request = release_request_for(publication.publication_id);
        let first = release_public(root.path(), &request).unwrap();
        let stored = read_public_release(root.path(), &first.release_id).unwrap();
        let path = root
            .path()
            .join("publications/releases")
            .join(format!("{}.json", first.release_id));
        fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
        assert!(release_public(root.path(), &request).is_err());
        fs::write(&path, canonical_release_bytes(&stored).unwrap()).unwrap();
        let foreign = root
            .path()
            .join("publications/releases")
            .join(format!("release_{}.json", "f".repeat(64)));
        fs::copy(&path, foreign).unwrap();
        assert!(release_public(root.path(), &request).is_err());
    }
    #[test]
    fn public_release_rejects_unsafe_source_urls_without_writes() {
        for value in [
            "file:///Users/synthetic/private",
            "https://user:secret@example.test/x",
            "mailto:test@example.test",
            "relative/path",
            "https://",
        ] {
            let root = tempdir().unwrap();
            let outside = tempdir().unwrap();
            let sentinel = outside.path().join("sentinel");
            fs::write(&sentinel, b"unchanged").unwrap();
            let publication = prepared_public_fixture_with_urls(
                root.path(),
                value,
                Some("https://example.test/canonical"),
            );
            let error = release_public(
                root.path(),
                &release_request_for(publication.publication_id),
            )
            .unwrap_err()
            .to_string();
            assert!(
                !error.contains(value) && !error.contains("/Users/") && !error.contains("secret")
            );
            assert!(
                !root.path().join("publications/releases").exists()
                    && !root.path().join("publications/public").exists()
            );
            assert_eq!(fs::read(sentinel).unwrap(), b"unchanged");
        }
        let root = tempdir().unwrap();
        let publication = prepared_public_fixture_with_urls(
            root.path(),
            "http://example.test/x",
            Some("https://example.test/canonical"),
        );
        let receipt = release_public(
            root.path(),
            &release_request_for(publication.publication_id),
        )
        .unwrap();
        assert_eq!(receipt.status, ApplyStatus::Created);
        assert!(root
            .path()
            .join("publications/public")
            .join(receipt.release_id)
            .exists());
    }
    #[test]
    fn public_release_rejects_unsafe_canonical_urls_without_writes() {
        for value in [
            "file:///Users/synthetic/private",
            "https://user:secret@example.test/x",
            "mailto:test@example.test",
            "relative/path",
            "https://",
        ] {
            let root = tempdir().unwrap();
            let outside = tempdir().unwrap();
            let sentinel = outside.path().join("sentinel");
            fs::write(&sentinel, b"unchanged").unwrap();
            let publication = prepared_public_fixture_with_urls(
                root.path(),
                "https://example.test/x",
                Some(value),
            );
            let error = release_public(
                root.path(),
                &release_request_for(publication.publication_id),
            )
            .unwrap_err()
            .to_string();
            assert!(
                !error.contains(value) && !error.contains("/Users/") && !error.contains("secret")
            );
            assert!(
                !root.path().join("publications/releases").exists()
                    && !root.path().join("publications/public").exists()
            );
            assert_eq!(fs::read(sentinel).unwrap(), b"unchanged");
        }
    }
    #[cfg(unix)]
    #[test]
    fn public_release_rejects_tamper_noncanonical_wrong_filename_and_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"unchanged").unwrap();
        let (publication, _, _) = prepared_public_fixture(root.path());
        let release = prepare_public_release(
            root.path(),
            &release_request_for(publication.publication_id),
        )
        .unwrap();
        apply_public_release(root.path(), &release).unwrap();
        let path = root
            .path()
            .join("publications/releases")
            .join(format!("{}.json", release.release_id));
        let mut legal: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        legal["published_at"] = serde_json::Value::String("2026-08-01T00:00:00Z".into());
        let mut tampered = serde_json::to_vec_pretty(&legal).unwrap();
        tampered.push(b'\n');
        fs::write(&path, tampered).unwrap();
        assert!(read_public_release(root.path(), &release.release_id).is_err());
        assert!(apply_public_release(root.path(), &release).is_err());
        fs::write(&path, serde_json::to_vec(&release).unwrap()).unwrap();
        assert!(read_public_release(root.path(), &release.release_id).is_err());
        fs::write(&path, canonical_release_bytes(&release).unwrap()).unwrap();
        let other = format!("release_{}", "f".repeat(64));
        fs::copy(
            &path,
            root.path()
                .join("publications/releases")
                .join(format!("{other}.json")),
        )
        .unwrap();
        assert!(read_public_release(root.path(), &other).is_err());
        fs::remove_file(&path).unwrap();
        symlink(outside.path(), &path).unwrap();
        assert!(read_public_release(root.path(), &release.release_id).is_err());
        assert!(apply_public_release(root.path(), &release).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
    }
}
