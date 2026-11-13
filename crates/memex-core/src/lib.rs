use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod bibliographic;
pub use bibliographic::{
    normalize_doi, normalize_identifier, normalize_isbn, normalize_issn, CitedReference,
    DatePrecision, IdentifierValidation, Institution, InstitutionIdentifier, NormalizedIdentifier,
    PartialDate, ValidationStatus, IDENTIFIER_NORMALIZER_VERSION,
};

pub use lots_scout_types::{
    captured_attachment_relative_path, BlobDescriptor, CaptureEnvelope, DraftAttachmentRef,
    Sensitivity, SourceProfile, ExtractedDraft, VerifiedBlob, VerifiedCapture,
};

pub const SCHEMA_VERSION: u32 = 1;

pub fn new_id(namespace: &str) -> String {
    format!("{namespace}_{}", Uuid::now_v7().simple())
}

/// How an item first entered MemeX, not the website it came from.
/// Unknown legacy adapters have no classification; never infer it from a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    Capture,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataOrigin {
    IntakeProvided,
    JsonLd,
    CitationMeta,
    OpenGraph,
    Readability,
    SourceUrlFallback,
    OwnerEdit,
    /// Owner-selected value from a frozen lookup preview; `provider_id` is the
    /// metadata provider and `source_refs` name the retained response.
    LookupSelected,
    /// Filled into a previously empty field by an owner-authorized model request;
    /// `provider_id`/`provider_version` name the routed model and settings revision.
    ModelSuggested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataProvenance {
    pub origin: MetadataOrigin,
    pub provider_id: String,
    pub provider_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
}

impl MetadataProvenance {
    pub fn new(
        origin: MetadataOrigin,
        provider_id: impl Into<String>,
        provider_version: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            provider_id: provider_id.into(),
            provider_version: provider_version.into(),
            source_refs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub provenance: MetadataProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatorRole {
    Author,
    Editor,
    Translator,
    Contributor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Creator {
    pub display_name: String,
    pub role: CreatorRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_type: Option<CreatorNameType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub institution_ids: Vec<String>,
}

impl Creator {
    /// Legacy display-only creator; no name structure is inferred.
    pub fn new(display_name: impl Into<String>, role: CreatorRole) -> Self {
        Self {
            display_name: display_name.into(),
            role,
            creator_id: None,
            name_type: None,
            family_name: None,
            given_name: None,
            literal_name: None,
            institution_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatorNameType {
    Person,
    Organization,
    Unparsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierKind {
    Doi,
    Isbn,
    Pmid,
    Issn,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemIdentifier {
    pub kind: IdentifierKind,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
}

impl ItemIdentifier {
    pub fn new(kind: IdentifierKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
            raw_value: None,
            qualifier: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightsStatus {
    Copyrighted,
    Licensed,
    PublicDomain,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRights {
    pub status: RightsStatus,
    pub statement: Option<String>,
    pub license_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creators: Option<Sourced<Vec<Creator>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_date: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifiers: Option<Sourced<Vec<ItemIdentifier>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_tags: Option<Sourced<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_rights: Option<Sourced<ContentRights>>,
    /// Typed publication date (bibliographic v2). The legacy string
    /// `published_date` stays for old readers as its projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_partial_date: Option<Sourced<PartialDate>>,
    /// RSS episode duration; absent on older records (and omitted from digests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<Sourced<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_title: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_place: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_pages: Option<Sourced<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_title: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_number: Option<Sourced<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Sourced<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institutions: Option<Sourced<Vec<Institution>>>,
    /// Absent: not supplied. Present but empty: source supplied no references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_references: Option<Sourced<Vec<CitedReference>>>,
}

impl ItemMetadata {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentRef {
    Remote {
        url: String,
        media_type: Option<String>,
        title: Option<String>,
    },
    Evidence {
        evidence_id: String,
        relative_path: String,
        title: Option<String>,
    },
}

pub fn image_signature_matches(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => {
            bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|value| value == b"WEBP")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub schema_version: u32,
    pub capture_id: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub external_capture_id: String,
    pub source_id: String,
    pub captured_at: String,
    pub capture_method: String,
    pub route_summary: Option<String>,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub sensitivity: Sensitivity,
    pub blob_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_input_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_descriptor_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub schema_version: u32,
    pub evidence_id: String,
    pub source_id: String,
    pub capture_id: String,
    pub blob_id: String,
    pub logical_locator: String,
    pub backend_ref: String,
    pub sensitivity: Sensitivity,
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<BlobDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub schema_version: u32,
    pub evidence_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceActivity {
    pub schema_version: u32,
    pub activity_id: String,
    pub activity_type: String,
    pub occurred_at: String,
    pub tool_id: String,
    pub tool_version: String,
    pub config_digest: String,
    #[serde(default)]
    pub input_ids: Vec<String>,
    #[serde(default)]
    pub input_checksums: Vec<String>,
    #[serde(default)]
    pub output_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRevision {
    pub schema_version: u32,
    pub revision_id: String,
    pub item_id: String,
    pub revision_number: u32,
    pub title: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub content_markdown: String,
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Vec<ProvenanceActivity>,
    #[serde(default, skip_serializing_if = "ItemMetadata::is_empty")]
    pub metadata: ItemMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub schema_version: u32,
    pub item_id: String,
    pub item_type: String,
    pub current_revision: String,
    pub sensitivity: Sensitivity,
    /// First durable inclusion in this MemeX library (RFC3339 UTC), set by
    /// the Store in the first successful item-creation transaction. Absent on
    /// items created before this field existed; never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
}

/// Exact Store-written form: RFC3339 UTC with a `Z` suffix.
pub fn valid_added_at(value: &str) -> bool {
    value.ends_with('Z')
        && value.len() <= 40
        && value.contains('T')
        && chrono_free_rfc3339_utc(value)
}

fn chrono_free_rfc3339_utc(value: &str) -> bool {
    // YYYY-MM-DDTHH:MM:SS[.fraction]Z
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    bytes.len() >= 20
        && digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19)
        && match bytes[19] {
            b'Z' => bytes.len() == 20,
            b'.' => bytes.len() > 21 && bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit),
            _ => false,
        }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestReceipt {
    pub schema_version: u32,
    pub capture_id: String,
    pub evidence_ids: Vec<String>,
    pub item_ids: Vec<String>,
    pub revision_ids: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemDraft {
    pub item_type: String,
    pub title: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub content_markdown: String,
    pub sensitivity: Sensitivity,
    pub translator_id: String,
    pub translator_version: String,
    pub metadata: ItemMetadata,
    pub attachments: Vec<DraftAttachmentRef>,
}

impl From<ExtractedDraft> for ItemDraft {
    /// An extractor never supplies bibliographic metadata; callers add it.
    fn from(draft: ExtractedDraft) -> Self {
        Self {
            item_type: draft.item_type,
            title: draft.title,
            source_url: draft.source_url,
            canonical_url: draft.canonical_url,
            content_markdown: draft.content_markdown,
            sensitivity: draft.sensitivity,
            translator_id: draft.extractor_id,
            translator_version: draft.extractor_version,
            metadata: ItemMetadata::default(),
            attachments: draft.attachments,
        }
    }
}
