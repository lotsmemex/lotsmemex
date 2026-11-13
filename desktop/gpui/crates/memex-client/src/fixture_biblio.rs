//! Synthetic bibliographic v2 for the fixture backend, following the agent's
//! rules (`memex-store` `revise_item_metadata_v2`, `lookup.rs`): identifier
//! normalization with checksums, field-scoped errors, request-identity replay,
//! revision CAS, and frozen lookup previews with conflict/unchanged/`requires`.
//!
//! Lookups never touch the network: candidates are canned, invented values, and
//! every preview carries a note saying so.

use crate::biblio::*;
use crate::dto::ItemUpdate;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

/// Note added to every synthetic preview (shown by the UI like provider notes).
pub const SYNTHETIC_NOTE: &str = "fixture_synthetic_candidates";
const MAX_PREVIEWS: usize = 256;

#[derive(Default)]
pub(crate) struct BiblioState {
    /// request_id -> (request body without the id, receipt)
    requests: HashMap<String, (String, ItemUpdate)>,
    previews: HashMap<String, (LookupPreview, bool)>,
    next_preview: u64,
}

impl BiblioState {
    pub(crate) fn expire_previews(&mut self) {
        for (_, expired) in self.previews.values_mut() {
            *expired = true;
        }
    }
}

pub(crate) fn capabilities() -> Value {
    json!({
        "schema_version": 1,
        "features": [BIBLIOGRAPHIC_FEATURE],
        "bibliographic_metadata_v2": {
            "metadata_schema_version": METADATA_SCHEMA_VERSION,
            "normalizer_version": "memex.identifier_normalizer.v1",
            "lookup_adapter_version": "fixture.synthetic_lookup.v1",
            "lookup_sources": [
                {"provider": "crossref", "identifier_kind": "doi", "route": "https://api.crossref.org/works/{doi}"},
                {"provider": "open_library", "identifier_kind": "isbn", "route": "https://openlibrary.org/isbn/{isbn}.json"}
            ],
            "lookup_preview_ttl_seconds": 86400,
            "lookup_batch": false
        }
    })
}

// ---------------------------------------------------------------- normalization

/// Normalized value or a validation reason, as the agent's normalizer decides.
pub(crate) fn normalize(kind: &str, raw: &str) -> Result<String, &'static str> {
    match kind {
        "doi" => normalize_doi(raw),
        "isbn" => normalize_isbn(raw),
        "issn" => normalize_issn(raw),
        _ => Err("not_validated"),
    }
}

fn normalize_doi(raw: &str) -> Result<String, &'static str> {
    let mut s = raw.trim();
    s = strip_label(s, &["https://doi.org/", "http://doi.org/", "https://dx.doi.org/", "http://dx.doi.org/"]);
    s = strip_label(s, &["doi:"]);
    let (prefix, suffix) = s.split_once('/').ok_or("syntax")?;
    let registrant = prefix.strip_prefix("10.").ok_or("prefix")?;
    if registrant.is_empty() || !registrant.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return Err("prefix");
    }
    if suffix.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("syntax");
    }
    Ok(s.to_ascii_lowercase())
}

/// Removes the first matching case-insensitive label (and a following `:`/space).
fn strip_label<'a>(t: &'a str, labels: &[&str]) -> &'a str {
    let t = t.trim();
    for label in labels {
        if t.get(..label.len()).is_some_and(|head| head.eq_ignore_ascii_case(label)) {
            return t[label.len()..].trim_start_matches([':', ' ']);
        }
    }
    t
}

fn compact(t: &str) -> String {
    t.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect()
}

fn normalize_isbn(raw: &str) -> Result<String, &'static str> {
    let s = compact(strip_label(raw, &["ISBN-13", "ISBN-10", "ISBN"]));
    match s.len() {
        10 => {
            let (body, check) = s.split_at(9);
            if !body.bytes().all(|b| b.is_ascii_digit()) || !check.bytes().all(|b| b.is_ascii_digit() || b == b'X') {
                return Err("syntax");
            }
            let sum: u32 = s.bytes().enumerate().map(|(k, b)| (10 - k as u32) * if b == b'X' { 10 } else { (b - b'0') as u32 }).sum();
            if sum % 11 != 0 {
                return Err("checksum");
            }
            Ok(isbn13_from_stem(&format!("978{body}")))
        }
        13 => {
            if !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err("syntax");
            }
            if !s.starts_with("978") && !s.starts_with("979") {
                return Err("prefix");
            }
            let sum: u32 = s.bytes().enumerate().map(|(k, b)| (b - b'0') as u32 * if k % 2 == 0 { 1 } else { 3 }).sum();
            if sum % 10 != 0 {
                return Err("checksum");
            }
            Ok(s)
        }
        _ => Err("length"),
    }
}

pub(crate) fn isbn13_from_stem(stem12: &str) -> String {
    let sum: u32 = stem12.bytes().enumerate().map(|(k, b)| (b - b'0') as u32 * if k % 2 == 0 { 1 } else { 3 }).sum();
    format!("{stem12}{}", (10 - sum % 10) % 10)
}

fn normalize_issn(raw: &str) -> Result<String, &'static str> {
    let s = compact(strip_label(raw, &["ISSN"]));
    if s.len() != 8 {
        return Err("length");
    }
    if !s[..7].bytes().all(|b| b.is_ascii_digit()) || !s[7..].bytes().all(|b| b.is_ascii_digit() || b == b'X') {
        return Err("syntax");
    }
    let sum: u32 = s.bytes().enumerate().map(|(k, b)| (8 - k as u32) * if b == b'X' { 10 } else { (b - b'0') as u32 }).sum();
    if sum % 11 != 0 {
        return Err("checksum");
    }
    Ok(format!("{}-{}", &s[..4], &s[4..]))
}

/// The read view of a stored identifier, with server-computed validation.
fn read_identifier(id: &Identifier) -> Value {
    let checked = normalize(&id.kind, &id.value);
    let key = checked.as_ref().ok().filter(|_| matches!(id.kind.as_str(), "doi" | "isbn")).cloned();
    json!({
        "kind": id.kind, "value": id.value, "raw_value": id.raw_value, "qualifier": id.qualifier,
        "normalized_value": checked.as_ref().ok(), "comparison_key": key,
        "validation": {
            "status": match checked { Ok(_) => "valid", Err("not_validated") => "unparsed", Err(_) => "invalid" },
            "reason": checked.err(),
            "normalizer_version": "memex.identifier_normalizer.v1"
        }
    })
}

// ---------------------------------------------------------------- read

pub(crate) fn read(b: &Bibliographic, item_id: &str, revision_id: &str, title: &str, captured_at: Option<&str>) -> Value {
    // Model-provided values name the synthetic model; the rest are imported.
    let src = |field: &str, v: Value| {
        let origin = b.origins.get(field).map(String::as_str).unwrap_or("import");
        let provider = if is_model_origin(origin) { "fixture-model-alpha" } else { "fixture" };
        json!({"value": v, "provenance": {"origin": origin, "provider_id": provider, "provider_version": "synthetic"}})
    };
    let mut metadata = Map::new();
    for field in EDITABLE_FIELDS.iter().chain(["institutions"].iter()) {
        if matches!(*field, "identifiers" | "published_date") {
            continue;
        }
        if let Some(v) = b.field_value(field) {
            metadata.insert((*field).into(), src(field, v));
        }
    }
    if let Some(ids) = b.field_value("identifiers") {
        metadata.insert("identifiers".into(), src("identifiers", ids));
    }
    if let Some(d) = &b.published_date {
        metadata.insert("published_partial_date".into(), src("published_date", serde_json::to_value(d).unwrap_or_default()));
    }
    let mut read = json!({
        "metadata_schema_version": METADATA_SCHEMA_VERSION,
        "item": {
            "item_id": item_id, "revision_id": revision_id, "revision_number": 1,
            "item_type": b.item_type, "title": title, "source_url": b.source_url,
            "canonical_url": b.canonical_url, "sensitivity": "private", "captured_at": captured_at,
            // Not sent by the current agent; the fixture shows the contract field.
            "added_at": b.added_at,
            "metadata": metadata
        },
        "identifiers": b.identifiers.iter().map(read_identifier).collect::<Vec<_>>(),
        "published_date": b.published_date,
        "published_date_origin": b.published_date.as_ref().map(|_| "typed")
    });
    // 时长: explicit null when unknown; books answer in the older shape without
    // the field, which a client reads as unknown too.
    if b.duration_seconds.is_some() || b.item_type != "book" {
        read["duration_seconds"] = json!(b.duration_seconds);
    }
    read
}

// ---------------------------------------------------------------- edit

fn field_error(field: &str, reason: &str) -> String {
    format!("invalid_metadata_field:{field}:{reason}")
}

const READONLY: [&str; 9] = ["added_at", "captured_at", "title", "manual_tags", "item_id", "item_type", "revision_id", "source_url", "validation"];

fn check_field(field: &str) -> Result<(), String> {
    if EDITABLE_FIELDS.contains(&field) || matches!(field, "institutions" | "cited_references") {
        Ok(())
    } else if READONLY.contains(&field) {
        Err(format!("readonly_metadata_field:{field}"))
    } else {
        Err(format!("unknown_metadata_field:{field}"))
    }
}

fn typed<T: serde::de::DeserializeOwned>(field: &str, v: &Value) -> Result<T, String> {
    if v.is_null() {
        return Err(field_error(field, "null_use_clear"));
    }
    serde_json::from_value(v.clone()).map_err(|_| field_error(field, "type"))
}

fn valid_date(d: &PartialDate) -> Result<(), &'static str> {
    let Some(value) = d.value.as_deref() else {
        return match (d.precision, d.raw.as_deref().map(str::trim)) {
            (DatePrecision::Unparsed, Some(raw)) if !raw.is_empty() => Ok(()),
            (DatePrecision::Unparsed, _) => Err("unparsed_without_raw"),
            _ => Err("missing_value"),
        };
    };
    let parts: Vec<&str> = value.split('-').collect();
    let n = match d.precision {
        DatePrecision::Year => 1,
        DatePrecision::Month => 2,
        DatePrecision::Day => 3,
        DatePrecision::Unparsed => return Err("unparsed_has_value"),
    };
    let num = |s: &str, len: usize| (s.len() == len && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten();
    if parts.len() != n {
        return Err("precision_mismatch");
    }
    let year = num(parts[0], 4).ok_or("format")?;
    if n >= 2 {
        let month = num(parts[1], 2).filter(|m| (1..=12).contains(m)).ok_or("format")?;
        if n == 3 {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            let days = match month {
                2 if leap => 29,
                2 => 28,
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            num(parts[2], 2).filter(|d| (1..=days).contains(d)).ok_or("format")?;
        }
    }
    Ok(())
}

fn set_field(b: &mut Bibliographic, title: &mut String, field: &str, v: &Value) -> Result<(), String> {
    let s = |v: &Value| typed::<String>(field, v);
    match field {
        "title" => *title = s(v)?,
        "creators" => b.creators = typed(field, v)?,
        "identifiers" => {
            let input: Vec<Identifier> = typed(field, v)?;
            if input.is_empty() {
                return Err(field_error(field, "duplicate_or_empty"));
            }
            let mut out: Vec<Identifier> = vec![];
            for id in input {
                let value = match normalize(&id.kind, &id.value) {
                    Ok(v) => v,
                    Err("not_validated") => id.value.trim().to_string(),
                    Err(reason) => return Err(field_error(field, reason)),
                };
                if out.iter().any(|o| o.kind == id.kind && o.value == value) {
                    return Err(field_error(field, "duplicate_or_empty"));
                }
                let raw = (value != id.value).then(|| id.value.clone());
                out.push(Identifier { raw_value: raw, ..Identifier::input(&id.kind, &value, id.qualifier) });
            }
            b.identifiers = out;
        }
        "published_date" => {
            let d: PartialDate = typed(field, v)?;
            valid_date(&d).map_err(|r| field_error(field, r))?;
            b.published_date = Some(d);
        }
        "number_of_pages" => match v.as_u64() {
            Some(n) if n > 0 && n <= u32::MAX as u64 => b.number_of_pages = Some(n as u32),
            // The agent reports this one without a reason.
            _ => return Err("invalid_metadata_field:number_of_pages".into()),
        },
        "keywords" => b.keywords = typed(field, v)?,
        "institutions" => b.institutions = typed(field, v)?,
        "cited_references" => {}
        _ => {
            let value = s(v)?;
            if value.trim().is_empty() {
                return Err("invalid_metadata_field:text".into());
            }
            let slot = match field {
                "journal_title" => &mut b.journal_title,
                "publisher" => &mut b.publisher,
                "volume" => &mut b.volume,
                "issue" => &mut b.issue,
                "pages" => &mut b.pages,
                "publication_place" => &mut b.publication_place,
                "edition" => &mut b.edition,
                "series_title" => &mut b.series_title,
                "series_number" => &mut b.series_number,
                "language" => &mut b.language,
                "site_name" => &mut b.site_name,
                "abstract_text" => &mut b.abstract_text,
                _ => return Err(format!("unknown_metadata_field:{field}")),
            };
            *slot = Some(value);
        }
    }
    Ok(())
}

fn clear_field(b: &mut Bibliographic, field: &str) {
    match field {
        "creators" => b.creators.clear(),
        "identifiers" => b.identifiers.clear(),
        "published_date" => b.published_date = None,
        "number_of_pages" => b.number_of_pages = None,
        "keywords" => b.keywords.clear(),
        "institutions" => b.institutions.clear(),
        "journal_title" => b.journal_title = None,
        "publisher" => b.publisher = None,
        "volume" => b.volume = None,
        "issue" => b.issue = None,
        "pages" => b.pages = None,
        "publication_place" => b.publication_place = None,
        "edition" => b.edition = None,
        "series_title" => b.series_title = None,
        "series_number" => b.series_number = None,
        "language" => b.language = None,
        "site_name" => b.site_name = None,
        "abstract_text" => b.abstract_text = None,
        _ => {}
    }
}

/// Outcome of one edit against one entry.
pub(crate) enum EditOutcome {
    Replay(ItemUpdate),
    /// Apply these values and bump the revision; then `record` the receipt.
    Changed { biblio: Bibliographic, title: String },
}

/// Validates and applies `edit` to a copy of `(biblio, title)` at `revision_id`.
pub(crate) fn edit(state: &BiblioState, biblio: &Bibliographic, title: &str, revision_id: &str, edit: &MetadataEdit) -> Result<EditOutcome, String> {
    if edit.metadata_schema_version != METADATA_SCHEMA_VERSION {
        return Err("unsupported_metadata_schema_version".into());
    }
    if edit.request_id.is_empty() || edit.request_id.len() > 256 {
        return Err("invalid_metadata_update".into());
    }
    if edit.preview_id.is_none() && !edit.selected_candidate_ids.is_empty() {
        return Err("preview_unavailable".into());
    }
    for field in &edit.clear {
        check_field(field)?;
        if edit.set.contains_key(field) {
            return Err(field_error(field, "set_clear_overlap"));
        }
    }
    for field in edit.set.keys() {
        check_field(field)?;
    }
    if let Some((body, receipt)) = state.requests.get(&edit.request_id) {
        return if *body == body_of(edit) && receipt.item_id == edit.item_id { Ok(EditOutcome::Replay(receipt.clone())) } else { Err("request_id_conflict".into()) };
    }
    if revision_id != edit.expected_revision_id {
        return Err("stale_item_revision".into());
    }
    let mut next = biblio.clone();
    let mut next_title = title.to_string();
    for (field, value) in &edit.set {
        set_field(&mut next, &mut next_title, field, value)?;
    }
    for field in &edit.clear {
        clear_field(&mut next, field);
    }
    if let Some(preview_id) = &edit.preview_id {
        let (preview, expired) = state.previews.get(preview_id).filter(|(p, _)| p.item_id == edit.item_id).ok_or("preview_unavailable")?;
        if preview.expected_revision_id != revision_id {
            return Err("stale_item_revision".into());
        }
        if *expired {
            return Err("preview_expired".into());
        }
        let mut fields = BTreeSet::new();
        let mut chosen = vec![];
        for id in &edit.selected_candidate_ids {
            let c = preview.candidates.iter().find(|c| &c.candidate_id == id).ok_or("invalid_candidate_selection")?;
            if !fields.insert(c.field.clone()) {
                return Err("invalid_candidate_selection".into());
            }
            if edit.set.contains_key(&c.field) || edit.clear.contains(&c.field) {
                return Err(field_error(&c.field, "set_clear_overlap"));
            }
            chosen.push(c);
        }
        for c in &chosen {
            if let Some(missing) = c.requires.iter().find(|f| !fields.contains(*f) && !edit.set.contains_key(*f) && !edit.clear.contains(*f)) {
                return Err(format!("candidate_dependency_missing:{}:{missing}", c.field));
            }
        }
        for c in chosen.into_iter().filter(|c| !c.unchanged) {
            set_field(&mut next, &mut next_title, &c.field, &c.proposed_value).map_err(|_| "invalid_lookup_preview".to_string())?;
        }
    }
    if next == *biblio && next_title == title {
        return Err("metadata_unchanged".into());
    }
    // Provenance follows the writer: owner edits and lookup selections replace it.
    for field in edit.set.keys() {
        next.origins.insert(field.clone(), "owner_edit".into());
    }
    for field in &edit.clear {
        next.origins.remove(field);
    }
    if let Some(preview_id) = &edit.preview_id {
        if let Some((preview, _)) = state.previews.get(preview_id) {
            for c in preview.candidates.iter().filter(|c| edit.selected_candidate_ids.contains(&c.candidate_id)) {
                next.origins.insert(c.field.clone(), "lookup_selected".into());
            }
        }
    }
    Ok(EditOutcome::Changed { biblio: next, title: next_title })
}

fn body_of(edit: &MetadataEdit) -> String {
    serde_json::to_string(&MetadataEdit { request_id: String::new(), ..edit.clone() }).unwrap_or_default()
}

pub(crate) fn record(state: &mut BiblioState, edit: &MetadataEdit, receipt: &ItemUpdate) {
    state.requests.insert(edit.request_id.clone(), (body_of(edit), receipt.clone()));
}

// ---------------------------------------------------------------- lookup

fn person(family: &str, given: &str, institutions: &[&str]) -> Value {
    json!({
        "display_name": format!("{given} {family}"), "role": "author", "name_type": "person",
        "family_name": family, "given_name": given,
        "institution_ids": institutions
    })
}

/// Canned provider proposals: (field, value, source label), plus notes.
fn canned(provider: &str, b: &Bibliographic, title: &str, seed: usize) -> (Vec<(&'static str, Value, &'static str)>, Vec<String>) {
    let mut out: Vec<(&'static str, Value, &'static str)> = vec![("title", json!(title), "")];
    let mut notes = vec![SYNTHETIC_NOTE.to_string()];
    if provider == "crossref" {
        out[0].2 = "crossref:title";
        let issue = b.issue.as_deref().and_then(|i| i.parse::<u32>().ok()).map_or("1".to_string(), |i| (i + 1).to_string());
        let year = b.published_date.as_ref().and_then(|d| d.year()).unwrap_or_else(|| "2001".into());
        out.extend([
            ("creators", json!([person("Example", "Ada", &["inst_fixture_1"]), person("Sample", "Ben", &["inst_fixture_1"])]), "crossref:author"),
            ("institutions", json!([{"institution_id": "inst_fixture_1", "name": "示例大学心理学系（合成）", "identifiers": [], "country": null}]), "crossref:author.affiliation"),
            ("journal_title", b.journal_title.clone().map_or(json!("Synthetic Review of Cognition"), Value::String), "crossref:container-title"),
            ("volume", b.volume.clone().map_or(json!("12"), Value::String), "crossref:volume"),
            ("issue", json!(issue), "crossref:issue"),
            ("published_date", json!({"value": format!("{year}-09-27"), "precision": "day", "raw": format!("{year}-09-27")}), "crossref:published-print"),
            ("published_date", json!({"value": format!("{year}-08-{:02}", 1 + seed % 28), "precision": "day", "raw": null}), "crossref:published-online"),
            ("keywords", json!(["judgment", "heuristic", "cognitive bias"]), "crossref:subject"),
            ("abstract_text", json!("We describe a series of synthetic experiments on judgment under uncertainty. (Synthetic abstract; no provider was contacted.)"), "crossref:abstract"),
        ]);
        notes.push("cited_references_not_supplied".into());
    } else {
        out[0].2 = "open_library:title";
        let pages = b.number_of_pages.map_or(320, |n| n + 16);
        let year = b.published_date.as_ref().and_then(|d| d.year()).unwrap_or_else(|| "2012".into());
        out.extend([
            ("publisher", json!("Synthetic Library Press"), "open_library:publishers"),
            ("published_date", json!({"value": year, "precision": "year", "raw": year}), "open_library:publish_date"),
            ("number_of_pages", json!(pages), "open_library:number_of_pages"),
            ("language", b.language.clone().map_or(json!("zh"), Value::String), "open_library:languages"),
            ("series_title", json!("示例思想丛书"), "open_library:series"),
        ]);
        notes.push("creators_not_resolved_author_keys_only".into());
        notes.push("cited_references_not_supplied".into());
    }
    (out, notes)
}

fn current_value(b: &Bibliographic, title: &str, field: &str) -> Option<Value> {
    if field == "title" { Some(json!(title)) } else { b.field_value(field) }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lookup(state: &mut BiblioState, b: &Bibliographic, title: &str, item_id: &str, revision_id: &str, kind: &str, value: &str, seed: usize) -> Result<LookupPreview, String> {
    let provider = match kind {
        "doi" => "crossref",
        "isbn" => "open_library",
        _ => return Err("lookup_identifier_unsupported".into()),
    };
    let normalized = normalize(kind, value).map_err(|_| "lookup_identifier_invalid".to_string())?;
    if !b.identifiers.iter().any(|i| i.kind == kind && normalize(kind, &i.value).as_deref() == Ok(normalized.as_str())) {
        return Err("lookup_identifier_not_on_item".into());
    }
    if state.previews.len() >= MAX_PREVIEWS {
        return Err("lookup_preview_limit_exceeded".into());
    }
    state.next_preview += 1;
    let preview_id = format!("preview_fixture_{}", state.next_preview);
    let (proposals, notes) = canned(provider, b, title, seed);
    let proposed_institutions: BTreeSet<String> = proposals
        .iter()
        .filter(|p| p.0 == "institutions")
        .filter_map(|p| serde_json::from_value::<Vec<Institution>>(p.1.clone()).ok())
        .flatten()
        .map(|i| i.institution_id)
        .collect();
    let current_links: BTreeSet<&str> = b.creators.iter().flat_map(|c| c.institution_ids.iter().map(String::as_str)).collect();
    let candidates = proposals
        .into_iter()
        .enumerate()
        .map(|(k, (field, proposed, label))| {
            let current = current_value(b, title, field);
            let unchanged = current.as_ref() == Some(&proposed);
            let requires = match field {
                "creators" if proposed.as_array().is_some_and(|cs| cs.iter().any(|c| c["institution_ids"].as_array().is_some_and(|a| !a.is_empty()))) => vec!["institutions".to_string()],
                "institutions" if current_links.iter().any(|id| !proposed_institutions.contains(*id)) => vec!["creators".to_string()],
                _ => vec![],
            };
            LookupCandidate {
                candidate_id: format!("{preview_id}_c{k}"),
                field: field.into(),
                conflict: current.is_some() && !unchanged,
                current_value: current,
                proposed_value: proposed,
                source_label: label.into(),
                unchanged,
                selected: false,
                requires,
            }
        })
        .collect();
    let preview = LookupPreview {
        preview_id: preview_id.clone(),
        item_id: item_id.into(),
        expected_revision_id: revision_id.into(),
        provider: provider.into(),
        identifier: LookupIdentifier { kind: kind.into(), value: normalized },
        fetched_at: "2026-11-02T09:00:00Z".into(),
        expires_at: "2026-11-03T09:00:00Z".into(),
        notes,
        candidates,
    };
    state.previews.insert(preview_id, (preview.clone(), false));
    Ok(preview)
}

/// Comparison key (DOI/ISBN only) or an error for invalid input.
pub(crate) fn comparison_key(kind: &str, value: &str) -> Result<Option<String>, String> {
    match normalize(kind, value) {
        Ok(v) if matches!(kind, "doi" | "isbn") => Ok(Some(v)),
        Ok(_) | Err("not_validated") => Ok(None),
        Err(_) => Err("invalid_identifier_query".into()),
    }
}
