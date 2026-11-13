use anyhow::{Context, Result};
use fs2::FileExt;
use memex_core::{
    captured_attachment_relative_path, image_signature_matches, new_id, AttachmentRef,
    CaptureRecord, CitedReference, Creator, CreatorNameType, DraftAttachmentRef, Evidence,
    EvidenceLink, IdentifierKind, IngestReceipt, Institution, Item, ItemDraft, ItemIdentifier,
    ItemMetadata, ItemRevision, MetadataOrigin, MetadataProvenance, PartialDate,
    ProvenanceActivity, Sensitivity, Sourced, ValidationStatus, VerifiedCapture, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use url::Url;

mod reconversion;
pub use reconversion::{reconvert_file_item, FileReconversion};
mod body;
pub use body::{body_identity, same_body_revision, same_body_revisions};
mod reading;
pub use reading::{edit_markdown, read_annotations, save_annotations, MarkdownEdit, ReadingAnnotations, Underline};
pub mod reader;
pub mod book_projects;
mod images;
pub use images::{read_image, ImageChunk, IMAGE_CHUNK_BYTES};
pub mod covers;
mod origin;
pub use origin::item_origin;
pub mod dates;
mod batch;
pub use batch::{ingest_batch, MAX_INGEST_BATCH_ITEMS};
mod web_capture;
pub use web_capture::{find_web_capture, ingest_web_capture, WebCaptureReceipt};
mod added_at;
pub use added_at::{
    apply_added_at_backfill, effective as verified_item_added_at,
    for_import_commit as added_at_for_import_commit,
    from_capture_time as added_at_from_capture_time, preview_added_at_backfill, AddedAtApplyCounts,
    AddedAtApplyResult, AddedAtApplyRow, AddedAtBackfillPreview, AddedAtBackfillReceipt,
    AddedAtBackfillRow, AddedAtBackfillStatus, AddedAtItemBinding, AddedAtSourceIdentity,
    AddedAtSourceRef, AddedAtStatusCounts,
};
mod metadata_v2;
pub mod podcast_metadata;
#[cfg(test)]
use metadata_v2::metadata_request_path;
pub use metadata_v2::{
    find_identifier_duplicates, read_item_metadata_v2, revise_item_metadata_v2,
    IdentifierDuplicate, IdentifierRead, ItemMetadataEditV2, ItemMetadataV2Read,
    METADATA_SCHEMA_VERSION,
};
use metadata_v2::{valid_request_id, write_metadata_request};
mod trash;
pub use trash::{
    is_item_trashed, list_trash, read_trash_record, restore_item, trash_item, TrashEntry,
    TrashRecord, TrashRestoreReceipt, TRASH_PAGE_SIZE,
};
mod groups;
pub use groups::group_delete;
pub use groups::{edit_group_trash, import_native_groups, list_group_trash, NativeGroupImport};
pub use groups::{
    archive_podcast, PodcastArchiveRequest, PodcastArchiveSource,
    edit_groups, group_children, read_group_tree, read_visible_groups, GroupEditOp,
    GroupEditReceipt, GroupKind, GroupMembership, GroupNode, GroupNodeView, GroupSource,
    GroupTreePage, GroupTreeSnapshot, VisibleGroups, MAX_GROUP_EDIT_ITEMS, MAX_GROUP_TREE_PAGE,
};
mod lookup;
pub use lookup::{
    create_lookup_preview, lookup_redirect_allowed, lookup_request_url, lookup_route,
    page_lookup_preview, prepare_bibliographic_lookup, read_lookup_preview, read_lookup_value,
    LookupCandidate, LookupCandidateView, LookupPreview, LookupPreviewPage, LookupProvider,
    LookupTarget, LookupValueKind, LookupValuePage, LookupValueSide, LookupValueSummary,
    LOOKUP_ADAPTER_VERSION, LOOKUP_PAGE_BUDGET_BYTES, LOOKUP_PREVIEW_TTL_SECONDS,
    LOOKUP_VALUE_PAGE_BYTES, MAX_INLINE_LOOKUP_VALUE_BYTES, MAX_LOOKUP_CANDIDATE_PAGE,
    MAX_LOOKUP_PREVIEWS, MAX_LOOKUP_RESPONSE_BYTES,
};
const MAX_ATTACHMENT_READ_BYTES: u64 = 2 * 1024 * 1024;
const INGEST_JOURNAL_VERSION: u32 = 1;
const ITEM_UPDATE_JOURNAL_VERSION: u32 = 1;
const MAX_ITEM_REVISION_CHAIN: u32 = 4096;
const MAX_ITEM_UPDATE_JOURNALS: usize = 4096;
pub const MAX_ITEM_METADATA_PAGE_SIZE: usize = 50;
pub const MAX_ITEM_METADATA_PAGE_SCAN_ENTRIES: usize = 4096;

#[cfg(test)]
thread_local! { static FAIL_APPLY_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
thread_local! { static FAIL_ITEM_UPDATE_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }

fn ingest_apply_step(step: usize) -> Result<()> {
    #[cfg(test)]
    if FAIL_APPLY_STEP.with(|value| value.get()) == Some(step) {
        anyhow::bail!("simulated ingest interruption");
    }
    #[cfg(not(test))]
    let _ = step;
    Ok(())
}

fn item_update_step(step: usize) -> Result<()> {
    #[cfg(test)]
    if FAIL_ITEM_UPDATE_STEP.with(|value| value.get()) == Some(step) {
        anyhow::bail!("simulated item update interruption");
    }
    #[cfg(not(test))]
    let _ = step;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Prepared,
    Committed,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct JournalEntry {
    staged: String,
    path: String,
    sha256: String,
    byte_size: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct IngestJournal {
    journal_version: u32,
    schema_version: u32,
    ingest_key: String,
    input_sha256: String,
    source_id: String,
    capture_id: String,
    item_id: String,
    revision_id: String,
    evidence_ids: Vec<String>,
    activity_id: String,
    receipt_path: String,
    receipt: IngestReceipt,
    directories: Vec<String>,
    entries: Vec<JournalEntry>,
    state: JournalState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ItemCurrentPointer {
    schema_version: u32,
    item: Item,
    revision_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ItemUpdateJournal {
    journal_version: u32,
    schema_version: u32,
    item_id: String,
    base_revision_id: String,
    revision_id: String,
    pointer: ItemCurrentPointer,
    /// Durable request-identity record for a v2 edit, installed by reconcile
    /// before the journal is removed (so recovery completes it too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<MetadataEditRequestRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataEditRequestRecord {
    schema_version: u32,
    request_id: String,
    request_sha256: String,
    receipt: ItemMetadataUpdateReceipt,
}

#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
struct ItemIntegrityManifest {
    schema_version: u32,
    item_id: String,
    current_revision: String,
    sensitivity: memex_core::Sensitivity,
    item_sha256: String,
    revision_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortableItemIndex {
    pub schema_version: u32,
    pub projection: String,
    pub projection_version: u32,
    pub item_id: String,
    pub revision_id: String,
    pub revision_number: u32,
    pub item_type: String,
    pub title: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub sensitivity: Sensitivity,
    pub captured_at: Option<String>,
    pub translator: Option<PortableTranslator>,
    pub metadata: ItemMetadata,
    pub provenance: Vec<ProvenanceActivity>,
    pub content: PortableContent,
    pub attachments: Vec<PortableAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemReadMetadata {
    pub item_id: String,
    pub revision_id: String,
    pub revision_number: u32,
    pub item_type: String,
    pub title: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub sensitivity: Sensitivity,
    pub captured_at: Option<String>,
    pub metadata: ItemMetadata,
    /// First inclusion (read-only); omitted for legacy items in this DTO.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemMetadataUpdate {
    pub expected_revision_id: String,
    pub title: String,
    #[serde(default)]
    pub manual_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemMetadataUpdateReceipt {
    pub schema_version: u32,
    pub item_id: String,
    pub previous_revision_id: String,
    pub revision_id: String,
    pub revision_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReadAttachment {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortableTranslator {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortableContent {
    pub path: String,
    pub media_type: String,
    pub byte_size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortableAttachment {
    Remote {
        url: String,
        media_type: Option<String>,
        title: Option<String>,
    },
    Evidence {
        evidence_id: String,
        path: String,
        media_type: String,
        byte_size: u64,
        sha256: String,
        source_url: Option<String>,
        title: Option<String>,
    },
}

#[derive(Debug)]
pub struct IngestConflict;

impl std::fmt::Display for IngestConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("immutable ingest input conflicts with existing capture")
    }
}

impl std::error::Error for IngestConflict {}

fn ensure_contained_dir(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure_contained_dir_with_sync(root, relative, sync_dir)
}

fn ensure_contained_dir_with_sync(
    root: &Path, relative: &Path, sync: fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    for part in relative.components() {
        let std::path::Component::Normal(part) = part else {
            anyhow::bail!("unsafe output directory");
        };
        let candidate = current.join(part);
        match fs::symlink_metadata(&candidate) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    anyhow::bail!("unsafe output directory");
                }
                let canonical = candidate.canonicalize()?;
                if !canonical.starts_with(root) {
                    anyhow::bail!("output directory escapes root");
                }
                current = canonical;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let created = match fs::create_dir(&candidate) {
                    Ok(()) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                    Err(error) => return Err(error.into()),
                };
                current = candidate.canonicalize()?;
                if !current.starts_with(root) || !current.is_dir() {
                    anyhow::bail!("output directory escapes root");
                }
                if created {
                    sync(
                        current
                            .parent()
                            .context("created directory has no parent")?,
                    )?;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current)
}
fn contained_output_path(root: &Path, path: &Path) -> Result<PathBuf> {
    contained_output_path_with_sync(root, path, sync_dir)
}

fn contained_output_path_with_sync(
    root: &Path, path: &Path, sync: fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("output outside root"))?;
    let parent = relative.parent().unwrap_or(Path::new(""));
    let name = relative.file_name().context("output has no filename")?;
    let parent = ensure_contained_dir_with_sync(root, parent, sync)?;
    let target = parent.join(name);
    if let Ok(meta) = fs::symlink_metadata(&target) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            anyhow::bail!("unsafe output file");
        }
    }
    Ok(target)
}
fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn durable_replace(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    durable_replace_with_gate(root, path, bytes, |publish| publish())
}

fn durable_replace_with_gate<G>(root: &Path, path: &Path, bytes: &[u8], gate: G) -> Result<()>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    let path = contained_output_path(root, path)?;
    let parent = path.parent().context("output path has no parent")?;
    let temporary = parent.join(format!(".journal-{}.tmp", new_id("write")));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut published = false;
        gate(Box::new(|| {
            fs::rename(&temporary, &path)?;
            published = true;
            Ok(())
        }))?;
        anyhow::ensure!(published, "publication_gate_did_not_publish");
        sync_dir(parent)
    })();
    if result.is_err() && fs::remove_file(&temporary).is_ok() {
        let _ = sync_dir(parent);
    }
    result
}

/// Repair only derived files, after their canonical inputs have been verified.
/// Avoid renaming healthy projections during every snapshot, but retain the
/// private-file and no-symlink guarantees of durable replacement.
fn repair_derived_file(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let path = contained_output_path(root, path)?;
    #[cfg(unix)]
    match open_regular_nofollow(&path) {
        Ok(file) => {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if metadata.len() == bytes.len() as u64
                && metadata.mode() & 0o7777 == 0o600
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.nlink() == 1
            {
                let mut found = Vec::with_capacity(bytes.len());
                file.take(bytes.len() as u64 + 1).read_to_end(&mut found)?;
                if found == bytes {
                    return Ok(());
                }
            }
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    durable_replace(root, &path, bytes)
}

fn open_regular_nofollow(path: &Path) -> Result<fs::File> {
    #[cfg(not(unix))]
    {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            anyhow::bail!("unsafe attachment projection");
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("unsafe attachment projection");
    }
    record_verified_file(path, &file)?;
    Ok(file)
}
fn verify_binary(path: &Path, expected: &[u8]) -> Result<()> {
    let file = open_regular_nofollow(path)?;
    if file.metadata()?.len() != expected.len() as u64 {
        anyhow::bail!("unsafe attachment projection");
    }
    let mut bytes = Vec::with_capacity(expected.len() + 1);
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected {
        anyhow::bail!("attachment projection mismatch");
    }
    Ok(())
}
fn install_binary(root: &Path, path: &Path, contents: &[u8]) -> Result<()> {
    install_binary_with_gate(root, path, contents, |publish| publish())
}

fn install_binary_with_gate<G>(root: &Path, path: &Path, contents: &[u8], gate: G) -> Result<()>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    let path = contained_output_path(root, path)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => return verify_binary(&path, contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().context("attachment path has no parent")?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        new_id("attachment")
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        let mut published = false;
        gate(Box::new(|| {
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    verify_binary(&path, contents)?;
                }
                Err(error) => return Err(error.into()),
            }
            published = true;
            Ok(())
        }))?;
        anyhow::ensure!(published, "publication_gate_did_not_publish");
        sync_dir(parent)
    })();
    let _ = fs::remove_file(temporary);
    result
}
fn ingest_key(capture: &VerifiedCapture) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                capture.capture.adapter_id.as_str(),
                capture.capture.external_capture_id.as_str()
            ))
            .expect("tuple serialization")
        )
    )
}
fn effective_sensitivity(capture: &VerifiedCapture) -> memex_core::Sensitivity {
    std::cmp::max(
        capture.source.default_sensitivity,
        capture.capture.sensitivity,
    )
}
fn safe_component(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_item_id(item_id: &str) -> bool {
    safe_component(item_id, "item_")
}
fn valid_source_id(source_id: &str) -> bool {
    safe_component(source_id, "")
}
fn safe_receipt_path(root: &Path, value: &str) -> Result<std::path::PathBuf> {
    let p = Path::new(value);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("unsafe catalog receipt path");
    }
    let candidate = root.join(p);
    if !candidate.starts_with(root) {
        anyhow::bail!("catalog receipt path escapes library");
    }
    Ok(candidate)
}
fn canonical_root(root: &Path) -> Result<PathBuf> {
    if let Err(error) = fs::symlink_metadata(root) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
        let mut missing = Vec::new();
        let mut ancestor = root;
        loop {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => break,
                Ok(_) => anyhow::bail!("library root ancestor is not a safe directory"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(
                        ancestor
                            .file_name()
                            .context("library root has no existing ancestor")?
                            .to_owned(),
                    );
                    ancestor = ancestor
                        .parent()
                        .context("library root has no existing ancestor")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut current = ancestor.canonicalize()?;
        for component in missing.iter().rev() {
            let next = current.join(component);
            fs::create_dir(&next)?;
            sync_dir(&current)?;
            current = next.canonicalize()?;
        }
    }
    Ok(root.canonicalize()?)
}
fn existing_canonical_root(root: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        anyhow::bail!("library root is not a directory");
    }
    Ok(root)
}
fn contained_existing(root: &Path, path: &Path) -> Result<PathBuf> {
    let value = path.canonicalize()?;
    if !value.starts_with(root) {
        anyhow::bail!("library path escapes root");
    }
    VERIFYING_READS.with(|active| {
        for reads in active.borrow_mut().iter_mut() {
            if !reads.complete { continue; }
            if reads.root != root { reads.complete = false; }
            if let Some(old) = reads.resolutions.insert(path.to_path_buf(), value.clone()) {
                if old != value { reads.complete = false; }
            }
            reads.enforce_limit();
        }
    });
    Ok(value)
}
fn valid_revision_id(value: &str) -> bool {
    safe_component(value, "revision_")
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}
fn valid_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn valid_http_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}
fn valid_captured_source_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}
fn valid_provenance(value: &MetadataProvenance) -> bool {
    valid_text(&value.provider_id, 128)
        && valid_text(&value.provider_version, 128)
        && value.source_refs.len() <= 16
        && value.source_refs.iter().all(|r| valid_text(r, 4096))
}
fn valid_sourced<T>(value: &Sourced<T>) -> bool {
    valid_provenance(&value.provenance)
}
fn strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}
fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}
fn valid_optional_text(value: &Option<String>, max_bytes: usize) -> bool {
    value
        .as_deref()
        .is_none_or(|value| valid_text(value, max_bytes))
}
fn is_owner_provenance(value: &MetadataProvenance) -> bool {
    value.origin == MetadataOrigin::OwnerEdit
        && value.provider_id == "memex.desktop"
        && value.provider_version == "1"
}
fn unique<'a>(values: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    values.into_iter().all(|value| seen.insert(value))
}
fn valid_partial_date(value: &PartialDate) -> bool {
    value.validate().is_ok() && valid_optional_text(&value.raw, 4096)
}
fn valid_creators(creators: &[Creator], institution_ids: &BTreeSet<&str>) -> bool {
    !creators.is_empty()
        && creators.len() <= 64
        && unique(creators.iter().filter_map(|c| c.creator_id.as_deref()))
        && creators.iter().all(|creator| {
            let names_ok = match creator.name_type {
                Some(CreatorNameType::Person) => creator.literal_name.is_none(),
                Some(CreatorNameType::Organization | CreatorNameType::Unparsed) => {
                    creator.family_name.is_none() && creator.given_name.is_none()
                }
                None => {
                    creator.family_name.is_none()
                        && creator.given_name.is_none()
                        && creator.literal_name.is_none()
                }
            };
            names_ok
                && valid_text(&creator.display_name, 4096)
                && creator.creator_id.as_deref().is_none_or(valid_opaque_id)
                && valid_optional_text(&creator.family_name, 4096)
                && valid_optional_text(&creator.given_name, 4096)
                && valid_optional_text(&creator.literal_name, 4096)
                && creator.institution_ids.len() <= 64
                && unique(creator.institution_ids.iter().map(String::as_str))
                && creator
                    .institution_ids
                    .iter()
                    .all(|id| institution_ids.contains(id.as_str()))
        })
}
/// Owner-entered DOI/ISBN/ISSN must be stored in normalized, checksum-valid
/// form. Source values are retained as supplied (invalid ones never dedup).
fn valid_identifiers(identifiers: &[ItemIdentifier], manual: bool) -> bool {
    !identifiers.is_empty()
        && identifiers.len() <= 64
        && identifiers.iter().all(|identifier| {
            valid_text(&identifier.value, 4096)
                && valid_optional_text(&identifier.raw_value, 4096)
                && valid_optional_text(&identifier.qualifier, 128)
                && (!manual
                    || identifier.kind == IdentifierKind::Pmid
                    || identifier.normalized().validation.status == ValidationStatus::Valid
                        && identifier.normalized().value.as_deref()
                            == Some(identifier.value.as_str()))
        })
}
fn valid_institutions(institutions: &[Institution]) -> bool {
    !institutions.is_empty()
        && institutions.len() <= 1024
        && unique(institutions.iter().map(|i| i.institution_id.as_str()))
        && institutions.iter().all(|institution| {
            valid_opaque_id(&institution.institution_id)
                && valid_text(&institution.name, 4096)
                && valid_optional_text(&institution.country, 64)
                && institution.identifiers.len() <= 16
                && institution
                    .identifiers
                    .iter()
                    .all(|id| valid_text(&id.kind, 64) && valid_text(&id.value, 4096))
        })
}
fn valid_cited_references(references: &[CitedReference], manual: bool) -> bool {
    let no_institutions = BTreeSet::new();
    references.len() <= 4096
        && unique(references.iter().map(|r| r.reference_id.as_str()))
        && references.iter().all(|reference| {
            valid_opaque_id(&reference.reference_id)
                && valid_optional_text(&reference.raw_text, 16 * 1024)
                && valid_optional_text(&reference.title, 4096)
                && reference
                    .creators
                    .as_deref()
                    .is_none_or(|creators| valid_creators(creators, &no_institutions))
                && reference
                    .published_date
                    .as_ref()
                    .is_none_or(valid_partial_date)
                && valid_optional_text(&reference.journal_title, 4096)
                && valid_optional_text(&reference.volume, 4096)
                && valid_optional_text(&reference.issue, 4096)
                && valid_optional_text(&reference.pages, 4096)
                && reference
                    .identifiers
                    .as_deref()
                    .is_none_or(|ids| valid_identifiers(ids, manual))
                && reference
                    .target_item_id
                    .as_deref()
                    .is_none_or(valid_item_id)
                && reference.source_refs.len() <= 64
                && reference.source_refs.iter().all(|r| valid_text(r, 4096))
        })
}
fn validate_draft(draft: &ItemDraft) -> Result<()> {
    validate_metadata(&draft.metadata)?;
    validate_draft_attachments(draft)
}
fn validate_metadata(metadata: &ItemMetadata) -> Result<()> {
    if metadata.duration_seconds.as_ref().is_some_and(|v| !valid_sourced(v) || v.value == 0) {
        anyhow::bail!("invalid_metadata_field:duration_seconds");
    }
    let institution_ids: BTreeSet<&str> = metadata
        .institutions
        .iter()
        .flat_map(|value| value.value.iter().map(|i| i.institution_id.as_str()))
        .collect();
    if let Some(value) = &metadata.creators {
        if !valid_sourced(value) || !valid_creators(&value.value, &institution_ids) {
            anyhow::bail!("invalid item creators");
        }
    }
    if let Some(value) = &metadata.institutions {
        if !valid_sourced(value) || !valid_institutions(&value.value) {
            anyhow::bail!("invalid_metadata_field:institutions");
        }
    }
    if let Some(value) = &metadata.published_partial_date {
        if !valid_sourced(value) || !valid_partial_date(&value.value) {
            anyhow::bail!("invalid_metadata_field:published_date");
        }
    }
    for (name, value) in [
        ("journal_title", &metadata.journal_title),
        ("publisher", &metadata.publisher),
        ("volume", &metadata.volume),
        ("issue", &metadata.issue),
        ("pages", &metadata.pages),
        ("publication_place", &metadata.publication_place),
        ("edition", &metadata.edition),
        ("series_title", &metadata.series_title),
        ("series_number", &metadata.series_number),
    ] {
        if value
            .as_ref()
            .is_some_and(|field| !valid_sourced(field) || !valid_text(&field.value, 4096))
        {
            anyhow::bail!("invalid_metadata_field:{name}");
        }
    }
    if let Some(value) = &metadata.number_of_pages {
        if !valid_sourced(value) || value.value == 0 || value.value > 1_000_000 {
            anyhow::bail!("invalid_metadata_field:number_of_pages");
        }
    }
    if let Some(value) = &metadata.keywords {
        if !valid_sourced(value)
            || value.value.is_empty()
            || value.value.len() > 256
            || !unique(value.value.iter().map(String::as_str))
            || value.value.iter().any(|keyword| !valid_text(keyword, 1024))
        {
            anyhow::bail!("invalid_metadata_field:keywords");
        }
    }
    if let Some(value) = &metadata.cited_references {
        if !valid_sourced(value)
            || !valid_cited_references(&value.value, is_owner_provenance(&value.provenance))
        {
            anyhow::bail!("invalid_metadata_field:cited_references");
        }
    }
    for (value, max) in [
        (metadata.published_date.as_ref(), 4096),
        (metadata.language.as_ref(), 4096),
        (metadata.site_name.as_ref(), 4096),
        (metadata.abstract_text.as_ref(), 64 * 1024),
    ] {
        if value.is_some_and(|field| !valid_sourced(field) || !valid_text(&field.value, max)) {
            anyhow::bail!("invalid item metadata text");
        }
    }
    if let Some(value) = &metadata.identifiers {
        if !valid_sourced(value)
            || !strictly_sorted(&value.value)
            || !valid_identifiers(&value.value, is_owner_provenance(&value.provenance))
        {
            anyhow::bail!("invalid item identifiers");
        }
    }
    if let Some(value) = &metadata.manual_tags {
        if !valid_sourced(value)
            || value.value.is_empty()
            || value.value.len() > 256
            || !strictly_sorted(&value.value)
            || value.value.iter().any(|tag| !valid_text(tag, 4096))
        {
            anyhow::bail!("invalid item manual tags");
        }
    }
    if let Some(value) = &metadata.content_rights {
        if !valid_sourced(value)
            || value
                .value
                .statement
                .as_deref()
                .is_some_and(|statement| !valid_text(statement, 4096))
            || value
                .value
                .license_url
                .as_deref()
                .is_some_and(|license_url| !valid_http_url(license_url))
        {
            anyhow::bail!("invalid item content rights");
        }
    }
    Ok(())
}
fn validate_draft_attachments(draft: &ItemDraft) -> Result<()> {
    if draft.attachments.len() > 128 || !strictly_sorted(&draft.attachments) {
        anyhow::bail!("invalid item attachment ordering");
    }
    for attachment in &draft.attachments {
        match attachment {
            DraftAttachmentRef::Remote {
                url,
                media_type,
                title,
            } if valid_http_url(url)
                && media_type
                    .as_deref()
                    .is_none_or(|value| valid_text(value, 4096))
                && title.as_deref().is_none_or(|value| valid_text(value, 4096)) => {}
            DraftAttachmentRef::Remote { .. } => anyhow::bail!("invalid remote attachment"),
            DraftAttachmentRef::Captured { blob_id, title, .. }
                if valid_sha256(blob_id)
                    && title.as_deref().is_none_or(|value| valid_text(value, 4096)) => {}
            DraftAttachmentRef::Captured { .. } => {
                anyhow::bail!("invalid captured attachment")
            }
        }
    }
    Ok(())
}
fn captured_attachment_paths(
    capture: &VerifiedCapture,
    draft: &ItemDraft,
) -> Result<BTreeMap<usize, String>> {
    let mut paths = BTreeMap::new();
    let mut source_urls = BTreeSet::new();
    let mut blob_ids = BTreeSet::new();
    for attachment in &draft.attachments {
        let DraftAttachmentRef::Captured {
            blob_index,
            blob_id,
            ..
        } = attachment
        else {
            continue;
        };
        let index = usize::try_from(*blob_index).context("captured attachment index overflow")?;
        let blob = capture
            .blobs
            .get(index)
            .context("captured attachment index is out of bounds")?;
        if &blob.blob_id != blob_id
            || blob.descriptor.schema_version != SCHEMA_VERSION
            || blob.descriptor.byte_size != blob.bytes.len() as u64
            || blob.descriptor.sha256 != format!("{:x}", Sha256::digest(&blob.bytes))
            || blob.blob_id != format!("sha256:{}", blob.descriptor.sha256)
            || !image_signature_matches(&blob.descriptor.media_type, &blob.bytes)
        {
            anyhow::bail!("captured attachment does not match verified image blob");
        }
        if !capture.capture.artifacts.is_empty()
            && capture.capture.artifacts.get(index) != Some(&blob.descriptor)
        {
            anyhow::bail!("captured attachment descriptor occurrence mismatch");
        }
        let source_url = blob
            .descriptor
            .source_url
            .as_deref()
            .filter(|value| valid_captured_source_url(value))
            .context("captured attachment source URL is invalid")?;
        let relative_path =
            captured_attachment_relative_path(&blob.blob_id, &blob.descriptor.media_type)
                .context("captured attachment media type is unsupported")?;
        if paths.insert(index, relative_path).is_some()
            || !source_urls.insert(source_url)
            || !blob_ids.insert(blob_id)
        {
            anyhow::bail!("duplicate captured attachment")
        }
    }
    Ok(paths)
}
fn resolve_attachments(
    draft: &ItemDraft,
    evidence_ids: &[String],
    paths: &BTreeMap<usize, String>,
) -> Result<Vec<AttachmentRef>> {
    let mut attachments = Vec::with_capacity(draft.attachments.len());
    for attachment in &draft.attachments {
        attachments.push(match attachment {
            DraftAttachmentRef::Remote {
                url,
                media_type,
                title,
            } => AttachmentRef::Remote {
                url: url.clone(),
                media_type: media_type.clone(),
                title: title.clone(),
            },
            DraftAttachmentRef::Captured {
                blob_index, title, ..
            } => {
                let index = usize::try_from(*blob_index)?;
                AttachmentRef::Evidence {
                    evidence_id: evidence_ids
                        .get(index)
                        .context("captured attachment evidence occurrence missing")?
                        .clone(),
                    relative_path: paths
                        .get(&index)
                        .context("captured attachment path missing")?
                        .clone(),
                    title: title.clone(),
                }
            }
        });
    }
    attachments.sort();
    Ok(attachments)
}
fn evidence_backend_ref(
    blob_index: usize,
    item_id: &str,
    paths: &BTreeMap<usize, String>,
    capture: &VerifiedCapture,
) -> Result<String> {
    if let Some(path) = paths.get(&blob_index) {
        Ok(format!("memex-store:{item_id}/{path}"))
    } else {
        capture.blobs[blob_index]
            .source_backend_ref
            .clone()
            .context("verified blob has no durable backend")
    }
}
fn expected_evidence(
    evidence_id: String,
    capture_id: String,
    blob_index: usize,
    item_id: &str,
    paths: &BTreeMap<usize, String>,
    capture: &VerifiedCapture,
    include_descriptor: bool,
) -> Result<Evidence> {
    let blob = &capture.blobs[blob_index];
    Ok(Evidence {
        schema_version: SCHEMA_VERSION,
        evidence_id,
        source_id: capture.source.source_id.clone(),
        capture_id,
        blob_id: blob.blob_id.clone(),
        logical_locator: blob.descriptor.locator.clone(),
        backend_ref: evidence_backend_ref(blob_index, item_id, paths, capture)?,
        sensitivity: effective_sensitivity(capture),
        supersedes: None,
        descriptor: include_descriptor.then(|| blob.descriptor.clone()),
    })
}
fn load<T: serde::de::DeserializeOwned>(root: &Path, path: PathBuf) -> Result<T> {
    let mut text = String::new();
    open_regular_nofollow(&contained_existing(root, &path)?)?.read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}
fn translator_config_digest(draft: &ItemDraft) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(format!(
            "{}:{}",
            draft.translator_id, draft.translator_version
        ))
    )
}
pub fn ingest_input_sha256(verified: &VerifiedCapture, draft: &ItemDraft) -> Result<String> {
    let blobs = verified
        .blobs
        .iter()
        .map(|blob| (&blob.descriptor, &blob.blob_id, &blob.source_backend_ref))
        .collect::<Vec<_>>();
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(
            &verified.source,
            &verified.capture,
            blobs,
            draft,
        ))?)
    ))
}
fn expected_activity(
    activity_id: String,
    verified: &VerifiedCapture,
    draft: &ItemDraft,
    evidence_ids: Vec<String>,
    revision_id: String,
) -> ProvenanceActivity {
    ProvenanceActivity {
        schema_version: SCHEMA_VERSION,
        activity_id,
        activity_type: "translate".into(),
        occurred_at: verified.capture.captured_at.clone(),
        tool_id: draft.translator_id.clone(),
        tool_version: draft.translator_version.clone(),
        config_digest: translator_config_digest(draft),
        input_ids: evidence_ids,
        input_checksums: verified.blobs.iter().map(|b| b.blob_id.clone()).collect(),
        output_ids: vec![revision_id],
    }
}
fn validate_replay_graph(
    root: &Path,
    key: &str,
    receipt_path: &str,
    verified: &VerifiedCapture,
    draft: &ItemDraft,
) -> Result<IngestReceipt> {
    let paths = captured_attachment_paths(verified, draft)?;
    let receipt: IngestReceipt = load(root, safe_receipt_path(root, receipt_path)?)?;
    if receipt.idempotent_replay {
        anyhow::bail!("invalid replay receipt state");
    }
    if receipt.schema_version != SCHEMA_VERSION
        || receipt_path != format!("receipts/{}.json", receipt.capture_id)
        || !safe_component(&receipt.capture_id, "capture_")
        || receipt.item_ids.len() != 1
        || receipt.revision_ids.len() != 1
        || receipt.evidence_ids.is_empty()
        || receipt
            .evidence_ids
            .iter()
            .any(|id| !safe_component(id, "evidence_"))
        || receipt
            .evidence_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != receipt.evidence_ids.len()
        || receipt.item_ids.iter().any(|id| !valid_item_id(id))
        || receipt.revision_ids.iter().any(|id| !valid_revision_id(id))
    {
        anyhow::bail!("invalid replay receipt");
    }
    let capture: CaptureRecord = load(
        root,
        root.join("captures")
            .join(&receipt.capture_id)
            .join("capture.json"),
    )?;
    let incoming_input_sha256 = ingest_input_sha256(verified, draft)?;
    if let Some(stored) = &capture.ingest_input_sha256 {
        if !valid_sha256(stored) {
            anyhow::bail!("invalid replay capture fingerprint");
        }
        if stored != &incoming_input_sha256 {
            return Err(IngestConflict.into());
        }
    }
    // A changed supplemental artifact count is a changed payload, not evidence of
    // a corrupt stored receipt. Compare the durable fingerprint before its shape.
    if receipt.evidence_ids.len() != verified.blobs.len() {
        anyhow::bail!("invalid replay receipt state");
    }
    let expected_capture = CaptureRecord {
        schema_version: SCHEMA_VERSION,
        capture_id: receipt.capture_id.clone(),
        adapter_id: verified.capture.adapter_id.clone(),
        adapter_version: verified.capture.adapter_version.clone(),
        external_capture_id: verified.capture.external_capture_id.clone(),
        source_id: verified.capture.source_id.clone(),
        captured_at: verified.capture.captured_at.clone(),
        capture_method: verified.capture.capture_method.clone(),
        route_summary: verified.capture.route_summary.clone(),
        source_url: verified.capture.source_url.clone(),
        canonical_url: verified.capture.canonical_url.clone(),
        sensitivity: effective_sensitivity(verified),
        blob_ids: verified.blobs.iter().map(|b| b.blob_id.clone()).collect(),
        ingest_input_sha256: capture.ingest_input_sha256.clone(),
        evidence_descriptor_version: capture.evidence_descriptor_version,
    };
    if capture != expected_capture || key != ingest_key(verified) {
        anyhow::bail!("invalid replay capture");
    }
    let source: memex_core::SourceProfile = load(
        root,
        root.join("sources")
            .join(&capture.source_id)
            .join("source.json"),
    )?;
    if source != verified.source {
        anyhow::bail!("invalid replay source");
    }
    if capture
        .evidence_descriptor_version
        .is_some_and(|version| version != 1)
    {
        anyhow::bail!("unsupported replay evidence descriptor version");
    }
    for (index, id) in receipt.evidence_ids.iter().enumerate() {
        let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        let expected = expected_evidence(
            id.clone(),
            receipt.capture_id.clone(),
            index,
            &receipt.item_ids[0],
            &paths,
            verified,
            capture.evidence_descriptor_version == Some(1),
        )?;
        if e != expected {
            anyhow::bail!("invalid replay evidence");
        }
    }
    let (item, revision) = read_item_without_integrity(root, &receipt.item_ids[0])?;
    let expected_sensitivity = std::cmp::max(effective_sensitivity(verified), draft.sensitivity);
    let expected_item = Item {
        schema_version: SCHEMA_VERSION,
        item_id: receipt.item_ids[0].clone(),
        item_type: draft.item_type.clone(),
        current_revision: receipt.revision_ids[0].clone(),
        sensitivity: expected_sensitivity,
        // Replay never resets first inclusion; it only verifies the stored form.
        added_at: item.added_at.clone(),
    };
    if item != expected_item
        || item
            .added_at
            .as_deref()
            .is_some_and(|value| !memex_core::valid_added_at(value))
        || revision.provenance.len() != 1
        || !safe_component(&revision.provenance[0].activity_id, "activity_")
    {
        anyhow::bail!("invalid replay item");
    }
    let activity = expected_activity(
        revision.provenance[0].activity_id.clone(),
        verified,
        draft,
        receipt.evidence_ids.clone(),
        receipt.revision_ids[0].clone(),
    );
    let expected_revision = ItemRevision {
        schema_version: SCHEMA_VERSION,
        revision_id: receipt.revision_ids[0].clone(),
        item_id: receipt.item_ids[0].clone(),
        revision_number: 1,
        title: draft.title.clone(),
        source_url: draft.source_url.clone(),
        canonical_url: draft.canonical_url.clone(),
        content_markdown: draft.content_markdown.clone(),
        sensitivity: expected_sensitivity,
        evidence_ids: receipt.evidence_ids.clone(),
        provenance: vec![activity],
        metadata: draft.metadata.clone(),
        attachments: resolve_attachments(draft, &receipt.evidence_ids, &paths)?,
    };
    if revision != expected_revision {
        anyhow::bail!("invalid replay draft provenance");
    }
    let links: Vec<EvidenceLink> = load(
        root,
        root.join("items").join(&item.item_id).join("links.json"),
    )?;
    if links != evidence_links(&expected_revision) {
        anyhow::bail!("invalid replay links");
    }
    let path = root
        .join("integrity/items")
        .join(format!("{}.json", expected_item.item_id));
    let expected = integrity_manifest(&expected_item, &expected_revision)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => durable_replace(
            root,
            &path,
            serde_json::to_string_pretty(&expected)?.as_bytes(),
        )?,
        Ok(m) if !m.file_type().is_symlink() && m.is_file() => {
            let found: ItemIntegrityManifest = load(root, path)?;
            if found != expected {
                anyhow::bail!("invalid replay integrity manifest")
            }
        }
        Ok(_) => anyhow::bail!("unsafe replay integrity manifest"),
        Err(e) => return Err(e.into()),
    }
    for (index, relative_path) in &paths {
        install_binary(
            root,
            &root.join("items").join(&item.item_id).join(relative_path),
            &verified.blobs[*index].bytes,
        )?;
    }
    install_item_projections(root, &item, &revision)?;
    Ok(receipt)
}

fn journal_key_name(key: &str) -> Result<&str> {
    if !valid_sha256(key) {
        anyhow::bail!("invalid ingest journal key");
    }
    Ok(&key[7..])
}

fn catalog_keys(root: &Path) -> Result<BTreeMap<String, String>> {
    let path = root.join("catalog/ingest-keys.json");
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            anyhow::bail!("unsafe ingest catalog")
        }
        Ok(_) => Ok(serde_json::from_slice(&fs::read(contained_existing(
            root, &path,
        )?)?)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

fn validate_journal_entry_bytes(
    journal: &IngestJournal,
    entry: &JournalEntry,
    bytes: &[u8],
) -> Result<()> {
    if entry.path == format!("captures/{}/capture.json", journal.capture_id) {
        let capture: CaptureRecord = serde_json::from_slice(bytes)?;
        if capture.capture_id != journal.capture_id
            || capture.source_id != journal.source_id
            || capture.ingest_input_sha256.as_deref() != Some(&journal.input_sha256)
        {
            anyhow::bail!("ingest journal capture mismatch");
        }
    } else if entry.path == format!("items/{}/item.json", journal.item_id) {
        let item: Item = serde_json::from_slice(bytes)?;
        if item.item_id != journal.item_id || item.current_revision != journal.revision_id {
            anyhow::bail!("ingest journal item mismatch");
        }
    } else if entry.path
        == format!(
            "items/{}/revisions/{}.json",
            journal.item_id, journal.revision_id
        )
    {
        let revision: ItemRevision = serde_json::from_slice(bytes)?;
        if revision.item_id != journal.item_id
            || revision.revision_id != journal.revision_id
            || revision.evidence_ids != journal.evidence_ids
            || revision.provenance.len() != 1
            || revision.provenance[0].activity_id != journal.activity_id
        {
            anyhow::bail!("ingest journal revision mismatch");
        }
    } else if entry.path == format!("sources/{}/source.json", journal.source_id) {
        let source: memex_core::SourceProfile = serde_json::from_slice(bytes)?;
        if source.source_id != journal.source_id {
            anyhow::bail!("ingest journal source mismatch");
        }
    } else if let Some(evidence_id) = journal
        .evidence_ids
        .iter()
        .find(|id| entry.path == format!("evidence/{id}/evidence.json"))
    {
        let evidence: Evidence = serde_json::from_slice(bytes)?;
        if evidence.evidence_id != *evidence_id
            || evidence.capture_id != journal.capture_id
            || evidence.source_id != journal.source_id
        {
            anyhow::bail!("ingest journal evidence mismatch");
        }
    } else if entry.path == format!("integrity/items/{}.json", journal.item_id) {
        let manifest: ItemIntegrityManifest = serde_json::from_slice(bytes)?;
        if manifest.item_id != journal.item_id || manifest.current_revision != journal.revision_id {
            anyhow::bail!("ingest journal integrity mismatch");
        }
    }
    Ok(())
}

fn validate_journal_graph(
    journal: &IngestJournal,
    bytes: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let get = |path: String| bytes.get(&path).context("incomplete ingest journal graph");
    let capture: CaptureRecord = serde_json::from_slice(get(format!(
        "captures/{}/capture.json",
        journal.capture_id
    ))?)?;
    if journal.ingest_key
        != format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&(
                capture.adapter_id.as_str(),
                capture.external_capture_id.as_str()
            ))?)
        )
        || capture.schema_version != SCHEMA_VERSION
        || capture.blob_ids.len() != journal.evidence_ids.len()
    {
        anyhow::bail!("ingest journal capture graph mismatch");
    }
    let source: memex_core::SourceProfile =
        serde_json::from_slice(get(format!("sources/{}/source.json", journal.source_id))?)?;
    if source.schema_version != SCHEMA_VERSION
        || source.source_id != journal.source_id
        || capture.source_id != source.source_id
    {
        anyhow::bail!("ingest journal source graph mismatch");
    }
    let item: Item = serde_json::from_slice(get(format!("items/{}/item.json", journal.item_id))?)?;
    let revision: ItemRevision = serde_json::from_slice(get(format!(
        "items/{}/revisions/{}.json",
        journal.item_id, journal.revision_id
    ))?)?;
    let activity = revision.provenance.first();
    if item.schema_version != SCHEMA_VERSION
        || revision.schema_version != SCHEMA_VERSION
        || item.item_id != journal.item_id
        || item.current_revision != journal.revision_id
        || revision.item_id != item.item_id
        || revision.revision_id != item.current_revision
        || revision.revision_number != 1
        || revision.sensitivity != item.sensitivity
        || revision.evidence_ids != journal.evidence_ids
        || revision.provenance.len() != 1
        || activity.is_none_or(|activity| {
            activity.schema_version != SCHEMA_VERSION
                || activity.activity_id != journal.activity_id
                || activity.activity_type != "translate"
                || activity.occurred_at != capture.captured_at
                || !valid_text(&activity.tool_id, 128)
                || !valid_text(&activity.tool_version, 128)
                || activity.config_digest
                    != format!(
                        "sha256:{:x}",
                        Sha256::digest(format!("{}:{}", activity.tool_id, activity.tool_version))
                    )
                || activity.input_ids != journal.evidence_ids
                || activity.input_checksums.len() != journal.evidence_ids.len()
                || activity.output_ids != [journal.revision_id.clone()]
        })
    {
        anyhow::bail!("ingest journal item graph mismatch");
    }
    let mut evidence = BTreeMap::new();
    for (index, id) in journal.evidence_ids.iter().enumerate() {
        let value: Evidence = serde_json::from_slice(get(format!("evidence/{id}/evidence.json"))?)?;
        if value.schema_version != SCHEMA_VERSION
            || value.evidence_id != *id
            || value.capture_id != journal.capture_id
            || value.source_id != journal.source_id
            || capture.blob_ids.get(index) != Some(&value.blob_id)
            || revision.provenance[0].input_checksums.get(index) != Some(&value.blob_id)
        {
            anyhow::bail!("ingest journal evidence graph mismatch");
        }
        evidence.insert(id.as_str(), value);
    }
    let links: Vec<EvidenceLink> =
        serde_json::from_slice(get(format!("items/{}/links.json", journal.item_id))?)?;
    if links != evidence_links(&revision) {
        anyhow::bail!("ingest journal links mismatch");
    }
    if journal.state == JournalState::Prepared {
        let manifest: ItemIntegrityManifest =
            serde_json::from_slice(get(format!("integrity/items/{}.json", journal.item_id))?)?;
        if manifest != integrity_manifest(&item, &revision)? {
            anyhow::bail!("ingest journal integrity mismatch");
        }
        let markdown = render_markdown(&item, &revision);
        if get(format!("items/{}/index.md", journal.item_id))?.as_slice() != markdown.as_bytes() {
            anyhow::bail!("ingest journal markdown projection mismatch");
        }
        let portable: PortableItemIndex =
            serde_json::from_slice(get(format!("items/{}/index.json", journal.item_id))?)?;
        let expected = build_portable_item_index_from_evidence(&item, &revision, |id| {
            evidence
                .get(id)
                .cloned()
                .context("missing portable evidence")
        })?;
        if portable != expected {
            anyhow::bail!("ingest journal portable projection mismatch");
        }
    }
    for attachment in &revision.attachments {
        if let AttachmentRef::Evidence {
            evidence_id,
            relative_path,
            ..
        } = attachment
        {
            let value = evidence
                .get(evidence_id.as_str())
                .context("missing attachment evidence")?;
            let descriptor = value
                .descriptor
                .as_ref()
                .context("missing attachment descriptor")?;
            let expected =
                captured_attachment_relative_path(&value.blob_id, &descriptor.media_type)
                    .context("unsupported attachment descriptor")?;
            let stored = get(format!("items/{}/{}", journal.item_id, relative_path))?;
            if relative_path != &expected
                || descriptor.byte_size != stored.len() as u64
                || descriptor.sha256 != format!("{:x}", Sha256::digest(stored))
                || value.blob_id != format!("sha256:{}", descriptor.sha256)
            {
                anyhow::bail!("ingest journal attachment mismatch");
            }
        }
    }
    Ok(())
}

fn journal_entry_is_derived(journal: &IngestJournal, path: &str) -> bool {
    path == format!("items/{}/index.md", journal.item_id)
        || path == format!("items/{}/index.json", journal.item_id)
        || path == format!("integrity/items/{}.json", journal.item_id)
}

fn validate_journal(root: &Path, dir: &Path, expected_name: &str) -> Result<IngestJournal> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("unsafe ingest journal directory");
    }
    contained_existing(root, dir)?;
    let journal_path = dir.join("journal.json");
    let mut journal_bytes = Vec::new();
    open_regular_nofollow(&journal_path)?.read_to_end(&mut journal_bytes)?;
    let journal: IngestJournal = serde_json::from_slice(&journal_bytes)?;
    if journal.journal_version != INGEST_JOURNAL_VERSION
        || journal.schema_version != SCHEMA_VERSION
        || journal_key_name(&journal.ingest_key)? != expected_name
        || !valid_sha256(&journal.input_sha256)
        || !valid_source_id(&journal.source_id)
        || !safe_component(&journal.capture_id, "capture_")
        || !valid_item_id(&journal.item_id)
        || !valid_revision_id(&journal.revision_id)
        || !safe_component(&journal.activity_id, "activity_")
        || journal.receipt_path != format!("receipts/{}.json", journal.capture_id)
        || journal.receipt.schema_version != SCHEMA_VERSION
        || journal.receipt.capture_id != journal.capture_id
        || journal.receipt.item_ids != [journal.item_id.clone()]
        || journal.receipt.revision_ids != [journal.revision_id.clone()]
        || journal.receipt.evidence_ids != journal.evidence_ids
        || journal.evidence_ids.is_empty()
        || journal
            .evidence_ids
            .iter()
            .any(|id| !safe_component(id, "evidence_"))
        || journal.evidence_ids.iter().collect::<BTreeSet<_>>().len() != journal.evidence_ids.len()
        || journal.receipt.idempotent_replay
        || journal.directories != [format!("items/{}/attachments", journal.item_id)]
        || journal.entries.is_empty()
    {
        anyhow::bail!("malformed ingest journal");
    }
    for directory in &journal.directories {
        let relative = Path::new(directory);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            anyhow::bail!("unsafe ingest journal intended directory");
        }
    }
    if journal.state == JournalState::Prepared {
        let entries = dir.join("entries");
        let metadata = fs::symlink_metadata(&entries)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!("unsafe ingest journal staging directory");
        }
        let canonical = entries.canonicalize()?;
        if !canonical.starts_with(dir.canonicalize()?) {
            anyhow::bail!("unsafe ingest journal staging directory");
        }
    }
    let receipt_bytes = serde_json::to_string_pretty(&journal.receipt)?.into_bytes();
    let receipt_digest = format!("sha256:{:x}", Sha256::digest(&receipt_bytes));
    let mut paths = BTreeSet::new();
    let mut graph_bytes = BTreeMap::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        let expected_staged = format!("entries/{index:04}");
        if entry.staged != expected_staged
            || entry.byte_size > usize::MAX as u64
            || !valid_sha256(&entry.sha256)
            || !paths.insert(entry.path.clone())
        {
            anyhow::bail!("malformed ingest journal entry");
        }
        let relative = Path::new(&entry.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !journal_entry_path_allowed(&journal, &entry.path)
        {
            anyhow::bail!("unsafe ingest journal intended path");
        }
        safe_receipt_path(root, &entry.path)?;
        if entry.path == journal.receipt_path
            && (entry.byte_size != receipt_bytes.len() as u64 || entry.sha256 != receipt_digest)
        {
            anyhow::bail!("ingest journal receipt mismatch");
        }
        if journal.state == JournalState::Prepared {
            let staged = dir.join(&entry.staged);
            let mut file = open_regular_nofollow(&staged)?;
            if file.metadata()?.len() != entry.byte_size {
                anyhow::bail!("ingest journal staged size mismatch");
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if format!("sha256:{:x}", Sha256::digest(&bytes)) != entry.sha256 {
                anyhow::bail!("ingest journal staged digest mismatch");
            }
            validate_journal_entry_bytes(&journal, entry, &bytes)?;
            graph_bytes.insert(entry.path.clone(), bytes);
        }
    }
    for required in [
        journal.receipt_path.clone(),
        format!("captures/{}/capture.json", journal.capture_id),
        format!("items/{}/item.json", journal.item_id),
        format!(
            "items/{}/revisions/{}.json",
            journal.item_id, journal.revision_id
        ),
        format!("items/{}/links.json", journal.item_id),
        format!("items/{}/index.md", journal.item_id),
        format!("items/{}/index.json", journal.item_id),
        format!("integrity/items/{}.json", journal.item_id),
        format!("sources/{}/source.json", journal.source_id),
    ] {
        if !paths.contains(&required) {
            anyhow::bail!("incomplete ingest journal");
        }
    }
    for evidence_id in &journal.evidence_ids {
        if !paths.contains(&format!("evidence/{evidence_id}/evidence.json")) {
            anyhow::bail!("incomplete ingest journal");
        }
    }
    if journal.state == JournalState::Committed {
        for entry in &journal.entries {
            let mut canonical = Vec::new();
            match open_regular_nofollow(&root.join(&entry.path)) {
                Ok(mut file) => file.read_to_end(&mut canonical)?,
                Err(_) if journal_entry_is_derived(&journal, &entry.path) => continue,
                Err(error) => return Err(error),
            };
            if canonical.len() as u64 != entry.byte_size
                || format!("sha256:{:x}", Sha256::digest(&canonical)) != entry.sha256
            {
                if journal_entry_is_derived(&journal, &entry.path) {
                    continue;
                }
                anyhow::bail!("committed ingest journal entry mismatch");
            }
            validate_journal_entry_bytes(&journal, entry, &canonical)?;
            graph_bytes.insert(entry.path.clone(), canonical);
        }
        let receipt_path = contained_existing(root, &root.join(&journal.receipt_path))?;
        verify_binary(&receipt_path, &receipt_bytes)?;
    }
    let revision_path = format!(
        "items/{}/revisions/{}.json",
        journal.item_id, journal.revision_id
    );
    let revision: ItemRevision = serde_json::from_slice(
        graph_bytes
            .get(&revision_path)
            .context("incomplete ingest journal revision")?,
    )?;
    let mut expected_paths = BTreeSet::from([
        journal.receipt_path.clone(),
        format!("captures/{}/capture.json", journal.capture_id),
        format!("items/{}/item.json", journal.item_id),
        revision_path,
        format!("items/{}/links.json", journal.item_id),
        format!("items/{}/index.md", journal.item_id),
        format!("items/{}/index.json", journal.item_id),
        format!("integrity/items/{}.json", journal.item_id),
        format!("sources/{}/source.json", journal.source_id),
    ]);
    expected_paths.extend(
        journal
            .evidence_ids
            .iter()
            .map(|id| format!("evidence/{id}/evidence.json")),
    );
    for attachment in &revision.attachments {
        if let AttachmentRef::Evidence { relative_path, .. } = attachment {
            expected_paths.insert(format!("items/{}/{}", journal.item_id, relative_path));
        }
    }
    if paths != expected_paths {
        anyhow::bail!("ingest journal path set mismatch");
    }
    validate_journal_graph(&journal, &graph_bytes)?;
    Ok(journal)
}

fn journal_entry_path_allowed(journal: &IngestJournal, path: &str) -> bool {
    let fixed = [
        journal.receipt_path.clone(),
        format!("sources/{}/source.json", journal.source_id),
        format!("captures/{}/capture.json", journal.capture_id),
        format!("items/{}/item.json", journal.item_id),
        format!(
            "items/{}/revisions/{}.json",
            journal.item_id, journal.revision_id
        ),
        format!("items/{}/links.json", journal.item_id),
        format!("items/{}/index.md", journal.item_id),
        format!("items/{}/index.json", journal.item_id),
        format!("integrity/items/{}.json", journal.item_id),
    ];
    if fixed.iter().any(|expected| expected == path)
        || journal
            .evidence_ids
            .iter()
            .any(|id| path == format!("evidence/{id}/evidence.json"))
    {
        return true;
    }
    let Some(name) = path.strip_prefix(&format!("items/{}/attachments/", journal.item_id)) else {
        return false;
    };
    name.starts_with("sha256-")
        && name.rsplit_once('.').is_some_and(|(stem, extension)| {
            stem.len() == 71
                && stem[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && matches!(extension, "gif" | "jpg" | "png" | "webp")
        })
}

fn valid_journal_replace_temporary(name: &str) -> bool {
    name.strip_prefix(".journal-write_")
        .and_then(|value| value.strip_suffix(".tmp"))
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn cleanup_journal_staging(dir: &Path) -> Result<()> {
    cleanup_journal_staging_with_sync(dir, sync_dir)
}

fn cleanup_journal_staging_with_sync(dir: &Path, sync: fn(&Path) -> Result<()>) -> Result<()> {
    let entries = dir.join("entries");
    match fs::symlink_metadata(&entries) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe ingest journal staging directory")
        }
        Ok(_) => fs::remove_dir_all(&entries)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == "journal.json" {
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                anyhow::bail!("unsafe ingest journal state file");
            }
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unexpected ingest journal staging entry"))?;
        if !valid_journal_replace_temporary(&name) {
            anyhow::bail!("unexpected ingest journal staging entry");
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!("unsafe ingest journal replacement temporary");
        }
        drop(open_regular_nofollow(&entry.path())?);
        fs::remove_file(entry.path())?;
    }
    sync(dir)
}

fn remove_tree_nofollow(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("unsafe abandoned ingest preparation");
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(entry.path())?;
        } else if metadata.is_dir() {
            remove_tree_nofollow(&entry.path())?;
        } else {
            anyhow::bail!("unsafe abandoned ingest preparation entry");
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

fn valid_abandoned_prepare_name(name: &str) -> bool {
    name.strip_prefix(".prepare-journal_").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn apply_journal(root: &Path, dir: &Path, journal: &mut IngestJournal) -> Result<()> {
    apply_journal_files(root, dir, journal)?;
    let mut keys = catalog_keys(root)?;
    match keys.get(&journal.ingest_key) {
        Some(path) if path != &journal.receipt_path => anyhow::bail!("ingest catalog conflict"),
        _ => {
            keys.insert(journal.ingest_key.clone(), journal.receipt_path.clone());
        }
    }
    durable_replace(
        root,
        &root.join("catalog/ingest-keys.json"),
        serde_json::to_string_pretty(&keys)?.as_bytes(),
    )?;
    finish_journal(root, dir, journal)
}

fn apply_journal_files(root: &Path, dir: &Path, journal: &IngestJournal) -> Result<()> {
    ingest_apply_step(0)?;
    for directory in &journal.directories {
        ensure_contained_dir(root, Path::new(directory))?;
        sync_dir(
            root.join(directory)
                .parent()
                .context("journal directory has no parent")?,
        )?;
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        let mut bytes = Vec::with_capacity(entry.byte_size as usize);
        open_regular_nofollow(&dir.join(&entry.staged))?.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != entry.byte_size
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != entry.sha256
        {
            anyhow::bail!("ingest journal staged content changed");
        }
        let output = root.join(&entry.path);
        install_binary(root, &output, &bytes)?;
        sync_dir(output.parent().context("journal output has no parent")?)?;
        ingest_apply_step(index + 1)?;
    }
    Ok(())
}

fn finish_journal(root: &Path, dir: &Path, journal: &mut IngestJournal) -> Result<()> {
    ingest_apply_step(journal.entries.len() + 1)?;
    journal.state = JournalState::Committed;
    durable_replace(
        root,
        &dir.join("journal.json"),
        serde_json::to_string_pretty(journal)?.as_bytes(),
    )?;
    ingest_apply_step(journal.entries.len() + 2)?;
    cleanup_journal_staging(dir)?;
    archive_committed_journal(root, dir)
}

fn recover_journals_locked(root: &Path) -> Result<usize> {
    let parent = ensure_contained_dir(root, Path::new("catalog/ingest-journal"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            if !valid_abandoned_prepare_name(&name) {
                anyhow::bail!("unexpected hidden ingest journal entry");
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!("unsafe abandoned ingest preparation");
            }
            let canonical = entry.path().canonicalize()?;
            if canonical.parent() != Some(parent.as_path()) {
                anyhow::bail!("abandoned ingest preparation escapes owner");
            }
            remove_tree_nofollow(&canonical)?;
            sync_dir(&parent)?;
            continue;
        }
        dirs.push((name, entry.path()));
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    // Completed ingests have immutable, item-disjoint outputs. Recover them in
    // bounded parallel groups while the caller STILL holds the Store lock.
    // Prepared plans retain the original ordered path below. This preflight is
    // only a scheduling decision: each worker repeats full graph validation,
    // projection repair and the durability barrier, including no-op cleanup.
    let mut item_ids = BTreeSet::new();
    let completed_disjoint = dirs.len() > 1
        && dirs.iter().all(|(_, dir)| {
            load::<IngestJournal>(root, dir.join("journal.json")).is_ok_and(|journal| {
                journal.state == JournalState::Committed && item_ids.insert(journal.item_id)
            })
        });
    if completed_disjoint {
        ensure_contained_dir(root, Path::new(COMMITTED_JOURNALS))?;
        let catalog = catalog_keys(root)?;
        std::thread::scope(|scope| -> Result<()> {
            let mut workers = Vec::new();
            for group in dirs.chunks(dirs.len().div_ceil(4)) {
                let catalog = &catalog;
                workers.push(scope.spawn(move || -> Result<()> {
                    for (name, dir) in group {
                        let journal = validate_journal(root, dir, name)?;
                        if journal.state != JournalState::Committed {
                            anyhow::bail!("ingest journal changed during recovery");
                        }
                        recover_committed_journal(root, dir, &journal, catalog)?;
                    }
                    Ok(())
                }));
            }
            // Joining the entire scope also on errors keeps every repair under
            // ingest_lock; no worker survives into a caller's snapshot/writer.
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("ingest recovery worker failed"))??;
            }
            Ok(())
        })?;
        return Ok(0);
    }
    let mut recovered = 0;
    // The caller holds ingest_lock, so a catalog read can be reused until this
    // recovery pass itself commits a prepared ingest. Old writers use that lock
    // too; no new generation marker is assumed here.
    let mut catalog = None;
    for (name, dir) in dirs {
        let mut journal = validate_journal(root, &dir, &name)?;
        match journal.state {
            JournalState::Prepared => {
                apply_journal(root, &dir, &mut journal)?;
                catalog = None;
                recovered += 1;
            }
            JournalState::Committed => {
                let keys = match &catalog {
                    Some(keys) => keys,
                    None => catalog.insert(catalog_keys(root)?),
                };
                recover_committed_journal(root, &dir, &journal, keys)?;
            }
        }
    }
    Ok(recovered)
}

fn recover_committed_journal(
    root: &Path,
    dir: &Path,
    journal: &IngestJournal,
    keys: &BTreeMap<String, String>,
) -> Result<()> {
    if keys.get(&journal.ingest_key) != Some(&journal.receipt_path) {
        anyhow::bail!("committed ingest journal is absent from catalog");
    }
    let (item, revision) = read_item_without_integrity(root, &journal.item_id)?;
    let manifest = integrity_manifest(&item, &revision)?;
    repair_derived_file(
        root,
        &root
            .join("integrity/items")
            .join(format!("{}.json", item.item_id)),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    install_item_projections(root, &item, &revision)?;
    cleanup_journal_staging(dir)?;
    archive_committed_journal(root, dir)
}

fn open_ingest_lock(root: &Path) -> Result<fs::File> {
    let catalog_dir = ensure_contained_dir(root, Path::new("catalog"))?;
    let lock_path = catalog_dir.join(".ingest.lock");
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            anyhow::bail!("unsafe catalog lock");
        }
        contained_existing(root, &lock_path)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(&lock_path)?;
    if !lock.metadata()?.is_file() {
        anyhow::bail!("unsafe catalog lock");
    }
    Ok(lock)
}

/// The exclusive Store writer lock. Releasing it (explicitly or on drop, including error and
/// unwind paths) first replaces `catalog/generation` with a fresh token, so a reader that next
/// takes the shared lock can tell whether any writer ran since its cached view.
pub(crate) struct WriterLock {
    file: Option<fs::File>,
    root: PathBuf,
}

impl WriterLock {
    pub(crate) fn unlock(mut self) -> Result<()> {
        self.release()
    }

    fn release(&mut self) -> Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        let bumped = bump_generation(&self.root);
        file.unlock()?;
        bumped
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn ingest_lock(root: &Path) -> Result<WriterLock> {
    let lock = open_ingest_lock(root)?;
    lock.lock_exclusive()?;
    Ok(WriterLock {
        file: Some(lock),
        root: root.to_path_buf(),
    })
}

/// The exclusive lock for callers that only read (dry runs): no generation bump, so a preview
/// leaves the library byte-identical. Must not be used around any write or recovery.
pub(crate) fn exclusive_read_lock(root: &Path) -> Result<fs::File> {
    let lock = open_ingest_lock(root)?;
    lock.lock_exclusive()?;
    Ok(lock)
}

pub(crate) const GENERATION_FILE: &str = "catalog/generation";
const RECOVERY_MARKER: &str = "catalog/recovery-pending";
const COMMITTED_JOURNALS: &str = "catalog/ingest-journal-committed";

/// Written under the exclusive lock only, so no reader observes it mid-write. Not synced: it is
/// a cache-invalidation token, and a fresh process has no cache to invalidate.
fn bump_generation(root: &Path) -> Result<()> {
    let path = root.join(GENERATION_FILE);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!("unsafe catalog generation");
        }
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(&path)?
        .write_all(new_id("generation").as_bytes())?;
    Ok(())
}

/// The token of the last Store write (`None` if no write was recorded since this layout).
/// Callers read it inside `with_consistent_snapshot`.
pub fn catalog_generation(root: &Path) -> Result<Option<String>> {
    let path = root.join(GENERATION_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            let mut text = String::new();
            open_regular_nofollow(&path)?
                .take(256)
                .read_to_string(&mut text)?;
            Ok(Some(text))
        }
        Ok(_) => anyhow::bail!("unsafe catalog generation"),
        Err(error) => Err(error.into()),
    }
}

/// A writer sets this durable marker before it publishes an ingest or item-update journal and
/// clears it after the journal is fully applied. A marker left by an interrupted or failed write
/// makes the next reader or writer run journal recovery; otherwise reads skip it entirely.
fn recovery_pending(root: &Path) -> Result<bool> {
    for directory in ["catalog/ingest-journal", "catalog/item-update-journal"] {
        match fs::symlink_metadata(root.join(directory)) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                anyhow::bail!("unsafe journal directory")
            }
            _ => {}
        }
    }
    match fs::symlink_metadata(root.join(RECOVERY_MARKER)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(true),
        Ok(_) => anyhow::bail!("unsafe recovery marker"),
        Err(error) => Err(error.into()),
    }
}

fn mark_recovery_pending(root: &Path) -> Result<()> {
    if recovery_pending(root)? {
        return Ok(());
    }
    let catalog = ensure_contained_dir(root, Path::new("catalog"))?;
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(root.join(RECOVERY_MARKER))?.sync_all()?;
    sync_dir(&catalog)
}

fn clear_recovery_pending(root: &Path) -> Result<()> {
    if !recovery_pending(root)? {
        return Ok(());
    }
    fs::remove_file(root.join(RECOVERY_MARKER))?;
    sync_dir(&root.join("catalog"))
}

/// Under the exclusive lock: recovers journals only when a write left the marker behind.
pub(crate) fn recover_if_pending_locked(root: &Path) -> Result<()> {
    if recovery_pending(root)? {
        recover_journals_locked(root)?;
        recover_item_updates_locked(root)?;
        clear_recovery_pending(root)?;
    }
    Ok(())
}

/// Moves a fully applied ingest journal (metadata only; staging already removed) out of the
/// recovery directory. Kept for audit; never read again on the request path.
fn archive_committed_journal(root: &Path, dir: &Path) -> Result<()> {
    archive_committed_journal_with_sync(root, dir, sync_dir)
}

fn archive_committed_journal_with_sync(
    root: &Path, dir: &Path, sync: fn(&Path) -> Result<()>,
) -> Result<()> {
    let archive = ensure_contained_dir_with_sync(root, Path::new(COMMITTED_JOURNALS), sync)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o700))?;
    }
    let name = dir.file_name().context("ingest journal has no name")?;
    let target = archive.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        anyhow::bail!("committed ingest journal already archived");
    }
    fs::rename(dir, &target)?;
    sync(&archive)?;
    sync(dir.parent().context("ingest journal has no parent")?)
}

/// Runs a read-only callback against one consistent Store snapshot.
///
/// The exclusive `ingest_lock` is the Store's commit authority; readers hold it
/// shared, so they never observe a half-applied write and never wait on another
/// reader. Journal recovery runs at startup (`recover_pending_ingests`) and, if
/// an interrupted or failed write left the recovery marker, before the callback
/// under the exclusive lock. The callback must not
/// re-enter ingest, metadata revision, or recovery, and must not perform model,
/// external-I/O, or other long-running work. The lock covers Store state only;
/// it does not lock Analysis and is not a cross-system transaction.
pub fn with_consistent_snapshot<T>(
    library: impl AsRef<Path>,
    read: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let input = library.as_ref();
    let metadata = fs::symlink_metadata(input)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("library root is not a safe directory");
    }
    let root = existing_canonical_root(input)?;
    // Keep the existing file-backed lock guard in scope through recovery and
    // the callback. Dropping it releases the lock on success, callback error,
    // and panic unwind without changing the callback's result.
    let lock = open_ingest_lock(&root)?;
    lock.lock_shared()?;
    if recovery_pending(&root)? {
        // An interrupted or failed write: recover under the exclusive lock, then read under it.
        lock.unlock()?;
        let writer = ingest_lock(&root)?;
        recover_if_pending_locked(&root)?;
        let value = read(&root);
        writer.unlock()?;
        return value;
    }
    let value = read(&root);
    lock.unlock()?;
    value
}

fn item_update_parent(root: &Path) -> Result<PathBuf> {
    let parent = ensure_contained_dir(root, Path::new("catalog/item-update-journal"))?;
    #[cfg(unix)]
    fs::set_permissions(&parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(parent)
}

fn item_current_path(root: &Path, item_id: &str) -> PathBuf {
    root.join("items").join(item_id).join("current.json")
}

fn load_current_pointer(
    root: &Path,
    item_id: &str,
) -> Result<Option<(ItemCurrentPointer, ItemRevision)>> {
    let path = item_current_path(root, item_id);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("unsafe item current pointer")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let pointer: ItemCurrentPointer = load(root, path)?;
    if pointer.schema_version != SCHEMA_VERSION
        || pointer.item.schema_version != SCHEMA_VERSION
        || pointer.item.item_id != item_id
        || !valid_revision_id(&pointer.item.current_revision)
        || !valid_sha256(&pointer.revision_sha256)
    {
        anyhow::bail!("invalid item current pointer")
    }
    let revision: ItemRevision = load(
        root,
        root.join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{}.json", pointer.item.current_revision)),
    )?;
    if revision.schema_version != SCHEMA_VERSION
        || revision.item_id != item_id
        || revision.revision_id != pointer.item.current_revision
        || revision.sensitivity != pointer.item.sensitivity
        || pointer.revision_sha256
            != format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&revision)?)
            )
    {
        anyhow::bail!("invalid item current pointer")
    }
    verified_owner_edit_chain(root, &pointer, &revision)?;
    Ok(Some((pointer, revision)))
}

type FileStamp = (u64, u64, u64, i64, i64, i64, i64);

#[derive(Clone, Default)]
struct VerifiedReads {
    root: PathBuf,
    resolutions: BTreeMap<PathBuf, PathBuf>,
    files: BTreeMap<PathBuf, FileStamp>,
    complete: bool,
}

thread_local! {
    // Nested projection validations contribute to their enclosing chain validation.
    static VERIFYING_READS: std::cell::RefCell<Vec<VerifiedReads>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn file_stamp(m: &fs::Metadata) -> FileStamp {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec())
}

fn record_verified_file(path: &Path, file: &fs::File) -> Result<()> {
    VERIFYING_READS.with(|active| {
        let mut active = active.borrow_mut();
        if active.is_empty() { return Ok(()); }
        let stamp = file_stamp(&file.metadata()?);
        for reads in active.iter_mut() {
            if !reads.complete { continue; }
            if !path.starts_with(&reads.root) { reads.complete = false; }
            if let Some(old) = reads.files.insert(path.to_path_buf(), stamp) {
                if old != stamp { reads.complete = false; }
            }
            reads.enforce_limit();
        }
        Ok(())
    })
}

impl VerifiedReads {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.root.as_os_str().len()
            + self.resolutions.iter().map(|(p, t)| p.as_os_str().len() + t.as_os_str().len() + 128).sum::<usize>()
            + self.files.keys().map(|p| p.as_os_str().len() + 128).sum::<usize>()
    }

    fn enforce_limit(&mut self) {
        if self.bytes() > 1024 * 1024 {
            self.complete = false;
            self.resolutions.clear();
            self.files.clear();
        }
    }

    fn unchanged(&self) -> bool {
        // Resolve original paths again, not just canonical targets: moving a parent
        // outside the root and replacing it with a symlink must invalidate the hit.
        self.complete && self.resolutions.iter().all(|(path, target)| {
            contained_existing(&self.root, path).is_ok_and(|p| p == *target)
        }) && self.files.iter().all(|(path, stamp)| {
            contained_existing(&self.root, path).and_then(|p| open_regular_nofollow(&p))
                .and_then(|f| Ok(file_stamp(&f.metadata()?)))
                .is_ok_and(|current| current == *stamp)
        })
    }
}

/// Record actual safe opens rather than a second hand-maintained list of validator
/// dependencies. The guard also unwinds on errors/panics, and inner cache hits are
/// reopened above so that their dependencies reach every enclosing recording.
struct VerifyReads;
impl VerifyReads {
    fn begin(root: &Path) -> Self {
        VERIFYING_READS.with(|active| active.borrow_mut().push(VerifiedReads {
            root: root.to_path_buf(), complete: true, ..Default::default()
        }));
        Self
    }
    fn finish(self) -> VerifiedReads {
        let reads = VERIFYING_READS.with(|active| {
            let mut reads = active.borrow().last().unwrap().clone();
            // A resolved regular file that did not use the recorded open helper is
            // an unsupported read path; never cache such a partial verification.
            reads.complete &= reads.resolutions.values().all(|target| {
                fs::metadata(target).is_ok_and(|m| m.is_dir() || reads.files.contains_key(target))
            });
            reads
        });
        reads
    }
}
impl Drop for VerifyReads {
    fn drop(&mut self) {
        VERIFYING_READS.with(|active| { active.borrow_mut().pop(); });
    }
}

/// Owner edit chains already proven in this process, keyed by the library root, the
/// item header digest and the current revision digest, and valid only while the files
/// the proof actually opened are unchanged. Without this every
/// reader chunk re-read and re-hashed the item's whole revision history.
struct VerifiedChain {
    key: (PathBuf, String, String),
    reads: VerifiedReads,
}
static VERIFIED_CHAINS: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<VerifiedChain>>> =
    std::sync::OnceLock::new();

fn verified_owner_edit_chain(root: &Path, pointer: &ItemCurrentPointer, revision: &ItemRevision) -> Result<()> {
    let key = (
        root.to_path_buf(),
        format!("{:x}", Sha256::digest(serde_json::to_vec(&pointer.item)?)),
        pointer.revision_sha256.clone(),
    );
    let cache = VERIFIED_CHAINS.get_or_init(Default::default);
    let cached = {
        let cache = cache.lock().map_err(|_| anyhow::anyhow!("invalid owner edit chain"))?;
        cache.iter().find(|c| c.key == key).map(|c| c.reads.clone())
    };
    if cached.is_some_and(|reads| reads.unchanged()) {
        return Ok(());
    }
    let recording = VerifyReads::begin(root);
    validate_owner_edit_chain(root, &pointer.item, revision)?;
    let reads = recording.finish();
    if !reads.complete { return Ok(()); }
    let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("invalid owner edit chain"))?;
    cache.retain(|c| c.key != key);
    while cache.len() >= 1024 || cache.iter().map(|c| c.reads.bytes()).sum::<usize>() + reads.bytes() > 16 * 1024 * 1024 {
        cache.pop_front();
    }
    cache.push_back(VerifiedChain { key, reads });
    Ok(())
}

fn revision_digest(revision: &ItemRevision) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(revision)?)
    ))
}

const LEGACY_EDIT_ACTIVITY: &str = "metadata_edit";
const BIBLIOGRAPHIC_EDIT_ACTIVITY: &str = "bibliographic_metadata_edit";
const MODEL_SUGGESTION_ACTIVITY: &str = "model_metadata_suggestion";

fn is_model_provenance(value: &MetadataProvenance) -> bool {
    value.origin == MetadataOrigin::ModelSuggested
        && valid_text(&value.provider_id, 128)
        && valid_text(&value.provider_version, 128)
        && value.source_refs.is_empty()
}

/// A model suggestion may only fill keywords, creators and the publication date where the base
/// revision had no value or an earlier model value; owner, intake and lookup values and every
/// other field are unchanged, and nothing is removed.
fn model_fill_transition_ok(base: &ItemMetadata, next: &ItemMetadata) -> bool {
    let mut rest = next.clone();
    macro_rules! fill {
        ($($field:ident),*) => {$(
            if next.$field != base.$field
                && (base
                    .$field
                    .as_ref()
                    .is_some_and(|value| !is_model_provenance(&value.provenance))
                    || next
                        .$field
                        .as_ref()
                        .is_none_or(|value| !is_model_provenance(&value.provenance)))
            {
                return false;
            }
            rest.$field = base.$field.clone();
        )*};
    }
    fill!(keywords, creators, published_date, published_partial_date);
    rest == *base
}

fn owner_edit_config_digest(activity_type: &str) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(format!("memex.desktop:1:{activity_type}").as_bytes())
    )
}

fn owner_provenance() -> MetadataProvenance {
    MetadataProvenance {
        origin: MetadataOrigin::OwnerEdit,
        provider_id: "memex.desktop".into(),
        provider_version: "1".into(),
        source_refs: Vec::new(),
    }
}

/// Applies `$body` to every bibliographic field a v2 edit may change. Title,
/// manual_tags and content_rights are deliberately absent.
macro_rules! for_each_bibliographic_field {
    ($body:ident) => {
        $body!(
            creators,
            identifiers,
            published_date,
            published_partial_date,
            language,
            site_name,
            abstract_text,
            journal_title,
            publisher,
            volume,
            issue,
            pages,
            publication_place,
            edition,
            number_of_pages,
            series_title,
            series_number,
            keywords,
            institutions,
            cited_references
        )
    };
}

/// A bibliographic edit may only change editable fields, each to absent or an
/// owner-provenance value; everything else must equal the base revision.
fn bibliographic_transition_ok(
    base: &ItemMetadata,
    next: &ItemMetadata,
    allowed: impl Fn(&MetadataProvenance) -> bool,
) -> bool {
    let mut rest = next.clone();
    macro_rules! check {
        ($($field:ident),*) => {$(
            if next.$field != base.$field
                && next
                    .$field
                    .as_ref()
                    .is_some_and(|value| !allowed(&value.provenance))
            {
                return false;
            }
            rest.$field = base.$field.clone();
        )*};
    }
    for_each_bibliographic_field!(check);
    rest == *base
}

fn validate_owner_edit_transition(base: &ItemRevision, revision: &ItemRevision) -> Result<()> {
    let activity = revision
        .provenance
        .last()
        .context("owner edit revision has no activity")?;
    if activity.activity_type == reconversion::ACTIVITY {
        return reconversion::validate_transition(base, revision);
    }
    if activity.activity_type == reader::repair::ACTIVITY {
        return reader::repair::validate_transition(base, revision);
    }
    if activity.activity_type == reader::chapter_edit::ACTIVITY {
        return reader::chapter_edit::validate_transition(base, revision);
    }
    if matches!(activity.activity_type.as_str(), reader::content_format::ACTIVITY | reader::content_format::RESTORE_ACTIVITY) {
        return reader::content_format::validate_transition(base, revision);
    }
    if activity.activity_type == reading::ACTIVITY {
        return reading::validate_transition(base, revision);
    }
    let metadata_ok = match activity.activity_type.as_str() {
        podcast_metadata::ACTIVITY => podcast_metadata::transition_ok(base, revision),
        LEGACY_EDIT_ACTIVITY => {
            let mut expected_metadata = base.metadata.clone();
            expected_metadata.manual_tags = revision.metadata.manual_tags.clone();
            revision.metadata == expected_metadata
        }
        BIBLIOGRAPHIC_EDIT_ACTIVITY => {
            revision.title == base.title
                && activity.input_ids.len() == 1
                && activity.input_checksums.len() == 1
                && bibliographic_transition_ok(
                    &base.metadata,
                    &revision.metadata,
                    is_owner_provenance,
                )
                && validate_metadata(&revision.metadata).is_ok()
        }
        MODEL_SUGGESTION_ACTIVITY => {
            revision.title == base.title
                && model_fill_transition_ok(&base.metadata, &revision.metadata)
                && validate_metadata(&revision.metadata).is_ok()
        }
        lookup::LOOKUP_APPLY_ACTIVITY => {
            // input_ids/checksums = [base revision, preview] / [base digest, response].
            match (
                activity.input_ids.as_slice(),
                activity.input_checksums.as_slice(),
            ) {
                ([_, preview_id], [_, response_sha256])
                    if safe_component(preview_id, "preview_") && valid_sha256(response_sha256) =>
                {
                    bibliographic_transition_ok(&base.metadata, &revision.metadata, |p| {
                        is_owner_provenance(p)
                            || lookup::is_lookup_provenance(p, preview_id, response_sha256)
                    }) && validate_metadata(&revision.metadata).is_ok()
                }
                _ => false,
            }
        }
        _ => false,
    };
    let single_input = !matches!(activity.activity_type.as_str(), lookup::LOOKUP_APPLY_ACTIVITY | podcast_metadata::ACTIVITY);
    if !metadata_ok
        || revision.revision_number
            != base
                .revision_number
                .checked_add(1)
                .context("revision overflow")?
        || revision.item_id != base.item_id
        || revision.source_url != base.source_url
        || revision.canonical_url != base.canonical_url
        || revision.content_markdown != base.content_markdown
        || revision.sensitivity != base.sensitivity
        || revision.evidence_ids != base.evidence_ids
        || revision.attachments != base.attachments
        || revision.provenance.len() != base.provenance.len() + 1
        || revision.provenance[..base.provenance.len()] != base.provenance
        || activity.schema_version != SCHEMA_VERSION
        || !safe_component(&activity.activity_id, "activity_")
        || chrono::DateTime::parse_from_rfc3339(&activity.occurred_at).is_err()
        || activity.tool_id != "memex.desktop"
        || activity.tool_version != "1"
        || activity.config_digest != owner_edit_config_digest(&activity.activity_type)
        || activity.input_ids.first() != Some(&base.revision_id)
        || activity.input_checksums.first() != Some(&revision_digest(base)?)
        || (single_input && (activity.input_ids.len() != 1 || activity.input_checksums.len() != 1))
        || activity.output_ids != [revision.revision_id.clone()]
        || !valid_text(&revision.title, 4096)
        || (revision.metadata.manual_tags != base.metadata.manual_tags
            && revision.metadata.manual_tags.as_ref().is_some_and(|tags| {
                tags.provenance.origin != MetadataOrigin::OwnerEdit
                    || tags.provenance.provider_id != "memex.desktop"
                    || tags.provenance.provider_version != "1"
                    || tags.value.is_empty()
                    || tags.value.len() > 256
                    || !strictly_sorted(&tags.value)
                    || tags.value.iter().any(|tag| !valid_text(tag, 4096))
            }))
    {
        anyhow::bail!("invalid owner edit revision")
    }
    Ok(())
}

fn validate_owner_edit_chain(root: &Path, item: &Item, revision: &ItemRevision) -> Result<()> {
    let (legacy_item, legacy_revision) = read_legacy_item(root, &item.item_id)?;
    if item.sensitivity != legacy_item.sensitivity
        || item.added_at != legacy_item.added_at
        || revision.revision_number <= legacy_revision.revision_number
        || revision.revision_number - legacy_revision.revision_number > MAX_ITEM_REVISION_CHAIN
    {
        anyhow::bail!("invalid owner edit chain")
    }
    let mut current = revision.clone();
    let mut reconverted = false;
    while current.revision_id != legacy_revision.revision_id {
        let base_id = current
            .provenance
            .last()
            .and_then(|activity| activity.input_ids.first())
            .filter(|value| valid_revision_id(value))
            .context("invalid owner edit chain")?;
        let base: ItemRevision = load(
            root,
            root.join("items")
                .join(&item.item_id)
                .join("revisions")
                .join(format!("{base_id}.json")),
        )?;
        validate_owner_edit_transition(&base, &current)?;
        if current.provenance.last().is_some_and(|a| a.activity_type == podcast_metadata::ACTIVITY) {
            podcast_metadata::validate_source(root, &base, &current)?;
        }
        if current.provenance.last().is_some_and(|a| a.activity_type == reconversion::ACTIVITY) {
            reconversion::validate_evidence(root, &current)?;
            reconverted = true;
        }
        if current.provenance.last().is_some_and(|a| a.activity_type == reader::repair::ACTIVITY) {
            reader::repair::validate_evidence(root, &base, &current)?;
        }
        if current.provenance.last().is_some_and(|a| a.activity_type == reader::chapter_edit::ACTIVITY) {
            reader::chapter_edit::validate_evidence(root, &base, &current)?;
        }
        if current.provenance.last().is_some_and(|a| matches!(a.activity_type.as_str(), reader::content_format::ACTIVITY | reader::content_format::RESTORE_ACTIVITY)) {
            reader::content_format::validate_evidence(root, &base, &current)?;
        }
        current = base;
    }
    let expected_type = if reconverted { "document" } else { &legacy_item.item_type };
    if current != legacy_revision || item.item_type != expected_type {
        anyhow::bail!("invalid owner edit chain")
    }
    Ok(())
}

fn validate_item_update_journal(
    root: &Path,
    path: &Path,
    expected_item_id: &str,
) -> Result<(ItemUpdateJournal, ItemRevision)> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("unsafe item update journal")
    }
    let journal: ItemUpdateJournal = load(root, path.to_owned())?;
    if journal.journal_version != ITEM_UPDATE_JOURNAL_VERSION
        || journal.schema_version != SCHEMA_VERSION
        || journal.item_id != expected_item_id
        || journal.pointer.schema_version != SCHEMA_VERSION
        || journal.pointer.item.schema_version != SCHEMA_VERSION
        || journal.pointer.item.item_id != journal.item_id
        || journal.pointer.item.current_revision != journal.revision_id
        || !valid_revision_id(&journal.base_revision_id)
        || !valid_revision_id(&journal.revision_id)
        || journal.base_revision_id == journal.revision_id
        || !valid_sha256(&journal.pointer.revision_sha256)
    {
        anyhow::bail!("invalid item update journal")
    }
    let revision: ItemRevision = load(
        root,
        root.join("items")
            .join(&journal.item_id)
            .join("revisions")
            .join(format!("{}.json", journal.revision_id)),
    )?;
    if revision.schema_version != SCHEMA_VERSION
        || revision.item_id != journal.item_id
        || revision.revision_id != journal.revision_id
        || revision.sensitivity != journal.pointer.item.sensitivity
        || journal.pointer.revision_sha256
            != format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&revision)?)
            )
    {
        anyhow::bail!("invalid item update journal")
    }
    let base: ItemRevision = load(
        root,
        root.join("items")
            .join(&journal.item_id)
            .join("revisions")
            .join(format!("{}.json", journal.base_revision_id)),
    )?;
    validate_owner_edit_transition(&base, &revision)?;
    validate_owner_edit_chain(root, &journal.pointer.item, &revision)?;
    if let Some(request) = &journal.request {
        if request.schema_version != SCHEMA_VERSION
            || !valid_request_id(&request.request_id)
            || !valid_sha256(&request.request_sha256)
            || request.receipt
                != (ItemMetadataUpdateReceipt {
                    schema_version: SCHEMA_VERSION,
                    item_id: journal.item_id.clone(),
                    previous_revision_id: journal.base_revision_id.clone(),
                    revision_id: journal.revision_id.clone(),
                    revision_number: revision.revision_number,
                })
            || revision
                .provenance
                .last()
                .is_none_or(|activity| !matches!(activity.activity_type.as_str(), BIBLIOGRAPHIC_EDIT_ACTIVITY | podcast_metadata::ACTIVITY | reading::ACTIVITY | reader::chapter_edit::ACTIVITY | reader::content_format::ACTIVITY | reader::content_format::RESTORE_ACTIVITY))
        {
            anyhow::bail!("invalid item update journal")
        }
    }
    Ok((journal, revision))
}

fn reconcile_item_update(
    root: &Path,
    journal_path: &Path,
    journal: &ItemUpdateJournal,
    revision: &ItemRevision,
) -> Result<()> {
    let current = load_current_pointer(root, &journal.item_id)?;
    match current
        .as_ref()
        .map(|(pointer, _)| pointer.item.current_revision.as_str())
    {
        Some(current) if current == journal.revision_id => {
            reader::repair::install_index(root, revision)?;
            if let Some(request) = &journal.request {
                write_metadata_request(root, request)?;
            }
            fs::remove_file(journal_path)?;
            return sync_dir(
                journal_path
                    .parent()
                    .context("item update journal has no parent")?,
            );
        }
        Some(current) if current == journal.base_revision_id => {}
        Some(_) => anyhow::bail!("item update conflict"),
        None => {
            let (item, _) = read_legacy_item(root, &journal.item_id)?;
            if item.current_revision != journal.base_revision_id {
                anyhow::bail!("item update conflict")
            }
        }
    }
    install_item_revision_projections(root, &journal.pointer.item, revision)?;
    reader::repair::install_index(root, revision)?;
    item_update_step(2)?;
    durable_replace(
        root,
        &item_current_path(root, &journal.item_id),
        serde_json::to_string_pretty(&journal.pointer)?.as_bytes(),
    )?;
    item_update_step(3)?;
    if let Some(request) = &journal.request {
        write_metadata_request(root, request)?;
    }
    fs::remove_file(journal_path)?;
    item_update_step(4)?;
    sync_dir(
        journal_path
            .parent()
            .context("item update journal has no parent")?,
    )
}

fn recover_item_updates_locked(root: &Path) -> Result<usize> {
    let parent = root.join("catalog/item-update-journal");
    match fs::symlink_metadata(&parent) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe item update journal directory")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let parent = contained_existing(root, &parent)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&parent)? {
        if entries.len() == MAX_ITEM_UPDATE_JOURNALS {
            anyhow::bail!("item update journal limit exceeded")
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe item update journal"))?;
        let item_id = name
            .strip_suffix(".json")
            .filter(|value| valid_item_id(value))
            .ok_or_else(|| anyhow::anyhow!("unsafe item update journal"))?;
        entries.push((item_id.to_owned(), entry.path()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut recovered = 0;
    for (item_id, path) in entries {
        let (journal, revision) = validate_item_update_journal(root, &path, &item_id)?;
        reconcile_item_update(root, &path, &journal, &revision)?;
        recovered += 1;
    }
    Ok(recovered)
}

/// Completes any Store-owned prepared ingest plans under the same exclusive
/// lock used by `ingest`. It writes no new plan and allocates no identity.
pub fn recover_pending_ingests(library: impl AsRef<Path>) -> Result<usize> {
    let library = library.as_ref();
    match fs::symlink_metadata(library) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("library root is not a safe directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    let root = existing_canonical_root(library)?;
    let ingest_journals = match fs::symlink_metadata(root.join("catalog/ingest-journal")) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe ingest journal directory")
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let item_updates = match fs::symlink_metadata(root.join("catalog/item-update-journal")) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe item update journal directory")
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    // Taking the writer lock also records a generation token, which read caches rely on.
    if !ingest_journals
        && !item_updates
        && !recovery_pending(&root)?
        && (catalog_generation(&root)?.is_some() || !root.join("catalog").exists())
    {
        return Ok(0);
    }
    // Startup: always a full pass, so journals left by daemons that predate the recovery marker
    // are recovered, and committed journals they retained are archived once.
    let lock = ingest_lock(&root)?;
    let recovered = if ingest_journals {
        recover_journals_locked(&root)?
    } else {
        0
    } + recover_item_updates_locked(&root)?;
    clear_recovery_pending(&root)?;
    lock.unlock()?;
    Ok(recovered)
}

/// Owner-only receipt lookup for durable post-commit work. Does not grant item reads.
pub fn lookup_ingest_receipt(
    library: impl AsRef<Path>,
    adapter_id: &str,
    external_capture_id: &str,
    expected_input_sha256: Option<&str>,
) -> Result<Option<IngestReceipt>> {
    let root = canonical_root(library.as_ref())?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let key = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(adapter_id, external_capture_id))?)
    );
    let keys = catalog_keys(&root)?;
    let Some(path) = keys.get(&key) else {
        return Ok(None);
    };
    let receipt: IngestReceipt = load(&root, safe_receipt_path(&root, path)?)?;
    if receipt.schema_version != SCHEMA_VERSION
        || !safe_component(&receipt.capture_id, "capture_")
        || path != &format!("receipts/{}.json", receipt.capture_id)
        || receipt.item_ids.len() != 1
        || receipt.revision_ids.len() != 1
        || !valid_item_id(&receipt.item_ids[0])
        || !valid_revision_id(&receipt.revision_ids[0])
    {
        anyhow::bail!("invalid capture receipt")
    }
    let capture: CaptureRecord = load(
        &root,
        root.join("captures")
            .join(&receipt.capture_id)
            .join("capture.json"),
    )?;
    if capture.capture_id != receipt.capture_id
        || capture.adapter_id != adapter_id
        || capture.external_capture_id != external_capture_id
    {
        anyhow::bail!("invalid capture receipt relationship")
    }
    if expected_input_sha256
        .is_some_and(|expected| capture.ingest_input_sha256.as_deref() != Some(expected))
    {
        return Err(IngestConflict.into());
    }
    Ok(Some(receipt))
}

pub fn ingest(
    library: impl AsRef<Path>,
    capture: &VerifiedCapture,
    draft: impl Into<ItemDraft>,
) -> Result<IngestReceipt> {
    let draft = draft.into();
    validate_draft(&draft)?;
    let root = canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let keys = catalog_keys(&root)?;
    let prepared = prepare_ingest_locked(&root, capture, draft, &keys)?;
    let receipt = match prepared {
        PreparedIngest::Replay(receipt) => return Ok(receipt),
        PreparedIngest::Journal(path, mut journal) => {
            apply_journal(&root, &path, &mut journal)?;
            journal.receipt
        }
    };
    clear_recovery_pending(&root)?;
    lock.unlock()?;
    Ok(receipt)
}

enum PreparedIngest {
    Replay(IngestReceipt),
    Journal(PathBuf, IngestJournal),
}

enum IngestPlan {
    Replay(IngestReceipt),
    New(IngestJournal, Vec<Vec<u8>>),
}

fn prepare_ingest_locked(
    root: &Path,
    capture: &VerifiedCapture,
    draft: ItemDraft,
    keys: &BTreeMap<String, String>,
) -> Result<PreparedIngest> {
    match plan_ingest_locked(root, capture, draft, keys)? {
        IngestPlan::Replay(receipt) => Ok(PreparedIngest::Replay(receipt)),
        IngestPlan::New(journal, contents) => stage_ingest(root, journal, contents),
    }
}

fn plan_ingest_locked(
    root: &Path,
    capture: &VerifiedCapture,
    draft: ItemDraft,
    keys: &BTreeMap<String, String>,
) -> Result<IngestPlan> {
    plan_ingest_with_item_locked(root, capture, draft, keys, None)
}

// Book projects reserve stable item identities before the first visible ingest.
// Ordinary callers retain the existing allocation and replay behavior.
fn plan_ingest_with_item_locked(
    root: &Path,
    capture: &VerifiedCapture,
    draft: ItemDraft,
    keys: &BTreeMap<String, String>,
    reserved_item: Option<&str>,
) -> Result<IngestPlan> {
    validate_draft(&draft)?;
    let attachment_paths = captured_attachment_paths(capture, &draft)?;
    if !valid_source_id(&capture.source.source_id) {
        anyhow::bail!("invalid source id");
    }
    let key = ingest_key(capture);
    if let Some(receipt_path) = keys.get(&key) {
        // Committed journals are no longer revalidated per write; a replay still refuses to
        // vouch for (or re-create files of) an item whose canonical files changed.
        let prior: IngestReceipt = load(&root, safe_receipt_path(&root, receipt_path)?)?;
        if let Some(id) = reserved_item {
            anyhow::ensure!(prior.item_ids == [id], "book_project_identity_conflict");
        }
        for item_id in &prior.item_ids {
            let (item, revision) = read_item_without_integrity(&root, item_id)?;
            verify_revision_attachments(&root, &item, &revision)?;
        }
        let mut receipt = validate_replay_graph(&root, &key, receipt_path, capture, &draft)?;
        receipt.idempotent_replay = true;
        return Ok(IngestPlan::Replay(receipt));
    }
    let source_file = root
        .join("sources")
        .join(&capture.source.source_id)
        .join("source.json");
    match fs::symlink_metadata(&source_file) {
        Ok(_) => {
            let existing: memex_core::SourceProfile = load(&root, source_file.clone())?;
            if existing != capture.source {
                anyhow::bail!("source profile conflicts with existing source id");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let input_sha256 = ingest_input_sha256(capture, &draft)?;
    let item_id = match reserved_item {
        Some(id) => {
            anyhow::ensure!(valid_item_id(id), "invalid_book_project_identity");
            anyhow::ensure!(!root.join("items").join(id).try_exists()?, "book_project_identity_conflict");
            id.to_owned()
        }
        None => new_id("item"),
    };
    let revision_id = new_id("revision");
    let capture_id = new_id("capture");
    let sensitivity = effective_sensitivity(capture);
    let evidence_ids: Vec<String> = capture.blobs.iter().map(|_| new_id("evidence")).collect();
    let evidence: Vec<Evidence> = evidence_ids
        .iter()
        .enumerate()
        .map(|(index, evidence_id)| {
            expected_evidence(
                evidence_id.clone(),
                capture_id.clone(),
                index,
                &item_id,
                &attachment_paths,
                capture,
                true,
            )
        })
        .collect::<Result<_>>()?;
    let activity_id = new_id("activity");
    let activity = expected_activity(
        activity_id.clone(),
        capture,
        &draft,
        evidence.iter().map(|e| e.evidence_id.clone()).collect(),
        revision_id.clone(),
    );
    let resolved_attachments = resolve_attachments(&draft, &evidence_ids, &attachment_paths)?;
    let revision = ItemRevision {
        schema_version: SCHEMA_VERSION,
        revision_id: revision_id.clone(),
        item_id: item_id.clone(),
        revision_number: 1,
        title: draft.title,
        source_url: draft.source_url,
        canonical_url: draft.canonical_url,
        content_markdown: draft.content_markdown,
        sensitivity: std::cmp::max(sensitivity, draft.sensitivity),
        evidence_ids: evidence.iter().map(|e| e.evidence_id.clone()).collect(),
        provenance: vec![activity],
        metadata: draft.metadata,
        attachments: resolved_attachments,
    };
    let item = Item {
        schema_version: SCHEMA_VERSION,
        item_id: item_id.clone(),
        item_type: draft.item_type,
        current_revision: revision_id.clone(),
        sensitivity: revision.sensitivity,
        // The creating capture's time (owner definition), fixed inside the
        // creation transaction; the journal carries these bytes through recovery.
        added_at: added_at::for_capture_creation(&capture.capture.captured_at),
    };
    let record = CaptureRecord {
        schema_version: SCHEMA_VERSION,
        capture_id: capture_id.clone(),
        adapter_id: capture.capture.adapter_id.clone(),
        adapter_version: capture.capture.adapter_version.clone(),
        external_capture_id: capture.capture.external_capture_id.clone(),
        source_id: capture.source.source_id.clone(),
        captured_at: capture.capture.captured_at.clone(),
        capture_method: capture.capture.capture_method.clone(),
        route_summary: capture.capture.route_summary.clone(),
        source_url: capture.capture.source_url.clone(),
        canonical_url: capture.capture.canonical_url.clone(),
        sensitivity,
        blob_ids: capture.blobs.iter().map(|b| b.blob_id.clone()).collect(),
        ingest_input_sha256: Some(input_sha256.clone()),
        evidence_descriptor_version: Some(1),
    };
    let manifest = ItemIntegrityManifest {
        schema_version: SCHEMA_VERSION,
        item_id: item.item_id.clone(),
        current_revision: item.current_revision.clone(),
        sensitivity: item.sensitivity,
        item_sha256: format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&item)?)),
        revision_sha256: format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&revision)?)
        ),
    };
    let receipt_rel = format!("receipts/{capture_id}.json");
    let receipt = IngestReceipt {
        schema_version: SCHEMA_VERSION,
        capture_id: capture_id.clone(),
        evidence_ids: evidence.iter().map(|e| e.evidence_id.clone()).collect(),
        item_ids: vec![item_id.clone()],
        revision_ids: vec![revision_id.clone()],
        idempotent_replay: false,
    };
    let mut planned: Vec<(String, Vec<u8>)> = Vec::new();
    for (index, relative_path) in &attachment_paths {
        planned.push((
            format!("items/{item_id}/{relative_path}"),
            capture.blobs[*index].bytes.clone(),
        ));
    }
    for value in &evidence {
        planned.push((
            format!("evidence/{}/evidence.json", value.evidence_id),
            serde_json::to_string_pretty(value)?.into_bytes(),
        ));
    }
    planned.extend([
        (
            format!("sources/{}/source.json", capture.source.source_id),
            serde_json::to_string_pretty(&capture.source)?.into_bytes(),
        ),
        (
            format!("captures/{capture_id}/capture.json"),
            serde_json::to_string_pretty(&record)?.into_bytes(),
        ),
        (
            format!("items/{item_id}/item.json"),
            serde_json::to_string_pretty(&item)?.into_bytes(),
        ),
        (
            format!("items/{item_id}/revisions/{revision_id}.json"),
            serde_json::to_string_pretty(&revision)?.into_bytes(),
        ),
        (
            format!("items/{item_id}/links.json"),
            serde_json::to_string_pretty(&evidence_links(&revision))?.into_bytes(),
        ),
        (
            format!("integrity/items/{item_id}.json"),
            serde_json::to_string_pretty(&manifest)?.into_bytes(),
        ),
        (
            receipt_rel.clone(),
            serde_json::to_string_pretty(&receipt)?.into_bytes(),
        ),
    ]);

    // Build derived bytes from the same canonical values without first writing
    // and syncing a second, disposable copy of the entire item tree. The only
    // staged copy is the durable journal below; its format and barriers stay intact.
    let portable = build_portable_item_index_from_evidence(&item, &revision, |id| {
        evidence
            .iter()
            .find(|value| value.evidence_id == id)
            .cloned()
            .context("missing portable evidence")
    })?;
    let mut portable_json = serde_json::to_string_pretty(&portable)?;
    portable_json.push('\n');
    planned.push((
        format!("items/{item_id}/index.md"),
        render_markdown(&item, &revision).into_bytes(),
    ));
    planned.push((
        format!("items/{item_id}/index.json"),
        portable_json.into_bytes(),
    ));

    let entries = planned.iter().enumerate().map(|(index, (path, bytes))| {
        JournalEntry {
            staged: format!("entries/{index:04}"),
            path: path.clone(),
            sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
            byte_size: bytes.len() as u64,
        }
    }).collect();
    let journal = IngestJournal {
        journal_version: INGEST_JOURNAL_VERSION,
        schema_version: SCHEMA_VERSION,
        ingest_key: key.clone(),
        input_sha256,
        source_id: capture.source.source_id.clone(),
        capture_id: capture_id.clone(),
        item_id: item_id.clone(),
        revision_id: revision_id.clone(),
        evidence_ids: receipt.evidence_ids.clone(),
        activity_id,
        receipt_path: receipt_rel,
        receipt: receipt.clone(),
        directories: vec![format!("items/{item_id}/attachments")],
        entries,
        state: JournalState::Prepared,
    };
    Ok(IngestPlan::New(journal, planned.into_iter().map(|(_, bytes)| bytes).collect()))
}

fn stage_ingest(root: &Path, journal: IngestJournal, contents: Vec<Vec<u8>>) -> Result<PreparedIngest> {
    let journal_parent = ensure_contained_dir(root, Path::new("catalog/ingest-journal"))?;
    mark_recovery_pending(root)?;
    let temporary = journal_parent.join(format!(".prepare-{}", new_id("journal")));
    fs::create_dir(&temporary)?;
    sync_dir(&journal_parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
    }
    let entries_dir = temporary.join("entries");
    fs::create_dir(&entries_dir)?;
    sync_dir(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&entries_dir, fs::Permissions::from_mode(0o700))?;
    }
    for (entry, bytes) in journal.entries.iter().zip(contents) {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary.join(&entry.staged))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    sync_dir(&entries_dir)?;
    sync_dir(&temporary)?;
    durable_replace(
        &temporary,
        &temporary.join("journal.json"),
        serde_json::to_string_pretty(&journal)?.as_bytes(),
    )?;
    sync_dir(&temporary)?;
    let journal_dir = journal_parent.join(journal_key_name(&journal.ingest_key)?);
    fs::rename(&temporary, &journal_dir)?;
    sync_dir(&journal_parent)?;
    Ok(PreparedIngest::Journal(journal_dir, journal))
}

pub fn revise_item_metadata(
    library: impl AsRef<Path>,
    item_id: &str,
    update: ItemMetadataUpdate,
) -> Result<ItemMetadataUpdateReceipt> {
    revise_item_metadata_at_sensitivity(library, item_id, update, Sensitivity::Restricted)
}

/// The authenticated adapter supplies the ceiling, never the remote request.
/// Check it under the writer lock, before revealing revision/CAS information.
pub fn revise_item_metadata_at_sensitivity(
    library: impl AsRef<Path>,
    item_id: &str,
    update: ItemMetadataUpdate,
    max_sensitivity: Sensitivity,
) -> Result<ItemMetadataUpdateReceipt> {
    if !valid_item_id(item_id)
        || !valid_revision_id(&update.expected_revision_id)
        || !valid_text(&update.title, 4096)
        || update.manual_tags.len() > 256
        || update.manual_tags.iter().any(|tag| !valid_text(tag, 4096))
    {
        anyhow::bail!("invalid_metadata_update")
    }
    let mut manual_tags = update.manual_tags;
    manual_tags.sort();
    manual_tags.dedup();
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, current) = read_item_at_sensitivity(&root, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    if current.revision_id != update.expected_revision_id {
        anyhow::bail!("stale_item_revision")
    }
    let (_, legacy_revision) = read_legacy_item(&root, item_id)?;
    let owner_revision_count = current
        .revision_number
        .checked_sub(legacy_revision.revision_number)
        .context("invalid owner edit chain")?;
    if owner_revision_count >= MAX_ITEM_REVISION_CHAIN {
        anyhow::bail!("item_revision_limit_exceeded")
    }
    let old_tags = current
        .metadata
        .manual_tags
        .as_ref()
        .map(|tags| tags.value.as_slice())
        .unwrap_or_default();
    if current.title == update.title && old_tags == manual_tags.as_slice() {
        anyhow::bail!("metadata_unchanged")
    }
    let mut metadata = current.metadata.clone();
    metadata.manual_tags = if manual_tags.is_empty() {
        None
    } else {
        Some(Sourced {
            value: manual_tags,
            provenance: owner_provenance(),
        })
    };
    let revision = owner_edit_revision(&current, LEGACY_EDIT_ACTIVITY, update.title, metadata)?;
    let receipt = commit_owner_revision_locked(&root, item, &current, &revision, None)?;
    lock.unlock()?;
    Ok(receipt)
}

/// Model-suggested bibliographic values (already validated by the caller's strict parser).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelMetadataSuggestion {
    pub keywords: Option<Vec<String>>,
    pub creators: Option<Vec<Creator>>,
    pub published_date: Option<PartialDate>,
}

/// Fills only fields that are empty in `expected_revision_id`, with model provenance, through
/// the owner-revision journal. A concurrent edit makes it `stale_item_revision` (the caller
/// drops the suggestion). `Ok(None)`: nothing to fill.
pub fn apply_model_metadata_suggestion(
    library: impl AsRef<Path>,
    item_id: &str,
    expected_revision_id: &str,
    suggestion: ModelMetadataSuggestion,
    provenance: MetadataProvenance,
) -> Result<Option<ItemMetadataUpdateReceipt>> {
    if !valid_item_id(item_id)
        || !valid_revision_id(expected_revision_id)
        || !is_model_provenance(&provenance)
    {
        anyhow::bail!("invalid_metadata_update")
    }
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, current) = read_item_at_sensitivity(&root, item_id, Sensitivity::Restricted)?
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    if current.revision_id != expected_revision_id {
        anyhow::bail!("stale_item_revision")
    }
    let (_, legacy_revision) = read_legacy_item(&root, item_id)?;
    if current
        .revision_number
        .checked_sub(legacy_revision.revision_number)
        .context("invalid owner edit chain")?
        >= MAX_ITEM_REVISION_CHAIN
    {
        anyhow::bail!("item_revision_limit_exceeded")
    }
    let mut next = current.metadata.clone();
    let sourced = |value| Sourced {
        value,
        provenance: provenance.clone(),
    };
    // Empty fields and earlier model values are replaceable; a value the owner, intake or a
    // lookup provided never is. An absent suggestion never removes an earlier model value.
    fn replaceable<T>(field: &Option<Sourced<T>>) -> bool {
        field
            .as_ref()
            .is_none_or(|value| is_model_provenance(&value.provenance))
    }
    if replaceable(&next.keywords) {
        if let Some(keywords) = suggestion.keywords.filter(|value| !value.is_empty()) {
            if next.keywords.as_ref().map(|field| &field.value) != Some(&keywords) {
                next.keywords = Some(sourced(keywords));
            }
        }
    }
    let same_people = |current: &Option<Sourced<Vec<Creator>>>, suggested: &[Creator]| {
        current.as_ref().is_some_and(|field| {
            field.value.len() == suggested.len()
                && field
                    .value
                    .iter()
                    .zip(suggested)
                    .all(|(a, b)| a.display_name == b.display_name && a.role == b.role)
        })
    };
    if replaceable(&next.creators)
        && suggestion
            .creators
            .as_ref()
            .is_some_and(|c| !c.is_empty() && !same_people(&next.creators, c))
    {
        next.creators = suggestion
            .creators
            .filter(|value| !value.is_empty())
            .map(|creators| {
                creators
                    .into_iter()
                    .map(|mut creator| {
                        creator.creator_id.get_or_insert_with(|| new_id("creator"));
                        creator
                    })
                    .collect()
            })
            .map(|value| Sourced {
                value,
                provenance: provenance.clone(),
            });
    }
    if replaceable(&next.published_date) && replaceable(&next.published_partial_date) {
        if let Some(date) = suggestion
            .published_date
            .filter(|date| next.published_partial_date.as_ref().map(|f| &f.value) != Some(date))
        {
            if date.validate().is_err() {
                anyhow::bail!("invalid_metadata_field:published_date")
            }
            let legacy = date
                .legacy_string()
                .context("validated date has a projection")?
                .to_owned();
            next.published_date = Some(Sourced {
                value: legacy,
                provenance: provenance.clone(),
            });
            next.published_partial_date = Some(Sourced {
                value: date,
                provenance: provenance.clone(),
            });
        }
    }
    if next == current.metadata {
        lock.unlock()?;
        return Ok(None);
    }
    validate_metadata(&next)?;
    let revision = owner_edit_revision(
        &current,
        MODEL_SUGGESTION_ACTIVITY,
        current.title.clone(),
        next,
    )?;
    let receipt = commit_owner_revision_locked(&root, item, &current, &revision, None)?;
    lock.unlock()?;
    Ok(Some(receipt))
}

/// Builds the next immutable owner revision: body, evidence, attachments,
/// URLs and sensitivity are carried over unchanged.
fn owner_edit_revision(
    current: &ItemRevision,
    activity_type: &str,
    title: String,
    metadata: ItemMetadata,
) -> Result<ItemRevision> {
    let revision_number = current
        .revision_number
        .checked_add(1)
        .context("item revision overflow")?;
    let revision_id = new_id("revision");
    let activity = ProvenanceActivity {
        schema_version: SCHEMA_VERSION,
        activity_id: new_id("activity"),
        activity_type: activity_type.into(),
        occurred_at: chrono::Utc::now().to_rfc3339(),
        tool_id: "memex.desktop".into(),
        tool_version: "1".into(),
        config_digest: owner_edit_config_digest(activity_type),
        input_ids: vec![current.revision_id.clone()],
        input_checksums: vec![revision_digest(current)?],
        output_ids: vec![revision_id.clone()],
    };
    let mut provenance = current.provenance.clone();
    provenance.push(activity);
    Ok(ItemRevision {
        schema_version: SCHEMA_VERSION,
        revision_id,
        item_id: current.item_id.clone(),
        revision_number,
        title,
        source_url: current.source_url.clone(),
        canonical_url: current.canonical_url.clone(),
        content_markdown: current.content_markdown.clone(),
        sensitivity: current.sensitivity,
        evidence_ids: current.evidence_ids.clone(),
        provenance,
        metadata,
        attachments: current.attachments.clone(),
    })
}

/// Shared owner-edit commit path; caller holds the writer lock and has
/// already authorized the item and checked `expected_revision_id`.
fn prepare_owner_revision_locked(
    root: &Path,
    mut item: Item,
    base: &ItemRevision,
    revision: &ItemRevision,
) -> Result<(ItemCurrentPointer, ItemMetadataUpdateReceipt)> {
    validate_draft(&ItemDraft {
        item_type: item.item_type.clone(),
        title: revision.title.clone(),
        source_url: revision.source_url.clone(),
        canonical_url: revision.canonical_url.clone(),
        content_markdown: revision.content_markdown.clone(),
        sensitivity: revision.sensitivity,
        translator_id: "memex.desktop".into(),
        translator_version: "1".into(),
        metadata: revision.metadata.clone(),
        attachments: Vec::new(),
    })?;
    let item_id = item.item_id.clone();
    let revision_id = revision.revision_id.clone();
    let receipt = ItemMetadataUpdateReceipt {
        schema_version: SCHEMA_VERSION,
        item_id: item_id.clone(),
        previous_revision_id: base.revision_id.clone(),
        revision_id: revision_id.clone(),
        revision_number: revision.revision_number,
    };
    item.current_revision = revision_id.clone();
    let pointer = ItemCurrentPointer {
        schema_version: SCHEMA_VERSION,
        item,
        revision_sha256: revision_digest(revision)?,
    };
    let revision_path = root
        .join("items")
        .join(&item_id)
        .join("revisions")
        .join(format!("{revision_id}.json"));
    install_binary(
        root,
        &revision_path,
        serde_json::to_string_pretty(revision)?.as_bytes(),
    )?;
    sync_dir(
        revision_path
            .parent()
            .context("item revision has no parent")?,
    )?;
    Ok((pointer, receipt))
}

/// No replayable intent: unreachable immutable preparation may survive rejection,
/// but recovery must never turn that preparation into a current revision.
fn commit_owner_revision_with_gate_locked<G>(
    root: &Path,
    item: Item,
    base: &ItemRevision,
    revision: &ItemRevision,
    gate: G,
) -> Result<()>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    let (pointer, _) = prepare_owner_revision_locked(root, item, base, revision)?;
    validate_owner_edit_transition(base, revision)?;
    validate_owner_edit_chain(root, &pointer.item, revision)?;
    install_item_revision_projections(root, &pointer.item, revision)?;
    reader::repair::install_index(root, revision)?;
    durable_replace_with_gate(
        root,
        &item_current_path(root, &pointer.item.item_id),
        serde_json::to_string_pretty(&pointer)?.as_bytes(),
        gate,
    )
}

fn commit_owner_revision_locked(
    root: &Path,
    item: Item,
    base: &ItemRevision,
    revision: &ItemRevision,
    request: Option<(String, String)>,
) -> Result<ItemMetadataUpdateReceipt> {
    let (pointer, receipt) = prepare_owner_revision_locked(root, item, base, revision)?;
    let item_id = pointer.item.item_id.clone();
    let revision_id = revision.revision_id.clone();
    item_update_step(0)?;
    let journal = ItemUpdateJournal {
        journal_version: ITEM_UPDATE_JOURNAL_VERSION,
        schema_version: SCHEMA_VERSION,
        item_id: item_id.clone(),
        base_revision_id: base.revision_id.clone(),
        revision_id,
        pointer,
        request: request.map(|(request_id, request_sha256)| MetadataEditRequestRecord {
            schema_version: SCHEMA_VERSION,
            request_id,
            request_sha256,
            receipt: receipt.clone(),
        }),
    };
    let journal_parent = item_update_parent(root)?;
    let journal_path = journal_parent.join(format!("{item_id}.json"));
    if fs::symlink_metadata(&journal_path).is_ok() {
        anyhow::bail!("item_update_in_progress")
    }
    mark_recovery_pending(root)?;
    durable_replace(
        root,
        &journal_path,
        serde_json::to_string_pretty(&journal)?.as_bytes(),
    )?;
    item_update_step(1)?;
    reconcile_item_update(root, &journal_path, &journal, revision)?;
    // The caller recovered any earlier interrupted write under this lock, so this journal was
    // the only pending one.
    clear_recovery_pending(root)?;
    Ok(receipt)
}

fn verify_attachment_bytes(path: &Path, descriptor: &memex_core::BlobDescriptor) -> Result<()> {
    let mut file = open_regular_nofollow(path)?;
    if file.metadata()?.len() != descriptor.byte_size || !valid_lower_hex_64(&descriptor.sha256) {
        anyhow::bail!("invalid stored attachment");
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if format!("{:x}", digest.finalize()) != descriptor.sha256 {
        anyhow::bail!("stored attachment checksum mismatch");
    }
    Ok(())
}

fn read_attachment_bytes(path: &Path, descriptor: &memex_core::BlobDescriptor) -> Result<Vec<u8>> {
    let mut file = open_regular_nofollow(path)?;
    if descriptor.byte_size > MAX_ATTACHMENT_READ_BYTES
        || file.metadata()?.len() != descriptor.byte_size
        || !valid_lower_hex_64(&descriptor.sha256)
    {
        anyhow::bail!("invalid stored attachment");
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(descriptor.byte_size).context("stored attachment is too large")?,
    );
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != descriptor.byte_size
        || format!("{:x}", Sha256::digest(&bytes)) != descriptor.sha256
        || !image_signature_matches(&descriptor.media_type, &bytes)
    {
        anyhow::bail!("stored attachment checksum mismatch");
    }
    Ok(bytes)
}

fn validate_attachment_evidence<'a>(
    item: &Item,
    evidence_id: &str,
    relative_path: &str,
    evidence: &'a Evidence,
) -> Result<&'a memex_core::BlobDescriptor> {
    let descriptor = evidence
        .descriptor
        .as_ref()
        .context("attachment evidence has no descriptor")?;
    let expected_path =
        captured_attachment_relative_path(&evidence.blob_id, &descriptor.media_type)
            .context("attachment evidence media type is unsupported")?;
    if evidence.schema_version != SCHEMA_VERSION
        || evidence.evidence_id != evidence_id
        || evidence.sensitivity > item.sensitivity
        || relative_path != expected_path
        || evidence.blob_id != format!("sha256:{}", descriptor.sha256)
        || evidence.logical_locator != descriptor.locator
        || descriptor.schema_version != SCHEMA_VERSION
        || descriptor
            .source_url
            .as_deref()
            .is_none_or(|url| !valid_captured_source_url(url))
        || evidence.backend_ref != format!("memex-store:{}/{relative_path}", item.item_id)
    {
        anyhow::bail!("invalid attachment evidence");
    }
    Ok(descriptor)
}

fn verify_revision_attachments(root: &Path, item: &Item, revision: &ItemRevision) -> Result<()> {
    for attachment in &revision.attachments {
        let AttachmentRef::Evidence {
            evidence_id,
            relative_path,
            ..
        } = attachment
        else {
            continue;
        };
        if !safe_component(evidence_id, "evidence_") || !revision.evidence_ids.contains(evidence_id)
        {
            anyhow::bail!("invalid attachment evidence relationship");
        }
        let evidence: Evidence = load(
            root,
            root.join("evidence")
                .join(evidence_id)
                .join("evidence.json"),
        )?;
        let descriptor = validate_attachment_evidence(item, evidence_id, relative_path, &evidence)?;
        let item_dir = contained_existing(root, &root.join("items").join(&item.item_id))?;
        let relative = Path::new(relative_path);
        if relative.parent() != Some(Path::new("attachments")) {
            anyhow::bail!("invalid attachment path");
        }
        let filename = relative
            .file_name()
            .context("attachment path has no filename")?;
        let attachments = item_dir.join("attachments");
        let meta = fs::symlink_metadata(&attachments)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            anyhow::bail!("unsafe attachments directory");
        }
        let attachments = contained_existing(root, &attachments)?;
        let path = attachments.join(filename);
        verify_attachment_bytes(&path, descriptor)?;
    }
    Ok(())
}
fn read_legacy_item(root: &Path, item_id: &str) -> Result<(Item, ItemRevision)> {
    let (item, rev) = read_legacy_revision(root, item_id)?;
    verify_revision_attachments(root, &item, &rev)?;
    Ok((item, rev))
}

// Validated identity/revision graph only. Image reads validate the selected
// attachment separately; ordinary full item reads retain all-attachment checks.
fn read_legacy_revision(root: &Path, item_id: &str) -> Result<(Item, ItemRevision)> {
    if !valid_item_id(item_id) {
        anyhow::bail!("invalid item id");
    }
    let item = load_legacy_item_header(root, item_id)?;
    let item_dir = contained_existing(root, &root.join("items").join(item_id))?;
    let manifest: ItemIntegrityManifest = load(
        root,
        root.join("integrity/items").join(format!("{item_id}.json")),
    )?;
    let rev: ItemRevision = load(
        root,
        item_dir
            .join("revisions")
            .join(format!("{}.json", item.current_revision)),
    )?;
    if rev.schema_version != SCHEMA_VERSION
        || rev.item_id != item.item_id
        || rev.revision_id != item.current_revision
        || rev.sensitivity != item.sensitivity
        || manifest.revision_sha256
            != format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&rev)?))
    {
        anyhow::bail!("invalid revision relationship");
    }
    Ok((item, rev))
}

pub fn read_item(root: impl AsRef<Path>, item_id: &str) -> Result<(Item, ItemRevision)> {
    if !valid_item_id(item_id) {
        anyhow::bail!("invalid item id");
    }
    let root = existing_canonical_root(root.as_ref())?;
    if let Some((pointer, revision)) = load_current_pointer(&root, item_id)? {
        verify_revision_attachments(&root, &pointer.item, &revision)?;
        return Ok((pointer.item, revision));
    }
    read_legacy_item(&root, item_id)
}
pub fn read_item_at_sensitivity(
    root: impl AsRef<Path>,
    item_id: &str,
    max: memex_core::Sensitivity,
) -> Result<Option<(Item, ItemRevision)>> {
    if !valid_item_id(item_id) {
        return Ok(None);
    }
    let input = root.as_ref();
    let root = match fs::symlink_metadata(input) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => existing_canonical_root(input)?,
        Ok(_) => anyhow::bail!("unsafe library root"),
        Err(e) => return Err(e.into()),
    };
    let items_path = root.join("items");
    match fs::symlink_metadata(&items_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe items directory"),
        Err(e) => return Err(e.into()),
    }
    let item_path = items_path.join(item_id);
    match fs::symlink_metadata(&item_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if !m.file_type().is_symlink() && m.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe item directory"),
        Err(e) => return Err(e.into()),
    }
    // One verified load of the current pointer serves both the visibility check and the
    // read; loading it twice parsed and hashed the whole current revision twice per call.
    if let Some((pointer, revision)) = load_current_pointer(&root, item_id)? {
        if pointer.item.sensitivity > max || trash::is_trashed(&root, item_id)? {
            return Ok(None);
        }
        verify_revision_attachments(&root, &pointer.item, &revision)?;
        return Ok(Some((pointer.item, revision)));
    }
    let item = load_verified_item_header(&root, item_id)?.0;
    if item.sensitivity > max || trash::is_trashed(&root, item_id)? {
        return Ok(None);
    }
    Ok(Some(read_item(&root, item_id)?))
}

pub fn read_item_attachment_at_sensitivity(
    root: impl AsRef<Path>,
    item_id: &str,
    relative_path: &str,
    max: Sensitivity,
) -> Result<Option<ItemReadAttachment>> {
    let Some((item, revision)) = read_item_at_sensitivity(root.as_ref(), item_id, max)? else {
        return Ok(None);
    };
    let Some((evidence_id, stored_path)) = revision.attachments.iter().find_map(|attachment| {
        let AttachmentRef::Evidence {
            evidence_id,
            relative_path: stored_path,
            ..
        } = attachment
        else {
            return None;
        };
        (stored_path == relative_path).then_some((evidence_id, stored_path))
    }) else {
        return Ok(None);
    };
    let root = existing_canonical_root(root.as_ref())?;
    let evidence: Evidence = load(
        &root,
        root.join("evidence")
            .join(evidence_id)
            .join("evidence.json"),
    )?;
    let descriptor = validate_attachment_evidence(&item, evidence_id, stored_path, &evidence)?;
    let item_dir = contained_existing(&root, &root.join("items").join(&item.item_id))?;
    let path = item_dir.join(stored_path);
    Ok(Some(ItemReadAttachment {
        media_type: descriptor.media_type.clone(),
        bytes: read_attachment_bytes(&path, descriptor)?,
    }))
}
fn load_legacy_item_header(root: &Path, item_id: &str) -> Result<Item> {
    let dir = contained_existing(root, &root.join("items").join(item_id))?;
    let item: Item = load(root, dir.join("item.json"))?;
    if item.schema_version != SCHEMA_VERSION
        || item.item_id != item_id
        || !valid_revision_id(&item.current_revision)
        || item
            .added_at
            .as_deref()
            .is_some_and(|value| !memex_core::valid_added_at(value))
    {
        anyhow::bail!("invalid item relationship")
    }
    let path = root.join("integrity/items").join(format!("{item_id}.json"));
    let meta = fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        anyhow::bail!("unsafe item integrity manifest")
    }
    let manifest: ItemIntegrityManifest = load(root, path)?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.item_id != item.item_id
        || manifest.current_revision != item.current_revision
        || manifest.sensitivity != item.sensitivity
        || !valid_sha256(&manifest.item_sha256)
        || !valid_sha256(&manifest.revision_sha256)
        || manifest.item_sha256
            != format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&item)?))
    {
        anyhow::bail!("invalid item integrity manifest")
    }
    Ok(item)
}

fn load_verified_item_header(root: &Path, item_id: &str) -> Result<(Item, String)> {
    if let Some((pointer, _)) = load_current_pointer(root, item_id)? {
        return Ok((pointer.item, pointer.revision_sha256));
    }
    let item = load_legacy_item_header(root, item_id)?;
    let manifest: ItemIntegrityManifest = load(
        root,
        root.join("integrity/items").join(format!("{item_id}.json")),
    )?;
    Ok((item, manifest.revision_sha256))
}

/// Count-only visibility for navigation reads: the item's head (`current.json`, or the legacy
/// header with its integrity manifest) and the trash set, without loading or hashing the
/// revision body. Full verification stays on every read that discloses item content.
pub(crate) fn item_head_visible(
    root: &Path,
    item_id: &str,
    max: Sensitivity,
    trashed: &BTreeSet<String>,
) -> Result<bool> {
    if !valid_item_id(item_id) || trashed.contains(item_id) {
        return Ok(false);
    }
    match fs::symlink_metadata(root.join("items").join(item_id)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe item directory"),
        Err(error) => return Err(error.into()),
    }
    let path = item_current_path(root, item_id);
    let item = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            load_legacy_item_header(root, item_id)?
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            let pointer: ItemCurrentPointer = load(root, path)?;
            if pointer.schema_version != SCHEMA_VERSION
                || pointer.item.schema_version != SCHEMA_VERSION
                || pointer.item.item_id != item_id
                || !valid_revision_id(&pointer.item.current_revision)
                || !valid_sha256(&pointer.revision_sha256)
            {
                anyhow::bail!("invalid item current pointer")
            }
            pointer.item
        }
        Ok(_) => anyhow::bail!("unsafe item current pointer"),
        Err(error) => return Err(error.into()),
    };
    Ok(item.sensitivity <= max)
}

fn read_item_metadata_from_root(
    root: &Path,
    item_id: &str,
    max: Sensitivity,
) -> Result<Option<ItemReadMetadata>> {
    let (item, revision_sha256) = load_verified_item_header(root, item_id)?;
    if item.sensitivity > max || trash::is_trashed(root, item_id)? {
        return Ok(None);
    }
    let revision: ItemRevision = load(
        root,
        root.join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{}.json", item.current_revision)),
    )?;
    if revision.schema_version != SCHEMA_VERSION
        || revision.item_id != item.item_id
        || revision.revision_id != item.current_revision
        || revision.revision_number == 0
        || revision.sensitivity != item.sensitivity
        || revision_sha256
            != format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&revision)?)
            )
    {
        anyhow::bail!("invalid item metadata relationship");
    }
    let captured_at = revision
        .provenance
        .iter()
        .find(|activity| activity.activity_type == "translate")
        .map(|activity| activity.occurred_at.clone());
    let ItemRevision {
        revision_id,
        revision_number,
        title,
        source_url,
        canonical_url,
        metadata,
        ..
    } = revision;
    let added_at = added_at::effective(root, &item)?;
    Ok(Some(ItemReadMetadata {
        item_id: item.item_id,
        revision_id,
        revision_number,
        item_type: item.item_type,
        title,
        source_url,
        canonical_url,
        sensitivity: item.sensitivity,
        captured_at,
        metadata,
        added_at,
    }))
}

pub fn read_item_metadata_at_sensitivity(
    root: impl AsRef<Path>,
    item_id: &str,
    max: Sensitivity,
) -> Result<Option<ItemReadMetadata>> {
    if !valid_item_id(item_id) {
        return Ok(None);
    }
    let input = root.as_ref();
    let root = match fs::symlink_metadata(input) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
            existing_canonical_root(input)?
        }
        Ok(_) => anyhow::bail!("unsafe library root"),
        Err(error) => return Err(error.into()),
    };
    let item_path = root.join("items").join(item_id);
    match fs::symlink_metadata(&item_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe item directory"),
        Err(error) => return Err(error.into()),
    }
    read_item_metadata_from_root(&root, item_id, max)
}

pub fn scan_item_metadata_at_sensitivity(
    root: impl AsRef<Path>,
    max: Sensitivity,
    mut visit: impl FnMut(ItemReadMetadata) -> Result<()>,
) -> Result<()> {
    let input = root.as_ref();
    let metadata = fs::symlink_metadata(input)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("unsafe library root");
    }
    let root = existing_canonical_root(input)?;
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&items)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        let item_id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe item entry"))?;
        if metadata.is_symlink() || !metadata.is_dir() || !valid_item_id(&item_id) {
            anyhow::bail!("unsafe item entry");
        }
        ids.push(item_id);
    }
    ids.sort();
    for item_id in ids {
        if let Some(metadata) = read_item_metadata_from_root(&root, &item_id, max)? {
            visit(metadata)?;
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct ItemMetadataPageWork {
    scanned_entries: usize,
    peak_retained_ids: usize,
    metadata_reads: usize,
}

fn page_all_item_metadata_with_scan_limit(
    root: impl AsRef<Path>,
    cursor: Option<&str>,
    page_size: usize,
    scan_limit: usize,
) -> Result<(Vec<ItemReadMetadata>, Option<String>, ItemMetadataPageWork)> {
    if page_size == 0
        || page_size > MAX_ITEM_METADATA_PAGE_SIZE
        || scan_limit == 0
        || cursor.is_some_and(|value| !valid_item_id(value))
    {
        anyhow::bail!("invalid_cursor");
    }
    let input = root.as_ref();
    let metadata = fs::symlink_metadata(input)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("unsafe library root");
    }
    let root = existing_canonical_root(input)?;
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if cursor.is_some() {
                anyhow::bail!("invalid_cursor");
            }
            return Ok((
                Vec::new(),
                None,
                ItemMetadataPageWork {
                    scanned_entries: 0,
                    peak_retained_ids: 0,
                    metadata_reads: 0,
                },
            ));
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }

    // Store creation gives each immutable item identity a UUIDv7-based ID. Lexical
    // descending is the explicit cursor order; it does not rely on read_dir order
    // or mutable revision timestamps.
    let retain = page_size + 1;
    let trashed = trash::trashed_ids(&root)?;
    let mut candidates = BTreeSet::new();
    let mut scanned_entries = 0;
    let mut peak_retained_ids = 0;
    let mut cursor_seen = cursor.is_none();
    for entry in fs::read_dir(&items)? {
        if scanned_entries == scan_limit {
            anyhow::bail!("library_scan_limit_exceeded");
        }
        scanned_entries += 1;
        let entry = entry?;
        let metadata = entry.file_type()?;
        let item_id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe item entry"))?;
        if metadata.is_symlink() || !metadata.is_dir() || !valid_item_id(&item_id) {
            anyhow::bail!("unsafe item entry");
        }
        if cursor == Some(item_id.as_str()) {
            cursor_seen = true;
        } else if trashed.contains(&item_id) {
            continue;
        } else if cursor.is_none_or(|value| item_id.as_str() < value) {
            candidates.insert(item_id);
            if candidates.len() > retain {
                candidates.pop_first();
            }
            peak_retained_ids = peak_retained_ids.max(candidates.len());
        }
    }
    if !cursor_seen {
        anyhow::bail!("invalid_cursor");
    }

    // Revalidate a continuation identity so a removed or corrupt cursor cannot
    // silently move the page boundary.
    let mut metadata_reads = 0;
    if let Some(cursor) = cursor {
        // A cursor item trashed between pages still marks a valid boundary.
        load_verified_item_header(&root, cursor).map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
        metadata_reads += 1;
    }
    let mut metadata = Vec::with_capacity(candidates.len());
    for item_id in candidates.into_iter().rev() {
        metadata.push(
            read_item_metadata_from_root(&root, &item_id, Sensitivity::Restricted)?
                .ok_or_else(|| anyhow::anyhow!("invalid item metadata relationship"))?,
        );
        metadata_reads += 1;
    }
    let has_more = metadata.len() > page_size;
    metadata.truncate(page_size);
    let next_cursor = has_more.then(|| {
        metadata
            .last()
            .expect("a continuation requires one returned item")
            .item_id
            .clone()
    });
    Ok((
        metadata,
        next_cursor,
        ItemMetadataPageWork {
            scanned_entries,
            peak_retained_ids,
            metadata_reads,
        },
    ))
}

/// Returns one deterministic, all-sensitivity owner page. A call advances the
/// filesystem iterator at most `MAX_ITEM_METADATA_PAGE_SCAN_ENTRIES + 1` times,
/// retains at most `page_size + 1` item IDs/metadata rows, and fails closed if
/// the Store is larger than that explicit scan ceiling.
pub fn page_all_item_metadata(
    root: impl AsRef<Path>,
    cursor: Option<&str>,
    page_size: usize,
) -> Result<(Vec<ItemReadMetadata>, Option<String>)> {
    let (items, next_cursor, _) = page_all_item_metadata_with_scan_limit(
        root,
        cursor,
        page_size,
        MAX_ITEM_METADATA_PAGE_SCAN_ENTRIES,
    )?;
    Ok((items, next_cursor))
}

fn read_item_without_integrity(root: &Path, item_id: &str) -> Result<(Item, ItemRevision)> {
    if !valid_item_id(item_id) {
        anyhow::bail!("invalid item id")
    }
    let dir = contained_existing(root, &root.join("items").join(item_id))?;
    let item: Item = load(root, dir.join("item.json"))?;
    if item.schema_version != SCHEMA_VERSION
        || item.item_id != item_id
        || !valid_revision_id(&item.current_revision)
        || item
            .added_at
            .as_deref()
            .is_some_and(|value| !memex_core::valid_added_at(value))
    {
        anyhow::bail!("invalid item relationship")
    }
    let revision: ItemRevision = load(
        root,
        dir.join("revisions")
            .join(format!("{}.json", item.current_revision)),
    )?;
    if revision.schema_version != SCHEMA_VERSION
        || revision.item_id != item.item_id
        || revision.revision_id != item.current_revision
        || revision.sensitivity != item.sensitivity
    {
        anyhow::bail!("invalid revision relationship")
    }
    Ok((item, revision))
}
fn integrity_manifest(item: &Item, revision: &ItemRevision) -> Result<ItemIntegrityManifest> {
    Ok(ItemIntegrityManifest {
        schema_version: SCHEMA_VERSION,
        item_id: item.item_id.clone(),
        current_revision: item.current_revision.clone(),
        sensitivity: item.sensitivity,
        item_sha256: format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(item)?)),
        revision_sha256: format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(revision)?)),
    })
}
/// Stat-only change token of one item's canonical head (`current.json`, or `item.json` for
/// legacy items). Every head update is an atomic replace, so the inode changes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemChangeStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    ino: u64,
    // A legacy backfill is immutable/create-once and does not replace the item head.
    added_at_present: bool,
}

/// `(item_id, stamp)` for every non-trashed item, sorted by id. Checks directory,
/// head and added-at metadata without file reads, inside `with_consistent_snapshot`.
pub fn item_change_stamps(root: &Path) -> Result<Vec<(String, ItemChangeStamp)>> {
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let trashed = trash::trashed_ids(root)?;
    let entries = fs::read_dir(&items)?.collect::<std::io::Result<Vec<_>>>()?;
    let read_stamp = |entry: &fs::DirEntry| -> Result<Option<(String, ItemChangeStamp)>> {
        let kind = entry.file_type()?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if kind.is_symlink() || !kind.is_dir() || !valid_item_id(&id) {
            anyhow::bail!("unsafe item entry")
        }
        if trashed.contains(&id) {
            return Ok(None);
        }
        let head = match fs::symlink_metadata(entry.path().join("current.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::symlink_metadata(entry.path().join("item.json"))?
            }
            other => other?,
        };
        if head.file_type().is_symlink() || !head.is_file() {
            anyhow::bail!("unsafe item head")
        }
        let added_at_present = match fs::symlink_metadata(entry.path().join("added_at.json")) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                anyhow::bail!("unsafe added_at record")
            }
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        Ok(Some((
            id,
            ItemChangeStamp {
                len: head.len(),
                modified: head.modified().ok(),
                #[cfg(unix)]
                ino: std::os::unix::fs::MetadataExt::ino(&head),
                added_at_present,
            },
        )))
    };
    // Bounded parallel stats, with every worker joined before the snapshot unlocks.
    let mut out = std::thread::scope(|scope| -> Result<Vec<_>> {
        let workers: Vec<_> = entries.chunks(entries.len().div_ceil(8).max(1))
            .map(|chunk| {
                let read_stamp = &read_stamp;
                scope.spawn(move || chunk.iter().map(read_stamp).collect::<Result<Vec<_>>>())
            }).collect();
        let mut out = Vec::new();
        for worker in workers {
            out.extend(worker.join().map_err(|_| anyhow::anyhow!("item scan worker failed"))??.into_iter().flatten());
        }
        Ok(out)
    })?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
pub fn list_items(root: impl AsRef<Path>) -> Result<Vec<(Item, ItemRevision)>> {
    let root_input = root.as_ref();
    let root_metadata = fs::symlink_metadata(root_input)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        anyhow::bail!("unsafe library root");
    }
    let root = existing_canonical_root(root_input)?;
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&items)? {
        let entry = entry?;
        let meta = entry.file_type()?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if meta.is_symlink() || !meta.is_dir() || !valid_item_id(&id) {
            anyhow::bail!("unsafe item entry")
        }
        ids.push(id);
    }
    ids.sort();
    let trashed = trash::trashed_ids(&root)?;
    ids.into_iter()
        .filter(|id| !trashed.contains(id))
        .map(|id| read_item(&root, &id))
        .collect()
}
pub fn list_items_at_sensitivity(
    root: impl AsRef<Path>,
    max: memex_core::Sensitivity,
) -> Result<Vec<(Item, ItemRevision)>> {
    let input = root.as_ref();
    let meta = fs::symlink_metadata(input)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("unsafe library root")
    }
    let root = existing_canonical_root(input)?;
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&items)? {
        let entry = entry?;
        let m = entry.file_type()?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe item entry"))?;
        if m.is_symlink() || !m.is_dir() || !valid_item_id(&id) {
            anyhow::bail!("unsafe item entry")
        }
        ids.push(id)
    }
    ids.sort();
    let mut items_out = Vec::new();
    for id in ids {
        if let Some(item) = read_item_at_sensitivity(&root, &id, max)? {
            items_out.push(item);
        }
    }
    Ok(items_out)
}
fn build_portable_item_index(
    root: &Path,
    item: &Item,
    revision: &ItemRevision,
) -> Result<PortableItemIndex> {
    build_portable_item_index_from_evidence(item, revision, |evidence_id| {
        load(
            root,
            root.join("evidence")
                .join(evidence_id)
                .join("evidence.json"),
        )
    })
}
fn build_portable_item_index_from_evidence(
    item: &Item,
    revision: &ItemRevision,
    mut evidence_by_id: impl FnMut(&str) -> Result<Evidence>,
) -> Result<PortableItemIndex> {
    let markdown = render_markdown(item, revision);
    let mut attachments = Vec::with_capacity(revision.attachments.len());
    for attachment in &revision.attachments {
        attachments.push(match attachment {
            AttachmentRef::Remote {
                url,
                media_type,
                title,
            } => PortableAttachment::Remote {
                url: url.clone(),
                media_type: media_type.clone(),
                title: title.clone(),
            },
            AttachmentRef::Evidence {
                evidence_id,
                relative_path,
                title,
            } => {
                let evidence = evidence_by_id(evidence_id)?;
                let descriptor =
                    validate_attachment_evidence(item, evidence_id, relative_path, &evidence)?;
                PortableAttachment::Evidence {
                    evidence_id: evidence_id.clone(),
                    path: relative_path.clone(),
                    media_type: descriptor.media_type.clone(),
                    byte_size: descriptor.byte_size,
                    sha256: evidence.blob_id.clone(),
                    source_url: descriptor.source_url.clone(),
                    title: title.clone(),
                }
            }
        });
    }
    let translator = revision
        .provenance
        .iter()
        .find(|activity| activity.activity_type == "translate");
    Ok(PortableItemIndex {
        schema_version: SCHEMA_VERSION,
        projection: "memex.portable-item".into(),
        projection_version: 1,
        item_id: item.item_id.clone(),
        revision_id: revision.revision_id.clone(),
        revision_number: revision.revision_number,
        item_type: item.item_type.clone(),
        title: revision.title.clone(),
        source_url: revision.source_url.clone(),
        canonical_url: revision.canonical_url.clone(),
        sensitivity: item.sensitivity,
        captured_at: translator.map(|activity| activity.occurred_at.clone()),
        translator: translator.map(|activity| PortableTranslator {
            id: activity.tool_id.clone(),
            version: activity.tool_version.clone(),
        }),
        metadata: revision.metadata.clone(),
        provenance: revision.provenance.clone(),
        content: PortableContent {
            path: "index.md".into(),
            media_type: "text/markdown".into(),
            byte_size: markdown.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(markdown.as_bytes())),
        },
        attachments,
    })
}
fn portable_item_index_json(root: &Path, item: &Item, revision: &ItemRevision) -> Result<String> {
    let mut output =
        serde_json::to_string_pretty(&build_portable_item_index(root, item, revision)?)?;
    output.push('\n');
    Ok(output)
}
fn install_item_projections(root: &Path, item: &Item, revision: &ItemRevision) -> Result<()> {
    let item_dir = root.join("items").join(&item.item_id);
    let markdown_path = item_dir.join("index.md");
    let index_path = item_dir.join("index.json");
    let markdown = render_markdown(item, revision);
    let index = portable_item_index_json(root, item, revision)?;

    contained_output_path(root, &markdown_path)?;
    contained_output_path(root, &index_path)?;
    ensure_contained_dir(
        root,
        &Path::new("items").join(&item.item_id).join("attachments"),
    )?;

    repair_derived_file(root, &markdown_path, markdown.as_bytes())?;
    repair_derived_file(root, &index_path, index.as_bytes())
}

fn revision_projection_paths(
    root: &Path,
    item: &Item,
    revision: &ItemRevision,
) -> (PathBuf, PathBuf) {
    let projection_dir = root
        .join("items")
        .join(&item.item_id)
        .join("projections")
        .join(&revision.revision_id);
    (
        projection_dir.join("index.md"),
        projection_dir.join("index.json"),
    )
}

fn install_item_revision_projections(
    root: &Path,
    item: &Item,
    revision: &ItemRevision,
) -> Result<()> {
    let (markdown_path, index_path) = revision_projection_paths(root, item, revision);
    let markdown = render_markdown(item, revision);
    let index = portable_item_index_json(root, item, revision)?;
    install_binary(root, &markdown_path, markdown.as_bytes())?;
    install_binary(root, &index_path, index.as_bytes())
}
pub fn render_markdown(item: &Item, revision: &ItemRevision) -> String {
    let item_id = serde_json::to_string(&item.item_id).expect("string serialization");
    let revision_id = serde_json::to_string(&revision.revision_id).expect("string serialization");
    let title = serde_json::to_string(&revision.title).expect("string serialization");
    format!(
        "---\nitem_id: {}\nrevision_id: {}\ntitle: {}\n---\n\n# {}\n\n{}\n",
        item_id, revision_id, title, revision.title, revision.content_markdown
    )
}
fn read_text_projection(path: &Path, expected: &str) -> Result<String> {
    let mut file = open_regular_nofollow(path)?;
    if file.metadata()?.len() != expected.len() as u64 {
        anyhow::bail!("stored item projection length mismatch");
    }
    let mut found = String::with_capacity(expected.len());
    file.read_to_string(&mut found)?;
    if found != expected {
        anyhow::bail!("stored item projection does not match canonical revision");
    }
    Ok(found)
}
pub fn item_markdown(root: impl AsRef<Path>, item_id: &str) -> Result<String> {
    let (item, revision) = read_item(root.as_ref(), item_id)?;
    let root = existing_canonical_root(root.as_ref())?;
    let item_dir = contained_existing(&root, &root.join("items").join(item_id))?;
    let expected = render_markdown(&item, &revision);
    let path = if item_current_path(&root, item_id).exists() {
        revision_projection_paths(&root, &item, &revision).0
    } else {
        item_dir.join("index.md")
    };
    read_text_projection(&path, &expected)
}
pub fn item_index_json(root: impl AsRef<Path>, item_id: &str) -> Result<String> {
    let (item, revision) = read_item(root.as_ref(), item_id)?;
    let root = existing_canonical_root(root.as_ref())?;
    let item_dir = contained_existing(&root, &root.join("items").join(item_id))?;
    let attachments = item_dir.join("attachments");
    let attachments_metadata = fs::symlink_metadata(&attachments)?;
    if attachments_metadata.file_type().is_symlink() || !attachments_metadata.is_dir() {
        anyhow::bail!("unsafe portable attachments directory");
    }
    contained_existing(&root, &attachments)?;
    let projection_paths = revision_projection_paths(&root, &item, &revision);
    let (markdown_path, index_path) = if item_current_path(&root, item_id).exists() {
        projection_paths
    } else {
        (item_dir.join("index.md"), item_dir.join("index.json"))
    };
    read_text_projection(&markdown_path, &render_markdown(&item, &revision))?;
    let expected = portable_item_index_json(&root, &item, &revision)?;
    read_text_projection(&index_path, &expected)
}
pub fn evidence_links(item: &ItemRevision) -> Vec<EvidenceLink> {
    let attachment_evidence: BTreeSet<&str> = item
        .attachments
        .iter()
        .filter_map(|attachment| match attachment {
            AttachmentRef::Evidence { evidence_id, .. } => Some(evidence_id.as_str()),
            AttachmentRef::Remote { .. } => None,
        })
        .collect();
    item.evidence_ids
        .iter()
        .map(|id| EvidenceLink {
            schema_version: SCHEMA_VERSION,
            evidence_id: id.clone(),
            item_id: item.item_id.clone(),
            revision_id: item.revision_id.clone(),
            role: if attachment_evidence.contains(id.as_str()) {
                "attachment"
            } else {
                "primary"
            }
            .into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, CreatorRole, ItemMetadata, MetadataOrigin,
        MetadataProvenance, Sensitivity, SourceProfile, Sourced, VerifiedBlob,
    };
    use std::{sync::mpsc, thread};
    use tempfile::tempdir;
    pub(crate) fn capture() -> VerifiedCapture {
        VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: "s".into(),
                name: "s".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: Sensitivity::Public,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "a".into(),
                adapter_version: "1".into(),
                external_capture_id: "e".into(),
                source_id: "s".into(),
                captured_at: "2026-01-01T00:00:00Z".into(),
                capture_method: "x".into(),
                sensitivity: Sensitivity::Public,
                route_summary: None,
                title: Some("t".into()),
                source_url: "u".into(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "a".into(),
                    media_type: "text/plain".into(),
                    byte_size: 1,
                    sha256: "x".into(),
                    source_url: None,
                },
                blob_id: "sha256:x".into(),
                source_backend_ref: Some("capture-bundle:test".into()),
                bytes: b"b".to_vec(),
            }],
        }
    }
    pub(crate) fn draft() -> ItemDraft {
        ItemDraft {
            item_type: "webpage".into(),
            title: "t".into(),
            source_url: "u".into(),
            canonical_url: None,
            content_markdown: "b".into(),
            sensitivity: Sensitivity::Public,
            translator_id: "g".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: Vec::new(),
        }
    }
    fn captured_png(capture: &mut VerifiedCapture, draft: &mut ItemDraft) -> String {
        let digest = "4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6";
        capture.blobs.push(VerifiedBlob {
            descriptor: BlobDescriptor {
                schema_version: SCHEMA_VERSION,
                locator: "inline_image_0".into(),
                media_type: "image/png".into(),
                byte_size: 8,
                sha256: digest.into(),
                source_url: Some("https://images.example.invalid/diagram.png?version=1".into()),
            },
            blob_id: format!("sha256:{digest}"),
            source_backend_ref: None,
            bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
        });
        draft.attachments = vec![DraftAttachmentRef::Captured {
            blob_index: 1,
            blob_id: format!("sha256:{digest}"),
            title: Some("Diagram".into()),
        }];
        let relative = format!("attachments/sha256-{digest}.png");
        draft.content_markdown = format!("![Diagram]({relative})");
        relative
    }
    pub(crate) fn seed() -> (tempfile::TempDir, VerifiedCapture, IngestReceipt) {
        let d = tempdir().unwrap();
        let c = capture();
        let r = ingest(d.path(), &c, draft()).unwrap();
        (d, c, r)
    }
    /// Simulates a crash after the commit record but before archiving: the committed journal is
    /// back in the recovery directory and the recovery marker is set.
    pub(crate) fn unarchive_committed(root: &Path, capture: &VerifiedCapture) {
        let name = journal_key_name(&ingest_key(capture)).unwrap().to_owned();
        let archived = root.join(COMMITTED_JOURNALS).join(&name);
        if archived.exists() {
            fs::rename(archived, root.join("catalog/ingest-journal").join(name)).unwrap();
        }
        fs::write(root.join(RECOVERY_MARKER), b"").unwrap();
    }
    /// The live (pending) journal, or its archived copy once it was fully applied.
    pub(crate) fn journal_path(root: &Path, capture: &VerifiedCapture) -> PathBuf {
        let name = journal_key_name(&ingest_key(capture)).unwrap().to_owned();
        let live = root.join("catalog/ingest-journal").join(&name);
        if live.exists() {
            return live.join("journal.json");
        }
        root.join(COMMITTED_JOURNALS)
            .join(name)
            .join("journal.json")
    }
    #[test]
    fn prepared_journal_recovers_identical_ids_and_single_item() {
        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(journal.state, JournalState::Prepared);
        assert_eq!(recover_pending_ingests(d.path()).unwrap(), 1);
        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.capture_id, journal.capture_id);
        assert_eq!(replay.item_ids, [journal.item_id]);
        assert_eq!(list_items(d.path()).unwrap().len(), 1);
        assert!(!path.parent().unwrap().join("entries").exists());
    }
    #[test]
    fn prepared_journal_recovers_owned_replace_residue_only() {
        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let residue = path
            .parent()
            .unwrap()
            .join(format!(".journal-write_{}.tmp", "a".repeat(32)));
        fs::write(&residue, b"partial replacement").unwrap();

        assert_eq!(recover_pending_ingests(d.path()).unwrap(), 1);
        assert!(!residue.exists());
        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.capture_id, journal.capture_id);
        assert_eq!(replay.item_ids, [journal.item_id]);
        assert_eq!(list_items(d.path()).unwrap().len(), 1);

        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let foreign = path
            .parent()
            .unwrap()
            .join(format!(".journal-write_{}.tmp", "b".repeat(31)));
        fs::write(&foreign, b"not Store-owned").unwrap();
        assert!(recover_pending_ingests(d.path()).is_err());
        assert!(foreign.exists());
    }
    #[test]
    fn malformed_committed_journal_fails_closed() {
        let (d, c, _) = seed();
        unarchive_committed(d.path(), &c);
        fs::write(journal_path(d.path(), &c), b"not json").unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
    }
    #[test]
    fn committed_authoritative_non_capture_entry_fails_closed() {
        let (d, c, receipt) = seed();
        fs::write(
            d.path()
                .join("items")
                .join(&receipt.item_ids[0])
                .join("item.json"),
            b"{}",
        )
        .unwrap();
        unarchive_committed(d.path(), &c);
        assert!(recover_pending_ingests(d.path()).is_err());
        assert!(journal_path(d.path(), &c).parent().unwrap().exists());
    }
    #[cfg(unix)]
    #[test]
    fn recovery_keeps_healthy_projections_and_repairs_changed_or_shared_files() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // Projection repair now runs where journals are recovered: at startup and after an
        // interrupted write (here: a committed journal not yet archived), not on every read.
        let (d, c, receipt) = seed();
        unarchive_committed(d.path(), &c);
        let id = &receipt.item_ids[0];
        let paths = [
            d.path().join("integrity/items").join(format!("{id}.json")),
            d.path().join("items").join(id).join("index.md"),
            d.path().join("items").join(id).join("index.json"),
        ];
        let before: Vec<_> = paths
            .iter()
            .map(|p| (fs::metadata(p).unwrap().ino(), fs::read(p).unwrap()))
            .collect();
        with_consistent_snapshot(d.path(), |_| Ok(())).unwrap();
        for (path, (inode, bytes)) in paths.iter().zip(&before) {
            assert_eq!(fs::metadata(path).unwrap().ino(), *inode);
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }

        // A damaged derived file is repaired, not treated as a new authority.
        fs::write(&paths[1], "broken projection").unwrap();
        // Byte equality alone must not retain a public or hard-linked file.
        fs::set_permissions(&paths[0], fs::Permissions::from_mode(0o644)).unwrap();
        let outside = tempdir().unwrap();
        let linked = outside.path().join("retained-copy");
        fs::hard_link(&paths[2], &linked).unwrap();
        unarchive_committed(d.path(), &c);
        with_consistent_snapshot(d.path(), |_| Ok(())).unwrap();
        for (path, (_, bytes)) in paths.iter().zip(&before) {
            let metadata = fs::metadata(path).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        assert_eq!(fs::read(linked).unwrap(), before[2].1);
        assert_eq!(item_markdown(d.path(), id).unwrap().as_bytes(), before[1].1);
    }
    #[test]
    fn abandoned_prepare_cleanup_is_exact_and_hidden_entries_fail_closed() {
        let (d, _, _) = seed();
        let parent = d.path().join("catalog/ingest-journal");
        let abandoned = parent.join(format!(".prepare-journal_{}", "a".repeat(32)));
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join("private"), b"bytes").unwrap();
        assert_eq!(recover_pending_ingests(d.path()).unwrap(), 0);
        assert!(!abandoned.exists());

        fs::create_dir(parent.join(".prepare-not-owned")).unwrap();
        assert!(recover_pending_ingests(d.path()).is_err());
        assert!(parent.join(".prepare-not-owned").exists());
    }
    #[test]
    fn prepared_semantic_link_mutation_fails_closed_even_with_updated_digest() {
        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let mut journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = journal
            .entries
            .iter_mut()
            .find(|entry| entry.path.ends_with("/links.json"))
            .unwrap();
        let staged = path.parent().unwrap().join(&entry.staged);
        let mut links: Vec<EvidenceLink> =
            serde_json::from_slice(&fs::read(&staged).unwrap()).unwrap();
        links.clear();
        let bytes = serde_json::to_vec_pretty(&links).unwrap();
        fs::write(&staged, &bytes).unwrap();
        entry.byte_size = bytes.len() as u64;
        entry.sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        fs::write(&path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
        assert!(recover_pending_ingests(d.path()).is_err());
        assert!(staged.exists());
    }
    #[test]
    fn prepared_projection_provenance_and_extra_attachment_fail_closed() {
        for case in ["projection", "provenance", "extra_attachment"] {
            let d = tempdir().unwrap();
            let c = capture();
            FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
            assert!(ingest(d.path(), &c, draft()).is_err());
            FAIL_APPLY_STEP.with(|value| value.set(None));
            let path = journal_path(d.path(), &c);
            let mut journal: IngestJournal =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            match case {
                "projection" => {
                    let entry = journal
                        .entries
                        .iter_mut()
                        .find(|entry| entry.path.ends_with("/index.json"))
                        .unwrap();
                    let staged = path.parent().unwrap().join(&entry.staged);
                    let mut projection: PortableItemIndex =
                        serde_json::from_slice(&fs::read(&staged).unwrap()).unwrap();
                    projection.title.push_str(" changed");
                    let bytes = serde_json::to_vec_pretty(&projection).unwrap();
                    fs::write(&staged, &bytes).unwrap();
                    entry.byte_size = bytes.len() as u64;
                    entry.sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
                }
                "provenance" => {
                    let entry = journal
                        .entries
                        .iter_mut()
                        .find(|entry| entry.path.contains("/revisions/"))
                        .unwrap();
                    let staged = path.parent().unwrap().join(&entry.staged);
                    let mut revision: ItemRevision =
                        serde_json::from_slice(&fs::read(&staged).unwrap()).unwrap();
                    revision.provenance[0].activity_type = "forged".into();
                    let bytes = serde_json::to_vec_pretty(&revision).unwrap();
                    fs::write(&staged, &bytes).unwrap();
                    entry.byte_size = bytes.len() as u64;
                    entry.sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
                }
                "extra_attachment" => {
                    let staged = format!("entries/{:04}", journal.entries.len());
                    let bytes = b"extra";
                    fs::write(path.parent().unwrap().join(&staged), bytes).unwrap();
                    journal.entries.push(JournalEntry {
                        staged,
                        path: format!(
                            "items/{}/attachments/sha256-{}.png",
                            journal.item_id,
                            "a".repeat(64)
                        ),
                        sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                        byte_size: bytes.len() as u64,
                    });
                }
                _ => unreachable!(),
            }
            fs::write(&path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
            assert!(recover_pending_ingests(d.path()).is_err(), "{case}");
            assert!(path.parent().unwrap().join("entries").exists(), "{case}");
        }
    }
    #[test]
    fn journal_metadata_and_prepared_bytes_fail_closed_on_mutation() {
        let (d, c, _) = seed();
        unarchive_committed(d.path(), &c);
        let path = journal_path(d.path(), &c);
        let mut journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        journal.input_sha256 = format!("sha256:{}", "0".repeat(64));
        fs::write(&path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
        assert!(recover_pending_ingests(d.path()).is_err());

        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        fs::write(
            path.parent().unwrap().join(&journal.entries[0].staged),
            b"mutated staged bytes",
        )
        .unwrap();
        assert!(recover_pending_ingests(d.path()).is_err());
        assert!(!d.path().join("catalog/ingest-keys.json").exists());
    }
    #[test]
    fn interruptions_after_prepared_entries_receipt_and_catalog_recover_stable_ids() {
        for step in 0..=12 {
            let d = tempdir().unwrap();
            let c = capture();
            FAIL_APPLY_STEP.with(|value| value.set(Some(step)));
            assert!(ingest(d.path(), &c, draft()).is_err(), "step {step}");
            FAIL_APPLY_STEP.with(|value| value.set(None));
            let journal: IngestJournal =
                serde_json::from_slice(&fs::read(journal_path(d.path(), &c)).unwrap()).unwrap();
            let replay = ingest(d.path(), &c, draft()).unwrap();
            assert!(replay.idempotent_replay);
            assert_eq!(replay.capture_id, journal.capture_id);
            assert_eq!(replay.item_ids, [journal.item_id]);
            assert_eq!(list_items(d.path()).unwrap().len(), 1);
            assert!(!journal_path(d.path(), &c)
                .parent()
                .unwrap()
                .join("entries")
                .exists());
        }
    }
    #[test]
    fn list_items_empty_does_not_create_items() {
        let d = tempdir().unwrap();
        assert!(list_items(d.path()).unwrap().is_empty());
        assert!(!d.path().join("items").exists());
    }
    #[test]
    fn list_items_returns_sorted_verified_items() {
        let d = tempdir().unwrap();
        let first = ingest(d.path(), &capture(), draft()).unwrap();
        let mut second_capture = capture();
        second_capture.capture.external_capture_id = "e2".into();
        let second = ingest(d.path(), &second_capture, draft()).unwrap();
        let listed = list_items(d.path()).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].0.item_id < listed[1].0.item_id);
        for (item, revision) in listed {
            assert_eq!(item.current_revision, revision.revision_id);
        }
        assert!(first.item_ids[0] != second.item_ids[0]);
    }

    #[test]
    fn owner_metadata_edit_is_an_immutable_current_revision_and_replay_safe() {
        let (d, capture, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let original_revision_id = &receipt.revision_ids[0];
        let item_path = d.path().join("items").join(item_id).join("item.json");
        let integrity_path = d
            .path()
            .join("integrity/items")
            .join(format!("{item_id}.json"));
        let original_revision_path = d
            .path()
            .join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{original_revision_id}.json"));
        let immutable_item = fs::read(&item_path).unwrap();
        let immutable_integrity = fs::read(&integrity_path).unwrap();
        let immutable_revision = fs::read(&original_revision_path).unwrap();

        let updated = revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: original_revision_id.clone(),
                title: "Owner edited title".into(),
                manual_tags: vec!["zeta".into(), "alpha".into(), "alpha".into()],
            },
        )
        .unwrap();
        assert_eq!(updated.previous_revision_id, *original_revision_id);
        assert_eq!(updated.revision_number, 2);
        assert_eq!(fs::read(&item_path).unwrap(), immutable_item);
        assert_eq!(fs::read(&integrity_path).unwrap(), immutable_integrity);
        assert_eq!(
            fs::read(&original_revision_path).unwrap(),
            immutable_revision
        );

        let (item, revision) = read_item(d.path(), item_id).unwrap();
        assert_eq!(item.current_revision, updated.revision_id);
        assert_eq!(revision.revision_number, 2);
        assert_eq!(revision.title, "Owner edited title");
        assert_eq!(revision.content_markdown, "b");
        let tags = revision.metadata.manual_tags.as_ref().unwrap();
        assert_eq!(tags.value, ["alpha", "zeta"]);
        assert_eq!(tags.provenance.origin, MetadataOrigin::OwnerEdit);
        let activity = revision.provenance.last().unwrap();
        assert_eq!(activity.activity_type, "metadata_edit");
        assert_eq!(
            activity.input_ids.as_slice(),
            std::slice::from_ref(original_revision_id)
        );
        let original: ItemRevision = serde_json::from_slice(&immutable_revision).unwrap();
        assert_eq!(
            activity.input_checksums,
            [revision_digest(&original).unwrap()]
        );
        assert_eq!(
            activity.output_ids.as_slice(),
            std::slice::from_ref(&updated.revision_id)
        );
        assert!(item_markdown(d.path(), item_id)
            .unwrap()
            .contains("# Owner edited title"));
        let portable: PortableItemIndex =
            serde_json::from_str(&item_index_json(d.path(), item_id).unwrap()).unwrap();
        assert_eq!(portable.revision_id, updated.revision_id);
        assert_eq!(portable.title, "Owner edited title");

        let replay = ingest(d.path(), &capture, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(read_item(d.path(), item_id).unwrap().1, revision);
        assert!(item_markdown(d.path(), item_id)
            .unwrap()
            .contains("# Owner edited title"));
        assert_eq!(
            revise_item_metadata(
                d.path(),
                item_id,
                ItemMetadataUpdate {
                    expected_revision_id: original_revision_id.clone(),
                    title: "Stale title".into(),
                    manual_tags: Vec::new(),
                },
            )
            .unwrap_err()
            .to_string(),
            "stale_item_revision"
        );
    }

    #[test]
    fn committed_batch_repairs_shared_projection_parents_without_changing_items() {
        let d = tempdir().unwrap();
        let mut originals = Vec::new();
        let mut captures = Vec::new();
        for n in 0..5 {
            let mut c = capture();
            c.capture.external_capture_id = format!("batch-{n}");
            let receipt = ingest(d.path(), &c, draft()).unwrap();
            captures.push(c);
            let id = &receipt.item_ids[0];
            originals.push(read_item(d.path(), id).unwrap());
        }
        for c in &captures {
            unarchive_committed(d.path(), c);
        }
        fs::remove_dir_all(d.path().join("integrity/items")).unwrap();
        let first = d
            .path()
            .join("items")
            .join(&originals[0].0.item_id)
            .join("index.json");
        for (item, _) in &originals[1..] {
            let path = d
                .path()
                .join("items")
                .join(&item.item_id)
                .join("index.json");
            fs::remove_file(&path).unwrap();
            fs::hard_link(&first, path).unwrap();
        }
        with_consistent_snapshot(d.path(), |root| {
            for (item, revision) in &originals {
                let (actual_item, actual_revision) = read_item(root, &item.item_id)?;
                assert_eq!(serde_json::to_vec(&actual_item)?, serde_json::to_vec(item)?);
                assert_eq!(actual_revision, *revision);
                let index: PortableItemIndex =
                    serde_json::from_str(&item_index_json(root, &item.item_id)?)?;
                assert_eq!(index.item_id, item.item_id);
                assert_eq!(index.revision_id, revision.revision_id);
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn reads_skip_journal_recovery_unless_a_write_was_interrupted() {
        let (d, c, receipt) = seed();
        let root = d.path();
        // A fully applied ingest is archived and leaves no recovery marker.
        assert!(journal_path(root, &c).starts_with(root.join(COMMITTED_JOURNALS)));
        assert!(!root.join(RECOVERY_MARKER).exists());
        // A clean read never opens the recovery directory...
        let junk = root.join("catalog/ingest-journal/junk");
        fs::create_dir(&junk).unwrap();
        with_consistent_snapshot(root, |root| read_item(root, &receipt.item_ids[0]).map(drop))
            .unwrap();
        // ...while startup recovery still validates every pending entry and fails closed.
        assert!(recover_pending_ingests(root).is_err());
        fs::remove_dir(&junk).unwrap();

        // An interrupted write leaves the marker; the next write recovers it first.
        let mut interrupted = capture();
        interrupted.capture.external_capture_id = "interrupted".into();
        FAIL_APPLY_STEP.with(|value| value.set(Some(1)));
        assert!(ingest(root, &interrupted, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        assert!(root.join(RECOVERY_MARKER).exists());
        let mut next = capture();
        next.capture.external_capture_id = "next".into();
        ingest(root, &next, draft()).unwrap();
        assert!(!root.join(RECOVERY_MARKER).exists());
        let replay = ingest(root, &interrupted, draft()).unwrap();
        assert!(replay.idempotent_replay);
        read_item(root, &replay.item_ids[0]).unwrap();
        assert!(journal_path(root, &interrupted).starts_with(root.join(COMMITTED_JOURNALS)));

        // An interrupted metadata update is recovered by the next start.
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(Some(2)));
        assert!(revise_item_metadata(
            root,
            &receipt.item_ids[0],
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "After restart".into(),
                manual_tags: vec![],
            },
        )
        .is_err());
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(None));
        assert!(root.join(RECOVERY_MARKER).exists());
        assert_eq!(recover_pending_ingests(root).unwrap(), 1);
        assert!(!root.join(RECOVERY_MARKER).exists());
        assert_eq!(
            read_item(root, &receipt.item_ids[0]).unwrap().1.title,
            "After restart"
        );
    }

    #[test]
    fn concurrent_reads_never_observe_a_half_written_item() {
        let d = tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let writer_root = root.clone();
        let writer = std::thread::spawn(move || {
            for n in 0..6 {
                let mut c = capture();
                c.capture.external_capture_id = format!("concurrent-{n}");
                ingest(&writer_root, &c, draft()).unwrap();
            }
        });
        // Every item visible to a shared-lock reader is complete and verifies.
        let count = |root: &Path| {
            with_consistent_snapshot(root, |root| Ok(list_items(root)?.len())).unwrap()
        };
        let mut largest = 0;
        let mut reads = 0;
        while !writer.is_finished() {
            let seen = count(&root);
            assert!(seen >= largest);
            largest = seen;
            reads += 1;
        }
        writer.join().unwrap();
        assert_eq!(count(&root), 6);
        assert!(reads > 0);
    }

    #[test]
    fn consistent_snapshot_recovers_interrupted_ingest_before_callback() {
        let d = tempdir().unwrap();
        for n in 0..3 {
            let mut committed = capture();
            committed.capture.external_capture_id = format!("committed-before-interruption-{n}");
            ingest(d.path(), &committed, draft()).unwrap();
        }
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));

        let path = journal_path(d.path(), &c);
        let prepared: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(prepared.state, JournalState::Prepared);
        let observed = with_consistent_snapshot(d.path(), |root| {
            let journal: IngestJournal =
                serde_json::from_slice(&fs::read(journal_path(root, &c))?)?;
            let (item, revision) = read_item(root, &prepared.item_id)?;
            let markdown = item_markdown(root, &prepared.item_id)?;
            let index: PortableItemIndex =
                serde_json::from_str(&item_index_json(root, &prepared.item_id)?)?;
            Ok((journal.state, item, revision, markdown, index))
        })
        .unwrap();

        assert_eq!(observed.0, JournalState::Committed);
        assert_eq!(observed.1.item_id, prepared.item_id);
        assert_eq!(observed.1.current_revision, prepared.revision_id);
        assert_eq!(observed.2.revision_id, prepared.revision_id);
        assert!(observed
            .3
            .contains(&format!("revision_id: \"{}\"", prepared.revision_id)));
        assert_eq!(observed.4.revision_id, prepared.revision_id);
        assert!(!path.parent().unwrap().join("entries").exists());
    }

    #[test]
    fn consistent_snapshot_recovers_interrupted_metadata_update_before_callback() {
        let (d, _, receipt) = seed();
        let item_id = receipt.item_ids[0].clone();
        let base_revision_id = receipt.revision_ids[0].clone();
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(Some(1)));
        assert!(revise_item_metadata(
            d.path(),
            &item_id,
            ItemMetadataUpdate {
                expected_revision_id: base_revision_id,
                title: "Recovered metadata title".into(),
                manual_tags: vec!["snapshot".into(), "recovered".into()],
            },
        )
        .is_err());
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(None));

        let journal_path = d
            .path()
            .join("catalog/item-update-journal")
            .join(format!("{item_id}.json"));
        assert!(journal_path.exists());
        let observed = with_consistent_snapshot(d.path(), |root| {
            let (item, revision) = read_item(root, &item_id)?;
            let (pointer, pointer_revision) = load_current_pointer(root, &item_id)?
                .context("metadata update did not publish a current pointer")?;
            let markdown = item_markdown(root, &item_id)?;
            let index: PortableItemIndex = serde_json::from_str(&item_index_json(root, &item_id)?)?;
            let tags = revision
                .metadata
                .manual_tags
                .as_ref()
                .context("metadata update did not publish tags")?;
            if pointer.item.current_revision != revision.revision_id
                || pointer_revision.revision_id != revision.revision_id
                || index.revision_id != revision.revision_id
                || !markdown.contains(&format!("revision_id: \"{}\"", revision.revision_id))
            {
                anyhow::bail!("snapshot projections are mixed across revisions");
            }
            Ok((
                item.current_revision,
                revision.revision_id,
                revision.revision_number,
                revision.title,
                tags.value.clone(),
                index.revision_id,
            ))
        })
        .unwrap();

        assert_eq!(observed.0, observed.1);
        assert_eq!(observed.2, 2);
        assert_eq!(observed.3, "Recovered metadata title");
        assert_eq!(observed.4, ["recovered", "snapshot"]);
        assert_eq!(observed.5, observed.1);
        assert!(!journal_path.exists());
    }

    #[test]
    fn consistent_snapshot_serializes_concurrent_ingest_and_metadata_edit() {
        let (d, capture_one, receipt) = seed();
        let root = d.path().to_path_buf();
        let item_id = receipt.item_ids[0].clone();
        let initial_revision_id = receipt.revision_ids[0].clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let snapshot_item_id = item_id.clone();
        let snapshot = thread::spawn(move || {
            with_consistent_snapshot(root.clone(), |root| {
                entered_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
                let (item, revision) = read_item(root, &snapshot_item_id)?;
                let markdown = item_markdown(root, &snapshot_item_id)?;
                let count = list_items(root)?.len();
                observed_tx
                    .send((item.current_revision, revision.title, markdown, count))
                    .unwrap();
                Ok(())
            })
        });
        entered_rx.recv().unwrap();

        let (attempt_tx, attempt_rx) = mpsc::channel();
        let metadata_root = d.path().to_path_buf();
        let metadata_item_id = item_id.clone();
        let metadata_attempt = attempt_tx.clone();
        let metadata = thread::spawn(move || {
            metadata_attempt.send(()).unwrap();
            revise_item_metadata(
                metadata_root,
                &metadata_item_id,
                ItemMetadataUpdate {
                    expected_revision_id: initial_revision_id,
                    title: "Concurrent metadata".into(),
                    manual_tags: vec!["during-snapshot".into()],
                },
            )
            .is_ok()
        });

        let mut capture_two = capture_one.clone();
        capture_two.capture.external_capture_id = "concurrent-ingest".into();
        let ingest_attempt = attempt_tx;
        let ingest_root = d.path().to_path_buf();
        let ingest_handle = thread::spawn(move || {
            ingest_attempt.send(()).unwrap();
            ingest(ingest_root, &capture_two, draft()).is_ok()
        });

        attempt_rx.recv().unwrap();
        attempt_rx.recv().unwrap();
        continue_tx.send(()).unwrap();
        let observed = observed_rx.recv().unwrap();

        assert_eq!(observed.0, receipt.revision_ids[0]);
        assert_eq!(observed.1, "t");
        assert!(observed
            .2
            .contains(&format!("revision_id: \"{}\"", receipt.revision_ids[0])));
        assert_eq!(observed.3, 1);
        assert!(snapshot.join().unwrap().is_ok());
        assert!(metadata.join().unwrap());
        assert!(ingest_handle.join().unwrap());

        let (_, revision) = read_item(d.path(), &item_id).unwrap();
        assert_eq!(revision.title, "Concurrent metadata");
        assert_eq!(list_items(d.path()).unwrap().len(), 2);
    }

    #[test]
    fn consistent_snapshot_releases_lock_after_callback_error_and_panic() {
        let (d, _, receipt) = seed();
        let callback_error = with_consistent_snapshot(d.path(), |_root| -> Result<()> {
            anyhow::bail!("callback error sentinel")
        });
        assert_eq!(
            callback_error.unwrap_err().to_string(),
            "callback error sentinel"
        );

        let first_update = revise_item_metadata(
            d.path(),
            &receipt.item_ids[0],
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "After callback error".into(),
                manual_tags: vec!["error".into()],
            },
        )
        .unwrap();
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_consistent_snapshot::<()>(d.path(), |_root| {
                panic!("callback panic sentinel");
            })
        }));
        assert!(panic_result.is_err());

        let second_update = revise_item_metadata(
            d.path(),
            &receipt.item_ids[0],
            ItemMetadataUpdate {
                expected_revision_id: first_update.revision_id,
                title: "After callback panic".into(),
                manual_tags: vec!["panic".into()],
            },
        )
        .unwrap();
        assert_eq!(second_update.revision_number, 3);
    }

    #[test]
    fn consistent_snapshot_observes_new_metadata_and_rejects_wrong_required_revision() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let updated = revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "New current title".into(),
                manual_tags: vec!["new".into(), "current".into()],
            },
        )
        .unwrap();
        let stale = revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "Wrong required revision".into(),
                manual_tags: Vec::new(),
            },
        )
        .unwrap_err();
        assert_eq!(stale.to_string(), "stale_item_revision");

        let observed = with_consistent_snapshot(d.path(), |root| {
            let (item, revision) = read_item(root, item_id)?;
            let tags = revision
                .metadata
                .manual_tags
                .as_ref()
                .context("new current tags missing")?;
            Ok((item.current_revision, revision.title, tags.value.clone()))
        })
        .unwrap();
        assert_eq!(observed.0, updated.revision_id);
        assert_eq!(observed.1, "New current title");
        assert_eq!(observed.2, ["current", "new"]);
    }

    #[cfg(unix)]
    #[test]
    fn consistent_snapshot_rejects_unsafe_root_and_journal_symlinks() {
        use std::os::unix::fs::symlink;

        let outside = tempdir().unwrap();
        let library = tempdir().unwrap();
        let root_alias = library.path().join("alias");
        symlink(outside.path(), &root_alias).unwrap();
        let mut callback_called = false;
        assert!(with_consistent_snapshot(&root_alias, |_| {
            callback_called = true;
            Ok(())
        })
        .is_err());
        assert!(!callback_called);

        let catalog = library.path().join("catalog");
        fs::create_dir(&catalog).unwrap();
        let ingest_journal = catalog.join("ingest-journal");
        symlink(outside.path(), &ingest_journal).unwrap();
        assert!(with_consistent_snapshot(library.path(), |_| Ok(())).is_err());
        assert!(ingest_journal.is_symlink());

        fs::remove_file(&ingest_journal).unwrap();
        fs::create_dir(&ingest_journal).unwrap();
        let item_journal = catalog.join("item-update-journal");
        symlink(outside.path(), &item_journal).unwrap();
        assert!(with_consistent_snapshot(library.path(), |_| Ok(())).is_err());
        assert!(item_journal.is_symlink());
        assert!(outside.path().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn owner_metadata_edit_recovers_every_commit_boundary_and_detects_chain_tampering() {
        for step in 0..=4 {
            let (d, _, receipt) = seed();
            let item_id = &receipt.item_ids[0];
            FAIL_ITEM_UPDATE_STEP.with(|value| value.set(Some(step)));
            let result = revise_item_metadata(
                d.path(),
                item_id,
                ItemMetadataUpdate {
                    expected_revision_id: receipt.revision_ids[0].clone(),
                    title: format!("Recovered title {step}"),
                    manual_tags: vec!["recovered".into()],
                },
            );
            FAIL_ITEM_UPDATE_STEP.with(|value| value.set(None));
            assert!(result.is_err(), "step {step}");
            if step == 0 {
                revise_item_metadata(
                    d.path(),
                    item_id,
                    ItemMetadataUpdate {
                        expected_revision_id: receipt.revision_ids[0].clone(),
                        title: format!("Recovered title {step}"),
                        manual_tags: vec!["recovered".into()],
                    },
                )
                .unwrap();
            } else if step < 4 {
                assert_eq!(recover_pending_ingests(d.path()).unwrap(), 1, "step {step}");
            }
            let (_, revision) = read_item(d.path(), item_id).unwrap();
            assert_eq!(revision.title, format!("Recovered title {step}"));
            assert_eq!(revision.revision_number, 2);
            assert!(item_markdown(d.path(), item_id)
                .unwrap()
                .contains(&format!("# Recovered title {step}")));
        }

        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let first = revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "First owner title".into(),
                manual_tags: vec!["first".into()],
            },
        )
        .unwrap();
        revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: first.revision_id.clone(),
                title: "Second owner title".into(),
                manual_tags: vec!["second".into()],
            },
        )
        .unwrap();
        let prior_path = d
            .path()
            .join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{}.json", first.revision_id));
        let mut prior: ItemRevision =
            serde_json::from_slice(&fs::read(&prior_path).unwrap()).unwrap();
        prior.title = "Tampered prior title".into();
        fs::write(prior_path, serde_json::to_vec_pretty(&prior).unwrap()).unwrap();
        assert!(read_item(d.path(), item_id).is_err());
    }

    #[test]
    fn metadata_scan_streams_verified_fields_without_retaining_bodies() {
        let d = tempdir().unwrap();
        let mut value = draft();
        value.title = "Synthetic metadata title".into();
        value.content_markdown = "x".repeat(2 * 1024 * 1024);
        value.metadata.published_date = Some(Sourced {
            value: "2026-08-10".into(),
            provenance: MetadataProvenance {
                origin: MetadataOrigin::IntakeProvided,
                provider_id: "test.intake".into(),
                provider_version: "1".into(),
                source_refs: Vec::new(),
            },
        });
        let receipt = ingest(d.path(), &capture(), value).unwrap();

        let mut found = Vec::new();
        scan_item_metadata_at_sensitivity(d.path(), Sensitivity::Public, |metadata| {
            found.push(metadata);
            Ok(())
        })
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].item_id, receipt.item_ids[0]);
        assert_eq!(found[0].title, "Synthetic metadata title");
        assert_eq!(
            found[0].captured_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            found[0].metadata.published_date.as_ref().unwrap().value,
            "2026-08-10"
        );

        let revision_path = d
            .path()
            .join("items")
            .join(&receipt.item_ids[0])
            .join("revisions")
            .join(format!("{}.json", receipt.revision_ids[0]));
        let mut revision: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&revision_path).unwrap()).unwrap();
        revision["title"] = serde_json::json!("tampered title");
        fs::write(revision_path, serde_json::to_vec(&revision).unwrap()).unwrap();
        assert!(
            scan_item_metadata_at_sensitivity(d.path(), Sensitivity::Public, |_| Ok(())).is_err()
        );
    }
    #[test]
    fn metadata_page_bounds_retention_and_work_and_invalidates_removed_cursor() {
        let (d, _, receipt) = seed();
        let original_id = &receipt.item_ids[0];
        let (template_item, template_revision) = read_item(d.path(), original_id).unwrap();
        fs::remove_dir_all(d.path().join("items").join(original_id)).unwrap();
        fs::remove_file(
            d.path()
                .join("integrity/items")
                .join(format!("{original_id}.json")),
        )
        .unwrap();
        for index in 0..75 {
            let item_id = format!("item_{index:032x}");
            let revision_id = format!("revision_{index:032x}");
            let mut item = template_item.clone();
            item.item_id.clone_from(&item_id);
            item.current_revision.clone_from(&revision_id);
            let mut revision = template_revision.clone();
            revision.item_id.clone_from(&item_id);
            revision.revision_id.clone_from(&revision_id);
            revision.title = format!("Synthetic page item {index:03}");
            let revisions = d.path().join("items").join(&item_id).join("revisions");
            fs::create_dir_all(&revisions).unwrap();
            fs::write(
                d.path().join("items").join(&item_id).join("item.json"),
                serde_json::to_vec(&item).unwrap(),
            )
            .unwrap();
            fs::write(
                revisions.join(format!("{revision_id}.json")),
                serde_json::to_vec(&revision).unwrap(),
            )
            .unwrap();
            fs::write(
                d.path()
                    .join("integrity/items")
                    .join(format!("{item_id}.json")),
                serde_json::to_vec(&integrity_manifest(&item, &revision).unwrap()).unwrap(),
            )
            .unwrap();
        }

        let (page, cursor, work) = page_all_item_metadata_with_scan_limit(
            d.path(),
            None,
            MAX_ITEM_METADATA_PAGE_SIZE,
            MAX_ITEM_METADATA_PAGE_SCAN_ENTRIES,
        )
        .unwrap();
        assert_eq!(page.len(), MAX_ITEM_METADATA_PAGE_SIZE);
        assert_eq!(work.scanned_entries, 75);
        assert_eq!(work.peak_retained_ids, MAX_ITEM_METADATA_PAGE_SIZE + 1);
        assert_eq!(work.metadata_reads, MAX_ITEM_METADATA_PAGE_SIZE + 1);
        assert!(page
            .windows(2)
            .all(|pair| pair[0].item_id > pair[1].item_id));
        let cursor = cursor.unwrap();
        assert_eq!(page.last().unwrap().item_id, cursor);

        fs::remove_dir_all(d.path().join("items").join(&cursor)).unwrap();
        assert_eq!(
            page_all_item_metadata(d.path(), Some(&cursor), MAX_ITEM_METADATA_PAGE_SIZE)
                .unwrap_err()
                .to_string(),
            "invalid_cursor"
        );

        let over_limit = tempdir().unwrap();
        let item_dir = over_limit.path().join("items");
        fs::create_dir(&item_dir).unwrap();
        for index in 0..65 {
            fs::create_dir(item_dir.join(format!("item_{index:04}"))).unwrap();
        }
        assert_eq!(
            page_all_item_metadata_with_scan_limit(over_limit.path(), None, 50, 64)
                .unwrap_err()
                .to_string(),
            "library_scan_limit_exceeded"
        );
        assert_eq!(MAX_ITEM_METADATA_PAGE_SCAN_ENTRIES, 4096);
    }
    #[test]
    fn metadata_reads_gate_hidden_bodies_and_defer_attachment_bytes() {
        let d = tempdir().unwrap();
        let mut public_capture = capture();
        let mut public_draft = draft();
        let attachment = captured_png(&mut public_capture, &mut public_draft);
        let public = ingest(d.path(), &public_capture, public_draft).unwrap();
        fs::write(
            d.path()
                .join("items")
                .join(&public.item_ids[0])
                .join(&attachment),
            b"TAMPERED",
        )
        .unwrap();
        assert!(read_item(d.path(), &public.item_ids[0]).is_err());
        assert!(read_item_metadata_at_sensitivity(
            d.path(),
            &public.item_ids[0],
            Sensitivity::Public
        )
        .unwrap()
        .is_some());
        fs::write(
            d.path()
                .join("items")
                .join(&public.item_ids[0])
                .join(attachment),
            b"\x89PNG\r\n\x1a\n",
        )
        .unwrap();

        let mut private_capture = capture();
        private_capture.capture.external_capture_id = "private".into();
        private_capture.capture.sensitivity = Sensitivity::Private;
        let mut private_draft = draft();
        private_draft.sensitivity = Sensitivity::Private;
        let private = ingest(d.path(), &private_capture, private_draft).unwrap();
        let private_revision = d
            .path()
            .join("items")
            .join(&private.item_ids[0])
            .join("revisions")
            .join(format!("{}.json", private.revision_ids[0]));
        fs::write(private_revision, b"not json").unwrap();

        let mut visible = Vec::new();
        scan_item_metadata_at_sensitivity(d.path(), Sensitivity::Public, |metadata| {
            visible.push(metadata.item_id);
            Ok(())
        })
        .unwrap();
        assert_eq!(visible, public.item_ids);
        assert!(read_item_metadata_at_sensitivity(
            d.path(),
            &private.item_ids[0],
            Sensitivity::Public
        )
        .unwrap()
        .is_none());
        assert!(
            read_item_metadata_at_sensitivity(d.path(), "item_missing", Sensitivity::Public)
                .unwrap()
                .is_none()
        );
    }
    #[cfg(unix)]
    #[test]
    fn list_items_rejects_symlink_entry() {
        use std::os::unix::fs::symlink;
        let (d, _, _) = seed();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("sentinel"), "x").unwrap();
        symlink(outside.path(), d.path().join("items").join("item_escape")).unwrap();
        assert!(list_items(d.path()).is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("sentinel")).unwrap(),
            "x"
        );
    }
    #[test]
    fn list_items_rejects_corrupt_item() {
        let (d, _, r) = seed();
        fs::write(
            d.path()
                .join("items")
                .join(&r.item_ids[0])
                .join("item.json"),
            "{}",
        )
        .unwrap();
        assert!(list_items(d.path()).is_err());
    }
    #[test]
    fn integrity_manifest_rejects_valid_revision_content_tamper() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        assert!(d
            .path()
            .join("integrity/items")
            .join(format!("{item_id}.json"))
            .exists());
        let item: Item = serde_json::from_str(
            &fs::read_to_string(d.path().join("items").join(item_id).join("item.json")).unwrap(),
        )
        .unwrap();
        let path = d
            .path()
            .join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{}.json", item.current_revision));
        let mut revision: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        revision["content_markdown"] = serde_json::json!("TAMPERED_VALID_BODY");
        fs::write(path, serde_json::to_string(&revision).unwrap()).unwrap();
        assert!(read_item(d.path(), item_id).is_err());
    }
    #[test]
    fn replay_backfills_missing_integrity_manifest_after_full_graph_validation() {
        let (d, c, receipt) = seed();
        let path = d
            .path()
            .join("integrity/items")
            .join(format!("{}.json", receipt.item_ids[0]));
        fs::remove_file(&path).unwrap();
        assert!(read_item(d.path(), &receipt.item_ids[0]).is_err());
        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert!(path.exists());
        assert!(read_item(d.path(), &receipt.item_ids[0]).is_ok());
    }
    #[test]
    fn replay_reuses_ids() {
        let d = tempdir().unwrap();
        let c = capture();
        let draft = ItemDraft {
            item_type: "webpage".into(),
            title: "t".into(),
            source_url: "u".into(),
            canonical_url: None,
            content_markdown: "b".into(),
            sensitivity: Sensitivity::Public,
            translator_id: "g".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: Vec::new(),
        };
        let first = ingest(d.path(), &c, draft.clone()).unwrap();
        let replay = ingest(d.path(), &c, draft).unwrap();
        assert_eq!(first.item_ids, replay.item_ids);
        assert!(replay.idempotent_replay);
    }
    #[test]
    fn historical_v1_capture_without_fingerprint_replays_via_structural_fallback() {
        let (d, c, receipt) = seed();
        fs::remove_dir_all(d.path().join("catalog/ingest-journal")).unwrap();
        let capture_path = d
            .path()
            .join("captures")
            .join(&receipt.capture_id)
            .join("capture.json");
        let mut capture_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&capture_path).unwrap()).unwrap();
        assert!(capture_json
            .as_object_mut()
            .unwrap()
            .remove("ingest_input_sha256")
            .is_some());
        assert!(capture_json
            .as_object_mut()
            .unwrap()
            .remove("evidence_descriptor_version")
            .is_some());
        fs::write(
            &capture_path,
            serde_json::to_string_pretty(&capture_json).unwrap(),
        )
        .unwrap();
        for evidence_id in &receipt.evidence_ids {
            let path = d
                .path()
                .join("evidence")
                .join(evidence_id)
                .join("evidence.json");
            let mut evidence: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            assert!(evidence
                .as_object_mut()
                .unwrap()
                .remove("descriptor")
                .is_some());
            fs::write(path, serde_json::to_string_pretty(&evidence).unwrap()).unwrap();
        }
        let revision_path = d
            .path()
            .join("items")
            .join(&receipt.item_ids[0])
            .join("revisions")
            .join(format!("{}.json", receipt.revision_ids[0]));
        let revision_json = fs::read_to_string(revision_path).unwrap();
        assert!(!revision_json.contains("\"metadata\""));
        assert!(!revision_json.contains("\"attachments\""));
        let item_dir = d.path().join("items").join(&receipt.item_ids[0]);
        let portable_path = item_dir.join("index.json");
        let canonical_paths = std::iter::once(capture_path.clone())
            .chain(std::iter::once(item_dir.join("item.json")))
            .chain(std::iter::once(
                item_dir
                    .join("revisions")
                    .join(format!("{}.json", receipt.revision_ids[0])),
            ))
            .chain(receipt.evidence_ids.iter().map(|evidence_id| {
                d.path()
                    .join("evidence")
                    .join(evidence_id)
                    .join("evidence.json")
            }))
            .collect::<Vec<_>>();
        let canonical_before = canonical_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>();
        fs::remove_file(&portable_path).unwrap();
        assert!(item_index_json(d.path(), &receipt.item_ids[0]).is_err());

        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(receipt.item_ids, replay.item_ids);
        assert_eq!(receipt.revision_ids, replay.revision_ids);
        assert!(portable_path.is_file());
        assert!(item_index_json(d.path(), &receipt.item_ids[0]).is_ok());
        for (path, expected) in canonical_paths.iter().zip(canonical_before) {
            assert_eq!(fs::read(path).unwrap(), expected);
        }
    }
    #[test]
    fn historical_v1_capture_with_fingerprint_replays_without_new_descriptor_fields() {
        let (d, c, receipt) = seed();
        fs::remove_dir_all(d.path().join("catalog/ingest-journal")).unwrap();
        let capture_path = d
            .path()
            .join("captures")
            .join(&receipt.capture_id)
            .join("capture.json");
        let mut capture_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&capture_path).unwrap()).unwrap();
        assert!(capture_json.get("ingest_input_sha256").is_some());
        assert!(capture_json
            .as_object_mut()
            .unwrap()
            .remove("evidence_descriptor_version")
            .is_some());
        fs::write(
            capture_path,
            serde_json::to_string_pretty(&capture_json).unwrap(),
        )
        .unwrap();
        for evidence_id in &receipt.evidence_ids {
            let path = d
                .path()
                .join("evidence")
                .join(evidence_id)
                .join("evidence.json");
            let mut evidence: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            assert!(evidence
                .as_object_mut()
                .unwrap()
                .remove("descriptor")
                .is_some());
            fs::write(path, serde_json::to_string_pretty(&evidence).unwrap()).unwrap();
        }

        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(receipt.item_ids, replay.item_ids);
        assert_eq!(receipt.revision_ids, replay.revision_ids);
    }
    #[test]
    fn metadata_and_remote_attachment_are_canonical_and_replay_stable() {
        let d = tempdir().unwrap();
        let c = capture();
        let provenance = MetadataProvenance {
            origin: MetadataOrigin::IntakeProvided,
            provider_id: "test.intake".into(),
            provider_version: "1".into(),
            source_refs: Vec::new(),
        };
        let mut value = draft();
        value.metadata = ItemMetadata {
            creators: Some(Sourced {
                value: vec![Creator::new("Synthetic Author", CreatorRole::Author)],
                provenance: provenance.clone(),
            }),
            identifiers: Some(Sourced {
                value: vec![ItemIdentifier::new(IdentifierKind::Doi, "10.1/synthetic")],
                provenance: provenance.clone(),
            }),
            manual_tags: Some(Sourced {
                value: vec!["rust".into(), "synthetic".into()],
                provenance,
            }),
            ..Default::default()
        };
        value.attachments = vec![DraftAttachmentRef::Remote {
            url: "https://example.invalid/synthetic.pdf".into(),
            media_type: Some("application/pdf".into()),
            title: Some("Synthetic PDF".into()),
        }];

        let first = ingest(d.path(), &c, value.clone()).unwrap();
        let (_, revision) = read_item(d.path(), &first.item_ids[0]).unwrap();
        assert_eq!(revision.metadata, value.metadata);
        assert_eq!(
            revision.attachments,
            vec![AttachmentRef::Remote {
                url: "https://example.invalid/synthetic.pdf".into(),
                media_type: Some("application/pdf".into()),
                title: Some("Synthetic PDF".into()),
            }]
        );
        let portable: PortableItemIndex =
            serde_json::from_str(&item_index_json(d.path(), &first.item_ids[0]).unwrap()).unwrap();
        assert_eq!(portable.metadata, value.metadata);
        assert_eq!(
            portable.attachments,
            vec![PortableAttachment::Remote {
                url: "https://example.invalid/synthetic.pdf".into(),
                media_type: Some("application/pdf".into()),
                title: Some("Synthetic PDF".into()),
            }]
        );
        assert_eq!(first.evidence_ids.len(), c.blobs.len());
        let replay = ingest(d.path(), &c, value.clone()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(first.item_ids, replay.item_ids);
        value
            .metadata
            .manual_tags
            .as_mut()
            .unwrap()
            .value
            .push("updated".into());
        assert!(ingest(d.path(), &c, value).is_err());
        assert_eq!(list_items(d.path()).unwrap().len(), 1);
    }
    #[test]
    fn portable_item_index_is_body_free_enriched_and_replay_repairs_projections() {
        let d = tempdir().unwrap();
        let mut capture = capture();
        let mut value = draft();
        value.content_markdown = "PORTABLE_BODY_SENTINEL".into();
        value.metadata.published_date = Some(Sourced {
            value: "2026-08-10".into(),
            provenance: MetadataProvenance {
                origin: MetadataOrigin::IntakeProvided,
                provider_id: "test.intake".into(),
                provider_version: "1".into(),
                source_refs: Vec::new(),
            },
        });
        let relative = captured_png(&mut capture, &mut value);
        let first = ingest(d.path(), &capture, value.clone()).unwrap();
        let item_dir = d.path().join("items").join(&first.item_ids[0]);
        let markdown = item_markdown(d.path(), &first.item_ids[0]).unwrap();
        let portable_json = item_index_json(d.path(), &first.item_ids[0]).unwrap();
        assert_eq!(
            fs::read_to_string(item_dir.join("index.json")).unwrap(),
            portable_json
        );
        assert!(!portable_json.contains("PORTABLE_BODY_SENTINEL"));
        assert!(!portable_json.contains("content_markdown"));
        assert!(!portable_json.contains("source_backend_ref"));
        assert!(!portable_json.contains("backend_ref"));
        assert!(!portable_json.contains("capture-bundle:test"));

        let portable: PortableItemIndex = serde_json::from_str(&portable_json).unwrap();
        assert_eq!(portable.projection, "memex.portable-item");
        assert_eq!(portable.projection_version, 1);
        assert_eq!(portable.item_id, first.item_ids[0]);
        assert_eq!(portable.revision_id, first.revision_ids[0]);
        assert_eq!(
            portable.captured_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            portable.translator,
            Some(PortableTranslator {
                id: "g".into(),
                version: "1".into(),
            })
        );
        assert_eq!(
            portable.metadata.published_date.unwrap().value,
            "2026-08-10"
        );
        assert_eq!(portable.content.path, "index.md");
        assert_eq!(portable.content.byte_size, markdown.len() as u64);
        assert_eq!(
            portable.content.sha256,
            format!("sha256:{:x}", Sha256::digest(markdown.as_bytes()))
        );
        assert_eq!(portable.attachments.len(), 1);
        let PortableAttachment::Evidence {
            evidence_id,
            path,
            media_type,
            byte_size,
            sha256,
            source_url,
            title,
        } = &portable.attachments[0]
        else {
            panic!("captured image was not projected as portable Evidence")
        };
        assert_eq!(evidence_id, &first.evidence_ids[1]);
        assert_eq!(path, &relative);
        assert_eq!(media_type, "image/png");
        assert_eq!(*byte_size, 8);
        assert_eq!(
            sha256,
            "sha256:4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6"
        );
        assert_eq!(
            source_url.as_deref(),
            Some("https://images.example.invalid/diagram.png?version=1")
        );
        assert_eq!(title.as_deref(), Some("Diagram"));
        assert!(fs::symlink_metadata(item_dir.join("attachments"))
            .unwrap()
            .is_dir());

        fs::write(item_dir.join("index.md"), "TAMPERED MARKDOWN").unwrap();
        assert!(item_index_json(d.path(), &first.item_ids[0]).is_err());
        fs::write(item_dir.join("index.json"), "{\"tampered\":true}\n").unwrap();
        assert!(item_markdown(d.path(), &first.item_ids[0]).is_err());
        assert!(item_index_json(d.path(), &first.item_ids[0]).is_err());
        let replay = ingest(d.path(), &capture, value).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(
            item_markdown(d.path(), &first.item_ids[0]).unwrap(),
            markdown
        );
        assert_eq!(
            item_index_json(d.path(), &first.item_ids[0]).unwrap(),
            portable_json
        );
    }
    #[cfg(unix)]
    #[test]
    fn portable_projection_replay_preflights_all_paths_before_repair() {
        use std::os::unix::fs::symlink;

        let (d, c, receipt) = seed();
        let item_dir = d.path().join("items").join(&receipt.item_ids[0]);
        let markdown = item_dir.join("index.md");
        let index = item_dir.join("index.json");
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "OUTSIDE_SENTINEL").unwrap();
        fs::write(&markdown, "TAMPERED MARKDOWN").unwrap();
        fs::remove_file(&index).unwrap();
        symlink(outside.path(), &index).unwrap();

        assert!(ingest(d.path(), &c, draft()).is_err());
        assert_eq!(fs::read_to_string(&markdown).unwrap(), "TAMPERED MARKDOWN");
        assert!(fs::symlink_metadata(&index)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(outside.path()).unwrap(),
            "OUTSIDE_SENTINEL"
        );
    }
    #[test]
    fn portable_package_keeps_empty_attachments_directory_replay_stable() {
        let (d, c, receipt) = seed();
        let attachments = d
            .path()
            .join("items")
            .join(&receipt.item_ids[0])
            .join("attachments");
        assert!(fs::symlink_metadata(&attachments).unwrap().is_dir());
        fs::remove_dir(&attachments).unwrap();
        assert!(item_index_json(d.path(), &receipt.item_ids[0]).is_err());

        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert!(fs::symlink_metadata(&attachments).unwrap().is_dir());
        assert!(item_index_json(d.path(), &receipt.item_ids[0]).is_ok());
    }
    #[test]
    fn chunked_image_on_legacy_head_verifies_only_the_requested_attachment() {
        let d = tempdir().unwrap();
        let mut capture = capture();
        let mut draft = draft();
        let first = captured_png(&mut capture, &mut draft);
        let mut second = capture.blobs[1].clone();
        second.bytes.push(7);
        second.descriptor.byte_size = second.bytes.len() as u64;
        second.descriptor.sha256 = format!("{:x}", Sha256::digest(&second.bytes));
        second.descriptor.locator = "inline_image_1".into();
        second.descriptor.source_url = Some("https://images.example.invalid/second.png".into());
        second.blob_id = format!("sha256:{}", second.descriptor.sha256);
        let second_path = format!("attachments/sha256-{}.png", second.descriptor.sha256);
        draft.attachments.push(DraftAttachmentRef::Captured { blob_index: 2, blob_id: second.blob_id.clone(), title: None });
        capture.blobs.push(second);
        let receipt = ingest(d.path(), &capture, draft).unwrap();
        let (id, rev) = (&receipt.item_ids[0], &receipt.revision_ids[0]);
        assert!(!item_current_path(d.path(), id).exists());
        assert_eq!(read_image(d.path(), id, rev, &first, 0, Sensitivity::Public).unwrap().bytes, b"\x89PNG\r\n\x1a\n");
        fs::write(d.path().join("items").join(id).join(second_path), b"corrupted").unwrap();
        // A selected image has an independent, verified read boundary. Full
        // item callers retain their previous all-attachment integrity contract.
        assert!(read_item(d.path(), id).is_err());
        assert_eq!(read_image(d.path(), id, rev, &first, 0, Sensitivity::Public).unwrap().bytes, b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn captured_image_is_evidence_backed_and_replay_repairs_only_missing_projection() {
        let d = tempdir().unwrap();
        let mut capture = capture();
        let mut draft = draft();
        let relative = captured_png(&mut capture, &mut draft);
        let first = ingest(d.path(), &capture, draft.clone()).unwrap();
        let (item, revision) = read_item(d.path(), &first.item_ids[0]).unwrap();
        let AttachmentRef::Evidence {
            evidence_id,
            relative_path,
            title,
        } = &revision.attachments[0]
        else {
            panic!("captured image did not resolve to Evidence")
        };
        assert_eq!(relative_path, &relative);
        assert_eq!(title.as_deref(), Some("Diagram"));
        assert_eq!(evidence_id, &first.evidence_ids[1]);
        let evidence: Evidence = load(
            &d.path().canonicalize().unwrap(),
            d.path()
                .join("evidence")
                .join(evidence_id)
                .join("evidence.json"),
        )
        .unwrap();
        assert_eq!(
            evidence.descriptor.as_ref(),
            Some(&capture.blobs[1].descriptor)
        );
        assert_eq!(
            evidence.backend_ref,
            format!("memex-store:{}/{relative}", item.item_id)
        );
        let links: Vec<EvidenceLink> = load(
            &d.path().canonicalize().unwrap(),
            d.path()
                .join("items")
                .join(&item.item_id)
                .join("links.json"),
        )
        .unwrap();
        assert_eq!(links[0].role, "primary");
        assert_eq!(links[1].role, "attachment");
        let path = d.path().join("items").join(&item.item_id).join(&relative);
        assert_eq!(fs::read(&path).unwrap(), b"\x89PNG\r\n\x1a\n");
        let attachment = read_item_attachment_at_sensitivity(
            d.path(),
            &item.item_id,
            &relative,
            Sensitivity::Public,
        )
        .unwrap()
        .unwrap();
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!(attachment.bytes, b"\x89PNG\r\n\x1a\n");
        assert!(read_item_attachment_at_sensitivity(
            d.path(),
            &item.item_id,
            "attachments/sha256-missing.png",
            Sensitivity::Public,
        )
        .unwrap()
        .is_none());

        fs::remove_file(&path).unwrap();
        assert!(ingest(d.path(), &capture, draft.clone()).is_err());
        assert!(!path.exists());
        assert!(journal_path(d.path(), &capture).parent().unwrap().exists());

        fs::write(&path, b"BADIMAGE").unwrap();
        assert!(read_item(d.path(), &item.item_id).is_err());
        assert!(ingest(d.path(), &capture, draft).is_err());
        assert_eq!(fs::read(path).unwrap(), b"BADIMAGE");
    }

    #[test]
    fn committed_attachment_mismatch_retains_unfinished_staging() {
        let d = tempdir().unwrap();
        let mut capture = capture();
        let mut draft = draft();
        let relative = captured_png(&mut capture, &mut draft);
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &capture, draft).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let journal_path = journal_path(d.path(), &capture);
        let mut journal: IngestJournal =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        FAIL_APPLY_STEP.with(|value| value.set(Some(journal.entries.len() + 2)));
        assert!(apply_journal(
            &d.path().canonicalize().unwrap(),
            &journal_path.parent().unwrap().canonicalize().unwrap(),
            &mut journal,
        )
        .is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        assert_eq!(journal.state, JournalState::Committed);
        let entries = journal_path.parent().unwrap().join("entries");
        assert!(entries.is_dir());
        fs::remove_file(d.path().join("items").join(&journal.item_id).join(relative)).unwrap();
        let result = recover_pending_ingests(d.path());
        assert!(result.is_err(), "unexpected recovery result: {result:?}");
        assert!(entries.is_dir());
    }

    #[test]
    fn captured_image_rejects_evidence_sensitivity_escalation() {
        let d = tempdir().unwrap();
        let mut capture = capture();
        let mut draft = draft();
        let relative = captured_png(&mut capture, &mut draft);
        let receipt = ingest(d.path(), &capture, draft.clone()).unwrap();
        let evidence_path = d
            .path()
            .join("evidence")
            .join(&receipt.evidence_ids[1])
            .join("evidence.json");
        let mut evidence: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&evidence_path).unwrap()).unwrap();
        evidence["sensitivity"] = serde_json::json!("restricted");
        fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap()).unwrap();

        assert!(read_item(d.path(), &receipt.item_ids[0]).is_err());
        assert!(read_item_attachment_at_sensitivity(
            d.path(),
            &receipt.item_ids[0],
            &relative,
            Sensitivity::Restricted,
        )
        .is_err());
        assert!(ingest(d.path(), &capture, draft).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn captured_attachment_symlinks_fail_read_and_replay_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let d = tempdir().unwrap();
        let mut capture_one = capture();
        let mut draft_one = draft();
        let relative = captured_png(&mut capture_one, &mut draft_one);
        let receipt = ingest(d.path(), &capture_one, draft_one.clone()).unwrap();
        let item_dir = d.path().join("items").join(&receipt.item_ids[0]);
        let attachment = item_dir.join(&relative);
        let same_bytes = item_dir.join("same-bytes.png");
        fs::write(&same_bytes, b"\x89PNG\r\n\x1a\n").unwrap();
        fs::remove_file(&attachment).unwrap();
        symlink(&same_bytes, &attachment).unwrap();
        assert!(read_item(d.path(), &receipt.item_ids[0]).is_err());
        assert!(ingest(d.path(), &capture_one, draft_one).is_err());
        assert!(fs::symlink_metadata(&attachment)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&same_bytes).unwrap(), b"\x89PNG\r\n\x1a\n");

        let d = tempdir().unwrap();
        let mut capture_two = capture();
        let mut draft_two = draft();
        let relative = captured_png(&mut capture_two, &mut draft_two);
        let receipt = ingest(d.path(), &capture_two, draft_two.clone()).unwrap();
        let item_dir = d.path().join("items").join(&receipt.item_ids[0]);
        let attachments = item_dir.join("attachments");
        let real_attachments = item_dir.join("attachments-real");
        fs::rename(&attachments, &real_attachments).unwrap();
        symlink(&real_attachments, &attachments).unwrap();
        assert!(read_item(d.path(), &receipt.item_ids[0]).is_err());
        assert!(ingest(d.path(), &capture_two, draft_two).is_err());
        assert!(fs::symlink_metadata(&attachments)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(real_attachments.join(Path::new(&relative).file_name().unwrap())).unwrap(),
            b"\x89PNG\r\n\x1a\n"
        );

        let d = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let mut capture_three = capture();
        let mut draft_three = draft();
        let relative = captured_png(&mut capture_three, &mut draft_three);
        let receipt = ingest(d.path(), &capture_three, draft_three.clone()).unwrap();
        let item_dir = d.path().join("items").join(&receipt.item_ids[0]);
        let attachments = item_dir.join("attachments");
        fs::remove_dir_all(&attachments).unwrap();
        let outside_attachment = outside
            .path()
            .join(Path::new(&relative).file_name().unwrap());
        fs::write(&outside_attachment, b"\x89PNG\r\n\x1a\n").unwrap();
        symlink(outside.path(), &attachments).unwrap();
        assert!(read_item(d.path(), &receipt.item_ids[0]).is_err());
        assert!(ingest(d.path(), &capture_three, draft_three).is_err());
        assert_eq!(fs::read(outside_attachment).unwrap(), b"\x89PNG\r\n\x1a\n");
    }
    #[test]
    fn replay_rejects_receipt_capture_revision_and_link_tamper() {
        let (d, c, r) = seed();
        let receipt_path = d
            .path()
            .join("receipts")
            .join(format!("{}.json", r.capture_id));
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&receipt_path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(9);
        fs::write(&receipt_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
        let (d, c, r) = seed();
        let path = d
            .path()
            .join("captures")
            .join(&r.capture_id)
            .join("capture.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["external_capture_id"] = serde_json::json!("tampered");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
        let (d, c, r) = seed();
        let path = d
            .path()
            .join("items")
            .join(&r.item_ids[0])
            .join("revisions")
            .join(format!("{}.json", r.revision_ids[0]));
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["evidence_ids"] = serde_json::json!([]);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
        let (d, c, r) = seed();
        let path = d
            .path()
            .join("items")
            .join(&r.item_ids[0])
            .join("links.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value[0]["item_id"] = serde_json::json!("item_wrong");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn replay_and_item_read_reject_library_symlink_escape() {
        use std::os::unix::fs::symlink;
        let (d, c, r) = seed();
        let receipt = d
            .path()
            .join("receipts")
            .join(format!("{}.json", r.capture_id));
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::copy(&receipt, outside.path()).unwrap();
        fs::remove_file(&receipt).unwrap();
        symlink(outside.path(), &receipt).unwrap();
        assert!(ingest(d.path(), &c, draft()).is_err());
        let (d, _c, r) = seed();
        let index = d.path().join("items").join(&r.item_ids[0]).join("index.md");
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "outside").unwrap();
        fs::remove_file(&index).unwrap();
        symlink(outside.path(), &index).unwrap();
        assert!(item_markdown(d.path(), &r.item_ids[0]).is_err());
        let (d, c, r) = seed();
        let item_dir = d.path().join("items").join(&r.item_ids[0]);
        let index = item_dir.join("index.json");
        let same_bytes = item_dir.join("same-index.json");
        fs::copy(&index, &same_bytes).unwrap();
        let expected = fs::read(&same_bytes).unwrap();
        fs::remove_file(&index).unwrap();
        symlink(&same_bytes, &index).unwrap();
        assert!(item_index_json(d.path(), &r.item_ids[0]).is_err());
        assert!(ingest(d.path(), &c, draft()).is_err());
        assert!(fs::symlink_metadata(&index)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(same_bytes).unwrap(), expected);
        let item = d
            .path()
            .join("items")
            .join(&r.item_ids[0])
            .join("item.json");
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::copy(&item, outside.path()).unwrap();
        fs::remove_file(&item).unwrap();
        symlink(outside.path(), &item).unwrap();
        assert!(read_item(d.path(), &r.item_ids[0]).is_err());
    }
    #[test]
    fn effective_sensitivity_and_escaped_front_matter() {
        let mut c = capture();
        c.source.default_sensitivity = Sensitivity::Restricted;
        c.capture.sensitivity = Sensitivity::Public;
        let d = tempdir().unwrap();
        let draft = ItemDraft {
            item_type: "webpage".into(),
            title: "a: \"quote\"\nnext".into(),
            source_url: "u".into(),
            canonical_url: None,
            content_markdown: "b".into(),
            sensitivity: Sensitivity::Public,
            translator_id: "g".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: Vec::new(),
        };
        let receipt = ingest(d.path(), &c, draft).unwrap();
        let (item, revision) = read_item(d.path(), &receipt.item_ids[0]).unwrap();
        assert_eq!(item.sensitivity, Sensitivity::Restricted);
        assert!(render_markdown(&item, &revision).contains("title: \"a: \\\"quote\\\"\\nnext\""));
        assert!(read_item(d.path(), "../bad").is_err());
    }
    #[test]
    fn item_markdown_rejects_tampered_content() {
        let (d, _c, r) = seed();
        fs::write(
            d.path().join("items").join(&r.item_ids[0]).join("index.md"),
            "tampered",
        )
        .unwrap();
        assert!(item_markdown(d.path(), &r.item_ids[0]).is_err());
    }
    #[test]
    fn reads_do_not_create_missing_library_root() {
        let d = tempdir().unwrap();
        let missing = d.path().join("missing");
        assert!(read_item(&missing, "item_x").is_err());
        assert!(item_markdown(&missing, "item_x").is_err());
        assert!(item_index_json(&missing, "item_x").is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn replay_rejects_source_privacy_and_provenance_tamper() {
        type Scenario = Box<dyn Fn(&tempfile::TempDir, &IngestReceipt)>;
        fn change(path: &Path, f: impl FnOnce(&mut serde_json::Value)) {
            let mut v: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            f(&mut v);
            fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
        }
        let scenarios: Vec<Scenario> = vec![
            Box::new(|d, _| {
                change(&d.path().join("sources/s/source.json"), |v| {
                    v["name"] = serde_json::json!("x")
                })
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("evidence")
                        .join(&r.evidence_ids[0])
                        .join("evidence.json"),
                    |v| v["sensitivity"] = serde_json::json!("public"),
                )
            }),
            Box::new(|d, _| {
                change(&d.path().join("sources/s/source.json"), |v| {
                    v["default_sensitivity"] = serde_json::json!("public")
                })
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["sensitivity"] = serde_json::json!("public"),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"][0]["input_ids"] = serde_json::json!([]),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"][0]["input_checksums"] = serde_json::json!([]),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"][0]["output_ids"] = serde_json::json!([]),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"][0]["config_digest"] = serde_json::json!("sha256:bad"),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("item.json"),
                    |v| v["sensitivity"] = serde_json::json!("public"),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"] = serde_json::json!([]),
                )
            }),
            Box::new(|d, r| {
                change(
                    &d.path()
                        .join("items")
                        .join(&r.item_ids[0])
                        .join("revisions")
                        .join(format!("{}.json", r.revision_ids[0])),
                    |v| v["provenance"][0]["tool_id"] = serde_json::json!("other"),
                )
            }),
        ];
        for scenario in scenarios {
            let d = tempdir().unwrap();
            let mut c = capture();
            c.source.default_sensitivity = Sensitivity::Restricted;
            let r = ingest(d.path(), &c, draft()).unwrap();
            scenario(&d, &r);
            assert!(ingest(d.path(), &c, draft()).is_err());
        }
    }
    #[test]
    fn duplicate_blob_multiplicity_replays_stably() {
        let d = tempdir().unwrap();
        let mut c = capture();
        let mut second = c.blobs[0].clone();
        second.descriptor.locator = "b".into();
        c.blobs.push(second);
        let first = ingest(d.path(), &c, draft()).unwrap();
        assert_eq!(first.evidence_ids.len(), 2);
        assert_ne!(first.evidence_ids[0], first.evidence_ids[1]);
        let one: Evidence = serde_json::from_str(
            &fs::read_to_string(
                d.path()
                    .join("evidence")
                    .join(&first.evidence_ids[0])
                    .join("evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let two: Evidence = serde_json::from_str(
            &fs::read_to_string(
                d.path()
                    .join("evidence")
                    .join(&first.evidence_ids[1])
                    .join("evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(one.logical_locator, "a");
        assert_eq!(two.logical_locator, "b");
        assert_eq!(one.blob_id, two.blob_id);
        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(first.capture_id, replay.capture_id);
        assert_eq!(first.evidence_ids, replay.evidence_ids);
        assert_eq!(first.item_ids, replay.item_ids);
        assert_eq!(first.revision_ids, replay.revision_ids);
    }
    #[test]
    fn replay_rejects_changed_current_verified_capture_or_draft() {
        for mutate in 0..6 {
            let (d, mut c, _) = seed();
            let mut value = draft();
            match mutate {
                0 => c.capture.adapter_version = "2".into(),
                1 => c.capture.captured_at = "2026-01-02T00:00:00Z".into(),
                2 => c.capture.source_url = "changed".into(),
                3 => value.title = "changed".into(),
                4 => value.content_markdown = "changed".into(),
                _ => value.translator_version = "2".into(),
            };
            assert!(ingest(d.path(), &c, value).is_err(), "{mutate}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn first_ingest_rejects_top_level_symlink_escape_without_outside_side_effects() {
        use std::os::unix::fs::symlink;
        for name in [
            "catalog", "sources", "captures", "evidence", "items", "receipts",
        ] {
            let library = tempdir().unwrap();
            let outside = tempdir().unwrap();
            fs::create_dir_all(library.path()).unwrap();
            symlink(outside.path(), library.path().join(name)).unwrap();
            let before: std::collections::BTreeSet<_> = fs::read_dir(outside.path())
                .unwrap()
                .map(|v| v.unwrap().file_name())
                .collect();
            assert!(
                ingest(library.path(), &capture(), draft()).is_err(),
                "{name}"
            );
            let after: std::collections::BTreeSet<_> = fs::read_dir(outside.path())
                .unwrap()
                .map(|v| v.unwrap().file_name())
                .collect();
            assert_eq!(before, after, "{name}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn first_ingest_rejects_lock_symlink_escape() {
        use std::os::unix::fs::symlink;
        let library = tempdir().unwrap();
        fs::create_dir_all(library.path().join("catalog")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "sentinel").unwrap();
        symlink(outside.path(), library.path().join("catalog/.ingest.lock")).unwrap();
        assert!(ingest(library.path(), &capture(), draft()).is_err());
        assert_eq!(fs::read_to_string(outside.path()).unwrap(), "sentinel");
    }
    #[test]
    fn provenance_paths_and_shared_blob_are_consistent() {
        let d = tempdir().unwrap();
        let first_capture = capture();
        let draft = ItemDraft {
            item_type: "webpage".into(),
            title: "t".into(),
            source_url: "u".into(),
            canonical_url: None,
            content_markdown: "b".into(),
            sensitivity: Sensitivity::Public,
            translator_id: "g".into(),
            translator_version: "1".into(),
            metadata: Default::default(),
            attachments: Vec::new(),
        };
        let first = ingest(d.path(), &first_capture, draft.clone()).unwrap();
        let mut second_capture = first_capture.clone();
        second_capture.capture.external_capture_id = "other".into();
        let second = ingest(d.path(), &second_capture, draft).unwrap();
        assert_ne!(first.capture_id, second.capture_id);
        assert_ne!(first.evidence_ids, second.evidence_ids);
        assert_eq!(
            first_capture.blobs[0].blob_id,
            second_capture.blobs[0].blob_id
        );
        let source: memex_core::SourceProfile = serde_json::from_str(
            &fs::read_to_string(d.path().join("sources/s/source.json")).unwrap(),
        )
        .unwrap();
        let record: CaptureRecord = serde_json::from_str(
            &fs::read_to_string(
                d.path()
                    .join("captures")
                    .join(&first.capture_id)
                    .join("capture.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let evidence: Evidence = serde_json::from_str(
            &fs::read_to_string(
                d.path()
                    .join("evidence")
                    .join(&first.evidence_ids[0])
                    .join("evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let links: Vec<EvidenceLink> = serde_json::from_str(
            &fs::read_to_string(
                d.path()
                    .join("items")
                    .join(&first.item_ids[0])
                    .join("links.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(source.source_id, record.source_id);
        assert_eq!(record.capture_id, evidence.capture_id);
        assert_eq!(evidence.evidence_id, links[0].evidence_id);
        let mut conflict = first_capture;
        conflict.source.name = "changed".into();
        conflict.capture.external_capture_id = "conflict".into();
        let evidence_before = fs::read_dir(d.path().join("evidence")).unwrap().count();
        assert!(ingest(
            d.path(),
            &conflict,
            ItemDraft {
                item_type: "webpage".into(),
                title: "x".into(),
                source_url: "u".into(),
                canonical_url: None,
                content_markdown: "b".into(),
                sensitivity: Sensitivity::Public,
                translator_id: "g".into(),
                translator_version: "1".into(),
                metadata: Default::default(),
                attachments: Vec::new()
            }
        )
        .is_err());
        assert_eq!(
            fs::read_dir(d.path().join("evidence")).unwrap().count(),
            evidence_before
        );
        assert!(ingest(
            d.path(),
            &VerifiedCapture {
                source: memex_core::SourceProfile {
                    schema_version: 1,
                    source_id: "../bad".into(),
                    name: "x".into(),
                    source_kind: "x".into(),
                    canonical_uris: vec![],
                    evidence_roles: vec![],
                    default_sensitivity: Sensitivity::Public,
                    preferred_extractor: None
                },
                ..second_capture
            },
            ItemDraft {
                item_type: "x".into(),
                title: "x".into(),
                source_url: "x".into(),
                canonical_url: None,
                content_markdown: "x".into(),
                sensitivity: Sensitivity::Public,
                translator_id: "g".into(),
                translator_version: "1".into(),
                metadata: Default::default(),
                attachments: Vec::new()
            }
        )
        .is_err());
    }
    #[test]
    fn ceiling_list_skips_revision_body_after_verified_header() {
        let d = tempdir().unwrap();
        let mut value = draft();
        value.sensitivity = Sensitivity::Restricted;
        let receipt = ingest(d.path(), &capture(), value).unwrap();
        let id = &receipt.item_ids[0];
        let item: Item = serde_json::from_str(
            &fs::read_to_string(d.path().join("items").join(id).join("item.json")).unwrap(),
        )
        .unwrap();
        fs::write(
            d.path()
                .join("items")
                .join(id)
                .join("revisions")
                .join(format!("{}.json", item.current_revision)),
            b"INVALID_RESTRICTED_BODY",
        )
        .unwrap();
        assert!(read_item(d.path(), id).is_err());
        assert!(list_items_at_sensitivity(d.path(), Sensitivity::Public)
            .unwrap()
            .is_empty());
    }
}
