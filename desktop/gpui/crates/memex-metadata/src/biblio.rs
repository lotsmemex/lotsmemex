//! Bibliographic edit draft (no GPUI types).
//!
//! Inputs hold exactly what the user typed. A field whose inputs equal their
//! initial text keeps the stored value untouched (source display names,
//! creator/institution links, raw identifier input); only changed fields go to
//! `bibliographic_metadata_edit`, as `set` or, when emptied, `clear`. Names are
//! never split: a person has separate family/given inputs, anything else one
//! literal name.

use memex_client::biblio::{Bibliographic, Creator, DatePrecision, Identifier, MetadataEdit, PartialDate, EDITABLE_FIELDS, METADATA_SCHEMA_VERSION};
use std::collections::BTreeMap;

/// Identifier kinds edited as single inputs (first of each kind; others kept).
pub const ID_KINDS: [&str; 3] = ["doi", "isbn", "issn"];
pub const ROLES: [&str; 4] = ["author", "editor", "translator", "contributor"];
/// Plain text fields, by agent field name.
pub const TEXT_FIELDS: [&str; 12] = [
    "journal_title",
    "publisher",
    "volume",
    "issue",
    "pages",
    "publication_place",
    "edition",
    "series_title",
    "series_number",
    "language",
    "site_name",
    "number_of_pages",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreatorInput {
    pub role: String,
    /// Person: family/given inputs. Otherwise one literal name.
    pub person: bool,
    pub family: String,
    pub given: String,
    pub literal: String,
    /// The stored creator this row edits (keeps creator_id, institution links).
    base: Option<Creator>,
}

impl CreatorInput {
    fn from_creator(c: &Creator) -> Self {
        let person = c.is_person();
        Self {
            role: c.role.clone(),
            person,
            family: c.family_name.clone().unwrap_or_default(),
            given: c.given_name.clone().unwrap_or_default(),
            literal: if person { String::new() } else { c.literal_name.clone().unwrap_or_else(|| c.display_name.clone()) },
            base: Some(c.clone()),
        }
    }

    pub fn new_person() -> Self {
        Self { role: "author".into(), person: true, ..Default::default() }
    }

    fn is_blank(&self) -> bool {
        if self.person { self.family.trim().is_empty() && self.given.trim().is_empty() } else { self.literal.trim().is_empty() }
    }

    fn to_creator(&self) -> Creator {
        if let Some(base) = &self.base {
            if Self::from_creator(base) == *self {
                return base.clone();
            }
        }
        let keep = self.base.clone();
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let (name_type, family, given, literal, display) = if self.person {
            let (f, g) = (self.family.trim(), self.given.trim());
            // Composed from the parts entered, never split from a whole name.
            let display = match (f.is_empty(), g.is_empty()) {
                (false, false) if f.is_ascii() && g.is_ascii() => format!("{g} {f}"),
                _ => format!("{f}{g}"),
            };
            ("person", opt(f), opt(g), None, display)
        } else {
            // A literal kept from a typed organization/unparsed creator; a legacy
            // display-only name that was edited becomes `unparsed`.
            let ty = match keep.as_ref().and_then(|c| c.name_type.as_deref()) {
                Some("organization") => "organization",
                Some("person") | None if keep.is_none() => "organization",
                _ => "unparsed",
            };
            (ty, None, None, opt(&self.literal), self.literal.trim().to_string())
        };
        Creator {
            display_name: display,
            role: self.role.clone(),
            creator_id: keep.as_ref().and_then(|c| c.creator_id.clone()),
            name_type: Some(name_type.into()),
            family_name: family,
            given_name: given,
            literal_name: literal,
            institution_ids: keep.map(|c| c.institution_ids).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inputs {
    /// TEXT_FIELDS, `doi`/`isbn`/`issn`, `date` and `date_raw`.
    pub text: BTreeMap<String, String>,
    pub precision: Option<DatePrecision>,
    pub creators: Vec<CreatorInput>,
    pub keywords: Vec<String>,
}

impl Inputs {
    fn from(b: &Bibliographic) -> Self {
        let mut text = BTreeMap::new();
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        for field in TEXT_FIELDS {
            let value = match field {
                "journal_title" => s(&b.journal_title),
                "publisher" => s(&b.publisher),
                "volume" => s(&b.volume),
                "issue" => s(&b.issue),
                "pages" => s(&b.pages),
                "publication_place" => s(&b.publication_place),
                "edition" => s(&b.edition),
                "series_title" => s(&b.series_title),
                "series_number" => s(&b.series_number),
                "language" => s(&b.language),
                "site_name" => s(&b.site_name),
                _ => b.number_of_pages.map(|n| n.to_string()).unwrap_or_default(),
            };
            text.insert(field.to_string(), value);
        }
        for kind in ID_KINDS {
            text.insert(kind.into(), b.identifier(kind).map(Identifier::display).unwrap_or_default());
        }
        let date = b.published_date.as_ref();
        let unparsed = date.is_some_and(|d| d.precision == DatePrecision::Unparsed);
        text.insert("date".into(), date.filter(|_| !unparsed).map(|d| d.display()).unwrap_or_default());
        text.insert("date_raw".into(), date.filter(|_| unparsed).and_then(|d| d.raw.clone()).unwrap_or_default());
        Self {
            text,
            precision: date.map(|d| d.precision),
            creators: b.creators.iter().map(CreatorInput::from_creator).collect(),
            keywords: b.keywords.clone(),
        }
    }

    pub fn get(&self, key: &str) -> &str {
        self.text.get(key).map(String::as_str).unwrap_or_default()
    }
}

/// Agent field an input key belongs to (field errors are reported per field).
pub fn field_of(key: &str) -> &str {
    match key {
        "doi" | "isbn" | "issn" => "identifiers",
        "date" | "date_raw" | "precision" => "published_date",
        k if k.starts_with("creator") => "creators",
        k if k.starts_with("keyword") => "keywords",
        k => k,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiblioDraft {
    original: Bibliographic,
    initial: Inputs,
    pub inputs: Inputs,
    /// Field -> reason (local or from the agent), shown next to the field.
    errors: BTreeMap<String, String>,
    /// The last edit whose outcome is unknown (timeout, lost reply). A retry with
    /// the same body reuses its request identity so the agent replays, never
    /// applies twice.
    pending: Option<MetadataEdit>,
}

impl BiblioDraft {
    pub fn new(original: Bibliographic) -> Self {
        let initial = Inputs::from(&original);
        Self { inputs: initial.clone(), initial, original, errors: BTreeMap::new(), pending: None }
    }

    pub fn original(&self) -> &Bibliographic {
        &self.original
    }

    pub fn is_dirty(&self) -> bool {
        self.inputs != self.initial
    }

    pub fn error(&self, field: &str) -> Option<&str> {
        self.errors.get(field).map(String::as_str)
    }

    pub fn errors(&self) -> &BTreeMap<String, String> {
        &self.errors
    }

    pub fn set_errors(&mut self, field: String, reason: String) {
        self.errors.insert(field, reason);
    }

    /// Editing a field clears its error.
    pub fn touched(&mut self, key: &str) {
        let field = field_of(key).to_string();
        self.errors.remove(&field);
    }

    pub fn reset(&mut self) {
        self.inputs = self.initial.clone();
        self.errors.clear();
    }

    /// After a confirmed save (the caller reloads the stored values).
    pub fn commit(&mut self) {
        self.initial = self.inputs.clone();
        self.errors.clear();
        self.pending = None;
    }

    pub fn set_pending(&mut self, edit: Option<MetadataEdit>) {
        self.pending = edit;
    }

    pub fn pending(&self) -> Option<&MetadataEdit> {
        self.pending.as_ref()
    }

    /// The draft as stored values, untouched fields exactly as read.
    pub fn draft(&self) -> Result<Bibliographic, BTreeMap<String, String>> {
        let (i, o) = (&self.inputs, &self.initial);
        let mut b = self.original.clone();
        let mut errors = BTreeMap::new();
        let changed = |key: &str| i.get(key) != o.get(key);
        let text = |key: &str| Some(i.get(key).trim().to_string()).filter(|v| !v.is_empty());
        for field in TEXT_FIELDS.into_iter().filter(|f| changed(f)) {
            let value = text(field);
            match field {
                "journal_title" => b.journal_title = value,
                "publisher" => b.publisher = value,
                "volume" => b.volume = value,
                "issue" => b.issue = value,
                "pages" => b.pages = value,
                "publication_place" => b.publication_place = value,
                "edition" => b.edition = value,
                "series_title" => b.series_title = value,
                "series_number" => b.series_number = value,
                "language" => b.language = value,
                "site_name" => b.site_name = value,
                _ => match value.map(|v| v.parse::<u32>()) {
                    None => b.number_of_pages = None,
                    Some(Ok(n)) if n > 0 => b.number_of_pages = Some(n),
                    Some(_) => {
                        errors.insert("number_of_pages".to_string(), "positive_integer".to_string());
                    }
                },
            }
        }
        for kind in ID_KINDS.into_iter().filter(|k| changed(k)) {
            let value = text(kind);
            match (b.identifiers.iter().position(|id| id.kind == kind), value) {
                (Some(at), Some(v)) => {
                    let q = b.identifiers[at].qualifier.clone();
                    b.identifiers[at] = Identifier::input(kind, &v, q);
                }
                (Some(at), None) => {
                    b.identifiers.remove(at);
                }
                (None, Some(v)) => b.identifiers.push(Identifier::input(kind, &v, None)),
                (None, None) => {}
            }
        }
        if changed("date") || changed("date_raw") || i.precision != o.precision {
            b.published_date = match i.precision.unwrap_or(DatePrecision::Day) {
                DatePrecision::Unparsed => text("date_raw").map(|raw| PartialDate { value: None, precision: DatePrecision::Unparsed, raw: Some(raw) }),
                precision => text("date").map(|v| PartialDate { value: Some(v), precision, raw: None }),
            };
        }
        if i.creators != o.creators {
            b.creators = i.creators.iter().filter(|c| !c.is_blank()).map(CreatorInput::to_creator).collect();
        }
        if i.keywords != o.keywords {
            b.keywords = i.keywords.clone();
        }
        if errors.is_empty() { Ok(b) } else { Err(errors) }
    }

    /// The edit for this draft, or None when nothing differs from what was read.
    /// Reuses the unknown-outcome request's identity when the body is the same.
    pub fn build_edit(&mut self, item_id: &str, expected_revision_id: &str, allow_reuse: bool) -> Result<Option<MetadataEdit>, ()> {
        let draft = match self.draft() {
            Ok(d) => d,
            Err(errors) => {
                self.errors.extend(errors);
                return Err(());
            }
        };
        let mut edit = MetadataEdit {
            metadata_schema_version: METADATA_SCHEMA_VERSION,
            item_id: item_id.into(),
            expected_revision_id: expected_revision_id.into(),
            request_id: String::new(),
            set: Default::default(),
            clear: vec![],
            preview_id: None,
            selected_candidate_ids: vec![],
        };
        for field in EDITABLE_FIELDS {
            let (before, after) = (self.original.field_value(field), draft.field_value(field));
            if before != after {
                match after {
                    Some(v) => {
                        edit.set.insert(field.to_string(), v);
                    }
                    None => edit.clear.push(field.to_string()),
                }
            }
        }
        if edit.set.is_empty() && edit.clear.is_empty() {
            return Ok(None);
        }
        edit.request_id = match &self.pending {
            Some(p) if allow_reuse && p.same_body(&edit) => p.request_id.clone(),
            _ => memex_client::biblio::new_request_id(),
        };
        Ok(Some(edit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Client, Sensitivity};

    fn load(id: &str) -> (Client, Bibliographic) {
        let client = Client::new(FixtureBackend::new(8));
        let rev = client.item_read(id).unwrap().revision_id;
        let b = client.bibliographic(id, &rev, Sensitivity::Restricted).unwrap();
        (client, b)
    }

    #[test]
    fn untouched_fields_are_not_sent_and_changes_become_set_or_clear() {
        let (_, b) = load("item_fixture_000001");
        let mut d = BiblioDraft::new(b.clone());
        assert!(!d.is_dirty());
        assert_eq!(d.build_edit("i", "r", true), Ok(None));
        d.inputs.text.insert("volume".into(), "99".into());
        d.inputs.text.insert("pages".into(), "  ".into());
        let e = d.build_edit("i", "r", true).unwrap().unwrap();
        assert_eq!(e.set.keys().collect::<Vec<_>>(), ["volume"]);
        assert_eq!(e.clear, ["pages"]);
        // Creators: a changed row keeps its row id; an untouched row is sent exactly as read.
        let mut d = BiblioDraft::new(b.clone());
        d.inputs.creators.push(CreatorInput { family: "林".into(), given: "晓".into(), ..CreatorInput::new_person() });
        let e = d.build_edit("i", "r", true).unwrap().unwrap();
        let sent: Vec<Creator> = serde_json::from_value(e.set["creators"].clone()).unwrap();
        assert_eq!(sent[..b.creators.len()], b.creators[..]);
        let new = sent.last().unwrap();
        assert_eq!((new.name_type.as_deref(), new.family_name.as_deref(), new.given_name.as_deref(), new.display_name.as_str()), (Some("person"), Some("林"), Some("晓"), "林晓"));
        // A literal name is never split into family/given.
        let mut d = BiblioDraft::new(b);
        d.inputs.creators[0] = CreatorInput { person: false, literal: "Synthetic Consortium of Readers".into(), ..d.inputs.creators[0].clone() };
        let sent: Vec<Creator> = serde_json::from_value(d.build_edit("i", "r", true).unwrap().unwrap().set["creators"].clone()).unwrap();
        assert_eq!((sent[0].family_name.as_deref(), sent[0].literal_name.as_deref()), (None, Some("Synthetic Consortium of Readers")));
        assert!(sent[0].creator_id.is_some());
    }

    #[test]
    fn dates_follow_the_chosen_precision_and_bad_page_counts_stay_local() {
        let (_, b) = load("item_fixture_000003");
        let mut d = BiblioDraft::new(b);
        d.inputs.precision = Some(DatePrecision::Unparsed);
        d.inputs.text.insert("date_raw".into(), "2019年春季".into());
        let e = d.build_edit("i", "r", true).unwrap().unwrap();
        assert_eq!(e.set["published_date"], serde_json::json!({"value": null, "precision": "unparsed", "raw": "2019年春季"}));
        d.inputs.precision = Some(DatePrecision::Year);
        d.inputs.text.insert("date".into(), "2019".into());
        let e = d.build_edit("i", "r", true).unwrap().unwrap();
        assert_eq!(e.set["published_date"], serde_json::json!({"value": "2019", "precision": "year", "raw": null}));
        d.inputs.text.insert("number_of_pages".into(), "三百".into());
        assert_eq!(d.build_edit("i", "r", true), Err(()));
        assert_eq!(d.error("number_of_pages"), Some("positive_integer"));
        d.touched("number_of_pages");
        assert_eq!(d.error("number_of_pages"), None);
    }

    #[test]
    fn a_retry_of_the_same_save_reuses_its_request_id_and_a_new_save_does_not() {
        let (_, b) = load("item_fixture_000001");
        let mut d = BiblioDraft::new(b);
        d.inputs.text.insert("issue".into(), "7".into());
        let first = d.build_edit("i", "r", true).unwrap().unwrap();
        d.set_pending(Some(first.clone()));
        let retry = d.build_edit("i", "r", true).unwrap().unwrap();
        assert_eq!(retry.request_id, first.request_id);
        // Not reused when the save also changes title/tags first (the body will be re-bound).
        assert_ne!(d.build_edit("i", "r", false).unwrap().unwrap().request_id, first.request_id);
        d.inputs.text.insert("issue".into(), "8".into());
        assert_ne!(d.build_edit("i", "r", true).unwrap().unwrap().request_id, first.request_id);
    }
}
