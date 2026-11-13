//! Bibliographic lookup preview (bibliographic_metadata_v1 draft2).
//!
//! Pure provider adapters, the approved route policy, preview persistence and
//! apply-time verification. This module has no network authority: the Mac
//! orchestrator fetches the exact approved route outside the writer lock and
//! hands the bounded response bytes here. Nothing here follows a URL found in a
//! provider response (fulltext/download links are ignored, never fetched).

use super::*;
use memex_core::{normalize_doi, normalize_identifier, CreatorRole, DatePrecision};
use serde_json::Value;

pub const LOOKUP_ADAPTER_VERSION: &str = "memex.bibliographic_lookup.v1";
pub const MAX_LOOKUP_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const LOOKUP_PREVIEW_TTL_SECONDS: i64 = 24 * 60 * 60;
pub const MAX_LOOKUP_PREVIEWS: usize = 256;
pub(crate) const LOOKUP_APPLY_ACTIVITY: &str = "bibliographic_lookup_apply";
const LOOKUP_DIR: &str = "catalog/metadata-lookup";
const LOOKUP_PREVIEW_VERSION: u32 = 1;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LookupProvider {
    Crossref,
    OpenLibrary,
}

impl LookupProvider {
    pub const ALL: [Self; 2] = [Self::Crossref, Self::OpenLibrary];

    pub fn provider_id(self) -> &'static str {
        match self {
            Self::Crossref => "crossref",
            Self::OpenLibrary => "open_library",
        }
    }
    pub fn host(self) -> &'static str {
        match self {
            Self::Crossref => "api.crossref.org",
            Self::OpenLibrary => "openlibrary.org",
        }
    }
    pub fn identifier_kind(self) -> IdentifierKind {
        match self {
            Self::Crossref => IdentifierKind::Doi,
            Self::OpenLibrary => IdentifierKind::Isbn,
        }
    }
    pub fn route_template(self) -> &'static str {
        match self {
            Self::Crossref => "https://api.crossref.org/works/{doi}",
            Self::OpenLibrary => "https://openlibrary.org/isbn/{isbn}.json",
        }
    }
    pub fn for_kind(kind: IdentifierKind) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.identifier_kind() == kind)
    }
    fn from_provider_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.provider_id() == value)
    }
}

/// Exact request URL for a locally valid, already-normalized identifier. The
/// identifier is one percent-encoded path segment (a DOI slash becomes %2F).
pub fn lookup_request_url(provider: LookupProvider, normalized: &str) -> Result<Url> {
    let checked = normalize_identifier(provider.identifier_kind(), normalized);
    if checked.validation.status != ValidationStatus::Valid
        || checked.comparison_key.as_deref() != Some(normalized)
    {
        anyhow::bail!("lookup_identifier_invalid")
    }
    let mut url = Url::parse(&format!("https://{}/", provider.host()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("lookup_identifier_invalid"))?;
        segments.clear();
        match provider {
            LookupProvider::Crossref => segments.push("works").push(normalized),
            LookupProvider::OpenLibrary => {
                segments.push("isbn").push(&format!("{normalized}.json"))
            }
        };
    }
    Ok(url)
}

fn decode_segment(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            if !pair.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(pair, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Classifies `url` as one of the approved lookup routes: the two exact request
/// shapes, plus Open Library's same-host edition redirect target.
pub fn lookup_route(url: &Url) -> Option<LookupProvider> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    match (url.host_str()?, segments.as_slice()) {
        ("api.crossref.org", ["works", doi]) => {
            let doi = decode_segment(doi)?;
            (lookup_request_url(LookupProvider::Crossref, &doi).ok()? == *url)
                .then_some(LookupProvider::Crossref)
        }
        ("openlibrary.org", ["isbn", file]) => {
            let isbn = file.strip_suffix(".json")?;
            (lookup_request_url(LookupProvider::OpenLibrary, isbn).ok()? == *url)
                .then_some(LookupProvider::OpenLibrary)
        }
        ("openlibrary.org", ["books", file]) => file
            .strip_prefix("OL")
            .and_then(|rest| rest.strip_suffix("M.json"))
            .filter(|digits| {
                !digits.is_empty()
                    && digits.len() <= 12
                    && digits.bytes().all(|b| b.is_ascii_digit())
            })
            .map(|_| LookupProvider::OpenLibrary),
        _ => None,
    }
}

/// Open Library resolves `/isbn/{isbn}.json` to its edition record with one
/// same-host redirect. Nothing else may redirect.
pub fn lookup_redirect_allowed(from: &Url, to: &Url) -> bool {
    lookup_route(from) == Some(LookupProvider::OpenLibrary)
        && from.path().starts_with("/isbn/")
        && lookup_route(to) == Some(LookupProvider::OpenLibrary)
        && to.path().starts_with("/books/")
}

/// Authorized, CAS-checked lookup request. Contains only the identifier that
/// leaves the Mac; no title, body, tags or other item data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LookupTarget {
    pub item_id: String,
    pub expected_revision_id: String,
    pub provider: LookupProvider,
    pub identifier: ItemIdentifier,
    pub request_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupCandidate {
    pub candidate_id: String,
    pub field: String,
    pub current_value: Option<Value>,
    pub proposed_value: Value,
    pub source_label: String,
    pub conflict: bool,
    pub unchanged: bool,
    pub selected: bool,
    /// Fields that must also be selected (or explicitly set/cleared) for this
    /// candidate to apply, e.g. creators that link proposed institutions.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// Frozen preview. Persisted by the Store; apply re-derives every candidate
/// from the retained response bytes and requires an exact match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupPreview {
    pub schema_version: u32,
    pub preview_version: u32,
    pub metadata_schema_version: u32,
    pub preview_id: String,
    pub item_id: String,
    pub expected_revision_id: String,
    pub sensitivity: Sensitivity,
    pub provider: LookupProvider,
    pub adapter_version: String,
    pub identifier: ItemIdentifier,
    pub request_url: String,
    pub fetched_at: String,
    pub created_at: String,
    pub expires_at: String,
    pub response_sha256: String,
    pub response_byte_size: u64,
    pub source_ref: String,
    pub candidates: Vec<LookupCandidate>,
    pub notes: Vec<String>,
}

struct Proposal {
    field: &'static str,
    value: Value,
    source_label: String,
}

#[derive(Default)]
pub(crate) struct Parsed {
    proposals: Vec<Proposal>,
    notes: Vec<String>,
}

impl Parsed {
    fn push(&mut self, field: &'static str, value: impl serde::Serialize, label: &str) {
        if let Ok(value) = serde_json::to_value(value) {
            self.proposals.push(Proposal {
                field,
                value,
                source_label: label.to_owned(),
            });
        }
    }
    fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }
}

fn clean(value: &str, max: usize) -> Option<String> {
    let joined = value
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty() && joined.len() <= max).then_some(joined)
}
fn text(value: Option<&Value>, max: usize) -> Option<String> {
    value?.as_str().and_then(|value| clean(value, max))
}
fn strings(value: Option<&Value>, max: usize) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().and_then(|value| clean(value, max)))
        .collect()
}
fn joined(values: Vec<String>, max: usize) -> Option<String> {
    (!values.is_empty())
        .then(|| values.join("; "))
        .filter(|value| value.len() <= max)
}
/// Crossref abstracts are JATS fragments; keep the text, drop markup.
fn strip_markup(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_tag = false;
    for c in value.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Source identifier: stored normalized when locally valid (raw retained),
/// otherwise retained as supplied and excluded from dedup by validation.
fn source_identifier(
    kind: IdentifierKind,
    raw: &str,
    qualifier: Option<String>,
) -> Option<ItemIdentifier> {
    let raw = clean(raw, 4096)?;
    let normalized = normalize_identifier(kind, &raw);
    let (value, raw_value) = match normalized.value {
        Some(value) if normalized.validation.status == ValidationStatus::Valid => {
            let raw_value = (value != raw).then_some(raw);
            (value, raw_value)
        }
        _ => (raw, None),
    };
    Some(ItemIdentifier {
        kind,
        value,
        raw_value,
        qualifier,
    })
}
fn finish_identifiers(mut ids: Vec<ItemIdentifier>) -> Option<Vec<ItemIdentifier>> {
    ids.sort();
    ids.dedup_by(|right, left| left.kind == right.kind && left.value == right.value);
    (!ids.is_empty() && ids.len() <= 64).then_some(ids)
}

fn crossref_date(value: Option<&Value>) -> Option<PartialDate> {
    let parts_json = value?.get("date-parts")?;
    let parts = parts_json.as_array()?.first()?.as_array()?;
    if parts.is_empty() || parts.iter().all(Value::is_null) {
        return None;
    }
    let raw = serde_json::to_string(parts_json).ok();
    let numbers: Option<Vec<u64>> = parts.iter().map(Value::as_u64).collect();
    let typed = numbers
        .filter(|numbers| numbers.len() <= 3 && numbers[0] <= 9999)
        .map(|numbers| {
            let (value, precision) = match numbers.as_slice() {
                [year] => (format!("{year:04}"), DatePrecision::Year),
                [year, month] => (format!("{year:04}-{month:02}"), DatePrecision::Month),
                [year, month, day] => {
                    (format!("{year:04}-{month:02}-{day:02}"), DatePrecision::Day)
                }
                _ => unreachable!("bounded above"),
            };
            PartialDate {
                value: Some(value),
                precision,
                raw: raw.clone(),
            }
        })
        .filter(|date| date.validate().is_ok());
    Some(typed.unwrap_or(PartialDate {
        value: None,
        precision: DatePrecision::Unparsed,
        raw,
    }))
}

fn crossref_creators(message: &Value, out: &mut Parsed) {
    let mut creators = Vec::new();
    let mut institutions = Vec::new();
    for (key, role) in [
        ("author", CreatorRole::Author),
        ("editor", CreatorRole::Editor),
        ("translator", CreatorRole::Translator),
    ] {
        for person in message
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let given = text(person.get("given"), 4096);
            let family = text(person.get("family"), 4096);
            let mut creator = if given.is_some() || family.is_some() {
                let display = [given.as_deref(), family.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                Creator {
                    name_type: Some(CreatorNameType::Person),
                    family_name: family,
                    given_name: given,
                    ..Creator::new(display, role)
                }
            } else if let Some(name) = text(person.get("name"), 4096) {
                Creator {
                    name_type: Some(CreatorNameType::Organization),
                    literal_name: Some(name.clone()),
                    ..Creator::new(name, role)
                }
            } else {
                out.note("creator_without_name_skipped");
                continue;
            };
            creator.creator_id = Some(format!("creator-{}", creators.len() + 1));
            // Only explicit per-author affiliations become linked institutions.
            for affiliation in person
                .get("affiliation")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = text(affiliation.get("name"), 4096) {
                    let institution_id = format!("institution-{}", institutions.len() + 1);
                    creator.institution_ids.push(institution_id.clone());
                    institutions.push(Institution {
                        institution_id,
                        name,
                        identifiers: Vec::new(),
                        country: None,
                    });
                }
            }
            creators.push(creator);
        }
    }
    if creators.is_empty() {
        return;
    }
    if creators.len() > 64 || institutions.len() > 1024 {
        out.note("creators_exceed_bound");
        return;
    }
    if !institutions.is_empty() {
        out.push("institutions", institutions, "crossref:author.affiliation");
    }
    out.push("creators", creators, "crossref:author");
}

fn crossref_reference(index: usize, value: &Value) -> CitedReference {
    let identifiers = text(value.get("DOI"), 4096)
        .and_then(|doi| source_identifier(IdentifierKind::Doi, &doi, None))
        .map(|identifier| vec![identifier]);
    CitedReference {
        reference_id: format!("reference-{}", index + 1),
        raw_text: text(value.get("unstructured"), 16 * 1024),
        title: text(value.get("article-title"), 4096)
            .or_else(|| text(value.get("volume-title"), 4096)),
        creators: text(value.get("author"), 4096)
            .map(|author| vec![Creator::new(author, CreatorRole::Author)]),
        published_date: text(value.get("year"), 64).map(|year| PartialDate::parse_legacy(&year)),
        journal_title: text(value.get("journal-title"), 4096),
        volume: text(value.get("volume"), 4096),
        issue: text(value.get("issue"), 4096),
        pages: text(value.get("first-page"), 4096),
        identifiers,
        target_item_id: None,
        source_refs: text(value.get("key"), 1024)
            .map(|key| vec![format!("crossref_reference_key:{key}")])
            .unwrap_or_default(),
    }
}

fn parse_crossref(doi: &str, root: &Value, out: &mut Parsed) -> Result<()> {
    if root.get("status").and_then(Value::as_str) != Some("ok")
        || root.get("message-type").and_then(Value::as_str) != Some("work")
    {
        anyhow::bail!("invalid_lookup_response")
    }
    let message = root
        .get("message")
        .filter(|value| value.is_object())
        .context("invalid_lookup_response")?;
    let response_doi = message.get("DOI").and_then(Value::as_str);
    if response_doi
        .map(normalize_doi)
        .and_then(|value| value.ok())
        .as_deref()
        != Some(doi)
    {
        anyhow::bail!("lookup_identifier_mismatch")
    }
    if let Some(title) = strings(message.get("title"), 4096).into_iter().next() {
        out.push("title", title, "crossref:title");
    }
    crossref_creators(message, out);
    if let Some(value) = strings(message.get("container-title"), 4096)
        .into_iter()
        .next()
    {
        out.push("journal_title", value, "crossref:container-title");
    }
    for (key, field) in [
        ("publisher", "publisher"),
        ("volume", "volume"),
        ("issue", "issue"),
        ("page", "pages"),
        ("language", "language"),
    ] {
        if let Some(value) = text(message.get(key), 4096) {
            out.push(field, value, &format!("crossref:{key}"));
        }
    }
    let mut identifiers = vec![ItemIdentifier {
        raw_value: response_doi
            .and_then(|raw| clean(raw, 4096))
            .filter(|raw| raw != doi),
        ..ItemIdentifier::new(IdentifierKind::Doi, doi)
    }];
    for (kind, typed_key, plain_key) in [
        (IdentifierKind::Issn, "issn-type", "ISSN"),
        (IdentifierKind::Isbn, "isbn-type", "ISBN"),
    ] {
        match message.get(typed_key).and_then(Value::as_array) {
            Some(typed) => identifiers.extend(typed.iter().filter_map(|entry| {
                let value = text(entry.get("value"), 4096)?;
                source_identifier(kind, &value, text(entry.get("type"), 128))
            })),
            None => identifiers.extend(
                strings(message.get(plain_key), 4096)
                    .iter()
                    .filter_map(|value| source_identifier(kind, value, None)),
            ),
        }
    }
    if let Some(identifiers) = finish_identifiers(identifiers) {
        out.push("identifiers", identifiers, "crossref:identifiers");
    }
    let mut keywords = Vec::new();
    for keyword in strings(message.get("subject"), 1024) {
        if !keywords.contains(&keyword) {
            keywords.push(keyword);
        }
    }
    if !keywords.is_empty() && keywords.len() <= 256 {
        out.push("keywords", keywords, "crossref:subject");
    }
    if let Some(raw) = message.get("abstract").and_then(Value::as_str) {
        // Never truncated: over the Store bound it is reported, not proposed.
        match clean(&strip_markup(raw), usize::MAX) {
            Some(value) if value.len() <= 64 * 1024 => {
                out.push("abstract_text", value, "crossref:abstract")
            }
            Some(_) => out.note("abstract_text_exceeds_bound"),
            None => {}
        }
    }
    // Distinct labelled candidates; no earliest/latest rule is applied.
    for key in ["issued", "published-print", "published-online"] {
        if let Some(date) = crossref_date(message.get(key)) {
            out.push("published_date", date, &format!("crossref:{key}"));
        }
    }
    if let Some(count) = message.get("reference-count").and_then(Value::as_u64) {
        out.note(format!("crossref_reference_count:{count}"));
    }
    match message.get("reference") {
        // Absent means not supplied; it is never presented as an empty list.
        None => out.note("cited_references_not_supplied"),
        Some(Value::Array(references)) if references.len() <= 4096 => {
            let references: Vec<_> = references
                .iter()
                .enumerate()
                .map(|(index, value)| crossref_reference(index, value))
                .collect();
            out.push("cited_references", references, "crossref:reference");
        }
        Some(Value::Array(_)) => out.note("cited_references_exceed_bound"),
        Some(_) => anyhow::bail!("invalid_lookup_response"),
    }
    Ok(())
}

fn parse_open_library(isbn: &str, root: &Value, out: &mut Parsed) -> Result<()> {
    let edition = root.as_object().context("invalid_lookup_response")?;
    if edition
        .get("type")
        .is_some_and(|kind| kind.get("key").and_then(Value::as_str) != Some("/type/edition"))
    {
        anyhow::bail!("invalid_lookup_response")
    }
    let mut identifiers = Vec::new();
    for key in ["isbn_13", "isbn_10"] {
        identifiers.extend(
            strings(edition.get(key), 64)
                .iter()
                .filter_map(|value| source_identifier(IdentifierKind::Isbn, value, None)),
        );
    }
    if !identifiers
        .iter()
        .any(|identifier| identifier.value == isbn)
    {
        anyhow::bail!("lookup_identifier_mismatch")
    }
    if let Some(title) = text(edition.get("title"), 4096) {
        out.push("title", title, "open_library:title");
    }
    for (key, field) in [
        ("publishers", "publisher"),
        ("publish_places", "publication_place"),
        ("series", "series_title"),
    ] {
        if let Some(value) = joined(strings(edition.get(key), 4096), 4096) {
            out.push(field, value, &format!("open_library:{key}"));
        }
    }
    if let Some(raw) = text(edition.get("publish_date"), 4096) {
        // Unambiguous forms only; e.g. "March 2004" stays unparsed with raw text.
        out.push(
            "published_date",
            PartialDate::parse_legacy(&raw),
            "open_library:publish_date",
        );
    }
    if let Some(value) = text(edition.get("edition_name"), 4096) {
        out.push("edition", value, "open_library:edition_name");
    }
    if let Some(pages) = edition
        .get("number_of_pages")
        .and_then(Value::as_u64)
        .filter(|pages| (1..=1_000_000).contains(pages))
    {
        out.push(
            "number_of_pages",
            pages as u32,
            "open_library:number_of_pages",
        );
    }
    if let Some(identifiers) = finish_identifiers(identifiers) {
        out.push("identifiers", identifiers, "open_library:isbn");
    }
    let languages: Vec<String> = edition
        .get("languages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|language| {
            let code = language.get("key")?.as_str()?.strip_prefix("/languages/")?;
            (2..=8)
                .contains(&code.len())
                .then_some(code)
                .filter(|code| code.bytes().all(|b| b.is_ascii_lowercase()))
                .map(str::to_owned)
        })
        .collect();
    if let Some(value) = joined(languages, 256) {
        out.push("language", value, "open_library:languages");
    }
    // Edition authors are /authors/ keys; resolving them is a separate request
    // that this approval does not cover, so creators stay absent.
    if edition.get("authors").is_some() {
        out.note("creators_not_resolved_author_keys_only");
    }
    Ok(())
}

fn placeholder_provenance(provider: LookupProvider) -> MetadataProvenance {
    MetadataProvenance::new(
        MetadataOrigin::LookupSelected,
        provider.provider_id(),
        LOOKUP_ADAPTER_VERSION,
    )
}

/// Pure adapter entry point. Drops (with a note) any proposal that could not
/// be committed, so a frozen candidate never fails Store validation on apply.
pub(crate) fn parse_provider_response(
    provider: LookupProvider,
    identifier: &str,
    bytes: &[u8],
) -> Result<Parsed> {
    if bytes.len() > MAX_LOOKUP_RESPONSE_BYTES {
        anyhow::bail!("lookup_response_too_large")
    }
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("invalid_lookup_response"))?;
    let mut parsed = Parsed::default();
    match provider {
        LookupProvider::Crossref => parse_crossref(identifier, &root, &mut parsed)?,
        LookupProvider::OpenLibrary => parse_open_library(identifier, &root, &mut parsed)?,
    }
    let institutions = parsed
        .proposals
        .iter()
        .find(|proposal| proposal.field == "institutions")
        .map(|proposal| proposal.value.clone());
    let mut dropped = Vec::new();
    parsed.proposals.retain(|proposal| {
        let mut metadata = ItemMetadata::default();
        let mut title = String::new();
        let linked = proposal.field != "creators"
            || institutions.as_ref().is_none_or(|value| {
                apply_value(
                    &mut metadata,
                    &mut title,
                    "institutions",
                    value.clone(),
                    placeholder_provenance(provider),
                )
                .is_ok()
            });
        let ok = linked
            && apply_value(
                &mut metadata,
                &mut title,
                proposal.field,
                proposal.value.clone(),
                placeholder_provenance(provider),
            )
            .is_ok()
            && validate_metadata(&metadata).is_ok()
            && (proposal.field != "title" || valid_text(&title, 4096));
        if !ok {
            dropped.push(format!("{}_invalid_dropped", proposal.field));
        }
        ok
    });
    for note in dropped {
        parsed.note(note);
    }
    Ok(parsed)
}

/// Writes one candidate value into the revision being built.
pub(crate) fn apply_value(
    metadata: &mut ItemMetadata,
    title: &mut String,
    field: &str,
    value: Value,
    provenance: MetadataProvenance,
) -> Result<()> {
    fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("invalid_lookup_preview"))
    }
    macro_rules! sourced {
        ($slot:expr) => {
            $slot = Some(Sourced {
                value: typed(value)?,
                provenance,
            })
        };
    }
    match field {
        "title" => *title = typed(value)?,
        "published_date" => {
            let date: PartialDate = typed(value)?;
            let legacy = date
                .legacy_string()
                .context("invalid_lookup_preview")?
                .to_owned();
            metadata.published_date = Some(Sourced {
                value: legacy,
                provenance: provenance.clone(),
            });
            metadata.published_partial_date = Some(Sourced {
                value: date,
                provenance,
            });
        }
        "creators" => sourced!(metadata.creators),
        "identifiers" => sourced!(metadata.identifiers),
        "language" => sourced!(metadata.language),
        "site_name" => sourced!(metadata.site_name),
        "abstract_text" => sourced!(metadata.abstract_text),
        "journal_title" => sourced!(metadata.journal_title),
        "publisher" => sourced!(metadata.publisher),
        "volume" => sourced!(metadata.volume),
        "issue" => sourced!(metadata.issue),
        "pages" => sourced!(metadata.pages),
        "publication_place" => sourced!(metadata.publication_place),
        "edition" => sourced!(metadata.edition),
        "number_of_pages" => sourced!(metadata.number_of_pages),
        "series_title" => sourced!(metadata.series_title),
        "series_number" => sourced!(metadata.series_number),
        "keywords" => sourced!(metadata.keywords),
        "institutions" => sourced!(metadata.institutions),
        "cited_references" => sourced!(metadata.cited_references),
        _ => anyhow::bail!("invalid_lookup_preview"),
    }
    Ok(())
}

fn current_value(field: &str, title: &str, metadata: &ItemMetadata) -> Option<Value> {
    fn inner<T: serde::Serialize>(value: &Option<Sourced<T>>) -> Option<Value> {
        value
            .as_ref()
            .and_then(|value| serde_json::to_value(&value.value).ok())
    }
    match field {
        "title" => Some(Value::String(title.to_owned())),
        "published_date" => match &metadata.published_partial_date {
            Some(date) => serde_json::to_value(&date.value).ok(),
            None => metadata
                .published_date
                .as_ref()
                .and_then(|date| serde_json::to_value(PartialDate::parse_legacy(&date.value)).ok()),
        },
        "creators" => inner(&metadata.creators),
        "identifiers" => inner(&metadata.identifiers),
        "language" => inner(&metadata.language),
        "site_name" => inner(&metadata.site_name),
        "abstract_text" => inner(&metadata.abstract_text),
        "journal_title" => inner(&metadata.journal_title),
        "publisher" => inner(&metadata.publisher),
        "volume" => inner(&metadata.volume),
        "issue" => inner(&metadata.issue),
        "pages" => inner(&metadata.pages),
        "publication_place" => inner(&metadata.publication_place),
        "edition" => inner(&metadata.edition),
        "number_of_pages" => inner(&metadata.number_of_pages),
        "series_title" => inner(&metadata.series_title),
        "series_number" => inner(&metadata.series_number),
        "keywords" => inner(&metadata.keywords),
        "institutions" => inner(&metadata.institutions),
        "cited_references" => inner(&metadata.cited_references),
        _ => None,
    }
}

fn build_candidates(
    parsed: &Parsed,
    title: &str,
    metadata: &ItemMetadata,
    next_id: &mut dyn FnMut() -> String,
) -> Vec<LookupCandidate> {
    let proposed_institutions: BTreeSet<String> = parsed
        .proposals
        .iter()
        .filter(|proposal| proposal.field == "institutions")
        .filter_map(|proposal| {
            serde_json::from_value::<Vec<Institution>>(proposal.value.clone()).ok()
        })
        .flatten()
        .map(|institution| institution.institution_id)
        .collect();
    let current_links: BTreeSet<&str> = metadata
        .creators
        .iter()
        .flat_map(|creators| &creators.value)
        .flat_map(|creator| creator.institution_ids.iter().map(String::as_str))
        .collect();
    parsed
        .proposals
        .iter()
        .map(|proposal| {
            let current_value = current_value(proposal.field, title, metadata);
            let unchanged = current_value.as_ref() == Some(&proposal.value);
            let requires = match proposal.field {
                "creators"
                    if serde_json::from_value::<Vec<Creator>>(proposal.value.clone())
                        .is_ok_and(|creators| {
                            creators
                                .iter()
                                .any(|creator| !creator.institution_ids.is_empty())
                        }) =>
                {
                    vec!["institutions".to_owned()]
                }
                "institutions"
                    if current_links
                        .iter()
                        .any(|id| !proposed_institutions.contains(*id)) =>
                {
                    vec!["creators".to_owned()]
                }
                _ => Vec::new(),
            };
            LookupCandidate {
                candidate_id: next_id(),
                field: proposal.field.to_owned(),
                conflict: current_value.is_some() && !unchanged,
                current_value,
                proposed_value: proposal.value.clone(),
                source_label: proposal.source_label.clone(),
                unchanged,
                selected: false,
                requires,
            }
        })
        .collect()
}

fn lookup_dir(root: &Path, sub: &str) -> Result<PathBuf> {
    let parent = ensure_contained_dir(root, Path::new(LOOKUP_DIR))?;
    let dir = ensure_contained_dir(root, &Path::new(LOOKUP_DIR).join(sub))?;
    #[cfg(unix)]
    for path in [&parent, &dir] {
        fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    Ok(dir)
}
fn preview_path(root: &Path, preview_id: &str) -> PathBuf {
    root.join(LOOKUP_DIR)
        .join("previews")
        .join(format!("{preview_id}.json"))
}
fn blob_name(response_sha256: &str) -> Result<String> {
    response_sha256
        .strip_prefix("sha256:")
        .filter(|_| valid_sha256(response_sha256))
        .map(|hex| format!("sha256-{hex}.json"))
        .context("invalid_lookup_preview")
}

fn read_lookup_blob(root: &Path, path: &Path, response_sha256: &str) -> Result<Vec<u8>> {
    let invalid = || anyhow::anyhow!("invalid_lookup_preview");
    let path = contained_existing(root, path).map_err(|_| invalid())?;
    let file = open_regular_nofollow(&path).map_err(|_| invalid())?;
    if file.metadata()?.len() > MAX_LOOKUP_RESPONSE_BYTES as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOOKUP_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if format!("sha256:{:x}", Sha256::digest(&bytes)) != response_sha256 {
        return Err(invalid());
    }
    Ok(bytes)
}

fn load_preview(root: &Path, preview_id: &str) -> Result<Option<LookupPreview>> {
    if !safe_component(preview_id, "preview_") || preview_id.len() > 128 {
        return Ok(None);
    }
    let path = preview_path(root, preview_id);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            anyhow::bail!("invalid_lookup_preview")
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    let preview: LookupPreview =
        load(root, path).map_err(|_| anyhow::anyhow!("invalid_lookup_preview"))?;
    if preview.schema_version != SCHEMA_VERSION
        || preview.preview_version != LOOKUP_PREVIEW_VERSION
        || preview.preview_id != preview_id
    {
        anyhow::bail!("invalid_lookup_preview")
    }
    Ok(Some(preview))
}

fn expired(preview: &LookupPreview, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(&preview.expires_at).map_or(true, |at| at <= now)
}

/// Under the writer lock: removes expired or unreadable previews, crash
/// residue, and response blobs no live preview references. Retained sources
/// of applied revisions (`sources/`) are never collected. Returns live count.
fn gc_lookup_locked(root: &Path, now: chrono::DateTime<chrono::Utc>) -> Result<usize> {
    let previews = lookup_dir(root, "previews")?;
    let responses = lookup_dir(root, "responses")?;
    let mut referenced = BTreeSet::new();
    let mut live = 0;
    for entry in fs::read_dir(&previews)? {
        let entry = entry?;
        let name = entry.file_name().into_string().unwrap_or_default();
        if entry.file_type()?.is_symlink() || !entry.file_type()?.is_file() {
            anyhow::bail!("unsafe lookup preview entry")
        }
        let keep = name
            .strip_suffix(".json")
            .and_then(|id| load_preview(root, id).ok().flatten())
            .filter(|preview| !expired(preview, now));
        match keep {
            Some(preview) => {
                referenced.insert(blob_name(&preview.response_sha256)?);
                live += 1;
            }
            None => fs::remove_file(entry.path())?,
        }
    }
    for entry in fs::read_dir(&responses)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() || !entry.file_type()?.is_file() {
            anyhow::bail!("unsafe lookup response entry")
        }
        let name = entry.file_name().into_string().unwrap_or_default();
        if !referenced.contains(&name) {
            fs::remove_file(entry.path())?;
        }
    }
    sync_dir(&previews)?;
    sync_dir(&responses)?;
    Ok(live)
}

fn visible_item(
    root: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<(Item, ItemRevision)> {
    read_item_at_sensitivity(root, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))
}

/// Authorizes one explicit single-item lookup before any network request. The
/// identifier must be locally valid and present on the item's current revision.
pub fn prepare_bibliographic_lookup(
    library: impl AsRef<Path>,
    item_id: &str,
    expected_revision_id: &str,
    identifier: &ItemIdentifier,
    max_sensitivity: Sensitivity,
) -> Result<LookupTarget> {
    let provider =
        LookupProvider::for_kind(identifier.kind).context("lookup_identifier_unsupported")?;
    let key = identifier
        .normalized()
        .comparison_key
        .context("lookup_identifier_invalid")?;
    if !valid_item_id(item_id) || !valid_revision_id(expected_revision_id) {
        anyhow::bail!("invalid_lookup_request")
    }
    let request_url = lookup_request_url(provider, &key)?.to_string();
    with_consistent_snapshot(library, |root| {
        let (_, current) = visible_item(root, item_id, max_sensitivity)?;
        if current.revision_id != expected_revision_id {
            anyhow::bail!("stale_item_revision")
        }
        let on_item = current
            .metadata
            .identifiers
            .iter()
            .flat_map(|identifiers| &identifiers.value)
            .any(|stored| {
                stored.kind == identifier.kind
                    && stored.normalized().comparison_key.as_deref() == Some(key.as_str())
            });
        if !on_item {
            anyhow::bail!("lookup_identifier_not_on_item")
        }
        Ok(LookupTarget {
            item_id: item_id.to_owned(),
            expected_revision_id: expected_revision_id.to_owned(),
            provider,
            identifier: ItemIdentifier::new(identifier.kind, key.clone()),
            request_url: request_url.clone(),
        })
    })
}

/// Freezes a preview from response bytes fetched (outside the lock) for
/// `target`. Re-checks authorization and the revision under the writer lock;
/// persists the raw response then the preview record, both crash-safe.
pub fn create_lookup_preview(
    library: impl AsRef<Path>,
    target: &LookupTarget,
    response: &[u8],
    fetched_at: &str,
    max_sensitivity: Sensitivity,
) -> Result<LookupPreview> {
    if lookup_request_url(target.provider, &target.identifier.value)?.as_str() != target.request_url
        || target.identifier.kind != target.provider.identifier_kind()
        || chrono::DateTime::parse_from_rfc3339(fetched_at).is_err()
    {
        anyhow::bail!("invalid_lookup_request")
    }
    let parsed = parse_provider_response(target.provider, &target.identifier.value, response)?;
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, current) = visible_item(&root, &target.item_id, max_sensitivity)?;
    if current.revision_id != target.expected_revision_id {
        anyhow::bail!("stale_item_revision")
    }
    let now = chrono::Utc::now();
    if gc_lookup_locked(&root, now)? >= MAX_LOOKUP_PREVIEWS {
        anyhow::bail!("lookup_preview_limit_exceeded")
    }
    let response_sha256 = format!("sha256:{:x}", Sha256::digest(response));
    let responses = lookup_dir(&root, "responses")?;
    install_binary(
        &root,
        &responses.join(blob_name(&response_sha256)?),
        response,
    )?;
    sync_dir(&responses)?;
    let preview = LookupPreview {
        schema_version: SCHEMA_VERSION,
        preview_version: LOOKUP_PREVIEW_VERSION,
        metadata_schema_version: METADATA_SCHEMA_VERSION,
        preview_id: new_id("preview"),
        item_id: target.item_id.clone(),
        expected_revision_id: current.revision_id.clone(),
        sensitivity: item.sensitivity.max(current.sensitivity),
        provider: target.provider,
        adapter_version: LOOKUP_ADAPTER_VERSION.into(),
        identifier: target.identifier.clone(),
        request_url: target.request_url.clone(),
        fetched_at: fetched_at.to_owned(),
        created_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::seconds(LOOKUP_PREVIEW_TTL_SECONDS)).to_rfc3339(),
        source_ref: format!("lookup_response:{response_sha256}"),
        response_sha256,
        response_byte_size: response.len() as u64,
        candidates: build_candidates(&parsed, &current.title, &current.metadata, &mut || {
            new_id("candidate")
        }),
        notes: parsed.notes,
    };
    let previews = lookup_dir(&root, "previews")?;
    durable_replace(
        &root,
        &previews.join(format!("{}.json", preview.preview_id)),
        serde_json::to_string_pretty(&preview)?.as_bytes(),
    )?;
    lock.unlock()?;
    Ok(preview)
}

pub(crate) struct AppliedLookup {
    pub preview_id: String,
    pub response_sha256: String,
    response: Vec<u8>,
}

/// Applies selected frozen candidates (caller holds the writer lock and has
/// authorized the item and checked `expected_revision_id`). Returns `None`
/// when no selected candidate changes anything.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_lookup_selection_locked(
    root: &Path,
    preview_id: &str,
    selected: &[String],
    current: &ItemRevision,
    max_sensitivity: Sensitivity,
    owner_fields: &BTreeSet<String>,
    metadata: &mut ItemMetadata,
    title: &mut String,
) -> Result<Option<AppliedLookup>> {
    let preview = load_preview(root, preview_id)?
        .filter(|preview| {
            preview.item_id == current.item_id && preview.sensitivity <= max_sensitivity
        })
        .ok_or_else(|| anyhow::anyhow!("preview_unavailable"))?;
    if preview.expected_revision_id != current.revision_id {
        anyhow::bail!("stale_item_revision")
    }
    if expired(&preview, chrono::Utc::now()) {
        anyhow::bail!("preview_expired")
    }
    let blob = root
        .join(LOOKUP_DIR)
        .join("responses")
        .join(blob_name(&preview.response_sha256)?);
    let response = read_lookup_blob(root, &blob, &preview.response_sha256)?;
    let parsed = parse_provider_response(preview.provider, &preview.identifier.value, &response)
        .map_err(|_| anyhow::anyhow!("invalid_lookup_preview"))?;
    let mut ids = preview
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone());
    let rebuilt = build_candidates(&parsed, &current.title, &current.metadata, &mut || {
        ids.next().unwrap_or_default()
    });
    if rebuilt != preview.candidates
        || parsed.notes != preview.notes
        || preview.adapter_version != LOOKUP_ADAPTER_VERSION
        || lookup_request_url(preview.provider, &preview.identifier.value)?.as_str()
            != preview.request_url
    {
        anyhow::bail!("invalid_lookup_preview")
    }
    let mut fields = BTreeSet::new();
    let mut chosen = Vec::new();
    for candidate_id in selected {
        let candidate = preview
            .candidates
            .iter()
            .find(|candidate| &candidate.candidate_id == candidate_id)
            .ok_or_else(|| anyhow::anyhow!("invalid_candidate_selection"))?;
        if !fields.insert(candidate.field.clone()) {
            anyhow::bail!("invalid_candidate_selection")
        }
        if owner_fields.contains(&candidate.field) {
            anyhow::bail!(
                "invalid_metadata_field:{}:set_clear_overlap",
                candidate.field
            )
        }
        chosen.push(candidate);
    }
    for candidate in &chosen {
        if let Some(missing) = candidate
            .requires
            .iter()
            .find(|field| !fields.contains(*field) && !owner_fields.contains(*field))
        {
            anyhow::bail!("candidate_dependency_missing:{}:{missing}", candidate.field)
        }
    }
    let mut changed = false;
    for candidate in chosen.into_iter().filter(|candidate| !candidate.unchanged) {
        let provenance = MetadataProvenance {
            source_refs: vec![
                format!("lookup_response:{}", preview.response_sha256),
                format!("lookup_preview:{preview_id}"),
                format!("lookup_candidate:{}", candidate.candidate_id),
            ],
            ..placeholder_provenance(preview.provider)
        };
        apply_value(
            metadata,
            title,
            &candidate.field,
            candidate.proposed_value.clone(),
            provenance,
        )?;
        changed = true;
    }
    Ok(changed.then(|| AppliedLookup {
        preview_id: preview_id.to_owned(),
        response_sha256: preview.response_sha256,
        response,
    }))
}

/// Applied provider responses become immutable, never-collected source
/// snapshots referenced by the revision provenance.
pub(crate) fn retain_lookup_source(root: &Path, applied: &AppliedLookup) -> Result<()> {
    let sources = lookup_dir(root, "sources")?;
    install_binary(
        root,
        &sources.join(blob_name(&applied.response_sha256)?),
        &applied.response,
    )?;
    sync_dir(&sources)
}

/// Chain-validation rule for a lookup-applied field.
pub(crate) fn is_lookup_provenance(
    provenance: &MetadataProvenance,
    preview_id: &str,
    response_sha256: &str,
) -> bool {
    provenance.origin == MetadataOrigin::LookupSelected
        && LookupProvider::from_provider_id(&provenance.provider_id).is_some()
        && provenance.provider_version == LOOKUP_ADAPTER_VERSION
        && provenance.source_refs.len() == 3
        && provenance.source_refs[0] == format!("lookup_response:{response_sha256}")
        && provenance.source_refs[1] == format!("lookup_preview:{preview_id}")
        && provenance.source_refs[2]
            .strip_prefix("lookup_candidate:")
            .is_some_and(|id| safe_component(id, "candidate_"))
}

/// Candidate page bounds: one IPC frame stays well under 256 KiB.
pub const MAX_LOOKUP_CANDIDATE_PAGE: usize = 50;
pub const LOOKUP_PAGE_BUDGET_BYTES: usize = 128 * 1024;
/// Values whose JSON exceeds this are summarized in pages; read them in full
/// with `read_lookup_value`.
pub const MAX_INLINE_LOOKUP_VALUE_BYTES: usize = 8 * 1024;
pub const LOOKUP_VALUE_PAGE_BYTES: usize = 64 * 1024;
const VALUE_SUMMARY_PREVIEW_BYTES: usize = 1024;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LookupValueKind {
    String,
    Array,
    Other,
}

/// Bounded description of a value too large to inline. `total` is UTF-8 bytes
/// for strings and element count for arrays; `preview` is a bounded prefix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupValueSummary {
    pub kind: LookupValueKind,
    pub json_bytes: u64,
    pub total: u64,
    pub preview: Value,
}

/// Wire view of a frozen candidate. Exactly one of `*_value` (inline) or
/// `*_value_summary` is set for a present value; both null means absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupCandidateView {
    pub candidate_id: String,
    pub field: String,
    pub current_value: Option<Value>,
    pub current_value_summary: Option<LookupValueSummary>,
    pub proposed_value: Option<Value>,
    pub proposed_value_summary: Option<LookupValueSummary>,
    pub source_label: String,
    pub conflict: bool,
    pub unchanged: bool,
    pub selected: bool,
    pub requires: Vec<String>,
}

/// One bounded page of a frozen preview (header + candidate window).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupPreviewPage {
    pub schema_version: u32,
    pub preview_version: u32,
    pub metadata_schema_version: u32,
    pub preview_id: String,
    pub item_id: String,
    pub expected_revision_id: String,
    pub sensitivity: Sensitivity,
    pub provider: LookupProvider,
    pub adapter_version: String,
    pub identifier: ItemIdentifier,
    pub request_url: String,
    pub fetched_at: String,
    pub created_at: String,
    pub expires_at: String,
    pub response_sha256: String,
    pub response_byte_size: u64,
    pub source_ref: String,
    pub notes: Vec<String>,
    /// The item's current revision no longer matches; apply would be stale.
    pub stale: bool,
    pub total_candidates: usize,
    pub candidate_offset: usize,
    pub next_offset: Option<usize>,
    pub candidates: Vec<LookupCandidateView>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LookupValueSide {
    Current,
    Proposed,
}

/// Window of one candidate value: byte range of a string (char-aligned) or
/// element range of an array. Other values are returned whole at offset 0.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupValuePage {
    pub preview_id: String,
    pub candidate_id: String,
    pub side: LookupValueSide,
    pub kind: LookupValueKind,
    pub total: u64,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub value: Option<Value>,
}

fn json_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn char_floor(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Array prefix (from `offset`) whose JSON fits `budget`; at least one element.
fn array_window(items: &[Value], offset: usize, budget: usize, min_one: bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut used = 2;
    for item in items.iter().skip(offset) {
        let size = json_len(item) + 1;
        if used + size > budget && !(min_one && out.is_empty()) {
            break;
        }
        used += size;
        out.push(item.clone());
    }
    out
}

fn split_value(value: Option<&Value>) -> (Option<Value>, Option<LookupValueSummary>) {
    let Some(value) = value else {
        return (None, None);
    };
    let json_bytes = json_len(value);
    if json_bytes <= MAX_INLINE_LOOKUP_VALUE_BYTES {
        return (Some(value.clone()), None);
    }
    let (kind, total, preview) = match value {
        Value::String(text) => (
            LookupValueKind::String,
            text.len() as u64,
            Value::String(text[..char_floor(text, VALUE_SUMMARY_PREVIEW_BYTES)].to_owned()),
        ),
        Value::Array(items) => (
            LookupValueKind::Array,
            items.len() as u64,
            Value::Array(array_window(items, 0, VALUE_SUMMARY_PREVIEW_BYTES, false)),
        ),
        _ => (LookupValueKind::Other, 1, Value::Null),
    };
    (
        None,
        Some(LookupValueSummary {
            kind,
            json_bytes: json_bytes as u64,
            total,
            preview,
        }),
    )
}

fn candidate_view(candidate: &LookupCandidate) -> LookupCandidateView {
    let (current_value, current_value_summary) = split_value(candidate.current_value.as_ref());
    let (proposed_value, proposed_value_summary) = split_value(Some(&candidate.proposed_value));
    LookupCandidateView {
        candidate_id: candidate.candidate_id.clone(),
        field: candidate.field.clone(),
        current_value,
        current_value_summary,
        proposed_value,
        proposed_value_summary,
        source_label: candidate.source_label.clone(),
        conflict: candidate.conflict,
        unchanged: candidate.unchanged,
        selected: candidate.selected,
        requires: candidate.requires.clone(),
    }
}

/// Bounded page over a frozen preview; does not modify it.
pub fn page_lookup_preview(
    preview: &LookupPreview,
    stale: bool,
    offset: usize,
    limit: usize,
) -> Result<LookupPreviewPage> {
    if limit == 0 || limit > MAX_LOOKUP_CANDIDATE_PAGE || offset > preview.candidates.len() {
        anyhow::bail!("invalid_lookup_window")
    }
    let mut candidates = Vec::new();
    let mut used = 0;
    for candidate in preview.candidates.iter().skip(offset).take(limit) {
        let view = candidate_view(candidate);
        let size = serde_json::to_vec(&view)?.len() + 1;
        if used + size > LOOKUP_PAGE_BUDGET_BYTES && !candidates.is_empty() {
            break;
        }
        used += size;
        candidates.push(view);
    }
    let next = offset + candidates.len();
    Ok(LookupPreviewPage {
        schema_version: preview.schema_version,
        preview_version: preview.preview_version,
        metadata_schema_version: preview.metadata_schema_version,
        preview_id: preview.preview_id.clone(),
        item_id: preview.item_id.clone(),
        expected_revision_id: preview.expected_revision_id.clone(),
        sensitivity: preview.sensitivity,
        provider: preview.provider,
        adapter_version: preview.adapter_version.clone(),
        identifier: preview.identifier.clone(),
        request_url: preview.request_url.clone(),
        fetched_at: preview.fetched_at.clone(),
        created_at: preview.created_at.clone(),
        expires_at: preview.expires_at.clone(),
        response_sha256: preview.response_sha256.clone(),
        response_byte_size: preview.response_byte_size,
        source_ref: preview.source_ref.clone(),
        notes: preview.notes.clone(),
        stale,
        total_candidates: preview.candidates.len(),
        candidate_offset: offset,
        next_offset: (next < preview.candidates.len()).then_some(next),
        candidates,
    })
}

/// Authorizes a preview read: hidden items, missing or above-ceiling previews
/// are indistinguishable (`preview_unavailable`); expiry is reported after.
fn authorized_preview(
    root: &Path,
    preview_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<(LookupPreview, bool)> {
    let preview = load_preview(root, preview_id)?
        .filter(|preview| preview.sensitivity <= max_sensitivity)
        .ok_or_else(|| anyhow::anyhow!("preview_unavailable"))?;
    let (_, current) = visible_item(root, &preview.item_id, max_sensitivity)
        .map_err(|_| anyhow::anyhow!("preview_unavailable"))?;
    if expired(&preview, chrono::Utc::now()) {
        anyhow::bail!("preview_expired")
    }
    let stale = current.revision_id != preview.expected_revision_id;
    Ok((preview, stale))
}

pub fn read_lookup_preview(
    library: impl AsRef<Path>,
    preview_id: &str,
    max_sensitivity: Sensitivity,
    candidate_offset: usize,
    candidate_limit: usize,
) -> Result<LookupPreviewPage> {
    with_consistent_snapshot(library, |root| {
        let (preview, stale) = authorized_preview(root, preview_id, max_sensitivity)?;
        page_lookup_preview(&preview, stale, candidate_offset, candidate_limit)
    })
}

pub fn read_lookup_value(
    library: impl AsRef<Path>,
    preview_id: &str,
    candidate_id: &str,
    side: LookupValueSide,
    offset: usize,
    max_sensitivity: Sensitivity,
) -> Result<LookupValuePage> {
    with_consistent_snapshot(library, |root| {
        let (preview, _) = authorized_preview(root, preview_id, max_sensitivity)?;
        let candidate = preview
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
            .ok_or_else(|| anyhow::anyhow!("invalid_candidate_selection"))?;
        let value = match side {
            LookupValueSide::Current => candidate.current_value.as_ref(),
            LookupValueSide::Proposed => Some(&candidate.proposed_value),
        };
        let window = || anyhow::anyhow!("invalid_lookup_window");
        let (kind, total, value, next) = match value {
            None if offset == 0 => (LookupValueKind::Other, 0, None, None),
            None => return Err(window()),
            Some(Value::String(text)) => {
                if offset > text.len() || !text.is_char_boundary(offset) {
                    return Err(window());
                }
                let end = char_floor(text, offset + LOOKUP_VALUE_PAGE_BYTES);
                let end = if end == offset && offset < text.len() {
                    // Defensive: a page always advances by at least one char.
                    offset + text[offset..].chars().next().map_or(0, char::len_utf8)
                } else {
                    end
                };
                (
                    LookupValueKind::String,
                    text.len() as u64,
                    Some(Value::String(text[offset..end].to_owned())),
                    (end < text.len()).then_some(end),
                )
            }
            Some(Value::Array(items)) => {
                if offset > items.len() {
                    return Err(window());
                }
                let slice = array_window(items, offset, LOOKUP_VALUE_PAGE_BYTES, true);
                let end = offset + slice.len();
                (
                    LookupValueKind::Array,
                    items.len() as u64,
                    Some(Value::Array(slice)),
                    (end < items.len()).then_some(end),
                )
            }
            Some(other) if offset == 0 => (LookupValueKind::Other, 1, Some(other.clone()), None),
            Some(_) => return Err(window()),
        };
        Ok(LookupValuePage {
            preview_id: preview.preview_id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            side,
            kind,
            total,
            offset,
            next_offset: next,
            value,
        })
    })
}

#[cfg(test)]
impl Parsed {
    pub(crate) fn notes_for_test(&self) -> &[String] {
        &self.notes
    }
}

#[cfg(test)]
pub(crate) fn build_candidates_for_test(
    parsed: &Parsed,
    title: &str,
    metadata: &ItemMetadata,
    next_id: &mut dyn FnMut() -> String,
) -> Vec<LookupCandidate> {
    build_candidates(parsed, title, metadata, next_id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::metadata_v2::tests::*;
    use crate::tests::*;
    use memex_core::{ItemMetadata, MetadataOrigin, Sensitivity};
    pub(crate) fn crossref_fixture(doi: &str, references: Option<serde_json::Value>) -> Vec<u8> {
        let mut message = serde_json::json!({
            "DOI": doi,
            "title": ["  Synthetic\nLookup Title "],
            "author": [
                {"given": "Ada", "family": "Example", "sequence": "first",
                 "affiliation": [{"name": "Synthetic Lab"}]},
                {"name": "Example Consortium", "sequence": "additional"}
            ],
            "container-title": ["Journal of Synthetic Tests"],
            "publisher": "Synthetic Press",
            "volume": "12", "issue": "3", "page": "e1001",
            "ISSN": ["0317-8471"],
            "issn-type": [{"value": "0317-8471", "type": "print"}],
            "subject": ["beta", "alpha", "beta"],
            "abstract": "<jats:p>An &amp; abstract</jats:p>",
            "issued": {"date-parts": [[2024, 2]]},
            "published-print": {"date-parts": [[2024, 3, 1]]},
            "published-online": {"date-parts": [[2024, 2, 29]]},
            "link": [{"URL": "https://fulltext.example.invalid/download.pdf"}],
            "reference-count": 1
        });
        if let Some(references) = references {
            message["reference"] = references;
        }
        serde_json::to_vec(&serde_json::json!({
            "status": "ok", "message-type": "work", "message-version": "1.0.0", "message": message
        }))
        .unwrap()
    }

    pub(crate) fn proposals(parsed: &lookup::Parsed) -> Vec<(String, serde_json::Value, String)> {
        let preview_candidates = lookup_candidates_for_test(parsed);
        preview_candidates
            .into_iter()
            .map(|c| (c.field, c.proposed_value, c.source_label))
            .collect()
    }

    pub(crate) fn lookup_candidates_for_test(parsed: &lookup::Parsed) -> Vec<LookupCandidate> {
        let mut n = 0;
        lookup::build_candidates_for_test(parsed, "t", &ItemMetadata::default(), &mut || {
            n += 1;
            format!("candidate_{n}")
        })
    }

    #[test]
    fn crossref_adapter_keeps_date_candidates_separate_and_reference_absence_distinct() {
        let bytes = crossref_fixture("10.1000/SYNTH(1)", Some(serde_json::json!([])));
        let parsed =
            lookup::parse_provider_response(LookupProvider::Crossref, "10.1000/synth(1)", &bytes)
                .unwrap();
        let all = proposals(&parsed);
        let dates: Vec<_> = all.iter().filter(|p| p.0 == "published_date").collect();
        assert_eq!(dates.len(), 3);
        assert_eq!(
            dates.iter().map(|p| p.2.as_str()).collect::<Vec<_>>(),
            [
                "crossref:issued",
                "crossref:published-print",
                "crossref:published-online"
            ]
        );
        assert_eq!(
            dates[0].1,
            serde_json::json!({"value": "2024-02", "precision": "month", "raw": "[[2024,2]]"})
        );
        assert_eq!(dates[2].1["value"], "2024-02-29");
        let field = |name: &str| all.iter().find(|p| p.0 == name).map(|p| p.1.clone());
        assert_eq!(field("title").unwrap(), "Synthetic Lookup Title");
        assert_eq!(field("abstract_text").unwrap(), "An & abstract");
        assert_eq!(
            field("keywords").unwrap(),
            serde_json::json!(["beta", "alpha"])
        );
        assert_eq!(field("cited_references").unwrap(), serde_json::json!([]));
        let creators: Vec<Creator> = serde_json::from_value(field("creators").unwrap()).unwrap();
        assert_eq!(creators[0].name_type, Some(CreatorNameType::Person));
        assert_eq!(
            (
                creators[0].given_name.as_deref(),
                creators[0].family_name.as_deref()
            ),
            (Some("Ada"), Some("Example"))
        );
        assert_eq!(creators[0].institution_ids, ["institution-1"]);
        assert_eq!(creators[1].name_type, Some(CreatorNameType::Organization));
        assert_eq!(
            creators[1].literal_name.as_deref(),
            Some("Example Consortium")
        );
        assert!(creators[1].institution_ids.is_empty());
        let identifiers: Vec<ItemIdentifier> =
            serde_json::from_value(field("identifiers").unwrap()).unwrap();
        assert_eq!(identifiers[0].value, "10.1000/synth(1)");
        assert_eq!(
            identifiers[0].raw_value.as_deref(),
            Some("10.1000/SYNTH(1)")
        );
        assert_eq!(
            (identifiers[1].kind, identifiers[1].qualifier.as_deref()),
            (IdentifierKind::Issn, Some("print"))
        );
        assert!(
            !serde_json::to_string(&all.iter().map(|p| &p.1).collect::<Vec<_>>())
                .unwrap()
                .contains("fulltext.example")
        );
        let candidates = lookup_candidates_for_test(&parsed);
        let creators = candidates.iter().find(|c| c.field == "creators").unwrap();
        assert_eq!(creators.requires, ["institutions"]);
        let title = candidates.iter().find(|c| c.field == "title").unwrap();
        assert!(title.conflict && !title.unchanged && !title.selected);
        assert_eq!(title.current_value, Some(serde_json::json!("t")));
        let journal = candidates
            .iter()
            .find(|c| c.field == "journal_title")
            .unwrap();
        assert_eq!(
            (journal.current_value.clone(), journal.conflict),
            (None, false)
        );

        // Absent reference list is not an empty one.
        let absent = lookup::parse_provider_response(
            LookupProvider::Crossref,
            "10.1000/synth(1)",
            &crossref_fixture("10.1000/synth(1)", None),
        )
        .unwrap();
        assert!(proposals(&absent).iter().all(|p| p.0 != "cited_references"));
        assert!(absent
            .notes_for_test()
            .contains(&"cited_references_not_supplied".to_owned()));
        let one = lookup::parse_provider_response(
            LookupProvider::Crossref,
            "10.1000/synth(1)",
            &crossref_fixture("10.1000/synth(1)", Some(serde_json::json!([
                {"key": "ref1", "DOI": "10.5555/CITED", "year": "2001", "unstructured": "A. Author, Cited work"}
            ]))),
        )
        .unwrap();
        let refs: Vec<CitedReference> = serde_json::from_value(
            proposals(&one)
                .into_iter()
                .find(|p| p.0 == "cited_references")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(
            refs[0].identifiers.as_ref().unwrap()[0].value,
            "10.5555/cited"
        );
        assert_eq!(
            refs[0].published_date.as_ref().unwrap().value.as_deref(),
            Some("2001")
        );
        assert_eq!(refs[0].target_item_id, None);

        for (doi, expected) in [("10.1000/other", "lookup_identifier_mismatch")] {
            let error = lookup::parse_provider_response(
                LookupProvider::Crossref,
                "10.1000/synth(1)",
                &crossref_fixture(doi, None),
            )
            .err()
            .unwrap();
            assert_eq!(error.to_string(), expected);
        }
        let error = lookup::parse_provider_response(
            LookupProvider::Crossref,
            "10.1000/x",
            b"Resource not found.",
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "invalid_lookup_response");
    }

    pub(crate) fn open_library_fixture(publish_date: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": {"key": "/type/edition"},
            "title": "Synthetic Edition",
            "publishers": ["First Press", "Second Press"],
            "publish_places": ["Example City"],
            "publish_date": publish_date,
            "edition_name": "2nd ed.",
            "number_of_pages": 321,
            "isbn_10": ["0306406152"],
            "isbn_13": ["9780306406157"],
            "series": ["Synthetic Series"],
            "languages": [{"key": "/languages/eng"}],
            "authors": [{"key": "/authors/OL1A"}],
            "ocaid": "syntheticocaid",
            "key": "/books/OL1M"
        }))
        .unwrap()
    }

    #[test]
    fn open_library_adapter_keeps_ambiguous_dates_unparsed_and_creators_absent() {
        let parsed = lookup::parse_provider_response(
            LookupProvider::OpenLibrary,
            "9780306406157",
            &open_library_fixture("March 2004"),
        )
        .unwrap();
        let all = proposals(&parsed);
        let field = |name: &str| all.iter().find(|p| p.0 == name).map(|p| p.1.clone());
        assert_eq!(
            field("published_date").unwrap(),
            serde_json::json!({"value": null, "precision": "unparsed", "raw": "March 2004"})
        );
        assert_eq!(field("publisher").unwrap(), "First Press; Second Press");
        assert_eq!(field("edition").unwrap(), "2nd ed.");
        assert_eq!(field("number_of_pages").unwrap(), 321);
        assert_eq!(field("language").unwrap(), "eng");
        let identifiers: Vec<ItemIdentifier> =
            serde_json::from_value(field("identifiers").unwrap()).unwrap();
        assert_eq!(identifiers.len(), 1, "ISBN-10 and its 978 form are one key");
        assert!(field("creators").is_none());
        assert!(parsed
            .notes_for_test()
            .contains(&"creators_not_resolved_author_keys_only".to_owned()));
        let year = lookup::parse_provider_response(
            LookupProvider::OpenLibrary,
            "9780306406157",
            &open_library_fixture("2004"),
        )
        .unwrap();
        assert_eq!(
            proposals(&year)
                .into_iter()
                .find(|p| p.0 == "published_date")
                .unwrap()
                .1["precision"],
            "year"
        );
        let error = lookup::parse_provider_response(
            LookupProvider::OpenLibrary,
            "9791090636071",
            &open_library_fixture("2004"),
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "lookup_identifier_mismatch");
    }

    #[test]
    fn lookup_routes_are_exact_and_identifiers_are_path_data() {
        let url = lookup_request_url(LookupProvider::Crossref, "10.1000/a/b?c#d").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.crossref.org/works/10.1000%2Fa%2Fb%3Fc%23d"
        );
        assert_eq!(lookup_route(&url), Some(LookupProvider::Crossref));
        assert!(
            lookup_request_url(LookupProvider::Crossref, "10.1000/ABC").is_err(),
            "not normalized"
        );
        assert!(lookup_request_url(LookupProvider::Crossref, "11.1/x").is_err());
        assert!(
            lookup_request_url(LookupProvider::OpenLibrary, "0306406152").is_err(),
            "ISBN-13 key only"
        );
        let book = lookup_request_url(LookupProvider::OpenLibrary, "9780306406157").unwrap();
        assert_eq!(
            book.as_str(),
            "https://openlibrary.org/isbn/9780306406157.json"
        );
        for rejected in [
            "http://api.crossref.org/works/10.1000%2Fx",
            "https://api.crossref.org:8443/works/10.1000%2Fx",
            "https://api.crossref.org/works/10.1000%2Fx?mailto=a",
            "https://api.crossref.org/works/10.1000/x",
            "https://evil.example/works/10.1000%2Fx",
            "https://user@openlibrary.org/isbn/9780306406157.json",
            "https://openlibrary.org/isbn/9780306406158.json",
            "https://openlibrary.org/books/OL1M.pdf",
            "https://openlibrary.org/authors/OL1A.json",
        ] {
            assert_eq!(
                lookup_route(&Url::parse(rejected).unwrap()),
                None,
                "{rejected}"
            );
        }
        let edition = Url::parse("https://openlibrary.org/books/OL123M.json").unwrap();
        assert!(lookup_redirect_allowed(&book, &edition));
        assert!(!lookup_redirect_allowed(&edition, &book));
        assert!(!lookup_redirect_allowed(&url, &edition));
        assert!(!lookup_redirect_allowed(
            &book,
            &Url::parse("https://archive.org/books/OL123M.json").unwrap()
        ));
    }

    pub(crate) fn seed_with_doi() -> (tempfile::TempDir, String, String) {
        let (d, _, receipt) = seed();
        let item_id = receipt.item_ids[0].clone();
        let edited = revise_item_metadata_v2(
            d.path(),
            &item_id,
            v2_edit(
                &item_id,
                &receipt.revision_ids[0],
                "req-seed-doi",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "doi:10.1000/SYNTH(1)"}],
                    "publisher": "Synthetic Press"
                }),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        (d, item_id, edited.revision_id)
    }

    pub(crate) fn lookup_preview_for(
        root: &Path,
        item_id: &str,
        revision_id: &str,
    ) -> LookupPreview {
        let target = prepare_bibliographic_lookup(
            root,
            item_id,
            revision_id,
            &ItemIdentifier::new(IdentifierKind::Doi, "https://doi.org/10.1000/SYNTH(1)"),
            Sensitivity::Restricted,
        )
        .unwrap();
        assert_eq!(
            target.request_url,
            "https://api.crossref.org/works/10.1000%2Fsynth(1)"
        );
        create_lookup_preview(
            root,
            &target,
            &crossref_fixture("10.1000/SYNTH(1)", None),
            "2026-11-02T00:00:00Z",
            Sensitivity::Restricted,
        )
        .unwrap()
    }

    pub(crate) fn apply_edit(
        item_id: &str,
        revision_id: &str,
        request: &str,
        preview: &LookupPreview,
        selected: Vec<String>,
    ) -> ItemMetadataEditV2 {
        let mut edit = v2_edit(item_id, revision_id, request, serde_json::json!({}), &[]);
        edit.preview_id = Some(preview.preview_id.clone());
        edit.selected_candidate_ids = selected;
        edit
    }

    pub(crate) fn candidate(preview: &LookupPreview, field: &str, label: Option<&str>) -> String {
        preview
            .candidates
            .iter()
            .find(|c| c.field == field && label.is_none_or(|l| c.source_label == l))
            .unwrap()
            .candidate_id
            .clone()
    }

    #[test]
    fn lookup_preview_apply_uses_only_frozen_candidates_and_records_selection() {
        let (d, item_id, revision_id) = seed_with_doi();
        let preview = lookup_preview_for(d.path(), &item_id, &revision_id);
        assert_eq!(preview.expected_revision_id, revision_id);
        assert_eq!(preview.identifier.value, "10.1000/synth(1)");
        assert!(preview.candidates.iter().all(|c| !c.selected));
        let publisher = preview
            .candidates
            .iter()
            .find(|c| c.field == "publisher")
            .unwrap();
        assert!(publisher.unchanged && !publisher.conflict);
        assert!(preview
            .notes
            .contains(&"cited_references_not_supplied".to_owned()));
        let lookup_root = d.path().join("catalog/metadata-lookup");
        let response_hex = preview.response_sha256.strip_prefix("sha256:").unwrap();
        assert!(lookup_root
            .join(format!("responses/sha256-{response_hex}.json"))
            .is_file());

        let before = tree(d.path());
        let journal = candidate(&preview, "journal_title", None);
        let print = candidate(&preview, "published_date", Some("crossref:published-print"));
        let online = candidate(
            &preview,
            "published_date",
            Some("crossref:published-online"),
        );
        let creators = candidate(&preview, "creators", None);
        for (selected, expected) in [
            (
                vec!["candidate_forged".to_owned()],
                "invalid_candidate_selection",
            ),
            (
                vec![print.clone(), online.clone()],
                "invalid_candidate_selection",
            ),
            (
                vec![journal.clone(), journal.clone()],
                "invalid_candidate_selection",
            ),
            (
                vec![creators.clone()],
                "candidate_dependency_missing:creators:institutions",
            ),
            (vec![], "metadata_unchanged"),
            (
                vec![candidate(&preview, "publisher", None)],
                "metadata_unchanged",
            ),
        ] {
            let edit = apply_edit(
                &item_id,
                &revision_id,
                "req-apply-bad",
                &preview,
                selected.clone(),
            );
            assert_eq!(v2_err(d.path(), &item_id, edit), expected, "{selected:?}");
            assert_eq!(tree(d.path()), before, "{selected:?}");
        }
        // A forged preview id or a client-supplied value cannot stand in.
        let mut forged = preview.clone();
        forged.preview_id = "preview_forged".into();
        assert_eq!(
            v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    &revision_id,
                    "req-apply-bad",
                    &forged,
                    vec![journal.clone()]
                )
            ),
            "preview_unavailable"
        );
        let mut overlap = apply_edit(
            &item_id,
            &revision_id,
            "req-apply-bad",
            &preview,
            vec![journal.clone()],
        );
        overlap
            .set
            .insert("journal_title".into(), "Owner Journal".into());
        assert_eq!(
            v2_err(d.path(), &item_id, overlap),
            "invalid_metadata_field:journal_title:set_clear_overlap"
        );
        assert_eq!(tree(d.path()), before);

        let selected = vec![
            journal.clone(),
            print.clone(),
            creators.clone(),
            candidate(&preview, "institutions", None),
        ];
        let edit = apply_edit(
            &item_id,
            &revision_id,
            "req-apply",
            &preview,
            selected.clone(),
        );
        let receipt =
            revise_item_metadata_v2(d.path(), &item_id, edit, Sensitivity::Restricted).unwrap();
        assert_eq!(receipt.previous_revision_id, revision_id);
        let (_, revision) = read_item(d.path(), &item_id).unwrap();
        assert_eq!(revision.title, "t", "unselected title kept");
        assert_eq!(revision.content_markdown, "b");
        let metadata = &revision.metadata;
        let journal_value = metadata.journal_title.as_ref().unwrap();
        assert_eq!(journal_value.value, "Journal of Synthetic Tests");
        assert_eq!(
            journal_value.provenance.origin,
            MetadataOrigin::LookupSelected
        );
        assert_eq!(journal_value.provenance.provider_id, "crossref");
        assert_eq!(
            journal_value.provenance.provider_version,
            LOOKUP_ADAPTER_VERSION
        );
        assert_eq!(
            journal_value.provenance.source_refs,
            [
                format!("lookup_response:{}", preview.response_sha256),
                format!("lookup_preview:{}", preview.preview_id),
                format!("lookup_candidate:{journal}"),
            ]
        );
        assert_eq!(
            metadata
                .published_partial_date
                .as_ref()
                .unwrap()
                .value
                .value
                .as_deref(),
            Some("2024-03-01")
        );
        assert_eq!(
            metadata.published_date.as_ref().unwrap().value,
            "2024-03-01"
        );
        assert_eq!(
            metadata.creators.as_ref().unwrap().value[0].institution_ids,
            ["institution-1"]
        );
        assert_eq!(
            metadata.publisher.as_ref().unwrap().provenance.origin,
            MetadataOrigin::OwnerEdit
        );
        assert_eq!(metadata.cited_references, None);
        let activity = revision.provenance.last().unwrap();
        assert_eq!(activity.activity_type, "bibliographic_lookup_apply");
        assert_eq!(
            activity.input_ids,
            [revision_id.clone(), preview.preview_id.clone()]
        );
        assert_eq!(activity.input_checksums[1], preview.response_sha256);
        assert!(lookup_root
            .join(format!("sources/sha256-{response_hex}.json"))
            .is_file());

        // Replay returns the original receipt; the old preview is now stale.
        let after = tree(d.path());
        let again = revise_item_metadata_v2(
            d.path(),
            &item_id,
            apply_edit(&item_id, &revision_id, "req-apply", &preview, selected),
            Sensitivity::Restricted,
        )
        .unwrap();
        assert_eq!(again, receipt);
        assert_eq!(
            v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    &receipt.revision_id,
                    "req-apply-stale",
                    &preview,
                    vec![online]
                )
            ),
            "stale_item_revision"
        );
        assert_eq!(tree(d.path()), after);

        // A lookup-provenance change that does not cite its activity breaks the chain.
        let path = d
            .path()
            .join("items")
            .join(&item_id)
            .join("revisions")
            .join(format!("{}.json", receipt.revision_id));
        let mut tampered: ItemRevision = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        tampered
            .metadata
            .journal_title
            .as_mut()
            .unwrap()
            .provenance
            .source_refs[1] = "lookup_preview:preview_other".into();
        let base: ItemRevision = serde_json::from_slice(
            &fs::read(
                d.path()
                    .join("items")
                    .join(&item_id)
                    .join("revisions")
                    .join(format!("{revision_id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(validate_owner_edit_transition(&base, &tampered).is_err());
        let untouched: ItemRevision = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        validate_owner_edit_transition(&base, &untouched).unwrap();
    }

    #[test]
    fn lookup_preview_tamper_stale_and_gc_fail_closed() {
        let (d, item_id, revision_id) = seed_with_doi();
        let preview = lookup_preview_for(d.path(), &item_id, &revision_id);
        let preview_file = d
            .path()
            .join("catalog/metadata-lookup/previews")
            .join(format!("{}.json", preview.preview_id));
        let original = fs::read(&preview_file).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let index = record["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c["field"] == "journal_title")
            .unwrap();
        record["candidates"][index]["proposed_value"] = "Forged Journal".into();
        fs::write(&preview_file, serde_json::to_vec(&record).unwrap()).unwrap();
        let journal = candidate(&preview, "journal_title", None);
        let before = tree(d.path());
        assert_eq!(
            v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    &revision_id,
                    "req-tamper",
                    &preview,
                    vec![journal.clone()]
                )
            ),
            "invalid_lookup_preview"
        );
        assert_eq!(tree(d.path()), before);
        fs::write(&preview_file, &original).unwrap();

        // Owner edit moves the item on: preview is stale and nothing is written.
        let moved = revise_item_metadata_v2(
            d.path(),
            &item_id,
            v2_edit(
                &item_id,
                &revision_id,
                "req-move",
                serde_json::json!({"volume": "9"}),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        let before = tree(d.path());
        for expected_revision in [revision_id.as_str(), moved.revision_id.as_str()] {
            let error = v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    expected_revision,
                    "req-stale",
                    &preview,
                    vec![journal.clone()],
                ),
            );
            assert_eq!(error, "stale_item_revision", "{expected_revision}");
            assert_eq!(tree(d.path()), before);
        }
        assert_eq!(
            create_lookup_preview(
                d.path(),
                &prepare_bibliographic_lookup(
                    d.path(),
                    &item_id,
                    &moved.revision_id,
                    &ItemIdentifier::new(IdentifierKind::Doi, "10.1000/synth(1)"),
                    Sensitivity::Restricted
                )
                .map(|mut t| {
                    t.expected_revision_id = revision_id.clone();
                    t
                })
                .unwrap(),
                &crossref_fixture("10.1000/synth(1)", None),
                "2026-11-02T00:00:00Z",
                Sensitivity::Restricted,
            )
            .unwrap_err()
            .to_string(),
            "stale_item_revision"
        );

        // Expired previews and their unreferenced responses are collected.
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["expires_at"] = "2000-01-01T00:00:00+00:00".into();
        fs::write(&preview_file, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(
            v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    &moved.revision_id,
                    "req-expired",
                    &preview,
                    vec![journal]
                )
            ),
            "stale_item_revision"
        );
        let responses = d.path().join("catalog/metadata-lookup/responses");
        fs::write(responses.join("sha256-orphan.json"), b"{}").unwrap();
        let fresh = lookup_preview_for(d.path(), &item_id, &moved.revision_id);
        assert!(!preview_file.exists());
        assert!(!responses.join("sha256-orphan.json").exists());
        let names: Vec<_> = fs::read_dir(&responses)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
        assert_eq!(
            fresh
                .candidates
                .iter()
                .find(|c| c.field == "volume")
                .unwrap()
                .current_value,
            Some("9".into())
        );
    }

    #[test]
    fn lookup_prepare_rejects_invalid_unowned_and_hidden_identifiers_before_fetch() {
        let (d, item_id, revision_id) = seed_with_doi();
        let prepare = |identifier: ItemIdentifier, max| {
            prepare_bibliographic_lookup(d.path(), &item_id, &revision_id, &identifier, max)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            prepare(
                ItemIdentifier::new(IdentifierKind::Doi, "10.1000"),
                Sensitivity::Restricted
            ),
            "lookup_identifier_invalid"
        );
        assert_eq!(
            prepare(
                ItemIdentifier::new(IdentifierKind::Isbn, "0-306-40615-3"),
                Sensitivity::Restricted
            ),
            "lookup_identifier_invalid"
        );
        assert_eq!(
            prepare(
                ItemIdentifier::new(IdentifierKind::Issn, "0317-8471"),
                Sensitivity::Restricted
            ),
            "lookup_identifier_unsupported"
        );
        assert_eq!(
            prepare(
                ItemIdentifier::new(IdentifierKind::Doi, "10.1000/other"),
                Sensitivity::Restricted
            ),
            "lookup_identifier_not_on_item"
        );
        assert_eq!(
            prepare_bibliographic_lookup(
                d.path(),
                &item_id,
                "revision_stale",
                &ItemIdentifier::new(IdentifierKind::Doi, "10.1000/synth(1)"),
                Sensitivity::Restricted
            )
            .unwrap_err()
            .to_string(),
            "stale_item_revision"
        );
        let mut c = capture();
        c.capture.external_capture_id = "hidden-lookup".into();
        c.capture.sensitivity = Sensitivity::Private;
        let mut value = draft();
        value.sensitivity = Sensitivity::Private;
        let hidden = ingest(d.path(), &c, value).unwrap();
        for revision in [hidden.revision_ids[0].as_str(), "revision_stale"] {
            let error = prepare_bibliographic_lookup(
                d.path(),
                &hidden.item_ids[0],
                revision,
                &ItemIdentifier::new(IdentifierKind::Doi, "10.1000/synth(1)"),
                Sensitivity::Public,
            )
            .unwrap_err()
            .to_string();
            assert_eq!(error, "library_resource_not_found");
        }
    }

    pub(crate) fn large_crossref(abstract_bytes: usize, references: usize) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&crossref_fixture("10.1000/SYNTH(1)", None)).unwrap();
        let words = "lorem ipsum 合成 ".repeat(abstract_bytes / 18 + 1);
        value["message"]["abstract"] = format!(
            "<jats:p>{}</jats:p>",
            &words[..words
                .char_indices()
                .take_while(|(i, _)| *i < abstract_bytes)
                .last()
                .unwrap()
                .0]
        )
        .into();
        value["message"]["reference"] = (0..references)
            .map(|n| serde_json::json!({"key": format!("ref{n}"), "DOI": format!("10.5555/ref.{n}"),
                "unstructured": format!("Synthetic Author {n}. Synthetic cited work number {n}. Journal of Examples."), "year": "2001"}))
            .collect();
        serde_json::to_vec(&value).unwrap()
    }

    pub(crate) fn preview_from(
        root: &Path,
        item_id: &str,
        revision_id: &str,
        bytes: &[u8],
    ) -> LookupPreview {
        let target = prepare_bibliographic_lookup(
            root,
            item_id,
            revision_id,
            &ItemIdentifier::new(IdentifierKind::Doi, "10.1000/synth(1)"),
            Sensitivity::Restricted,
        )
        .unwrap();
        create_lookup_preview(
            root,
            &target,
            bytes,
            "2026-11-02T00:00:00Z",
            Sensitivity::Restricted,
        )
        .unwrap()
    }

    #[test]
    fn large_lookup_preview_pages_candidates_and_values_within_frame_bounds() {
        let (d, item_id, revision_id) = seed_with_doi();
        let bytes = large_crossref(100 * 1024, 2000);
        assert!(bytes.len() > 300 * 1024 && bytes.len() < MAX_LOOKUP_RESPONSE_BYTES);
        let preview = preview_from(d.path(), &item_id, &revision_id, &bytes);
        // 100 KiB exceeds the Store abstract bound: reported, never truncated.
        assert!(preview
            .notes
            .contains(&"abstract_text_exceeds_bound".to_owned()));
        assert!(preview
            .candidates
            .iter()
            .all(|c| c.field != "abstract_text"));
        let first = page_lookup_preview(&preview, false, 0, MAX_LOOKUP_CANDIDATE_PAGE).unwrap();
        assert!(serde_json::to_vec(&first).unwrap().len() < 160 * 1024);
        assert_eq!(first.total_candidates, preview.candidates.len());
        let refs = first
            .candidates
            .iter()
            .find(|c| c.field == "cited_references")
            .unwrap();
        assert_eq!(refs.proposed_value, None);
        let summary = refs.proposed_value_summary.as_ref().unwrap();
        assert_eq!(
            (summary.kind, summary.total),
            (LookupValueKind::Array, 2000)
        );
        assert!(summary.json_bytes > 200 * 1024);
        let inline = first
            .candidates
            .iter()
            .find(|c| c.field == "journal_title")
            .unwrap();
        assert_eq!(
            inline.proposed_value,
            Some("Journal of Synthetic Tests".into())
        );

        // Small windows still reproduce every frozen candidate exactly once.
        let mut ids = Vec::new();
        let mut offset = Some(0);
        while let Some(at) = offset {
            let page = read_lookup_preview(
                d.path(),
                &preview.preview_id,
                Sensitivity::Restricted,
                at,
                3,
            )
            .unwrap();
            assert!(!page.stale);
            ids.extend(page.candidates.into_iter().map(|c| c.candidate_id));
            offset = page.next_offset;
        }
        assert_eq!(
            ids,
            preview
                .candidates
                .iter()
                .map(|c| c.candidate_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            read_lookup_preview(d.path(), &preview.preview_id, Sensitivity::Restricted, 0, 0)
                .unwrap_err()
                .to_string(),
            "invalid_lookup_window"
        );

        // Value windows reassemble the full frozen array.
        let mut joined = Vec::new();
        let mut offset = Some(0);
        let mut pages = 0;
        while let Some(at) = offset {
            let page = read_lookup_value(
                d.path(),
                &preview.preview_id,
                &refs.candidate_id,
                LookupValueSide::Proposed,
                at,
                Sensitivity::Restricted,
            )
            .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= LOOKUP_VALUE_PAGE_BYTES + 1024);
            joined.extend(page.value.unwrap().as_array().unwrap().clone());
            offset = page.next_offset;
            pages += 1;
        }
        assert!(pages > 3);
        let full = &preview
            .candidates
            .iter()
            .find(|c| c.field == "cited_references")
            .unwrap()
            .proposed_value;
        assert_eq!(&serde_json::Value::Array(joined), full);

        // Apply is unchanged: the frozen 2,000-reference value commits whole.
        let mut edit = v2_edit(
            &item_id,
            &revision_id,
            "req-large",
            serde_json::json!({}),
            &[],
        );
        edit.preview_id = Some(preview.preview_id.clone());
        edit.selected_candidate_ids = vec![refs.candidate_id.clone()];
        revise_item_metadata_v2(d.path(), &item_id, edit, Sensitivity::Restricted).unwrap();
        let (_, revision) = read_item(d.path(), &item_id).unwrap();
        assert_eq!(
            revision.metadata.cited_references.unwrap().value.len(),
            2000
        );
        assert!(
            read_lookup_preview(d.path(), &preview.preview_id, Sensitivity::Restricted, 0, 1)
                .unwrap()
                .stale
        );

        // A long abstract within the bound is summarized and readable in full.
        let (d, item_id, revision_id) = seed_with_doi();
        let preview = preview_from(
            d.path(),
            &item_id,
            &revision_id,
            &large_crossref(60 * 1024, 0),
        );
        let page = page_lookup_preview(&preview, false, 0, 50).unwrap();
        let abstract_view = page
            .candidates
            .iter()
            .find(|c| c.field == "abstract_text")
            .unwrap();
        let summary = abstract_view.proposed_value_summary.as_ref().unwrap();
        assert_eq!(summary.kind, LookupValueKind::String);
        let full = preview
            .candidates
            .iter()
            .find(|c| c.field == "abstract_text")
            .unwrap()
            .proposed_value
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(summary.total, full.len() as u64);
        let mut text = String::new();
        let mut offset = Some(0);
        while let Some(at) = offset {
            let page = read_lookup_value(
                d.path(),
                &preview.preview_id,
                &abstract_view.candidate_id,
                LookupValueSide::Proposed,
                at,
                Sensitivity::Restricted,
            )
            .unwrap();
            text.push_str(page.value.unwrap().as_str().unwrap());
            offset = page.next_offset;
        }
        assert_eq!(text, full);
        let misaligned = full
            .char_indices()
            .find(|(_, c)| c.len_utf8() > 1)
            .unwrap()
            .0
            + 1;
        assert_eq!(
            read_lookup_value(
                d.path(),
                &preview.preview_id,
                &abstract_view.candidate_id,
                LookupValueSide::Proposed,
                misaligned,
                Sensitivity::Restricted
            )
            .unwrap_err()
            .to_string(),
            "invalid_lookup_window"
        );
        assert_eq!(
            read_lookup_preview(d.path(), "preview_unknown", Sensitivity::Restricted, 0, 1)
                .unwrap_err()
                .to_string(),
            "preview_unavailable"
        );
    }

    #[test]
    fn apply_after_expiry_returns_preview_expired_with_zero_writes() {
        let (d, item_id, revision_id) = seed_with_doi();
        let preview = lookup_preview_for(d.path(), &item_id, &revision_id);
        let file = d
            .path()
            .join("catalog/metadata-lookup/previews")
            .join(format!("{}.json", preview.preview_id));
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        record["expires_at"] = "2000-01-01T00:00:00+00:00".into();
        fs::write(&file, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let before = tree(d.path());
        let journal = candidate(&preview, "journal_title", None);
        assert_eq!(
            v2_err(
                d.path(),
                &item_id,
                apply_edit(
                    &item_id,
                    &revision_id,
                    "req-expired-apply",
                    &preview,
                    vec![journal.clone()]
                )
            ),
            "preview_expired"
        );
        assert_eq!(
            read_lookup_preview(d.path(), &preview.preview_id, Sensitivity::Restricted, 0, 1)
                .unwrap_err()
                .to_string(),
            "preview_expired"
        );
        assert_eq!(tree(d.path()), before);
        assert_eq!(
            read_item(d.path(), &item_id).unwrap().1.revision_id,
            revision_id
        );
        // The rejected request id left no replay record.
        assert!(!metadata_request_path(d.path(), "req-expired-apply").exists());
    }
}
