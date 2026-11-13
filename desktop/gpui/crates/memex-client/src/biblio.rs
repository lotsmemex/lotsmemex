//! Bibliographic metadata v2 (`memex.bibliographic_metadata_v1.md` draft 2) as the
//! agent carries it over IPC (`memex-agent/src/ipc.rs`, `bibliographic.rs`,
//! `memex-store` `ItemMetadataEditV2`/`ItemMetadataV2Read`, `lookup.rs`).
//!
//! Nothing here is sent unless the agent's `capabilities` answer lists
//! `bibliographic_metadata_v2` with `metadata_schema_version: 2`. Responses ignore
//! unknown fields; edit requests carry exactly the fields the agent accepts (it
//! rejects unknown nested keys and a client-supplied `raw_value`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const METADATA_SCHEMA_VERSION: u32 = 2;
pub const BIBLIOGRAPHIC_FEATURE: &str = "bibliographic_metadata_v2";

// ---------------------------------------------------------------- capabilities

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentCapabilities {
    pub schema_version: u32,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub bibliographic_metadata_v2: Option<BibliographicCapability>,
    #[serde(default)]
    pub groups_v1: Option<GroupsCapability>,
    #[serde(default)]
    pub translation_provider_v1: Option<TranslationProviderCapability>,
    #[serde(default)]
    pub ai_insights_v1: Option<Value>,
}

/// `groups_v1`: tree read (paged) and, when `writes`, `group_edit`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupsCapability {
    #[serde(default)]
    pub tree_page_limit: usize,
    #[serde(default)]
    pub writes: bool,
    #[serde(default)]
    pub write_ops: Vec<String>,
    #[serde(default)]
    pub trash: bool,
    /// `groups_v1.query_unfiled`: `library_query` accepts `unfiled: true` (items in
    /// no visible library or folder; the Inbox).
    #[serde(default)]
    pub query_unfiled: bool,
    /// `groups_v1.node_delete`: the four `group_delete_*` commands (never a `group_edit` op).
    #[serde(default)]
    pub node_delete: bool,
}

/// `translation_provider_v1`: whether a translation model is configured. Only
/// `available` decides whether a request is sent; the request also asks for AI
/// minutes and metadata enrichment when the agent says it can (`minutes`,
/// `metadata`). Routes are read only for the chat purpose.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationProviderCapability {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub purpose_routes: Option<Value>,
    #[serde(default)]
    pub minutes: Option<bool>,
    #[serde(default)]
    pub metadata: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BibliographicCapability {
    pub metadata_schema_version: u32,
    #[serde(default)]
    pub normalizer_version: String,
    #[serde(default)]
    pub lookup_adapter_version: String,
    #[serde(default)]
    pub lookup_preview_ttl_seconds: i64,
    #[serde(default)]
    pub lookup_sources: Vec<LookupSource>,
    #[serde(default)]
    pub lookup_batch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LookupSource {
    /// `crossref | open_library`.
    pub provider: String,
    /// `doi | isbn`.
    pub identifier_kind: String,
    #[serde(default)]
    pub route: String,
}

/// What this desktop may use on the connected backend. Default: nothing beyond
/// the 916/P1 commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `bibliographic_metadata_read` / `_edit` / `_duplicate_check` (schema 2).
    pub bibliographic: bool,
    /// Single-item lookup sources the agent advertises.
    pub lookup_sources: Vec<LookupSource>,
    /// `group_tree_read` is available (`groups_v1` advertised).
    pub groups: bool,
    /// `group_edit` is available (`groups_v1.writes`).
    pub group_writes: bool,
    pub group_reorder: bool,
    /// `groups_v1.write_ops` lists `move_members` (atomic same-root folder move).
    /// Never inferred from the generic `moves` flag.
    pub group_move_members: bool,
    /// Atomic subtree move / snapshot mapping, explicitly advertised.
    pub group_transfer_folders: bool,
    pub group_trash: bool,
    /// `library_query` accepts `unfiled: true` (`groups_v1.query_unfiled`).
    pub group_query_unfiled: bool,
    /// `group_delete_preview / group_delete / _list / _restore` (`groups_v1.node_delete`).
    pub group_node_delete: bool,
    /// Largest tree page the agent accepts.
    pub group_page_limit: usize,
    /// `None`: the agent does not advertise `translation_provider_v1`, so no
    /// `translation_request` is ever sent. `Some(false)`: no model configured.
    pub translation: Option<bool>,
    /// `translation_provider_v1.minutes`: the request also produces AI minutes.
    pub minutes: Option<bool>,
    /// `translation_provider_v1.metadata`: the request also enriches metadata.
    pub metadata_enrichment: Option<bool>,
    /// `item_chat` is available (`item_chat_v1` in `features`).
    pub item_chat: bool,
    /// `item_chat_history_v1` (with `item_chat_v1`): stored turns can be read back.
    pub item_chat_history: bool,
    /// Whether a chat route is configured: `Some(false)` when the agent names the
    /// chat purpose without a route, `None` when it does not say.
    pub chat_route: Option<bool>,
    /// `ai_insights_v1`: which kinds can be requested; `None` hides them.
    pub insights: Option<crate::insights::InsightKinds>,
    /// `dedup_trash_v1`: the recoverable trash can be listed and restored.
    pub trash: bool,
    /// `minutes_request_v1`: AI 纪要 are asked for on their own (manual).
    pub minutes_request: bool,
    /// `item_cover_v1`: item covers can be read (batch and chunks).
    pub item_cover: bool,
    /// The agent's `schema_version` and feature list as sent (the 「版本」 tab);
    /// `None` / empty for an agent without `capabilities`.
    pub schema_version: Option<u32>,
    pub features: Vec<String>,
}

impl Capabilities {
    pub fn from_agent(agent: &AgentCapabilities) -> Self {
        let v2 = agent
            .bibliographic_metadata_v2
            .as_ref()
            .filter(|v2| agent.features.iter().any(|f| f == BIBLIOGRAPHIC_FEATURE) && v2.metadata_schema_version == METADATA_SCHEMA_VERSION);
        let mut caps = match v2 {
            Some(v2) => Self { bibliographic: true, lookup_sources: v2.lookup_sources.clone(), ..Self::default() },
            None => Self::default(),
        };
        if let Some(g) = &agent.groups_v1 {
            caps.groups = true;
            caps.group_writes = g.writes;
            caps.group_reorder = g.writes && g.write_ops.iter().any(|op| op == "reorder_root");
            caps.group_move_members = g.writes && g.write_ops.iter().any(|op| op == "move_members");
            caps.group_transfer_folders = g.writes && g.write_ops.iter().any(|op| op == "transfer_folders");
            caps.group_trash = g.trash;
            caps.group_query_unfiled = g.query_unfiled;
            caps.group_node_delete = g.node_delete;
            caps.group_page_limit = if g.tree_page_limit == 0 { 200 } else { g.tree_page_limit };
        }
        let provider = agent.translation_provider_v1.as_ref();
        caps.translation = provider.map(|t| t.available);
        caps.minutes = provider.and_then(|t| t.minutes);
        caps.metadata_enrichment = provider.and_then(|t| t.metadata);
        caps.schema_version = Some(agent.schema_version);
        caps.features = agent.features.clone();
        caps.item_chat = agent.features.iter().any(|f| f == crate::chat::ITEM_CHAT_FEATURE);
        caps.item_chat_history = caps.item_chat && agent.features.iter().any(|f| f == crate::chat::ITEM_CHAT_HISTORY_FEATURE);
        caps.trash = agent.features.iter().any(|f| f == crate::trash::TRASH_FEATURE);
        caps.minutes_request = agent.features.iter().any(|f| f == crate::dto::MINUTES_REQUEST_FEATURE);
        caps.item_cover = agent.features.iter().any(|f| f == crate::covers::COVER_FEATURE);
        caps.insights = agent
            .features
            .iter()
            .any(|f| f == crate::insights::INSIGHTS_FEATURE)
            .then(|| crate::insights::InsightKinds::from_agent(agent.ai_insights_v1.as_ref()));
        caps.chat_route = provider
            .and_then(|t| t.purpose_routes.as_ref())
            .and_then(|r| r.get("chat"))
            .map(|route| route.as_str().is_some_and(|s| !s.is_empty()) || route.is_object());
        caps
    }

    /// The bottom composer may send: `item_chat_v1` advertised and no sign that
    /// the chat purpose lacks a route.
    pub fn chat_ready(&self) -> bool {
        self.item_chat && self.chat_route != Some(false)
    }

    /// The advertised lookup source for an identifier kind (`doi` / `isbn`).
    pub fn lookup_source(&self, identifier_kind: &str) -> Option<&LookupSource> {
        self.lookup_sources.iter().find(|s| s.identifier_kind == identifier_kind)
    }
}

/// Display name of a lookup provider id.
pub fn provider_label(provider: &str) -> &str {
    match provider {
        "crossref" => "Crossref",
        "open_library" => "Open Library",
        other => other,
    }
}

/// Display name of an identifier kind.
pub fn identifier_label(kind: &str) -> String {
    match kind {
        "doi" | "isbn" | "issn" | "pmid" => kind.to_ascii_uppercase(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- value types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
    Year,
    Month,
    Day,
    Unparsed,
}

/// `{value, precision, raw}`: `value` is exactly YYYY / YYYY-MM / YYYY-MM-DD, or
/// null when unparsed; `raw` keeps the source wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialDate {
    pub value: Option<String>,
    pub precision: DatePrecision,
    #[serde(default)]
    pub raw: Option<String>,
}

impl PartialDate {
    /// Shown at its own precision: a year is never expanded to a day, and an
    /// unparsed date shows its raw wording.
    pub fn display(&self) -> String {
        let value = self.value.as_deref().unwrap_or_default();
        let cut = |n: usize| value.get(..n).unwrap_or(value).to_string();
        match self.precision {
            DatePrecision::Year => cut(4),
            DatePrecision::Month => cut(7),
            DatePrecision::Day => cut(10),
            DatePrecision::Unparsed => self.raw.clone().or_else(|| self.value.clone()).unwrap_or_default(),
        }
    }

    /// Four-digit year for the table, `None` when unparsed.
    pub fn year(&self) -> Option<String> {
        match self.precision {
            DatePrecision::Unparsed => None,
            _ => self.value.as_deref().and_then(|v| v.get(..4)).map(str::to_string),
        }
    }
}

/// Inner creator value. Row identity (`creator_id`) and `institution_ids` are
/// kept as read so an edit never drops links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creator {
    pub display_name: String,
    /// `author | editor | translator | contributor`.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_id: Option<String>,
    /// `person | organization | unparsed`; absent on legacy display-only creators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_type: Option<String>,
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
    pub fn is_person(&self) -> bool {
        self.name_type.as_deref() == Some("person")
    }

    /// "Family, Given" for people whose supplied parts are Latin-script; the
    /// supplied literal or display name otherwise (CJK names keep their own order).
    /// Names are never split heuristically.
    pub fn name(&self) -> String {
        match (&self.family_name, &self.given_name, &self.literal_name) {
            (Some(f), Some(g), _) if self.is_person() && f.is_ascii() && g.is_ascii() => format!("{f}, {g}"),
            (Some(f), Some(g), _) if self.is_person() => format!("{f}{g}"),
            (_, _, Some(l)) => l.clone(),
            _ => self.display_name.clone(),
        }
    }

    /// Short form for the table: a Latin family name, else the full name.
    pub fn short_name(&self) -> String {
        match &self.family_name {
            Some(f) if self.is_person() && f.is_ascii() => f.clone(),
            _ => self.name(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IdentifierValidation {
    /// `valid | invalid | unparsed`.
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub normalizer_version: String,
}

/// One identifier. Serializes as the agent's edit input (`kind`, `value`,
/// `qualifier`); `raw_value` and the server-computed views are read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identifier {
    /// `doi | isbn | issn | pmid`.
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing)]
    pub raw_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    #[serde(default, skip_serializing)]
    pub normalized_value: Option<String>,
    #[serde(default, skip_serializing)]
    pub comparison_key: Option<String>,
    #[serde(default, skip_serializing)]
    pub validation: Option<IdentifierValidation>,
}

impl Identifier {
    pub fn input(kind: &str, value: &str, qualifier: Option<String>) -> Self {
        Self { kind: kind.into(), value: value.into(), raw_value: None, qualifier, normalized_value: None, comparison_key: None, validation: None }
    }

    /// Locally valid structure/checksum per the agent's normalizer (not proof of
    /// registration or existence).
    pub fn is_valid(&self) -> bool {
        self.validation.as_ref().is_some_and(|v| v.status == "valid")
    }

    /// Value to show: the normalized form when valid, else as stored.
    pub fn display(&self) -> String {
        self.normalized_value.clone().unwrap_or_else(|| self.value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Institution {
    pub institution_id: String,
    pub name: String,
    #[serde(default)]
    pub identifiers: Vec<Value>,
    #[serde(default)]
    pub country: Option<String>,
}

/// Bibliographic fields of one item revision (inner values; provenance kept by
/// the agent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bibliographic {
    pub item_id: String,
    pub revision_id: String,
    /// `journal_article | book | webpage`, other values retained verbatim.
    pub item_type: String,
    pub creators: Vec<Creator>,
    pub identifiers: Vec<Identifier>,
    pub published_date: Option<PartialDate>,
    /// 时长 of the current revision in whole seconds, read-only (not an edit
    /// field). `None` when unknown or sent by an older agent.
    pub duration_seconds: Option<u32>,
    pub journal_title: Option<String>,
    pub publisher: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub publication_place: Option<String>,
    pub edition: Option<String>,
    pub number_of_pages: Option<u32>,
    pub series_title: Option<String>,
    pub series_number: Option<String>,
    pub language: Option<String>,
    pub keywords: Vec<String>,
    pub site_name: Option<String>,
    pub abstract_text: Option<String>,
    pub institutions: Vec<Institution>,
    /// 采集日期: first durable inclusion in this MemeX library (RFC3339 UTC),
    /// read-only. `None` is "not recorded"; the current agent does not send it yet.
    pub added_at: Option<String>,
    /// Source acquisition provenance. Not a UI date and never a fallback for
    /// `added_at` (draft 3): it is assigned before the item is committed.
    pub captured_at: Option<String>,
    /// Read-only: where the item was captured from, and its canonical form.
    pub source_url: Option<String>,
    pub canonical_url: Option<String>,
    /// Provenance origin per field (`keywords`, `creators`, `published_date`, …)
    /// exactly as the agent names it; unknown origins are kept and still shown.
    pub origins: std::collections::BTreeMap<String, String>,
}

/// A provenance origin that names a model (AI enrichment) rather than the page,
/// a lookup or the owner, e.g. `model_enrichment`.
pub fn is_model_origin(origin: &str) -> bool {
    origin.split(['_', '-', '.']).any(|part| matches!(part, "model" | "llm" | "ai"))
}

impl Bibliographic {
    pub fn identifier(&self, kind: &str) -> Option<&Identifier> {
        self.identifiers.iter().find(|i| i.kind == kind)
    }

    /// The field's current value was provided by a model.
    pub fn from_model(&self, field: &str) -> bool {
        self.origins.get(field).is_some_and(|o| is_model_origin(o))
    }

    /// The link to open: the canonical URL when present, else the source URL;
    /// only http(s).
    pub fn link(&self) -> Option<&str> {
        self.canonical_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .or(self.source_url.as_deref())
            .map(str::trim)
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
    }

    /// Host of `link`, shown (not saved) where the site name is empty.
    pub fn link_host(&self) -> Option<String> {
        let rest = self.link()?.split_once("://")?.1;
        let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
        let host = host.strip_prefix("www.").unwrap_or(host);
        (!host.is_empty()).then(|| host.to_string())
    }

    /// First author's short name (or the first creator's) and whether more
    /// authors follow. Formatted at render time, in the current language.
    pub fn first_author(&self) -> Option<(String, bool)> {
        let authors: Vec<&Creator> = self.creators.iter().filter(|c| c.role == "author").collect();
        let first = authors.first().copied().or(self.creators.first())?;
        Some((first.short_name(), authors.len() > 1))
    }

    /// Container for journal articles, publisher for books, site for webpages.
    pub fn container(&self) -> Option<String> {
        self.journal_title.clone().or_else(|| self.publisher.clone()).or_else(|| self.site_name.clone())
    }

    /// The edit-shaped value of one editable field, `None` when absent. Used to
    /// diff drafts into `set`/`clear`.
    pub fn field_value(&self, field: &str) -> Option<Value> {
        let s = |v: &Option<String>| v.as_ref().map(|v| Value::String(v.clone()));
        let list = |empty: bool, v: Value| (!empty).then_some(v);
        match field {
            "creators" => list(self.creators.is_empty(), serde_json::to_value(&self.creators).ok()?),
            "identifiers" => list(self.identifiers.is_empty(), serde_json::to_value(&self.identifiers).ok()?),
            "published_date" => self.published_date.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            "journal_title" => s(&self.journal_title),
            "publisher" => s(&self.publisher),
            "volume" => s(&self.volume),
            "issue" => s(&self.issue),
            "pages" => s(&self.pages),
            "publication_place" => s(&self.publication_place),
            "edition" => s(&self.edition),
            "number_of_pages" => self.number_of_pages.map(Value::from),
            "series_title" => s(&self.series_title),
            "series_number" => s(&self.series_number),
            "language" => s(&self.language),
            "keywords" => list(self.keywords.is_empty(), serde_json::to_value(&self.keywords).ok()?),
            "site_name" => s(&self.site_name),
            "abstract_text" => s(&self.abstract_text),
            "institutions" => list(self.institutions.is_empty(), serde_json::to_value(&self.institutions).ok()?),
            _ => None,
        }
    }
}

/// Fields the desktop edits through `bibliographic_metadata_edit`.
pub const EDITABLE_FIELDS: [&str; 17] = [
    "creators",
    "identifiers",
    "published_date",
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
    "language",
    "keywords",
    "site_name",
    "abstract_text",
];

// ---------------------------------------------------------------- read (wire)

#[derive(Debug, Clone, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    #[serde(default)]
    pub provenance: Option<WireProvenance>,
}

/// `provenance {origin, provider_id, …}`; only the origin is read, as a string,
/// so origins added later (model enrichment) still decode.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireProvenance {
    #[serde(default)]
    pub origin: String,
}

impl<T> Sourced<T> {
    fn origin(&self) -> Option<String> {
        self.provenance.as_ref().map(|p| p.origin.clone()).filter(|o| !o.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WireMetadata {
    pub creators: Option<Sourced<Vec<Creator>>>,
    pub language: Option<Sourced<String>>,
    pub site_name: Option<Sourced<String>>,
    pub abstract_text: Option<Sourced<String>>,
    pub journal_title: Option<Sourced<String>>,
    pub publisher: Option<Sourced<String>>,
    pub volume: Option<Sourced<String>>,
    pub issue: Option<Sourced<String>>,
    pub pages: Option<Sourced<String>>,
    pub publication_place: Option<Sourced<String>>,
    pub edition: Option<Sourced<String>>,
    pub number_of_pages: Option<Sourced<u32>>,
    pub series_title: Option<Sourced<String>>,
    pub series_number: Option<Sourced<String>>,
    pub keywords: Option<Sourced<Vec<String>>>,
    pub institutions: Option<Sourced<Vec<Institution>>>,
    /// Read for its provenance only; the value comes from the top-level `published_date`.
    pub published_partial_date: Option<Sourced<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireItemV2 {
    pub item_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub item_type: String,
    #[serde(default)]
    pub added_at: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub canonical_url: Option<String>,
    #[serde(default)]
    pub metadata: WireMetadata,
}

/// `bibliographic_metadata` response.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataV2Read {
    pub metadata_schema_version: u32,
    pub item: WireItemV2,
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
    #[serde(default)]
    pub published_date: Option<PartialDate>,
    #[serde(default)]
    pub published_date_origin: Option<String>,
    /// Additive (2026-11-09): omitted (older agent) and null are unknown.
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

impl From<MetadataV2Read> for Bibliographic {
    fn from(read: MetadataV2Read) -> Self {
        let m = read.item.metadata;
        let mut origins = std::collections::BTreeMap::new();
        let mut note = |field: &str, origin: Option<String>| {
            if let Some(o) = origin {
                origins.insert(field.to_string(), o);
            }
        };
        note("creators", m.creators.as_ref().and_then(Sourced::origin));
        note("keywords", m.keywords.as_ref().and_then(Sourced::origin));
        note("published_date", m.published_partial_date.as_ref().and_then(Sourced::origin));
        note("abstract_text", m.abstract_text.as_ref().and_then(Sourced::origin));
        note("site_name", m.site_name.as_ref().and_then(Sourced::origin));
        note("language", m.language.as_ref().and_then(Sourced::origin));
        let v = |s: Option<Sourced<String>>| s.map(|s| s.value);
        Bibliographic {
            item_id: read.item.item_id,
            revision_id: read.item.revision_id,
            item_type: read.item.item_type,
            creators: m.creators.map(|s| s.value).unwrap_or_default(),
            identifiers: read.identifiers,
            published_date: read.published_date,
            duration_seconds: read.duration_seconds,
            journal_title: v(m.journal_title),
            publisher: v(m.publisher),
            volume: v(m.volume),
            issue: v(m.issue),
            pages: v(m.pages),
            publication_place: v(m.publication_place),
            edition: v(m.edition),
            number_of_pages: m.number_of_pages.map(|s| s.value),
            series_title: v(m.series_title),
            series_number: v(m.series_number),
            language: v(m.language),
            keywords: m.keywords.map(|s| s.value).unwrap_or_default(),
            site_name: v(m.site_name),
            abstract_text: v(m.abstract_text),
            institutions: m.institutions.map(|s| s.value).unwrap_or_default(),
            added_at: read.item.added_at,
            captured_at: read.item.captured_at,
            source_url: read.item.source_url,
            canonical_url: read.item.canonical_url,
            origins,
        }
    }
}

// ---------------------------------------------------------------- edit (wire)

/// `ItemMetadataEditV2`. `set` values are unsourced owner inputs; `clear` removes
/// fields; a lookup apply sends `preview_id` + `selected_candidate_ids` only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataEdit {
    pub metadata_schema_version: u32,
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    pub set: Map<String, Value>,
    pub clear: Vec<String>,
    pub preview_id: Option<String>,
    pub selected_candidate_ids: Vec<String>,
}

impl MetadataEdit {
    /// Everything except `request_id`: two saves with the same body are the same
    /// save, so a retry reuses its request identity.
    pub fn same_body(&self, other: &MetadataEdit) -> bool {
        MetadataEdit { request_id: String::new(), ..self.clone() } == MetadataEdit { request_id: String::new(), ..other.clone() }
    }
}

/// A fresh request identity for one save (`[A-Za-z0-9_-]`, process-unique).
pub fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("desktop-{:x}-{:x}-{}", std::process::id(), nanos, COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// `{kind, value}` for duplicate checks and lookups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentifierQuery {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IdentifierDuplicate {
    pub item_id: String,
    pub revision_id: String,
    pub kind: String,
    pub key: String,
}

// ---------------------------------------------------------------- lookup preview

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LookupIdentifier {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LookupCandidate {
    pub candidate_id: String,
    pub field: String,
    #[serde(default)]
    pub current_value: Option<Value>,
    pub proposed_value: Value,
    #[serde(default)]
    pub source_label: String,
    pub conflict: bool,
    pub unchanged: bool,
    #[serde(default)]
    pub selected: bool,
    /// Fields that must also be selected for this candidate to apply.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// A frozen preview of one lookup. Apply sends only its id and chosen candidate
/// ids; the agent re-derives the values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LookupPreview {
    pub preview_id: String,
    pub item_id: String,
    pub expected_revision_id: String,
    pub provider: String,
    pub identifier: LookupIdentifier,
    pub fetched_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub notes: Vec<String>,
    pub candidates: Vec<LookupCandidate>,
}

// ---------------------------------------------------------------- 采集日期 display

/// Seconds since the Unix epoch for an RFC3339 timestamp with `Z` or `±HH:MM`.
pub fn parse_rfc3339(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, s) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut rest = &t[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) && &o[3..4] == ":" => {
            let v = o[1..3].parse::<i64>().ok()? * 3600 + o[4..6].parse::<i64>().ok()? * 60;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - offset)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "YYYY-MM-DD HH:MM" at a UTC offset (seconds east).
pub fn format_at_offset(epoch: i64, offset_seconds: i64) -> String {
    let t = epoch + offset_seconds;
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let secs = t.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// The system's UTC offset at that instant (daylight saving included).
pub(crate) fn local_offset(epoch: i64) -> i64 {
    let time = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; localtime_r is re-entrant.
    let ok = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as i64 } else { 0 }
}

/// The user's local calendar day (year, month, day) at that instant.
pub(crate) fn local_civil(epoch: i64) -> (i64, i64, i64) {
    civil_from_days((epoch + local_offset(epoch)).div_euclid(86_400))
}

/// Seconds since the epoch in the user's local time, "YYYY-MM-DD HH:MM".
pub fn local_datetime(epoch: i64) -> String {
    format_at_offset(epoch, local_offset(epoch))
}

/// `added_at` in the user's local time, minute precision; `None` when absent or
/// unreadable (shown as "not recorded", never replaced by another date).
pub fn added_at_local(added_at: Option<&str>) -> Option<String> {
    let epoch = parse_rfc3339(added_at?)?;
    Some(format_at_offset(epoch, local_offset(epoch)))
}
