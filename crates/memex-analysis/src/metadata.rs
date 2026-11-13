//! Model metadata enrichment ("metadata job").
//!
//! One owner-authorized request per revision (an explicit `translation_request` or a capture this
//! daemon just stored) asks the classification route for keywords, explicitly stated creators and
//! an explicitly stated publication date. Rows exist only for authorized work: the background
//! reconciler never creates them. The strictly validated suggestion is written by the Store into
//! fields that are still empty, through the owner-revision CAS path; a concurrent edit drops it.

use super::revision_ref;
use crate::translation::{
    model_error_reason, open_status_database, secure_database_family, validate_database_family,
    TranslationModelDescriptor, TranslationModelError, TranslationQueue, DATABASE_NAME,
};
use anyhow::{bail, Result};
use chrono::Utc;
use memex_core::{
    Creator, CreatorRole, DatePrecision, ItemMetadata, MetadataOrigin, MetadataProvenance,
    PartialDate,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const METADATA_PROMPT_ID: &str = "memex.metadata-enrichment";
/// 2: keywords must be copied from the text (grounded and validated); model values of older
/// versions may be replaced by an explicit re-request.
pub const METADATA_PROMPT_VERSION: &str = "2";
pub const METADATA_PROMPT: &str = include_str!("../prompts/metadata-enrichment-v2.txt");
/// Appended to the second request of an attempt whose keywords were not grounded in the text.
pub const METADATA_RETRY_REMINDER: &str = "Reminder: the previous answer was rejected because its keywords did not appear in the document. Copy every keyword exactly as it is written in the title or the document text, in the document's own language. Never translate a keyword.";
/// Fewer grounded keywords than this asks the model once more within the attempt.
const MIN_GROUNDED_KEYWORDS: usize = 2;
const OPENING_BYTES: usize = 12 * 1024;
const ENDING_BYTES: usize = 2 * 1024;
const MAX_EXISTING_METADATA_BYTES: usize = 4 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const LEASE_SECONDS: i64 = 180;

/// Source text is not `Debug`/`Serialize`; only the queue constructs it.
pub struct MetadataModelInput<'a> {
    title: &'a str,
    source_url: &'a str,
    existing_metadata: String,
    opening: &'a str,
    ending: &'a str,
    retry: bool,
}

impl<'a> MetadataModelInput<'a> {
    pub fn prompt(&self) -> &'static str {
        METADATA_PROMPT
    }
    pub fn title(&self) -> &'a str {
        self.title
    }
    pub fn source_url(&self) -> &'a str {
        self.source_url
    }
    /// Compact JSON of the bibliographic fields already present (values only).
    pub fn existing_metadata(&self) -> &str {
        &self.existing_metadata
    }
    /// About the first 12 KiB of the document.
    pub fn opening(&self) -> &'a str {
        self.opening
    }
    /// About the last 2 KiB of the document when it is longer than the opening, else empty.
    pub fn ending(&self) -> &'a str {
        self.ending
    }
    /// The reminder for the in-attempt retry after ungrounded keywords, else `None`.
    pub fn reminder(&self) -> Option<&'static str> {
        self.retry.then_some(METADATA_RETRY_REMINDER)
    }
}

pub trait MetadataModel {
    fn descriptor(&self) -> TranslationModelDescriptor;
    fn suggest(
        &mut self,
        input: MetadataModelInput<'_>,
    ) -> std::result::Result<String, TranslationModelError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataJobView {
    pub item_id: String,
    pub revision_id: String,
    /// `queued`, `running`, `succeeded`, `skipped` or `failed`.
    pub state: String,
    pub reason_code: Option<String>,
    /// The model-suggestion revision written by a succeeded job, if any field was filled.
    pub result_revision_id: Option<String>,
    /// Suggested keywords dropped because they do not occur in the title or document text.
    pub dropped_keywords: u32,
}

/// What one processed job did; `result_revision_id` is a new current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataOutcome {
    pub item_id: String,
    pub revision_id: String,
    pub result_revision_id: Option<String>,
}

pub struct MetadataQueue {
    connection: Connection,
    root: PathBuf,
}

impl MetadataQueue {
    pub fn open(library: impl AsRef<Path>) -> Result<Self> {
        // The established queue owner creates and validates the shared owner-only family.
        drop(TranslationQueue::open(library.as_ref())?);
        let root = super::safe_library_root(library.as_ref())?;
        let database = root.join("analysis").join(DATABASE_NAME);
        validate_database_family(&database)?;
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             PRAGMA secure_delete=ON;
             PRAGMA trusted_schema=OFF;
             CREATE TABLE IF NOT EXISTS metadata_jobs(
               item_id TEXT NOT NULL,
               revision_id TEXT NOT NULL,
               state TEXT NOT NULL
                 CHECK(state IN ('queued','running','succeeded','skipped','failed')),
               reason_code TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               lease_until INTEGER,
               result_revision_id TEXT,
               PRIMARY KEY(item_id,revision_id)
             );",
        )?;
        // Additive: rows written before version 2 ran the version-1 prompt.
        crate::translation::add_columns(
            &connection,
            "metadata_jobs",
            "prompt_version",
            "ALTER TABLE metadata_jobs ADD COLUMN prompt_version TEXT NOT NULL DEFAULT '1';
             ALTER TABLE metadata_jobs ADD COLUMN dropped_keywords INTEGER NOT NULL DEFAULT 0;",
        )?;
        validate_database_family(&database)?;
        secure_database_family(&database)?;
        Ok(Self { connection, root })
    }

    /// Owner authorization for one revision: creates the job, or requeues a failed or skipped
    /// one, or a succeeded one from an older prompt version (an explicit re-request re-runs it;
    /// a record already enriched by the current version skips again without a model request).
    /// Succeeded jobs of the current version are final for their revision.
    pub fn request(&mut self, item_id: &str, revision_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.connection.execute(
            "INSERT INTO metadata_jobs(item_id,revision_id,state,created_at,updated_at,
                                       prompt_version)
             VALUES(?1,?2,'queued',?3,?3,?4)
             ON CONFLICT(item_id,revision_id) DO UPDATE
               SET state='queued',reason_code=NULL,lease_until=NULL,result_revision_id=NULL,
                   dropped_keywords=0,prompt_version=excluded.prompt_version,
                   updated_at=excluded.updated_at
             WHERE metadata_jobs.state IN ('failed','skipped')
                OR (metadata_jobs.state='succeeded'
                    AND metadata_jobs.prompt_version<>excluded.prompt_version)",
            params![item_id, revision_id, now, METADATA_PROMPT_VERSION],
        )?;
        Ok(())
    }

    pub fn has_work(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM metadata_jobs WHERE state='queued')",
            [],
            |row| row.get(0),
        )?)
    }

    fn finish(
        &mut self,
        item_id: &str,
        revision_id: &str,
        state: &str,
        reason: Option<&str>,
        result_revision_id: Option<&str>,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE metadata_jobs
                SET state=?3,reason_code=?4,result_revision_id=?5,lease_until=NULL,updated_at=?6
              WHERE item_id=?1 AND revision_id=?2 AND state='running'",
            params![
                item_id,
                revision_id,
                state,
                reason,
                result_revision_id,
                Utc::now().to_rfc3339()
            ],
        )?;
        if changed != 1 {
            bail!("metadata_lease_lost")
        }
        Ok(())
    }

    fn lease(&mut self) -> Result<Option<(String, String)>> {
        let now = Utc::now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A lease that expired may have dispatched a (billed) request: never re-send it.
        transaction.execute(
            "UPDATE metadata_jobs SET state='failed',reason_code='provider_outcome_unknown',
                    lease_until=NULL,updated_at=?2
              WHERE state='running' AND lease_until<?1",
            params![now.timestamp(), now.to_rfc3339()],
        )?;
        let job = transaction
            .query_row(
                "SELECT item_id,revision_id FROM metadata_jobs WHERE state='queued'
                  ORDER BY created_at,item_id LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((item_id, revision_id)) = &job {
            transaction.execute(
                "UPDATE metadata_jobs SET state='running',lease_until=?3,updated_at=?4
                  WHERE item_id=?1 AND revision_id=?2 AND state='queued'",
                params![
                    item_id,
                    revision_id,
                    now.timestamp() + LEASE_SECONDS,
                    now.to_rfc3339()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(job)
    }

    /// Processes the oldest queued job. No database lock is held across the model request.
    pub fn process_next<M: MetadataModel>(
        &mut self,
        model: &mut M,
    ) -> Result<Option<MetadataOutcome>> {
        let Some((item_id, revision_id)) = self.lease()? else {
            return Ok(None);
        };
        let outcome = |result_revision_id| MetadataOutcome {
            item_id: item_id.clone(),
            revision_id: revision_id.clone(),
            result_revision_id,
        };
        let descriptor = model.descriptor();
        if !descriptor.is_valid() {
            self.finish(
                &item_id,
                &revision_id,
                "failed",
                Some("invalid_provider_configuration"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        }
        let (_, revision) = memex_store::read_item(&self.root, &item_id)?;
        // An owner edit of metadata only (same body) since the request does not make it stale:
        // the suggestion fills the still-empty fields of the current revision.
        let target = revision.revision_id.clone();
        if target != revision_id
            && memex_store::same_body_revision(&self.root, &item_id, &target, &revision_id)?
                .is_none()
        {
            self.finish(
                &item_id,
                &revision_id,
                "failed",
                Some("stale_revision"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        }
        let metadata = &revision.metadata;
        // Model values from an older prompt version may be replaced; values from the current
        // version mean this text was already enriched (no new request on every re-request).
        let current_version = format!(":prompt-{METADATA_PROMPT_VERSION}");
        let model_origin =
            |provenance: &MetadataProvenance| provenance.origin == MetadataOrigin::ModelSuggested;
        let enriched_now = |provenance: &MetadataProvenance| {
            model_origin(provenance) && provenance.provider_version.ends_with(&current_version)
        };
        if metadata
            .keywords
            .as_ref()
            .map(|field| &field.provenance)
            .into_iter()
            .chain(metadata.creators.as_ref().map(|field| &field.provenance))
            .chain(
                metadata
                    .published_partial_date
                    .as_ref()
                    .map(|field| &field.provenance),
            )
            .chain(
                metadata
                    .published_date
                    .as_ref()
                    .map(|field| &field.provenance),
            )
            .any(enriched_now)
        {
            self.finish(
                &item_id,
                &revision_id,
                "skipped",
                Some("metadata_current"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        }
        let settled = |provenance: Option<&MetadataProvenance>| {
            provenance.is_some_and(|provenance| !model_origin(provenance))
        };
        if settled(metadata.keywords.as_ref().map(|field| &field.provenance))
            && settled(metadata.creators.as_ref().map(|field| &field.provenance))
            && (settled(
                metadata
                    .published_date
                    .as_ref()
                    .map(|field| &field.provenance),
            ) || settled(
                metadata
                    .published_partial_date
                    .as_ref()
                    .map(|field| &field.provenance),
            ))
        {
            self.finish(
                &item_id,
                &revision_id,
                "skipped",
                Some("metadata_complete"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        }
        // Generated text that already succeeded for this revision is not a reason to skip:
        // the metadata-only revision carries it over (see `carry_translation`).
        let input_sha = revision_ref(&item_id, &revision)?.revision_sha256;
        let document = revision.content_markdown.as_str();
        let opening = prefix(document, OPENING_BYTES);
        let ending = if opening.len() < document.len() {
            suffix(&document[opening.len()..], ENDING_BYTES)
        } else {
            ""
        };
        let text = format!("{}\n{}", revision.title, document);
        let grounding = format!("{}\n{}\n{}", revision.title, revision.source_url, document);
        // One request, plus one more with a reminder when the reply is malformed or keeps
        // fewer than two grounded keywords; the better valid reply is written.
        let mut best: Option<(memex_store::ModelMetadataSuggestion, u32)> = None;
        for retry in [false, true] {
            let output = model.suggest(MetadataModelInput {
                title: &revision.title,
                source_url: &revision.source_url,
                existing_metadata: existing_metadata(metadata),
                opening,
                ending,
                retry,
            });
            let output = match output {
                Ok(output) => output,
                // A failed second request keeps a valid first reply.
                Err(_) if best.is_some() => break,
                Err(error) => {
                    let (reason, _) = model_error_reason(error);
                    self.finish(&item_id, &revision_id, "failed", Some(reason), None)?;
                    return Ok(Some(outcome(None)));
                }
            };
            if let Some(parsed) = parse_suggestion(&output, &text, &grounding) {
                let kept = |value: &(memex_store::ModelMetadataSuggestion, u32)| {
                    value.0.keywords.as_ref().map_or(0, Vec::len)
                };
                if best.as_ref().is_none_or(|best| kept(&parsed) > kept(best)) {
                    best = Some(parsed);
                }
            }
            if best.as_ref().is_some_and(|best| {
                best.0.keywords.as_ref().map_or(0, Vec::len) >= MIN_GROUNDED_KEYWORDS
            }) {
                break;
            }
        }
        let Some((suggestion, dropped)) = best else {
            self.finish(
                &item_id,
                &revision_id,
                "failed",
                Some("malformed_model_output"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        };
        // Re-check the exact revision bytes after the model call; the Store re-checks under
        // its writer lock (CAS on the revision id).
        let (_, current) = memex_store::read_item(&self.root, &item_id)?;
        if current.revision_id != target
            || revision_ref(&item_id, &current)?.revision_sha256 != input_sha
        {
            self.finish(
                &item_id,
                &revision_id,
                "failed",
                Some("stale_revision"),
                None,
            )?;
            return Ok(Some(outcome(None)));
        }
        let provenance = MetadataProvenance::new(
            MetadataOrigin::ModelSuggested,
            format!("{}:{}", descriptor.adapter_id, descriptor.model_id),
            format!(
                "{}:prompt-{METADATA_PROMPT_VERSION}",
                descriptor.adapter_version
            ),
        );
        self.connection.execute(
            "UPDATE metadata_jobs SET dropped_keywords=?3 WHERE item_id=?1 AND revision_id=?2",
            params![item_id, revision_id, dropped],
        )?;
        match memex_store::apply_model_metadata_suggestion(
            &self.root,
            &item_id,
            &target,
            suggestion,
            provenance,
        ) {
            Ok(receipt) => {
                let result = receipt.map(|receipt| receipt.revision_id);
                self.finish(&item_id, &revision_id, "succeeded", None, result.as_deref())?;
                Ok(Some(outcome(result)))
            }
            Err(error) if error.to_string() == "stale_item_revision" => {
                self.finish(
                    &item_id,
                    &revision_id,
                    "failed",
                    Some("stale_revision"),
                    None,
                )?;
                Ok(Some(outcome(None)))
            }
            Err(error) => {
                self.finish(
                    &item_id,
                    &revision_id,
                    "failed",
                    Some("metadata_write_failed"),
                    None,
                )?;
                Err(error)
            }
        }
    }
}

pub fn metadata_status(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<MetadataJobView>> {
    let Some((_, connection)) = open_status_database(library.as_ref())? else {
        return Ok(None);
    };
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata_jobs'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let dropped =
        if crate::translation::has_column(&connection, "metadata_jobs", "dropped_keywords")? {
            "dropped_keywords"
        } else {
            "0"
        };
    connection
        .query_row(
            &format!(
                "SELECT state,reason_code,result_revision_id,{dropped} FROM metadata_jobs
                  WHERE item_id=?1 AND revision_id=?2"
            ),
            params![item_id, revision_id],
            |row| {
                Ok(MetadataJobView {
                    item_id: item_id.to_owned(),
                    revision_id: revision_id.to_owned(),
                    state: row.get(0)?,
                    reason_code: row.get(1)?,
                    result_revision_id: row.get(2)?,
                    dropped_keywords: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn prefix(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn suffix(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Values already settled by the owner, intake or lookup (model suggestions are omitted, so a
/// re-run is not anchored on an earlier, possibly wrong, model answer).
fn existing_metadata(metadata: &ItemMetadata) -> String {
    let mut metadata = metadata.clone();
    macro_rules! drop_model {
        ($($field:ident),*) => {$(
            if metadata.$field.as_ref().is_some_and(|field| {
                field.provenance.origin == MetadataOrigin::ModelSuggested
            }) {
                metadata.$field = None;
            }
        )*};
    }
    drop_model!(keywords, creators, published_date, published_partial_date);
    let metadata = &metadata;
    let mut value = serde_json::Map::new();
    let mut put = |key: &str, item: serde_json::Value| {
        value.insert(key.into(), item);
    };
    if let Some(creators) = &metadata.creators {
        put(
            "creators",
            creators
                .value
                .iter()
                .map(|creator| creator.display_name.clone())
                .collect::<Vec<_>>()
                .into(),
        );
    }
    for (key, field) in [
        ("published_date", &metadata.published_date),
        ("language", &metadata.language),
        ("site_name", &metadata.site_name),
        ("publisher", &metadata.publisher),
        ("journal_title", &metadata.journal_title),
    ] {
        if let Some(field) = field {
            put(key, field.value.clone().into());
        }
    }
    if let Some(keywords) = &metadata.keywords {
        put("keywords", keywords.value.clone().into());
    }
    let mut text = serde_json::Value::Object(value).to_string();
    if text.len() > MAX_EXISTING_METADATA_BYTES {
        text = "{}".into();
    }
    text
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuggestion {
    keywords: Vec<String>,
    #[serde(default)]
    creators: Vec<RawCreator>,
    #[serde(default)]
    published_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCreator {
    name: String,
    role: String,
}

fn clean_text(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    let count = value.chars().count();
    (count >= 1 && count <= max_chars && !value.chars().any(char::is_control))
        .then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Strict contract: one JSON object with exactly the documented fields; 1–8 distinct keywords
/// (the prompt asks for 3–8); creators with a known role whose name appears verbatim in the
/// document; a publication date in an exact `YYYY[-MM[-DD]]` form whose year appears in the
/// document. Any shape or bound violation rejects the whole output; keywords that do not occur
/// in `text` (title + document) and ungrounded creators/dates are dropped. Returns the
/// suggestion and the number of dropped keywords.
pub(crate) fn parse_suggestion(
    output: &str,
    text: &str,
    grounding: &str,
) -> Option<(memex_store::ModelMetadataSuggestion, u32)> {
    if output.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    let trimmed = output.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.trim_end().strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let raw: RawSuggestion = serde_json::from_str(trimmed).ok()?;
    let mut keywords: Vec<String> = Vec::new();
    for keyword in &raw.keywords {
        let keyword = clean_text(keyword, 64)?;
        if !keywords
            .iter()
            .any(|seen| seen.to_lowercase() == keyword.to_lowercase())
        {
            keywords.push(keyword);
        }
    }
    if !(1..=8).contains(&keywords.len()) || raw.creators.len() > 32 {
        return None;
    }
    // Keywords are copied from the text: each must occur in the title or document (exact
    // substring when it contains CJK, else case-insensitive with normalized whitespace).
    let normalized = normalize_for_match(text);
    let before = keywords.len();
    keywords.retain(|keyword| {
        if keyword.chars().any(is_cjk) {
            text.contains(keyword.as_str())
        } else {
            normalized.contains(&normalize_for_match(keyword))
        }
    });
    let dropped = (before - keywords.len()) as u32;
    let grounding_lower = grounding.to_lowercase();
    let mut creators: Vec<Creator> = Vec::new();
    for creator in &raw.creators {
        let name = clean_text(&creator.name, 200)?;
        let role = match creator.role.trim() {
            "author" => CreatorRole::Author,
            "editor" => CreatorRole::Editor,
            "translator" => CreatorRole::Translator,
            "contributor" => CreatorRole::Contributor,
            _ => return None,
        };
        let duplicate = creators
            .iter()
            .any(|seen| seen.display_name.to_lowercase() == name.to_lowercase());
        if grounding_lower.contains(&name.to_lowercase()) && !duplicate {
            creators.push(Creator::new(name, role));
        }
    }
    let published_date = match raw.published_date.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) => {
            let precision = match value.len() {
                4 => DatePrecision::Year,
                7 => DatePrecision::Month,
                10 => DatePrecision::Day,
                _ => return None,
            };
            let date = PartialDate {
                value: Some(value.to_owned()),
                precision,
                raw: None,
            };
            date.validate().ok()?;
            grounding.contains(&value[..4]).then_some(date)
        }
    };
    Some((
        memex_store::ModelMetadataSuggestion {
            keywords: (!keywords.is_empty()).then_some(keywords),
            creators: (!creators.is_empty()).then_some(creators),
            published_date,
        },
        dropped,
    ))
}

fn normalize_for_match(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32,
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_parser_is_strict_and_drops_ungrounded_creators_and_dates() {
        let document = "Episode 12. Host Jane Example talks with Sam Guest about AI agents, \
                        coding and the podcast. Published 2026-03-04.";
        let parse = |output: &str| parse_suggestion(output, document, document);
        let (parsed, dropped) = parse(
            r#"{"keywords":["podcast","AI  Agents","podcast","coding"],"creators":[{"name":"Jane Example","role":"contributor"},{"name":"Invented Person","role":"author"}],"published_date":"2026-03-04"}"#,
        )
        .unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(
            parsed.keywords.as_deref(),
            Some(&["podcast".to_owned(), "AI Agents".into(), "coding".into()][..])
        );
        let creators = parsed.creators.unwrap();
        assert_eq!(creators.len(), 1);
        assert_eq!(creators[0].display_name, "Jane Example");
        assert_eq!(
            parsed.published_date.unwrap().value.as_deref(),
            Some("2026-03-04")
        );
        // Ungrounded date is dropped; an unknown field, a bad role, no or too many keywords or
        // an impossible date reject the whole output.
        let (ungrounded, _) =
            parse(r#"{"keywords":["podcast"],"creators":[],"published_date":"1999"}"#).unwrap();
        assert!(ungrounded.published_date.is_none() && ungrounded.creators.is_none());
        for bad in [
            r#"{"keywords":["podcast"],"creators":[],"published_date":null,"extra":1}"#,
            r#"{"keywords":["podcast"],"creators":[{"name":"Sam Guest","role":"host"}]}"#,
            r#"{"keywords":[],"creators":[],"published_date":null}"#,
            r#"{"keywords":["a","b","c","d","e","f","g","h","i"],"creators":[]}"#,
            r#"{"keywords":["podcast"],"creators":[],"published_date":"2026-02-30"}"#,
            "keywords: a, b, c",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn keywords_must_be_copied_from_the_text() {
        let english = "Team culture and code reviews: how AI agents changed our workflow.";
        // Language drift (translated keywords) is dropped and counted.
        let (parsed, dropped) = parse_suggestion(
            r#"{"keywords":["agentes de IA","cultura de equipo","Code   Reviews","team culture"],"creators":[]}"#,
            english,
            english,
        )
        .unwrap();
        assert_eq!(dropped, 2);
        assert_eq!(
            parsed.keywords.unwrap(),
            ["Code Reviews".to_owned(), "team culture".into()]
        );
        // Nothing grounded: a valid reply with no keywords (the caller retries once).
        let (none, dropped) = parse_suggestion(
            r#"{"keywords":["revisiones de código"],"creators":[]}"#,
            english,
            english,
        )
        .unwrap();
        assert_eq!((none.keywords, dropped), (None, 1));
        // CJK: exact substring only.
        let chinese = "人工智能代理改变了团队文化与代码审查。";
        let (parsed, dropped) = parse_suggestion(
            r#"{"keywords":["团队文化","代码审查","机器学习"],"creators":[]}"#,
            chinese,
            chinese,
        )
        .unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(
            parsed.keywords.unwrap(),
            ["团队文化".to_owned(), "代码审查".into()]
        );
    }
}
