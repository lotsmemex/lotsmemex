//! Title-only translations: a Chinese display title for list views, without translating the
//! body. Rows exist only for an explicit authorization: a capture this daemon stored
//! (`request_capture`) or an owner backfill (`request_backfill`); bulk imports and the
//! background reconciler never create them.
//!
//! A row is keyed by the item and the SHA-256 of its exact source title, so metadata-only and
//! tag-only revisions keep it and a changed title never shows a stale translation. The full
//! document translation translates the title again with the document as context; once its
//! title step succeeded for the current revision, that one (`with_context`) is displayed
//! instead of the title-only one (`title_only`).

use crate::translation::{
    model_error_reason, open_status_database, secure_database_family, valid_translation_output,
    validate_database_family, TranslationModelDescriptor, TranslationModelError, TranslationQueue,
    DATABASE_NAME,
};
use crate::GeneratedTextPurpose;
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const TITLE_ONLY_PROMPT: &str = include_str!("../prompts/title-only-translation-v1.txt");
pub const TITLE_SOURCE_TITLE_ONLY: &str = "title_only";
pub const TITLE_SOURCE_WITH_CONTEXT: &str = "with_context";
/// Titles sent in one model request.
pub const TITLES_PER_REQUEST: usize = 40;
/// Longest source title (characters) that is translated at all.
const MAX_SOURCE_TITLE_CHARS: usize = 512;
const LEASE_SECONDS: i64 = 10 * 60;

/// Source titles are not `Debug`/`Serialize`; only the queue constructs this.
pub struct TitleBatchInput<'a> {
    titles: Vec<&'a str>,
}

impl<'a> TitleBatchInput<'a> {
    pub fn prompt(&self) -> &'static str {
        TITLE_ONLY_PROMPT
    }
    pub fn titles(&self) -> &[&'a str] {
        &self.titles
    }
}

pub trait TitleTranslationModel {
    fn descriptor(&self) -> TranslationModelDescriptor;
    /// One translation per title, in order (`None` for a title the model did not return).
    fn translate_titles(
        &mut self,
        input: TitleBatchInput<'_>,
    ) -> std::result::Result<Vec<Option<String>>, TranslationModelError>;
}

/// Displayable translated title of one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleTranslationView {
    pub translated_title: String,
    /// `title_only` | `with_context`
    pub title_source: String,
}

pub fn title_sha256(title: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(title.as_bytes()))
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff)
}

/// A title worth translating: it has letters, is not (largely) Chinese/CJK already and is
/// within bounds. `"iPhone 17 发布会"` is treated as Chinese.
pub fn title_needs_translation(title: &str) -> bool {
    let title = title.trim();
    let cjk = title.chars().filter(|c| is_cjk(*c)).count();
    let letters = title
        .chars()
        .filter(|c| c.is_alphabetic() && !is_cjk(*c))
        .count();
    !title.is_empty() && title.chars().count() <= MAX_SOURCE_TITLE_CHARS && cjk < 2 && letters >= 2
}

/// Rough token estimate of translating `titles` title-only: per request the system prompt and
/// contract, per title its JSON-quoted source (about 4 bytes per token) and a Chinese output
/// (about half a token per source character, plus quoting).
pub fn estimate_title_tokens<'a>(titles: impl IntoIterator<Item = &'a str>) -> (u64, u64) {
    let (mut input, mut output, mut count) = (0u64, 0u64, 0u64);
    for title in titles {
        input += (title.len() as u64).div_ceil(4) + 3;
        output += (title.chars().count() as u64).div_ceil(2) + 3;
        count += 1;
    }
    let requests = count.div_ceil(TITLES_PER_REQUEST as u64);
    // Prompt plus output contract, about 3 bytes per token.
    let overhead = (TITLE_ONLY_PROMPT.len() as u64 + 400).div_ceil(3);
    (input + requests * overhead, output + requests * 2)
}

/// The job's title from the full translation of this revision (translated with the document
/// as context), when its title step succeeded.
fn context_title(
    root: &Path,
    connection: &Connection,
    item_id: &str,
    revision_id: &str,
    title: &str,
) -> Result<Option<String>> {
    let row = connection
        .query_row(
            "SELECT state,translated_title FROM translation_jobs
              WHERE item_id=?1 AND revision_id=?2 AND target_language='zh-CN'",
            params![item_id, revision_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    match row {
        // Completion clears the queue's frozen text. The validated Analysis pair owns it.
        Some((state, _)) if state == "succeeded" => {
            return context_pair_title(root, item_id, revision_id, title)
        }
        Some((_, Some(partial))) => return Ok(Some(partial)),
        _ => {}
    }
    // A metadata-only revision inherits a same-body revision's pair; its title counts only
    // while the title is unchanged. Cheap check first: list views call this per item.
    let other: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM translation_jobs WHERE item_id=?1 AND revision_id<>?2
           AND target_language='zh-CN' AND state='succeeded')",
        params![item_id, revision_id],
        |row| row.get(0),
    )?;
    if !other {
        return Ok(None);
    }
    context_pair_title(root, item_id, revision_id, title)
}

fn context_pair_title(
    root: &Path,
    item_id: &str,
    revision_id: &str,
    title: &str,
) -> Result<Option<String>> {
    crate::effective_translation(root, item_id, revision_id)?
        .filter(|(_, source)| source.title == title)
        .map(|(pair, _)| {
            crate::read_record(root, &pair.title_analysis_id).map(|record| record.body_markdown)
        })
        .transpose()
}

fn has_table(connection: &Connection, name: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Display translation of an edited title whose body translation is inherited from a revision
/// with another title: the title itself when it needs no translation, else its succeeded
/// title-only translation (keyed by the exact title), else `None`.
pub fn retitled_translation(root: &Path, item_id: &str, title: &str) -> Result<Option<String>> {
    if !title_needs_translation(title) {
        return Ok(Some(title.trim().to_owned()));
    }
    let Some((_, connection)) = open_status_database(root)? else {
        return Ok(None);
    };
    title_only(&connection, item_id, title)
}

fn title_only(connection: &Connection, item_id: &str, title: &str) -> Result<Option<String>> {
    if !has_table(connection, "title_translations")? {
        return Ok(None);
    }
    Ok(connection
        .query_row(
            "SELECT translated_title FROM title_translations
              WHERE item_id=?1 AND title_sha256=?2 AND state='succeeded'
                AND translated_title IS NOT NULL",
            params![item_id, title_sha256(title)],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Display titles of many `(item_id, revision_id, source title)` triples with one read-only
/// connection; `with_context` wins over `title_only`.
pub fn translated_titles(
    library: impl AsRef<Path>,
    items: &[(&str, &str, &str)],
) -> Result<Vec<Option<TitleTranslationView>>> {
    let Some((_, connection)) = open_status_database(library.as_ref())? else {
        return Ok(vec![None; items.len()]);
    };
    let jobs = has_table(&connection, "translation_jobs")?;
    items
        .iter()
        .map(|(item_id, revision_id, title)| {
            if jobs {
                if let Some(text) =
                    context_title(library.as_ref(), &connection, item_id, revision_id, title)?
                {
                    return Ok(Some(TitleTranslationView {
                        translated_title: text,
                        title_source: TITLE_SOURCE_WITH_CONTEXT.into(),
                    }));
                }
            }
            Ok(
                title_only(&connection, item_id, title)?.map(|text| TitleTranslationView {
                    translated_title: text,
                    title_source: TITLE_SOURCE_TITLE_ONLY.into(),
                }),
            )
        })
        .collect()
}

pub fn translated_title(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
    title: &str,
) -> Result<Option<TitleTranslationView>> {
    Ok(
        translated_titles(library, &[(item_id, revision_id, title)])?
            .pop()
            .flatten(),
    )
}

/// Per `(item_id, revision_id, source title)`: whether a backfill should select it — a
/// translatable title with neither a context title nor a queued, running or succeeded
/// title-only row (failed rows are selectable again). One read-only connection.
pub fn title_backfill_candidates(
    library: impl AsRef<Path>,
    items: &[(&str, &str, &str)],
) -> Result<Vec<bool>> {
    let connection = open_status_database(library.as_ref())?.map(|(_, connection)| connection);
    let (jobs, titles) = match &connection {
        Some(connection) => (
            has_table(connection, "translation_jobs")?,
            has_table(connection, "title_translations")?,
        ),
        None => (false, false),
    };
    items
        .iter()
        .map(|(item_id, revision_id, title)| {
            if !title_needs_translation(title) {
                return Ok(false);
            }
            let Some(connection) = &connection else {
                return Ok(true);
            };
            if jobs
                && context_title(library.as_ref(), connection, item_id, revision_id, title)?
                    .is_some()
            {
                return Ok(false);
            }
            if !titles {
                return Ok(true);
            }
            Ok(connection
                .query_row(
                    "SELECT 1 FROM title_translations WHERE item_id=?1 AND title_sha256=?2
                        AND state IN ('queued','running','succeeded')",
                    params![item_id, title_sha256(title)],
                    |_| Ok(()),
                )
                .optional()?
                .is_none())
        })
        .collect()
}

pub struct TitleTranslationQueue {
    connection: Connection,
    root: PathBuf,
}

struct TitleJob {
    item_id: String,
    title_sha256: String,
    lease_owner: String,
}

impl TitleTranslationQueue {
    pub fn open(library: impl AsRef<Path>) -> Result<Self> {
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
             CREATE TABLE IF NOT EXISTS title_translations(
               item_id TEXT NOT NULL,
               title_sha256 TEXT NOT NULL,
               origin TEXT NOT NULL CHECK(origin IN ('capture','backfill')),
               state TEXT NOT NULL CHECK(state IN ('queued','running','succeeded','failed')),
               reason_code TEXT,
               translated_title TEXT,
               title_source TEXT NOT NULL DEFAULT 'title_only',
               model_id TEXT,
               adapter_version TEXT,
               lease_until INTEGER,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(item_id,title_sha256)
             );",
        )?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        if !crate::translation::has_column(&connection, "title_translations", "lease_owner")? {
            connection
                .execute_batch("ALTER TABLE title_translations ADD COLUMN lease_owner TEXT")?;
        }
        connection.execute_batch("COMMIT")?;
        validate_database_family(&database)?;
        secure_database_family(&database)?;
        Ok(Self { connection, root })
    }

    /// A capture this daemon just stored: queue its title when it is translatable and the
    /// revision has no context title yet. Returns whether a row is (now) queued.
    pub fn request_capture(&mut self, item_id: &str, revision_id: &str) -> Result<bool> {
        let Some((_, revision)) = memex_store::read_item_at_sensitivity(
            &self.root,
            item_id,
            memex_core::Sensitivity::Restricted,
        )?
        else {
            return Ok(false);
        };
        if revision.revision_id != revision_id || !title_needs_translation(&revision.title) {
            return Ok(false);
        }
        if context_title(
            &self.root,
            &self.connection,
            item_id,
            revision_id,
            &revision.title,
        )?
        .is_some()
        {
            return Ok(false);
        }
        self.insert(item_id, &revision.title, "capture")
    }

    /// After an owner edit of the title: the edited title gets its own title-only translation
    /// when the item's translation was already authorized (a document translation the current
    /// revision inherits, or a title-only translation of the previous title). The body
    /// translation is kept; only the title is translated again. Returns whether a row is
    /// (now) queued.
    pub fn request_retitle(&mut self, item_id: &str, previous_title: &str) -> Result<bool> {
        let Some((_, revision)) = memex_store::read_item_at_sensitivity(
            &self.root,
            item_id,
            memex_core::Sensitivity::Restricted,
        )?
        else {
            return Ok(false);
        };
        if revision.title == previous_title || !title_needs_translation(&revision.title) {
            return Ok(false);
        }
        let translated_before = crate::effective_translation(&self.root, item_id, &revision.revision_id)?
            .is_some()
            || self
                .connection
                .query_row(
                    "SELECT 1 FROM title_translations WHERE item_id=?1 AND title_sha256=?2
                        AND state='succeeded'",
                    params![item_id, title_sha256(previous_title)],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        if !translated_before {
            return Ok(false);
        }
        self.insert(item_id, &revision.title, "capture")
    }

    /// Owner backfill of one already selected item (see `title_backfill_candidate`).
    pub fn request_backfill(&mut self, item_id: &str, title: &str) -> Result<bool> {
        if !title_needs_translation(title) {
            return Ok(false);
        }
        self.insert(item_id, title, "backfill")
    }

    /// Capture replay is one-shot, including an uncertain provider outcome. Only an
    /// explicit owner backfill can retry a failed row.
    fn insert(&mut self, item_id: &str, title: &str, origin: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        Ok(self.connection.execute(
            "INSERT INTO title_translations(item_id,title_sha256,origin,state,created_at,updated_at)
             VALUES(?1,?2,?3,'queued',?4,?4)
             ON CONFLICT(item_id,title_sha256) DO UPDATE
               SET state='queued',origin=excluded.origin,reason_code=NULL,translated_title=NULL,
                   model_id=NULL,adapter_version=NULL,lease_until=NULL,lease_owner=NULL,
                   updated_at=excluded.updated_at
             WHERE title_translations.state='failed' AND excluded.origin='backfill'",
            params![item_id, title_sha256(title), origin, now],
        )? == 1)
    }

    /// A lease that expired may have dispatched (billed) a request: never re-sent automatically.
    fn expire(&mut self) -> Result<()> {
        let now = Utc::now();
        self.connection.execute(
            "UPDATE title_translations SET state='failed',reason_code='provider_outcome_unknown',
                    lease_until=NULL,lease_owner=NULL,updated_at=?2
              WHERE state='running' AND lease_until<?1",
            params![now.timestamp(), now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn has_work(&mut self) -> Result<bool> {
        self.expire()?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM title_translations WHERE state='queued')",
            [],
            |row| row.get(0),
        )?)
    }

    /// Leases up to `TITLES_PER_REQUEST` queued rows (captures first).
    fn lease(&mut self) -> Result<Vec<TitleJob>> {
        let now = Utc::now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let jobs = {
            let mut statement = transaction.prepare(
                "SELECT item_id,title_sha256 FROM title_translations WHERE state='queued'
                  ORDER BY origin='capture' DESC,created_at,item_id LIMIT ?1",
            )?;
            let jobs = statement
                .query_map([TITLES_PER_REQUEST as i64], |row| {
                    Ok(TitleJob {
                        item_id: row.get(0)?,
                        title_sha256: row.get(1)?,
                        lease_owner: memex_core::new_id("title_lease"),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            jobs
        };
        for job in &jobs {
            transaction.execute(
                "UPDATE title_translations SET state='running',lease_until=?3,updated_at=?4,lease_owner=?5
                  WHERE item_id=?1 AND title_sha256=?2 AND state='queued'",
                params![
                    job.item_id,
                    job.title_sha256,
                    now.timestamp() + LEASE_SECONDS,
                    now.to_rfc3339(),
                    job.lease_owner
                ],
            )?;
        }
        transaction.commit()?;
        Ok(jobs)
    }

    fn finish(
        &mut self,
        job: &TitleJob,
        reason: Option<&str>,
        output: Option<(&str, &TranslationModelDescriptor)>,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE title_translations
                SET state=?3,reason_code=?4,translated_title=?5,model_id=?6,adapter_version=?7,
                    title_source='title_only',lease_until=NULL,lease_owner=NULL,updated_at=?8
              WHERE item_id=?1 AND title_sha256=?2 AND state='running' AND lease_owner=?9",
            params![
                job.item_id,
                job.title_sha256,
                if output.is_some() {
                    "succeeded"
                } else {
                    "failed"
                },
                reason,
                output.map(|(text, _)| text),
                output.map(|(_, model)| model.model_id.as_str()),
                output.map(|(_, model)| model.adapter_version.as_str()),
                Utc::now().to_rfc3339(),
                job.lease_owner
            ],
        )?;
        if changed != 1 {
            bail!("title_translation_lease_lost")
        }
        Ok(())
    }

    /// One model request for up to `TITLES_PER_REQUEST` queued titles. Every output is
    /// validated like the document translation's title step; an invalid one fails its row only.
    /// No database lock is held across the model call.
    pub fn process_next_batch<M: TitleTranslationModel>(&mut self, model: &mut M) -> Result<bool> {
        self.expire()?;
        let jobs = self.lease()?;
        if jobs.is_empty() {
            return Ok(false);
        }
        let descriptor = model.descriptor();
        let mut current = Vec::new();
        for job in jobs {
            if !descriptor.is_valid() {
                self.finish(&job, Some("invalid_provider_configuration"), None)?;
                continue;
            }
            let title = memex_store::read_item_at_sensitivity(
                &self.root,
                &job.item_id,
                memex_core::Sensitivity::Restricted,
            )
            .ok()
            .flatten()
            .map(|(_, revision)| revision.title)
            .filter(|title| title_sha256(title) == job.title_sha256);
            match title {
                Some(title) => current.push((job, title)),
                None => self.finish(&job, Some("stale_revision"), None)?,
            }
        }
        if current.is_empty() {
            return Ok(true);
        }
        let output = model.translate_titles(TitleBatchInput {
            titles: current.iter().map(|(_, title)| title.as_str()).collect(),
        });
        match output {
            Ok(outputs) if outputs.len() == current.len() => {
                for ((job, title), output) in current.iter().zip(outputs) {
                    let text = output.map(|text| text.trim().to_owned()).filter(|text| {
                        valid_translation_output(
                            text,
                            title,
                            GeneratedTextPurpose::TitleTranslation,
                        )
                    });
                    match text {
                        Some(text) => self.finish(job, None, Some((&text, &descriptor)))?,
                        None => self.finish(job, Some("invalid_title_translation"), None)?,
                    }
                }
            }
            Ok(_) => {
                for (job, _) in &current {
                    self.finish(job, Some("malformed_model_output"), None)?;
                }
            }
            Err(error) => {
                let (reason, _) = model_error_reason(error);
                for (job, _) in &current {
                    self.finish(job, Some(reason), None)?;
                }
            }
        }
        Ok(true)
    }
}
