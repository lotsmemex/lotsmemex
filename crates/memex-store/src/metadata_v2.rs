//! Metadata module (bibliographic_metadata_v1): versioned v2 edit intent,
//! durable request replay, the v2 read DTO and local identifier duplicates.
//! Commits go through the shared owner-revision journal in the crate root.

use super::*;

pub const METADATA_SCHEMA_VERSION: u32 = 2;

/// Versioned bibliographic edit intent (bibliographic_metadata_v1 draft2).
/// `set` values are unsourced owner inputs; the backend assigns provenance.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemMetadataEditV2 {
    pub metadata_schema_version: u32,
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    #[serde(default)]
    pub set: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub clear: Vec<String>,
    #[serde(default)]
    pub preview_id: Option<String>,
    #[serde(default)]
    pub selected_candidate_ids: Vec<String>,
}

/// Typed, canonical form of `set`; also the request-identity hash input.
#[derive(Debug, Default, serde::Serialize)]
struct MetadataSetV2 {
    creators: Option<Vec<Creator>>,
    identifiers: Option<Vec<ItemIdentifier>>,
    published_date: Option<PartialDate>,
    language: Option<String>,
    site_name: Option<String>,
    abstract_text: Option<String>,
    journal_title: Option<String>,
    publisher: Option<String>,
    volume: Option<String>,
    issue: Option<String>,
    pages: Option<String>,
    publication_place: Option<String>,
    edition: Option<String>,
    number_of_pages: Option<u32>,
    series_title: Option<String>,
    series_number: Option<String>,
    keywords: Option<Vec<String>>,
    institutions: Option<Vec<Institution>>,
    cited_references: Option<Vec<CitedReference>>,
}

const EDITABLE_METADATA_FIELDS: &[&str] = &[
    "creators",
    "identifiers",
    "published_date",
    "language",
    "site_name",
    "abstract_text",
    "journal_title",
    "publisher",
    "volume",
    "issue",
    "pages",
    "publication_place",
    "edition",
    "number_of_pages",
    "series_title",
    "series_number",
    "keywords",
    "institutions",
    "cited_references",
];
const READONLY_METADATA_FIELDS: &[&str] = &[
    "added_at",
    "captured_at",
    "title",
    "manual_tags",
    "content_rights",
    "item_id",
    "item_type",
    "revision_id",
    "revision_number",
    "source_url",
    "canonical_url",
    "sensitivity",
    "validation",
    "published_partial_date",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct IdentifierDuplicate {
    pub item_id: String,
    pub revision_id: String,
    pub kind: IdentifierKind,
    pub key: String,
}

pub(super) fn valid_request_id(value: &str) -> bool {
    valid_text(value, 256)
}

fn metadata_field_error(field: &str, reason: &str) -> anyhow::Error {
    anyhow::anyhow!("invalid_metadata_field:{field}:{reason}")
}

fn check_editable_field(field: &str) -> Result<()> {
    if EDITABLE_METADATA_FIELDS.contains(&field) {
        Ok(())
    } else if READONLY_METADATA_FIELDS.contains(&field) {
        anyhow::bail!("readonly_metadata_field:{field}")
    } else {
        anyhow::bail!("unknown_metadata_field:{field}")
    }
}

/// True when every non-null, non-empty key in `input` survives a typed
/// round trip, i.e. no unknown nested field was silently dropped.
fn keys_preserved(input: &serde_json::Value, typed: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (input, typed) {
        (Value::Object(input), Value::Object(typed)) => input.iter().all(|(key, value)| {
            let empty = value.is_null() || value.as_array().is_some_and(Vec::is_empty);
            match typed.get(key) {
                Some(typed) => keys_preserved(value, typed),
                None => empty,
            }
        }),
        (Value::Array(input), Value::Array(typed)) => {
            input.len() == typed.len() && input.iter().zip(typed).all(|(a, b)| keys_preserved(a, b))
        }
        (Value::Object(_), _) => false,
        _ => true,
    }
}

fn parse_set_value<T: serde::de::DeserializeOwned + serde::Serialize>(
    field: &str,
    value: serde_json::Value,
) -> Result<T> {
    if value.is_null() {
        return Err(metadata_field_error(field, "null_use_clear"));
    }
    let typed: T =
        serde_json::from_value(value.clone()).map_err(|_| metadata_field_error(field, "type"))?;
    if !keys_preserved(&value, &serde_json::to_value(&typed)?) {
        return Err(metadata_field_error(field, "unknown_field"));
    }
    Ok(typed)
}

fn parse_metadata_set(set: serde_json::Map<String, serde_json::Value>) -> Result<MetadataSetV2> {
    let mut out = MetadataSetV2::default();
    for (field, value) in set {
        check_editable_field(&field)?;
        let field = field.as_str();
        match field {
            "creators" => out.creators = Some(parse_set_value(field, value)?),
            "identifiers" => out.identifiers = Some(parse_set_value(field, value)?),
            "published_date" => out.published_date = Some(parse_set_value(field, value)?),
            "language" => out.language = Some(parse_set_value(field, value)?),
            "site_name" => out.site_name = Some(parse_set_value(field, value)?),
            "abstract_text" => out.abstract_text = Some(parse_set_value(field, value)?),
            "journal_title" => out.journal_title = Some(parse_set_value(field, value)?),
            "publisher" => out.publisher = Some(parse_set_value(field, value)?),
            "volume" => out.volume = Some(parse_set_value(field, value)?),
            "issue" => out.issue = Some(parse_set_value(field, value)?),
            "pages" => out.pages = Some(parse_set_value(field, value)?),
            "publication_place" => out.publication_place = Some(parse_set_value(field, value)?),
            "edition" => out.edition = Some(parse_set_value(field, value)?),
            "number_of_pages" => out.number_of_pages = Some(parse_set_value(field, value)?),
            "series_title" => out.series_title = Some(parse_set_value(field, value)?),
            "series_number" => out.series_number = Some(parse_set_value(field, value)?),
            "keywords" => out.keywords = Some(parse_set_value(field, value)?),
            "institutions" => out.institutions = Some(parse_set_value(field, value)?),
            "cited_references" => out.cited_references = Some(parse_set_value(field, value)?),
            _ => unreachable!("editable field list and parser diverged"),
        }
    }
    Ok(out)
}

/// Normalizes owner identifiers; any invalid DOI/ISBN/ISSN rejects the edit.
fn normalize_manual_identifiers(
    field: &str,
    input: &[ItemIdentifier],
) -> Result<Vec<ItemIdentifier>> {
    let mut out = Vec::with_capacity(input.len());
    for identifier in input {
        if identifier.raw_value.is_some() {
            return Err(metadata_field_error(field, "raw_value_readonly"));
        }
        let value = if identifier.kind == IdentifierKind::Pmid {
            identifier.value.trim().to_owned()
        } else {
            let normalized = identifier.normalized();
            if normalized.validation.status != ValidationStatus::Valid {
                return Err(metadata_field_error(
                    field,
                    normalized.validation.reason.as_deref().unwrap_or("invalid"),
                ));
            }
            normalized.value.context("valid identifier has a value")?
        };
        let raw_value = (identifier.value != value).then(|| identifier.value.clone());
        out.push(ItemIdentifier {
            kind: identifier.kind,
            value,
            raw_value,
            qualifier: identifier.qualifier.clone(),
        });
    }
    Ok(out)
}

fn validate_manual_date(field: &str, date: &PartialDate) -> Result<()> {
    date.validate()
        .map_err(|reason| metadata_field_error(field, reason))?;
    if !valid_optional_text(&date.raw, 4096) {
        return Err(metadata_field_error(field, "raw"));
    }
    Ok(())
}

pub(super) fn metadata_request_path(root: &Path, request_id: &str) -> PathBuf {
    root.join("catalog/metadata-edit-requests")
        .join(format!("{:x}.json", Sha256::digest(request_id.as_bytes())))
}

pub(super) fn load_metadata_request(
    root: &Path,
    request_id: &str,
) -> Result<Option<MetadataEditRequestRecord>> {
    let path = metadata_request_path(root, request_id);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("unsafe metadata request record")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let record: MetadataEditRequestRecord = load(root, path)?;
    if record.schema_version != SCHEMA_VERSION
        || record.request_id != request_id
        || !valid_sha256(&record.request_sha256)
        || !valid_item_id(&record.receipt.item_id)
        || !valid_revision_id(&record.receipt.revision_id)
    {
        anyhow::bail!("invalid metadata request record")
    }
    Ok(Some(record))
}

pub(super) fn write_metadata_request(
    root: &Path,
    record: &MetadataEditRequestRecord,
) -> Result<()> {
    let parent = ensure_contained_dir(root, Path::new("catalog/metadata-edit-requests"))?;
    #[cfg(unix)]
    fs::set_permissions(&parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    durable_replace(
        root,
        &metadata_request_path(root, &record.request_id),
        serde_json::to_string_pretty(record)?.as_bytes(),
    )
}

/// Applies one versioned bibliographic edit. The adapter supplies the
/// sensitivity ceiling. Authorization precedes replay and CAS disclosure;
/// replay records are committed with the revision under the writer lock.
pub fn revise_item_metadata_v2(
    library: impl AsRef<Path>,
    item_id: &str,
    edit: ItemMetadataEditV2,
    max_sensitivity: Sensitivity,
) -> Result<ItemMetadataUpdateReceipt> {
    if edit.metadata_schema_version != METADATA_SCHEMA_VERSION {
        anyhow::bail!("unsupported_metadata_schema_version")
    }
    if !valid_item_id(item_id)
        || edit.item_id != item_id
        || !valid_revision_id(&edit.expected_revision_id)
        || !valid_request_id(&edit.request_id)
    {
        anyhow::bail!("invalid_metadata_update")
    }
    if edit.preview_id.is_none() && !edit.selected_candidate_ids.is_empty() {
        anyhow::bail!("preview_unavailable")
    }
    let mut selected = edit.selected_candidate_ids;
    selected.sort();
    if selected.len() > 64
        || selected.windows(2).any(|pair| pair[0] == pair[1])
        || selected.iter().any(|id| !valid_text(id, 128))
    {
        anyhow::bail!("invalid_candidate_selection")
    }
    let set_fields: BTreeSet<String> = edit.set.keys().cloned().collect();
    let mut clear = edit.clear;
    for field in &clear {
        check_editable_field(field)?;
        if set_fields.contains(field) {
            return Err(metadata_field_error(field, "set_clear_overlap"));
        }
    }
    clear.sort();
    clear.dedup();
    let set = parse_metadata_set(edit.set)?;
    let mut identity = serde_json::json!({
        "metadata_schema_version": METADATA_SCHEMA_VERSION,
        "item_id": item_id,
        "expected_revision_id": edit.expected_revision_id,
        "set": set,
        "clear": clear,
    });
    if let Some(preview_id) = &edit.preview_id {
        identity["preview_id"] = preview_id.as_str().into();
        identity["selected_candidate_ids"] = serde_json::to_value(&selected)?;
    }
    let request_sha256 = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&identity)?)
    );
    let owner_fields: BTreeSet<String> = set_fields.iter().chain(&clear).cloned().collect();
    let identifiers = set
        .identifiers
        .as_deref()
        .map(|ids| normalize_manual_identifiers("identifiers", ids))
        .transpose()?
        .map(|mut ids| {
            ids.sort();
            ids
        });
    if identifiers.as_ref().is_some_and(|ids| {
        ids.is_empty()
            || ids
                .windows(2)
                .any(|pair| pair[0].kind == pair[1].kind && pair[0].value == pair[1].value)
    }) {
        return Err(metadata_field_error("identifiers", "duplicate_or_empty"));
    }
    if let Some(date) = &set.published_date {
        validate_manual_date("published_date", date)?;
    }
    let mut references = set.cited_references.clone();
    for reference in references.iter_mut().flatten() {
        if let Some(date) = &reference.published_date {
            validate_manual_date("cited_references", date)?;
        }
        if let Some(ids) = &reference.identifiers {
            reference.identifiers = Some(normalize_manual_identifiers("cited_references", ids)?);
        }
    }

    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, current) = read_item_at_sensitivity(&root, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    if let Some(record) = load_metadata_request(&root, &edit.request_id)? {
        if record.request_sha256 == request_sha256 && record.receipt.item_id == item_id {
            lock.unlock()?;
            return Ok(record.receipt);
        }
        anyhow::bail!("request_id_conflict")
    }
    if current.revision_id != edit.expected_revision_id {
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
    for target in references
        .iter()
        .flatten()
        .filter_map(|reference| reference.target_item_id.as_deref())
    {
        if !valid_item_id(target)
            || read_item_at_sensitivity(&root, target, max_sensitivity)?.is_none()
        {
            return Err(metadata_field_error("cited_references", "target_item_id"));
        }
    }

    let mut next = current.metadata.clone();
    fn assign<T: PartialEq>(slot: &mut Option<Sourced<T>>, value: Option<T>) {
        if let Some(value) = value {
            if slot.as_ref().is_none_or(|current| current.value != value) {
                *slot = Some(Sourced {
                    value,
                    provenance: owner_provenance(),
                });
            }
        }
    }
    let creators = set.creators.map(|creators| {
        let unchanged = next
            .creators
            .as_ref()
            .is_some_and(|current| current.value == creators);
        if unchanged {
            return creators;
        }
        creators
            .into_iter()
            .map(|mut creator| {
                creator.creator_id.get_or_insert_with(|| new_id("creator"));
                creator
            })
            .collect()
    });
    assign(&mut next.creators, creators);
    assign(&mut next.identifiers, identifiers);
    if let Some(date) = set.published_date {
        let legacy = date
            .legacy_string()
            .context("validated date has a projection")?
            .to_owned();
        assign(&mut next.published_partial_date, Some(date));
        assign(&mut next.published_date, Some(legacy));
    }
    assign(&mut next.language, set.language);
    assign(&mut next.site_name, set.site_name);
    assign(&mut next.abstract_text, set.abstract_text);
    assign(&mut next.journal_title, set.journal_title);
    assign(&mut next.publisher, set.publisher);
    assign(&mut next.volume, set.volume);
    assign(&mut next.issue, set.issue);
    assign(&mut next.pages, set.pages);
    assign(&mut next.publication_place, set.publication_place);
    assign(&mut next.edition, set.edition);
    assign(&mut next.number_of_pages, set.number_of_pages);
    assign(&mut next.series_title, set.series_title);
    assign(&mut next.series_number, set.series_number);
    assign(&mut next.keywords, set.keywords);
    assign(&mut next.institutions, set.institutions);
    assign(&mut next.cited_references, references);
    for field in &clear {
        match field.as_str() {
            "creators" => next.creators = None,
            "identifiers" => next.identifiers = None,
            "published_date" => {
                next.published_date = None;
                next.published_partial_date = None;
            }
            "language" => next.language = None,
            "site_name" => next.site_name = None,
            "abstract_text" => next.abstract_text = None,
            "journal_title" => next.journal_title = None,
            "publisher" => next.publisher = None,
            "volume" => next.volume = None,
            "issue" => next.issue = None,
            "pages" => next.pages = None,
            "publication_place" => next.publication_place = None,
            "edition" => next.edition = None,
            "number_of_pages" => next.number_of_pages = None,
            "series_title" => next.series_title = None,
            "series_number" => next.series_number = None,
            "keywords" => next.keywords = None,
            "institutions" => next.institutions = None,
            "cited_references" => next.cited_references = None,
            _ => unreachable!("clear fields were checked as editable"),
        }
    }
    let mut title = current.title.clone();
    let applied = match &edit.preview_id {
        Some(preview_id) => lookup::apply_lookup_selection_locked(
            &root,
            preview_id,
            &selected,
            &current,
            max_sensitivity,
            &owner_fields,
            &mut next,
            &mut title,
        )?,
        None => None,
    };
    if next == current.metadata && title == current.title {
        anyhow::bail!("metadata_unchanged")
    }
    validate_metadata(&next).map_err(|error| {
        let message = error.to_string();
        let field = match message.as_str() {
            "invalid item creators" => "creators",
            "invalid item identifiers" => "identifiers",
            "invalid item metadata text" => "text",
            _ => return error,
        };
        anyhow::anyhow!("invalid_metadata_field:{field}")
    })?;
    let activity_type = match applied {
        Some(_) => lookup::LOOKUP_APPLY_ACTIVITY,
        None => BIBLIOGRAPHIC_EDIT_ACTIVITY,
    };
    let mut revision = owner_edit_revision(&current, activity_type, title, next)?;
    if let Some(applied) = &applied {
        let activity = revision
            .provenance
            .last_mut()
            .context("owner edit revision has no activity")?;
        activity.input_ids.push(applied.preview_id.clone());
        activity
            .input_checksums
            .push(applied.response_sha256.clone());
        lookup::retain_lookup_source(&root, applied)?;
    }
    let receipt = commit_owner_revision_locked(
        &root,
        item,
        &current,
        &revision,
        Some((edit.request_id, request_sha256)),
    )?;
    lock.unlock()?;
    Ok(receipt)
}

/// One stored identifier with its server-computed, read-only validation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdentifierRead {
    pub kind: IdentifierKind,
    pub value: String,
    pub raw_value: Option<String>,
    pub qualifier: Option<String>,
    pub normalized_value: Option<String>,
    pub comparison_key: Option<String>,
    pub validation: memex_core::IdentifierValidation,
}

/// metadata_schema_version 2 read: the stored metadata plus derived views. The
/// typed date falls back to an unambiguous parse of the legacy string, and
/// `published_date_origin` says which (`typed` | `legacy_string`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemMetadataV2Read {
    pub metadata_schema_version: u32,
    pub item: ItemReadMetadata,
    pub identifiers: Vec<IdentifierRead>,
    pub published_date: Option<PartialDate>,
    pub published_date_origin: Option<String>,
    pub duration_seconds: Option<u32>,
    /// Read-only first inclusion; explicit null when not recorded.
    pub added_at: Option<String>,
}

pub fn read_item_metadata_v2(
    library: impl AsRef<Path>,
    item_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<Option<ItemMetadataV2Read>> {
    let Some(item) = read_item_metadata_at_sensitivity(library, item_id, max_sensitivity)? else {
        return Ok(None);
    };
    let identifiers = item
        .metadata
        .identifiers
        .iter()
        .flat_map(|identifiers| &identifiers.value)
        .map(|identifier| {
            let normalized = identifier.normalized();
            IdentifierRead {
                kind: identifier.kind,
                value: identifier.value.clone(),
                raw_value: identifier.raw_value.clone(),
                qualifier: identifier.qualifier.clone(),
                normalized_value: normalized.value,
                comparison_key: normalized.comparison_key,
                validation: normalized.validation,
            }
        })
        .collect();
    let (published_date, published_date_origin) = match (
        &item.metadata.published_partial_date,
        &item.metadata.published_date,
    ) {
        (Some(date), _) => (Some(date.value.clone()), Some("typed".to_owned())),
        (None, Some(legacy)) => (
            Some(PartialDate::parse_legacy(&legacy.value)),
            Some("legacy_string".to_owned()),
        ),
        (None, None) => (None, None),
    };
    Ok(Some(ItemMetadataV2Read {
        metadata_schema_version: METADATA_SCHEMA_VERSION,
        added_at: item.added_at.clone(),
        duration_seconds: item.metadata.duration_seconds.as_ref().map(|v| v.value).filter(|v| *v > 0),
        item,
        identifiers,
        published_date,
        published_date_origin,
    }))
}

fn identifier_identity_key(identifier: &ItemIdentifier) -> Option<(IdentifierKind, String)> {
    matches!(identifier.kind, IdentifierKind::Doi | IdentifierKind::Isbn)
        .then(|| identifier.normalized().comparison_key)
        .flatten()
        .map(|key| (identifier.kind, key))
}

/// Exact DOI / ISBN-13 duplicate candidates among current revisions within
/// the ceiling. Hidden items contribute nothing; ISSN/PMID/invalid values and
/// cited-reference identifiers never match.
pub fn find_identifier_duplicates(
    library: impl AsRef<Path>,
    identifiers: &[ItemIdentifier],
    max_sensitivity: Sensitivity,
    exclude_item_id: Option<&str>,
) -> Result<Vec<IdentifierDuplicate>> {
    if identifiers.len() > 64 {
        anyhow::bail!("invalid_identifier_query")
    }
    let keys: BTreeSet<(IdentifierKind, String)> = identifiers
        .iter()
        .filter_map(identifier_identity_key)
        .collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    with_consistent_snapshot(library, |root| {
        let mut found = BTreeSet::new();
        scan_item_metadata_at_sensitivity(root, max_sensitivity, |item| {
            if exclude_item_id == Some(item.item_id.as_str()) {
                return Ok(());
            }
            for identifier in item.metadata.identifiers.iter().flat_map(|ids| &ids.value) {
                if let Some((kind, key)) = identifier_identity_key(identifier) {
                    if keys.contains(&(kind, key.clone())) {
                        found.insert(IdentifierDuplicate {
                            item_id: item.item_id.clone(),
                            revision_id: item.revision_id.clone(),
                            kind,
                            key,
                        });
                    }
                }
            }
            Ok(())
        })?;
        Ok(found.into_iter().collect())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::*;
    use memex_core::{MetadataOrigin, MetadataProvenance, Sensitivity, Sourced};
    use tempfile::tempdir;

    pub(crate) fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_owned();
                if path.is_dir() {
                    out.insert(relative, Vec::new());
                    walk(root, &path, out);
                } else {
                    out.insert(relative, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        // The writer lock's cache-invalidation token is not Store content.
        out.remove(Path::new(GENERATION_FILE));
        out
    }

    pub(crate) fn v2_edit(
        item_id: &str,
        expected: &str,
        request_id: &str,
        set: serde_json::Value,
        clear: &[&str],
    ) -> ItemMetadataEditV2 {
        serde_json::from_value(serde_json::json!({
            "metadata_schema_version": 2,
            "item_id": item_id,
            "expected_revision_id": expected,
            "request_id": request_id,
            "set": set,
            "clear": clear,
            "preview_id": null,
            "selected_candidate_ids": [],
        }))
        .unwrap()
    }

    pub(crate) fn v2_err(root: &Path, item_id: &str, edit: ItemMetadataEditV2) -> String {
        revise_item_metadata_v2(root, item_id, edit, Sensitivity::Restricted)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn bibliographic_v2_edit_commits_typed_owner_metadata_without_touching_body() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let base_id = &receipt.revision_ids[0];
        let first = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                base_id,
                "req-typed",
                serde_json::json!({
                    "published_date": {"value": "2024", "precision": "year", "raw": "2024"},
                    "identifiers": [
                        {"kind": "doi", "value": "https://doi.org/10.1000/ABC(1)x."},
                        {"kind": "isbn", "value": "0-306-40615-2", "qualifier": "print"},
                        {"kind": "issn", "value": "03178471"}
                    ],
                    "institutions": [{"institution_id": "inst-1", "name": "Synthetic Lab"}],
                    "creators": [{
                        "display_name": "Ada Example", "role": "author", "name_type": "person",
                        "family_name": "Example", "given_name": "Ada",
                        "institution_ids": ["inst-1"]
                    }],
                    "journal_title": "Journal of Synthetic Tests",
                    "pages": "e1001–e1009",
                    "number_of_pages": 12,
                    "keywords": ["beta", "alpha"],
                    "cited_references": []
                }),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        assert_eq!(first.previous_revision_id, *base_id);
        assert_eq!(first.revision_number, 2);
        let (item, revision) = read_item(d.path(), item_id).unwrap();
        assert_eq!(item.current_revision, first.revision_id);
        assert_eq!(
            (revision.title.as_str(), revision.content_markdown.as_str()),
            ("t", "b")
        );
        assert_eq!(revision.metadata.manual_tags, None);
        let metadata = &revision.metadata;
        let date = metadata.published_partial_date.as_ref().unwrap();
        assert_eq!(date.value.value.as_deref(), Some("2024"));
        assert_eq!(date.value.precision, memex_core::DatePrecision::Year);
        assert_eq!(date.provenance.origin, MetadataOrigin::OwnerEdit);
        assert_eq!(metadata.published_date.as_ref().unwrap().value, "2024");
        let ids = &metadata.identifiers.as_ref().unwrap().value;
        let doi = ids.iter().find(|i| i.kind == IdentifierKind::Doi).unwrap();
        assert_eq!(doi.value, "10.1000/abc(1)x.");
        assert_eq!(
            doi.raw_value.as_deref(),
            Some("https://doi.org/10.1000/ABC(1)x.")
        );
        let isbn = ids.iter().find(|i| i.kind == IdentifierKind::Isbn).unwrap();
        assert_eq!(isbn.value, "9780306406157");
        assert_eq!(isbn.raw_value.as_deref(), Some("0-306-40615-2"));
        assert_eq!(isbn.qualifier.as_deref(), Some("print"));
        let issn = ids.iter().find(|i| i.kind == IdentifierKind::Issn).unwrap();
        assert_eq!(issn.value, "0317-8471");
        let creator = &metadata.creators.as_ref().unwrap().value[0];
        assert!(creator
            .creator_id
            .as_deref()
            .unwrap()
            .starts_with("creator_"));
        assert_eq!(creator.family_name.as_deref(), Some("Example"));
        assert_eq!(metadata.keywords.as_ref().unwrap().value, ["beta", "alpha"]);
        assert_eq!(metadata.cited_references.as_ref().unwrap().value, []);
        let activity = revision.provenance.last().unwrap();
        assert_eq!(activity.activity_type, BIBLIOGRAPHIC_EDIT_ACTIVITY);

        // Leap day accepted; legacy title/tag writer still works on the chain.
        let second = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                &first.revision_id,
                "req-leap",
                serde_json::json!({"published_date": {"value": "2024-02-29", "precision": "day", "raw": null}}),
                &["pages"],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        let legacy = revise_item_metadata(
            d.path(),
            item_id,
            ItemMetadataUpdate {
                expected_revision_id: second.revision_id.clone(),
                title: "Owner title".into(),
                manual_tags: vec!["tag".into()],
            },
        )
        .unwrap();
        let (_, revision) = read_item(d.path(), item_id).unwrap();
        assert_eq!(revision.revision_id, legacy.revision_id);
        assert_eq!(revision.metadata.pages, None);
        assert_eq!(
            revision.metadata.published_date.as_ref().unwrap().value,
            "2024-02-29"
        );
        assert_eq!(
            revision.metadata.journal_title.as_ref().unwrap().value,
            "Journal of Synthetic Tests"
        );
        assert!(item_index_json(d.path(), item_id)
            .unwrap()
            .contains("Journal of Synthetic Tests"));

        // Value-equal set is a no-op, not a new revision.
        assert_eq!(
            v2_err(
                d.path(),
                item_id,
                v2_edit(
                    item_id,
                    &legacy.revision_id,
                    "req-noop",
                    serde_json::json!({"journal_title": "Journal of Synthetic Tests"}),
                    &["series_title"]
                ),
            ),
            "metadata_unchanged"
        );

        // Bibliographic transitions accept only owner provenance on changed
        // editable fields and never title/manual_tags changes.
        let base: ItemRevision = serde_json::from_slice(
            &fs::read(
                d.path()
                    .join("items")
                    .join(item_id)
                    .join("revisions")
                    .join(format!("{}.json", second.revision_id)),
            )
            .unwrap(),
        )
        .unwrap();
        let mut metadata = base.metadata.clone();
        metadata.publisher = Some(Sourced {
            value: "P".into(),
            provenance: owner_provenance(),
        });
        let ok = owner_edit_revision(
            &base,
            BIBLIOGRAPHIC_EDIT_ACTIVITY,
            base.title.clone(),
            metadata.clone(),
        )
        .unwrap();
        validate_owner_edit_transition(&base, &ok).unwrap();
        let mut forged = metadata.clone();
        forged.publisher.as_mut().unwrap().provenance.origin = MetadataOrigin::CitationMeta;
        let forged = owner_edit_revision(
            &base,
            BIBLIOGRAPHIC_EDIT_ACTIVITY,
            base.title.clone(),
            forged,
        )
        .unwrap();
        assert!(validate_owner_edit_transition(&base, &forged).is_err());
        let retitled = owner_edit_revision(
            &base,
            BIBLIOGRAPHIC_EDIT_ACTIVITY,
            "Other".into(),
            metadata.clone(),
        )
        .unwrap();
        assert!(validate_owner_edit_transition(&base, &retitled).is_err());
        let mut tagged = metadata;
        tagged.manual_tags = Some(Sourced {
            value: vec!["x".into()],
            provenance: owner_provenance(),
        });
        let tagged = owner_edit_revision(
            &base,
            BIBLIOGRAPHIC_EDIT_ACTIVITY,
            base.title.clone(),
            tagged,
        )
        .unwrap();
        assert!(validate_owner_edit_transition(&base, &tagged).is_err());
    }

    #[test]
    fn bibliographic_v2_rejects_invalid_readonly_and_preview_inputs_with_zero_writes() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let base = &receipt.revision_ids[0];
        let before = tree(d.path());
        let cases: Vec<(serde_json::Value, Vec<&str>, &str)> = vec![
            (
                serde_json::json!({"published_date": {"value": "2023-02-29", "precision": "day", "raw": null}}),
                vec![],
                "invalid_metadata_field:published_date:day",
            ),
            (
                serde_json::json!({"published_date": {"value": "2024-01-01", "precision": "year", "raw": null}}),
                vec![],
                "invalid_metadata_field:published_date:precision_mismatch",
            ),
            (
                serde_json::json!({"published_date": {"value": null, "precision": "unparsed", "raw": null}}),
                vec![],
                "invalid_metadata_field:published_date:unparsed_without_raw",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "isbn", "value": "0-306-40615-3"}]}),
                vec![],
                "invalid_metadata_field:identifiers:checksum",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "isbn", "value": "9790306406157"}]}),
                vec![],
                "invalid_metadata_field:identifiers:checksum",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "issn", "value": "0317-8472"}]}),
                vec![],
                "invalid_metadata_field:identifiers:checksum",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "doi", "value": "https://doi.org/10.1/x?download=1"}]}),
                vec![],
                "invalid_metadata_field:identifiers:url_query_or_fragment",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "isbn", "value": "0-306-40615-2"}, {"kind": "isbn", "value": "978-0-306-40615-7"}]}),
                vec![],
                "invalid_metadata_field:identifiers:duplicate_or_empty",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "doi", "value": "10.1/x", "validation": {"status": "valid"}}]}),
                vec![],
                "invalid_metadata_field:identifiers:unknown_field",
            ),
            (
                serde_json::json!({"identifiers": [{"kind": "doi", "value": "10.1/x", "raw_value": "forged"}]}),
                vec![],
                "invalid_metadata_field:identifiers:raw_value_readonly",
            ),
            (
                serde_json::json!({"added_at": "2026-01-01T00:00:00Z"}),
                vec![],
                "readonly_metadata_field:added_at",
            ),
            (
                serde_json::json!({"title": "x"}),
                vec![],
                "readonly_metadata_field:title",
            ),
            (
                serde_json::json!({"colour": "x"}),
                vec![],
                "unknown_metadata_field:colour",
            ),
            (
                serde_json::json!({}),
                vec!["added_at"],
                "readonly_metadata_field:added_at",
            ),
            (
                serde_json::json!({"publisher": "P"}),
                vec!["publisher"],
                "invalid_metadata_field:publisher:set_clear_overlap",
            ),
            (
                serde_json::json!({"publisher": null}),
                vec![],
                "invalid_metadata_field:publisher:null_use_clear",
            ),
            (
                serde_json::json!({"number_of_pages": 0}),
                vec![],
                "invalid_metadata_field:number_of_pages",
            ),
            (
                serde_json::json!({"creators": [{"display_name": "A", "role": "author", "institution_ids": ["missing"]}]}),
                vec![],
                "invalid_metadata_field:creators",
            ),
            (
                serde_json::json!({"creators": [
                {"display_name": "A", "role": "author", "creator_id": "c1"},
                {"display_name": "B", "role": "author", "creator_id": "c1"}]}),
                vec![],
                "invalid_metadata_field:creators",
            ),
            (
                serde_json::json!({"cited_references": [{"reference_id": "r1"}, {"reference_id": "r1"}]}),
                vec![],
                "invalid_metadata_field:cited_references",
            ),
            (
                serde_json::json!({"cited_references": [{"reference_id": "r1", "target_item_id": "item_missing"}]}),
                vec![],
                "invalid_metadata_field:cited_references:target_item_id",
            ),
        ];
        for (set, clear, expected) in cases {
            let edit = v2_edit(item_id, base, "req-invalid", set.clone(), &clear);
            assert_eq!(v2_err(d.path(), item_id, edit), expected, "{set}");
            assert_eq!(tree(d.path()), before, "{set}");
        }
        let mut edit = v2_edit(
            item_id,
            base,
            "req-preview",
            serde_json::json!({"journal_title": "J"}),
            &[],
        );
        edit.preview_id = Some("preview_x".into());
        assert_eq!(v2_err(d.path(), item_id, edit), "preview_unavailable");
        let mut edit = v2_edit(item_id, base, "req-preview", serde_json::json!({}), &[]);
        edit.selected_candidate_ids = vec!["candidate_x".into()];
        assert_eq!(v2_err(d.path(), item_id, edit), "preview_unavailable");
        let mut edit = v2_edit(
            item_id,
            base,
            "req-version",
            serde_json::json!({"journal_title": "J"}),
            &[],
        );
        edit.metadata_schema_version = 1;
        assert_eq!(
            v2_err(d.path(), item_id, edit),
            "unsupported_metadata_schema_version"
        );
        assert!(
            serde_json::from_value::<ItemMetadataEditV2>(serde_json::json!({
                "metadata_schema_version": 2, "item_id": item_id, "expected_revision_id": base,
                "request_id": "r", "set": {}, "clear": [], "force": true
            }))
            .is_err()
        );
        assert_eq!(tree(d.path()), before);
        // Rejected requests leave no replay record: the same id still applies.
        revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                base,
                "req-invalid",
                serde_json::json!({"journal_title": "J"}),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
    }

    #[test]
    fn bibliographic_v2_stale_and_hidden_edits_write_nothing() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let base = &receipt.revision_ids[0];
        let first = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                base,
                "req-1",
                serde_json::json!({"publisher": "First"}),
                &[],
            ),
            Sensitivity::Public,
        )
        .unwrap();
        let before = tree(d.path());
        assert_eq!(
            v2_err(
                d.path(),
                item_id,
                v2_edit(
                    item_id,
                    base,
                    "req-2",
                    serde_json::json!({"publisher": "Second"}),
                    &[]
                )
            ),
            "stale_item_revision"
        );
        assert_eq!(tree(d.path()), before);
        assert_eq!(
            read_item(d.path(), item_id).unwrap().1.revision_id,
            first.revision_id
        );

        let mut private_capture = capture();
        private_capture.capture.external_capture_id = "private".into();
        private_capture.capture.sensitivity = Sensitivity::Private;
        let mut private_draft = draft();
        private_draft.sensitivity = Sensitivity::Private;
        let private = ingest(d.path(), &private_capture, private_draft).unwrap();
        let hidden_id = &private.item_ids[0];
        let before = tree(d.path());
        for expected in [private.revision_ids[0].as_str(), "revision_stale"] {
            let error = revise_item_metadata_v2(
                d.path(),
                hidden_id,
                v2_edit(
                    hidden_id,
                    expected,
                    "req-hidden",
                    serde_json::json!({"publisher": "X"}),
                    &[],
                ),
                Sensitivity::Public,
            )
            .unwrap_err()
            .to_string();
            assert_eq!(error, "library_resource_not_found");
        }
        // Visible public item cannot cite a hidden target either.
        assert_eq!(
            revise_item_metadata_v2(
                d.path(),
                item_id,
                v2_edit(
                    item_id,
                    &first.revision_id,
                    "req-cite",
                    serde_json::json!({
                        "cited_references": [{"reference_id": "r1", "target_item_id": hidden_id}]
                    }),
                    &[]
                ),
                Sensitivity::Public,
            )
            .unwrap_err()
            .to_string(),
            "invalid_metadata_field:cited_references:target_item_id"
        );
        assert_eq!(tree(d.path()), before);
    }

    #[test]
    fn bibliographic_v2_request_replay_is_durable_and_conflicts_on_new_payload() {
        let (d, _, receipt) = seed();
        let item_id = &receipt.item_ids[0];
        let base = &receipt.revision_ids[0];
        let set = serde_json::json!({"edition": "2nd", "identifiers": [{"kind": "isbn", "value": "979-10-90636-07-1"}]});
        // Interrupt after the pointer commit but before the replay record lands.
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(Some(3)));
        let interrupted = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(item_id, base, "req-durable", set.clone(), &[]),
            Sensitivity::Restricted,
        );
        FAIL_ITEM_UPDATE_STEP.with(|value| value.set(None));
        assert!(interrupted.is_err());
        let record = metadata_request_path(d.path(), "req-durable");
        assert!(!record.exists());
        // A fresh call (as after restart) recovers the journal, then replays.
        let replay = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(item_id, base, "req-durable", set.clone(), &[]),
            Sensitivity::Restricted,
        )
        .unwrap();
        assert!(record.exists());
        let (_, revision) = read_item(d.path(), item_id).unwrap();
        assert_eq!(replay.revision_id, revision.revision_id);
        assert_eq!(replay.previous_revision_id, *base);
        assert_eq!(revision.revision_number, 2);
        assert_eq!(
            revision.metadata.identifiers.as_ref().unwrap().value[0].value,
            "9791090636071"
        );

        // Uninterrupted path: second request, then an identical retry.
        let next = revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                &replay.revision_id,
                "req-two",
                serde_json::json!({"volume": "XII"}),
                &["edition"],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        let before = tree(d.path());
        for (request, expected_revision, set, clear, receipt) in [
            ("req-durable", base.as_str(), set.clone(), vec![], &replay),
            (
                "req-two",
                replay.revision_id.as_str(),
                serde_json::json!({"volume": "XII"}),
                vec!["edition"],
                &next,
            ),
        ] {
            let again = revise_item_metadata_v2(
                d.path(),
                item_id,
                v2_edit(item_id, expected_revision, request, set, &clear),
                Sensitivity::Restricted,
            )
            .unwrap();
            assert_eq!(&again, receipt);
        }
        assert_eq!(tree(d.path()), before);
        assert_eq!(read_item(d.path(), item_id).unwrap().1.revision_number, 3);
        assert_eq!(
            v2_err(
                d.path(),
                item_id,
                v2_edit(
                    item_id,
                    &replay.revision_id,
                    "req-two",
                    serde_json::json!({"volume": "XIII"}),
                    &["edition"]
                )
            ),
            "request_id_conflict"
        );
        assert_eq!(tree(d.path()), before);
    }

    #[test]
    fn identifier_duplicates_match_doi_and_isbn13_keys_only_within_ceiling() {
        let d = tempdir().unwrap();
        let source = MetadataProvenance {
            origin: MetadataOrigin::CitationMeta,
            provider_id: "test.source".into(),
            provider_version: "1".into(),
            source_refs: Vec::new(),
        };
        let mut ids = BTreeMap::new();
        for (name, sensitivity, identifiers) in [
            (
                "book",
                Sensitivity::Public,
                vec![ItemIdentifier::new(
                    IdentifierKind::Isbn,
                    "978-0-306-40615-7",
                )],
            ),
            (
                "paper",
                Sensitivity::Public,
                vec![
                    ItemIdentifier::new(IdentifierKind::Doi, "doi:10.1000/Shared"),
                    ItemIdentifier::new(IdentifierKind::Issn, "0317-8471"),
                ],
            ),
            (
                "other-paper",
                Sensitivity::Public,
                vec![ItemIdentifier::new(IdentifierKind::Issn, "0317-8471")],
            ),
            (
                "bad-isbn",
                Sensitivity::Public,
                vec![ItemIdentifier::new(IdentifierKind::Isbn, "0-306-40615-3")],
            ),
            (
                "hidden",
                Sensitivity::Private,
                vec![ItemIdentifier::new(IdentifierKind::Doi, "10.1000/hidden")],
            ),
        ] {
            let mut c = capture();
            c.capture.external_capture_id = name.into();
            c.capture.sensitivity = sensitivity;
            let mut value = draft();
            value.sensitivity = sensitivity;
            let mut identifiers = identifiers;
            identifiers.sort();
            value.metadata.identifiers = Some(Sourced {
                value: identifiers,
                provenance: source.clone(),
            });
            ids.insert(name, ingest(d.path(), &c, value).unwrap());
        }
        let item = |name: &str| ids[name].item_ids[0].clone();
        let query = |identifiers: Vec<ItemIdentifier>, max, exclude: Option<&str>| {
            find_identifier_duplicates(d.path(), &identifiers, max, exclude).unwrap()
        };
        let found = query(
            vec![ItemIdentifier::new(IdentifierKind::Isbn, "0-306-40615-2")],
            Sensitivity::Public,
            None,
        );
        assert_eq!(
            found,
            [IdentifierDuplicate {
                item_id: item("book"),
                revision_id: ids["book"].revision_ids[0].clone(),
                kind: IdentifierKind::Isbn,
                key: "9780306406157".into(),
            }]
        );
        assert!(query(
            vec![ItemIdentifier::new(IdentifierKind::Isbn, "0-306-40615-2")],
            Sensitivity::Public,
            Some(&item("book"))
        )
        .is_empty());
        let found = query(
            vec![ItemIdentifier::new(
                IdentifierKind::Doi,
                "https://dx.doi.org/10.1000/SHARED",
            )],
            Sensitivity::Public,
            None,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].item_id.clone(), found[0].key.as_str()),
            (item("paper"), "10.1000/shared")
        );
        // ISSN identifies a serial, never an item duplicate.
        assert!(query(
            vec![ItemIdentifier::new(IdentifierKind::Issn, "0317-8471")],
            Sensitivity::Restricted,
            None
        )
        .is_empty());
        // Invalid values never match, even against identical invalid source data.
        assert!(query(
            vec![ItemIdentifier::new(IdentifierKind::Isbn, "0-306-40615-3")],
            Sensitivity::Restricted,
            None
        )
        .is_empty());
        // Hidden duplicate contributes nothing below its sensitivity.
        let hidden = vec![ItemIdentifier::new(IdentifierKind::Doi, "10.1000/HIDDEN")];
        assert!(query(hidden.clone(), Sensitivity::Public, None).is_empty());
        assert_eq!(
            query(hidden, Sensitivity::Private, None)[0].item_id,
            item("hidden")
        );
    }

    #[test]
    fn bibliographic_edit_keeps_source_manual_tags_valid_in_chain() {
        let d = tempdir().unwrap();
        let mut value = draft();
        value.metadata.manual_tags = Some(Sourced {
            value: vec!["source-tag".into()],
            provenance: MetadataProvenance::new(MetadataOrigin::IntakeProvided, "test.intake", "1"),
        });
        let receipt = ingest(d.path(), &capture(), value).unwrap();
        let item_id = &receipt.item_ids[0];
        revise_item_metadata_v2(
            d.path(),
            item_id,
            v2_edit(
                item_id,
                &receipt.revision_ids[0],
                "req-tags",
                serde_json::json!({"publisher": "P"}),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        let (_, revision) = read_item(d.path(), item_id).unwrap();
        assert_eq!(
            revision.metadata.manual_tags.unwrap().provenance.origin,
            MetadataOrigin::IntakeProvided
        );
    }
}
