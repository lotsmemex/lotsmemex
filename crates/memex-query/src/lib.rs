//! Derived, in-memory query index, independent of Store, transports and models.
//!
//! Callers supply verified snapshots and ordered changes. An index generation
//! is NOT proof of canonical freshness or authorization. Do not expose this
//! module to remote callers without a Store synchronization and authorization
//! adapter. No canonical data or persistent index files are written here.
//!
//! Views created with [`QueryIndex::new_shared`] share ONE in-memory database:
//! each view keeps its own documents, groups, generation and view tokens, while
//! the contentless full-text postings are stored once per distinct (title, body)
//! and reference-counted. A view only ever matches through its own documents.

use anyhow::{bail, Result};
use memex_core::{new_id, ItemOrigin, Sensitivity};
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fmt::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

pub const MAX_WINDOW_SIZE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSummary {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    /// Stable source identity supplied by the Store adapter, not a UI label.
    pub source: String,
    pub item_type: String,
    pub captured_at: Option<String>,
    /// Actual publication day (YYYY-MM-DD), never the effective/capture date.
    pub published_at: Option<String>,
    pub duration_seconds: Option<u32>,
    /// Canonical MemeX entry time, independent of the source publication/capture time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ItemOrigin>,
    #[serde(default, flatten)]
    pub dates: memex_core::bibliographic::EffectiveDates,
    pub sensitivity: Sensitivity,
    /// Authorized direct group/collection memberships (groups_v1). Omitted
    /// when empty so libraries without groups keep the old item shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_ids: Vec<String>,
    /// Additive: Chinese display title (`title` stays the original). Filled by the agent from
    /// analysis after the query; never indexed, searched or sorted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_title: Option<String>,
    /// With `translated_title`: `title_only` | `with_context`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
}

#[derive(Clone)]
pub struct IndexedDocument {
    pub summary: ItemSummary,
    pub content_markdown: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    Title,
    Source,
    CapturedAt,
    AddedAt,
    PublishedAt,
    ItemType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    /// Literal, Unicode-lowercased substring within either title or Markdown.
    /// Empty means browse. No stemming, transliteration or query operators.
    pub text: String,
    pub source: Option<String>,
    pub item_type: Option<String>,
    /// Creation receipt classification. None includes unknown legacy adapters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ItemOrigin>,
    pub sort: SortField,
    pub descending: bool,
    /// groups_v1: exact root group or collection. Omitted = no group filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// With `group_id`, select the distinct union of the node and its
    /// authorized descendants before every other filter, sort, page and facet.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_descendants: bool,
    /// Inbox: no effective membership in this authorized view.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unfiled: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn origin_value(origin: Option<ItemOrigin>) -> Option<&'static str> {
    origin.map(|value| match value {
        ItemOrigin::Capture => "capture",
        ItemOrigin::Import => "import",
    })
}

/// Authorized group node supplied by the Store adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRef {
    pub group_id: String,
    pub name: String,
    pub kind: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupFacet {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Distinct direct members within the full filtered result.
    pub count: u64,
    /// Distinct union over the node's authorized subtree within that same
    /// result; never a sum of child counts.
    pub subtree_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facet {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryPage {
    pub schema_version: u32,
    pub generation: String,
    /// Binds generation, query, ordering AND the effective ceiling.
    pub view: String,
    pub total: u64,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub items: Vec<ItemSummary>,
    /// Counts over the entire authorized, filtered result, not just this page.
    pub sources: Vec<Facet>,
    pub item_types: Vec<Facet>,
    /// Groups with at least one filtered result in their subtree. Omitted when
    /// empty so libraries without groups keep the old page shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<GroupFacet>,
}

struct Shared {
    db: Mutex<Connection>,
    next_view: AtomicUsize,
}

pub struct QueryIndex {
    shared: Arc<Shared>,
    /// Table prefix of this view (`v<n>_`).
    p: String,
    generation: String,
    valid: bool,
}

impl QueryIndex {
    pub fn new() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        // Postings exist once per distinct (title, body); contentless, so no
        // copy of the encoded text is kept. Positions stay for phrase matches.
        db.execute_batch(
            "PRAGMA temp_store=MEMORY;
             CREATE TABLE bodies (id INTEGER PRIMARY KEY, key BLOB NOT NULL UNIQUE, refs INTEGER NOT NULL);
             CREATE VIRTUAL TABLE search USING fts5(title, body, content='', contentless_delete=1);",
        )?;
        Self::view(Arc::new(Shared { db: Mutex::new(db), next_view: AtomicUsize::new(0) }))
    }

    /// Another view over the same postings: its own documents, groups and
    /// generation, so a change in one view never rotates another's tokens.
    pub fn new_shared(&self) -> Result<Self> {
        Self::view(self.shared.clone())
    }

    fn view(shared: Arc<Shared>) -> Result<Self> {
        let p = format!("v{}_", shared.next_view.fetch_add(1, Ordering::Relaxed));
        shared.db.lock().map_err(|_| anyhow::anyhow!("query_index_unavailable"))?.execute_batch(&format!(
            "CREATE TABLE {p}documents (
               rowid INTEGER PRIMARY KEY, item_id TEXT NOT NULL UNIQUE,
               title_sort TEXT NOT NULL, source TEXT NOT NULL, item_type TEXT NOT NULL,
               captured_at INTEGER, captured_nanos INTEGER,
               added_at INTEGER, added_nanos INTEGER,
               effective_date TEXT, effective_capture_at INTEGER, effective_capture_nanos INTEGER,
               origin TEXT,
               sensitivity INTEGER NOT NULL, summary TEXT NOT NULL,
               body_id INTEGER NOT NULL
             );
             CREATE INDEX {p}by_title ON {p}documents(title_sort, item_id);
             CREATE INDEX {p}by_source ON {p}documents(source, item_id);
             CREATE INDEX {p}by_type ON {p}documents(item_type, item_id);
             CREATE INDEX {p}by_capture ON {p}documents(captured_at, captured_nanos, item_id);
             CREATE INDEX {p}by_added ON {p}documents(added_at, added_nanos, item_id);
             CREATE INDEX {p}by_effective_date ON {p}documents(effective_date, effective_capture_at, effective_capture_nanos, item_id);
             CREATE INDEX {p}by_body ON {p}documents(body_id);
             CREATE TABLE {p}groups (group_id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
             CREATE TABLE {p}memberships (group_id TEXT NOT NULL, item_id TEXT NOT NULL,
               PRIMARY KEY(group_id, item_id));
             CREATE INDEX {p}membership_items ON {p}memberships(item_id);
             CREATE TABLE {p}group_closure (ancestor TEXT NOT NULL, descendant TEXT NOT NULL,
               PRIMARY KEY(ancestor, descendant));"
        ))?;
        Ok(Self {
            shared,
            p,
            generation: new_id("query"),
            valid: false,
        })
    }

    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.shared.db.lock().map_err(|_| anyhow::anyhow!("query_index_unavailable"))
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// Fail closed after a missed change or an unverified source state.
    /// Only a full replacement can make this index queryable again.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Install a complete verified snapshot atomically. Failed input leaves the
    /// old snapshot intact (and does not clear a previous invalidation).
    pub fn replace(&mut self, documents: &[IndexedDocument]) -> Result<()> {
        self.stream(true, None, true, |writer| documents.iter().try_for_each(|d| writer.upsert(d)))
    }

    /// Like [`replace`](Self::replace), but documents are fed one at a time, so a
    /// caller never needs the whole corpus in memory. Nothing is visible until the
    /// feed returns Ok; an error rolls back every write of this call.
    pub fn replace_streaming(&mut self, feed: impl FnOnce(&mut IndexWriter<'_>) -> Result<()>) -> Result<()> {
        self.stream(true, None, true, feed)
    }

    /// Like [`apply`](Self::apply), fed incrementally in one transaction. The
    /// generation rotates only when the feed wrote something, so an update that
    /// turns out empty (for example a hidden-only change) keeps view tokens.
    pub fn apply_streaming(
        &mut self,
        expected_generation: &str,
        feed: impl FnOnce(&mut IndexWriter<'_>) -> Result<()>,
    ) -> Result<()> {
        self.stream(false, Some(expected_generation), false, feed)
    }

    fn stream(
        &mut self,
        full: bool,
        expected_generation: Option<&str>,
        always_rotate: bool,
        feed: impl FnOnce(&mut IndexWriter<'_>) -> Result<()>,
    ) -> Result<()> {
        if !full {
            if !self.valid {
                bail!("query_index_unavailable");
            }
            if expected_generation != Some(self.generation.as_str()) {
                bail!("stale_query_generation");
            }
        }
        let p = self.p.clone();
        let changes = {
            let mut db = self.db()?;
            let tx = db.transaction()?;
            if full {
                let ids: Vec<String> = tx
                    .prepare(&format!("SELECT item_id FROM {p}documents"))?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in &ids {
                    remove(&tx, &p, id)?;
                }
            }
            let mut writer = IndexWriter { tx, p, seen: HashSet::new(), changes: 0 };
            feed(&mut writer)?;
            let changes = writer.changes;
            writer.tx.commit()?;
            changes
        };
        if full || always_rotate || changes > 0 {
            self.generation = new_id("query");
        }
        if full {
            self.valid = true;
        }
        Ok(())
    }

    /// Replace the authorized group projection (nodes whose parents are all
    /// present, memberships of visible items). Rotates the generation so any
    /// earlier view fails `stale_query_view` instead of mixing hierarchies.
    pub fn replace_groups(
        &mut self,
        groups: &[GroupRef],
        memberships: &[(String, String)],
    ) -> Result<()> {
        let parents: std::collections::HashMap<&str, Option<&str>> = groups
            .iter()
            .map(|group| (group.group_id.as_str(), group.parent_id.as_deref()))
            .collect();
        if parents.len() != groups.len() {
            bail!("duplicate_group_id");
        }
        let p = self.p.clone();
        let mut db = self.db()?;
        let tx = db.transaction()?;
        tx.execute_batch(&format!("DELETE FROM {p}groups; DELETE FROM {p}memberships; DELETE FROM {p}group_closure;"))?;
        for group in groups {
            tx.execute(
                &format!("INSERT INTO {p}groups(group_id,name,kind) VALUES (?1,?2,?3)"),
                params![group.group_id, group.name, group.kind],
            )?;
            let mut current = Some(group.group_id.as_str());
            let mut depth = 0;
            while let Some(ancestor) = current {
                depth += 1;
                if depth > groups.len() {
                    bail!("group_cycle");
                }
                tx.execute(
                    &format!("INSERT INTO {p}group_closure(ancestor,descendant) VALUES (?1,?2)"),
                    params![ancestor, group.group_id],
                )?;
                current = match parents.get(ancestor) {
                    Some(parent) => *parent,
                    None => bail!("group_parent_missing"),
                };
            }
        }
        for (group_id, item_id) in memberships {
            if !parents.contains_key(group_id.as_str()) {
                bail!("group_parent_missing");
            }
            tx.execute(
                &format!("INSERT OR IGNORE INTO {p}memberships(group_id,item_id) VALUES (?1,?2)"),
                params![group_id, item_id],
            )?;
        }
        tx.commit()?;
        drop(db);
        self.generation = new_id("query");
        Ok(())
    }

    /// Atomic upsert/delete batch. A stale producer cannot overwrite a newer
    /// generation. Callers must still order Store changes and detect gaps.
    pub fn apply(
        &mut self,
        expected_generation: &str,
        upserts: &[IndexedDocument],
        deleted_ids: &[String],
    ) -> Result<()> {
        self.stream(false, Some(expected_generation), true, |writer| {
            deleted_ids.iter().try_for_each(|id| writer.remove(id))?;
            upserts.iter().try_for_each(|d| writer.upsert(d))
        })
    }

    /// Random-access window within one exact view. Adapters must derive ceiling
    /// from authenticated authority, never trust a remote client's ceiling.
    /// Offset > 0 requires a view from an earlier page. Null dates sort last in
    /// both directions; all ties use item_id ascending. Facets include all filters.
    pub fn query(
        &self,
        query: &Query,
        ceiling: Sensitivity,
        offset: usize,
        limit: usize,
        expected_view: Option<&str>,
    ) -> Result<QueryPage> {
        if !self.valid {
            bail!("query_index_unavailable");
        }
        if limit == 0
            || limit > MAX_WINDOW_SIZE
            || offset > i64::MAX as usize
            || query.text.len() > 1024
            || query.source.as_ref().is_some_and(|s| s.len() > 4096)
            || query.item_type.as_ref().is_some_and(|s| s.len() > 256)
            || (offset > 0 && expected_view.is_none())
        {
            bail!("invalid_query_window");
        }
        if query.include_descendants && query.group_id.is_none()
            || query.group_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 256)
            || query.unfiled && (query.group_id.is_some() || query.include_descendants)
        {
            bail!("invalid_group_query");
        }
        let view = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&(&self.generation, query, ceiling))?)
        );
        if expected_view.is_some_and(|expected| expected != view) {
            bail!("stale_query_view");
        }
        // FTS sees only generated ASCII hex tokens inside a quoted phrase, not
        // user syntax. A one-scalar phrase still uses its inverted index.
        let phrase = format!("\"{}\"", terms(&query.text));
        let matching = if query.text.is_empty() {
            ""
        } else {
            "AND body_id IN (SELECT rowid FROM search WHERE search MATCH ?4)"
        };
        let predicate = format!(
            "sensitivity <= ?1 AND (?2 IS NULL OR source=?2)
             AND (?3 IS NULL OR item_type=?3) {matching}"
        );
        // Use ?4 for browse too, so every prepared statement has the same arity.
        let predicate = format!("({predicate}) AND length(?4)>=0");
        let p = &self.p;
        let db = self.db()?;
        if let Some(group_id) = &query.group_id {
            let known: bool = db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {p}groups WHERE group_id=?1)"),
                [group_id],
                |row| row.get(0),
            )?;
            if !known {
                // Hidden and nonexistent groups are indistinguishable.
                bail!("library_resource_not_found");
            }
        }
        // Group selection is part of the base predicate, so it precedes every
        // other filter, sort, page, total and facet.
        let predicate = format!(
            "{predicate} AND (?5 IS NULL OR item_id IN (SELECT item_id FROM {p}memberships
               WHERE group_id=?5 OR (?6=1 AND group_id IN
                 (SELECT descendant FROM {p}group_closure WHERE ancestor=?5))))
               AND (?7 IS NULL OR origin=?7)
               AND (?8=0 OR NOT EXISTS (SELECT 1 FROM {p}memberships m
                 WHERE m.item_id={p}documents.item_id))"
        );
        let include = i64::from(query.include_descendants);
        let values = params![
            rank(ceiling),
            query.source,
            query.item_type,
            phrase,
            query.group_id,
            include,
            origin_value(query.origin),
            i64::from(query.unfiled)
        ];
        let total: u64 = db.query_row(
            &format!("SELECT count(*) FROM {p}documents WHERE {predicate}"),
            values,
            |row| row.get(0),
        )?;
        let facets = |column: &str| -> Result<Vec<Facet>> {
            let mut statement = db.prepare(&format!(
                "SELECT {column}, count(*) FROM {p}documents WHERE {predicate}
                 GROUP BY {column} ORDER BY {column}"
            ))?;
            let rows = statement.query_map(values, |row| {
                Ok(Facet {
                    value: row.get(0)?,
                    count: row.get(1)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        };
        let sources = facets("source")?;
        let item_types = facets("item_type")?;
        let groups = {
            let mut statement = db.prepare(&format!(
                // The matching set is computed once; per-group subqueries over
                // the whole predicate cost O(groups × documents) per query.
                "WITH matched AS MATERIALIZED (SELECT item_id FROM {p}documents WHERE {predicate}),
                   hits AS MATERIALIZED (SELECT m.group_id, m.item_id FROM {p}memberships m
                      JOIN matched d ON d.item_id = m.item_id),
                   direct AS (SELECT group_id, count(*) AS n FROM hits GROUP BY group_id),
                   subtree AS (SELECT c.ancestor AS group_id, count(DISTINCT h.item_id) AS n
                      FROM {p}group_closure c JOIN hits h ON h.group_id = c.descendant
                      GROUP BY c.ancestor)
                 SELECT g.group_id, g.name, g.kind, coalesce(direct.n, 0), coalesce(subtree.n, 0)
                 FROM {p}groups g
                 LEFT JOIN direct ON direct.group_id = g.group_id
                 LEFT JOIN subtree ON subtree.group_id = g.group_id
                 ORDER BY g.name, g.group_id"
            ))?;
            let rows = statement.query_map(values, |row| {
                Ok(GroupFacet {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    count: row.get(3)?,
                    subtree_count: row.get(4)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|facet| facet.subtree_count > 0)
                .collect::<Vec<_>>()
        };
        let column = match query.sort {
            SortField::Title => "title_sort",
            SortField::Source => "source",
            SortField::CapturedAt => "captured_at",
            SortField::AddedAt => "added_at",
            SortField::PublishedAt => "effective_date",
            SortField::ItemType => "item_type",
        };
        let direction = if query.descending { "DESC" } else { "ASC" };
        let subsecond_order = match query.sort {
            SortField::CapturedAt => format!("captured_nanos {direction},"),
            SortField::AddedAt => format!("added_nanos {direction},"),
            SortField::PublishedAt => "effective_capture_at IS NULL, effective_capture_at DESC, effective_capture_nanos DESC,".into(),
            _ => String::new(),
        };
        let mut statement = db.prepare(&format!(
            "SELECT summary FROM {p}documents WHERE {predicate}
             ORDER BY {column} IS NULL, {column} {direction}, {subsecond_order} item_id ASC LIMIT ?9 OFFSET ?10"
        ))?;
        let rows = statement.query_map(
            params![
                rank(ceiling),
                query.source,
                query.item_type,
                phrase,
                query.group_id,
                include,
                origin_value(query.origin),
                i64::from(query.unfiled),
                limit as i64,
                offset as i64
            ],
            |row| row.get::<_, String>(0),
        )?;
        let items = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<Vec<ItemSummary>>>()?;
        let next = offset + items.len();
        Ok(QueryPage {
            schema_version: 1,
            generation: self.generation.clone(),
            view,
            total,
            offset,
            next_offset: (next < total as usize).then_some(next),
            items,
            sources,
            item_types,
            groups,
        })
    }
}

/// One open write of a view (see [`QueryIndex::replace_streaming`]). Each item id
/// may appear once per write, as an upsert or a removal.
pub struct IndexWriter<'a> {
    tx: Transaction<'a>,
    p: String,
    seen: HashSet<String>,
    changes: usize,
}

impl IndexWriter<'_> {
    pub fn upsert(&mut self, document: &IndexedDocument) -> Result<()> {
        if !self.seen.insert(document.summary.item_id.clone()) {
            bail!("duplicate_item_id");
        }
        remove(&self.tx, &self.p, &document.summary.item_id)?;
        insert(&self.tx, &self.p, document)?;
        self.changes += 1;
        Ok(())
    }

    pub fn remove(&mut self, item_id: &str) -> Result<()> {
        if !self.seen.insert(item_id.to_owned()) {
            bail!("duplicate_item_id");
        }
        remove(&self.tx, &self.p, item_id)?;
        self.changes += 1;
        Ok(())
    }
}

impl Drop for QueryIndex {
    /// Releases this view's postings and tables; other views keep theirs.
    fn drop(&mut self) {
        let p = self.p.clone();
        let Ok(mut db) = self.shared.db.lock() else { return };
        let _ = (|| -> Result<()> {
            let tx = db.transaction()?;
            let ids: Vec<String> = tx
                .prepare(&format!("SELECT item_id FROM {p}documents"))?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in &ids {
                remove(&tx, &p, id)?;
            }
            tx.execute_batch(&format!(
                "DROP TABLE {p}documents; DROP TABLE {p}groups; DROP TABLE {p}memberships; DROP TABLE {p}group_closure;"
            ))?;
            tx.commit()?;
            Ok(())
        })();
    }
}

fn rank(sensitivity: Sensitivity) -> i64 {
    match sensitivity {
        Sensitivity::Public => 0,
        Sensitivity::Internal => 1,
        Sensitivity::Private => 2,
        Sensitivity::Restricted => 3,
    }
}

fn terms(text: &str) -> String {
    let mut encoded = String::new();
    for c in text.to_lowercase().chars() {
        write!(encoded, "u{:x} ", c as u32).expect("writing to String cannot fail");
    }
    encoded
}

/// Drop one item from view `p`, releasing its postings when no view uses them.
fn remove(tx: &Transaction<'_>, p: &str, item_id: &str) -> Result<()> {
    let body: Option<i64> = tx
        .query_row(&format!("SELECT body_id FROM {p}documents WHERE item_id=?1"), [item_id], |row| row.get(0))
        .map(Some)
        .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })?;
    let Some(body) = body else { return Ok(()) };
    tx.execute(&format!("DELETE FROM {p}documents WHERE item_id=?1"), [item_id])?;
    tx.execute("UPDATE bodies SET refs=refs-1 WHERE id=?1", [body])?;
    let refs: i64 = tx.query_row("SELECT refs FROM bodies WHERE id=?1", [body], |row| row.get(0))?;
    if refs <= 0 {
        tx.execute("DELETE FROM search WHERE rowid=?1", [body])?;
        tx.execute("DELETE FROM bodies WHERE id=?1", [body])?;
    }
    Ok(())
}

fn insert(tx: &Transaction<'_>, p: &str, document: &IndexedDocument) -> Result<()> {
    let summary = &document.summary;
    if summary.item_id.is_empty()
        || summary.item_id.len() > 256
        || summary.revision_id.is_empty()
        || summary.revision_id.len() > 256
        || summary.title.len() > 4096
        || summary.source.len() > 4096
        || summary.item_type.is_empty()
        || summary.item_type.len() > 256
    {
        bail!("invalid_index_document");
    }
    let timestamp = summary
        .captured_at
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| anyhow::anyhow!("invalid_capture_timestamp"))?;
    let added = summary
        .added_at
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| anyhow::anyhow!("invalid_added_timestamp"))?;
    let effective_capture = summary.dates.effective_capture_at.as_deref()
        .map(chrono::DateTime::parse_from_rfc3339).transpose()
        .map_err(|_| anyhow::anyhow!("invalid_effective_capture_timestamp"))?;
    let effective_date = summary.dates.effective_date.as_ref();
    if let Some(date) = effective_date {
        date.validate().map_err(|_| anyhow::anyhow!("invalid_effective_date"))?;
        if date.value.is_none() {
            bail!("invalid_effective_date");
        }
    }
    if effective_date.is_some() == (summary.dates.effective_date_basis == memex_core::bibliographic::EffectiveDateBasis::Unknown)
        || effective_capture.is_some() != summary.dates.effective_capture_origin.is_some()
    {
        bail!("invalid_effective_date_basis");
    }
    // Identical (title, body) share postings across items and views.
    let mut key = Sha256::new();
    key.update((summary.title.len() as u64).to_le_bytes());
    key.update(summary.title.as_bytes());
    key.update(document.content_markdown.as_bytes());
    let key: [u8; 32] = key.finalize().into();
    let existing: Option<i64> = tx
        .query_row("SELECT id FROM bodies WHERE key=?1", [&key[..]], |row| row.get(0))
        .map(Some)
        .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })?;
    let body = match existing {
        Some(id) => {
            tx.execute("UPDATE bodies SET refs=refs+1 WHERE id=?1", [id])?;
            id
        }
        None => {
            tx.execute("INSERT INTO bodies(key,refs) VALUES (?1,1)", [&key[..]])?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO search(rowid,title,body) VALUES (?1,?2,?3)",
                params![id, terms(&summary.title), terms(&document.content_markdown)],
            )?;
            id
        }
    };
    tx.execute(
        &format!("INSERT INTO {p}documents(item_id,title_sort,source,item_type,captured_at,captured_nanos,sensitivity,summary,added_at,added_nanos,origin,body_id,effective_date,effective_capture_at,effective_capture_nanos)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"),
        params![summary.item_id, summary.title.to_lowercase(), summary.source,
            summary.item_type, timestamp.map(|t| t.timestamp()),
            timestamp.map(|t| t.timestamp_subsec_nanos()), rank(summary.sensitivity), serde_json::to_string(summary)?,
            added.map(|t| t.timestamp()), added.map(|t| t.timestamp_subsec_nanos()), origin_value(summary.origin), body,
            effective_date.and_then(|date| date.value.as_deref()),
            effective_capture.map(|t| t.timestamp()), effective_capture.map(|t| t.timestamp_subsec_nanos())],
    )?;
    Ok(())
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    fn doc(id: &str, title: &str, body: &str, sensitivity: Sensitivity) -> IndexedDocument {
        IndexedDocument {
            summary: ItemSummary {
                item_id: id.into(), revision_id: format!("revision_{id}"), title: title.into(), source: "s".into(),
                item_type: "article".into(), captured_at: None, added_at: None, origin: None, sensitivity,
                published_at: None, duration_seconds: None,
                dates: Default::default(),
                group_ids: Vec::new(), translated_title: None, title_source: None,
            },
            content_markdown: body.into(),
        }
    }
    fn q(text: &str) -> Query {
        Query { text: text.into(), source: None, item_type: None, origin: None, sort: SortField::Title,
            descending: false, group_id: None, include_descendants: false, unfiled: false }
    }
    fn hits(index: &QueryIndex, text: &str, ceiling: Sensitivity) -> Vec<String> {
        index.query(&q(text), ceiling, 0, 50, None).unwrap().items.into_iter().map(|i| i.item_id).collect()
    }
    fn postings(index: &QueryIndex) -> (i64, i64) {
        let db = index.db().unwrap();
        (db.query_row("SELECT count(*) FROM bodies", [], |r| r.get(0)).unwrap(),
         db.query_row("SELECT coalesce(sum(refs),0) FROM bodies", [], |r| r.get(0)).unwrap())
    }

    #[test]
    fn shared_postings_stay_per_view_and_contentless_deletes_unmatch() {
        use Sensitivity::*;
        let mut high = QueryIndex::new().unwrap();
        let mut low = high.new_shared().unwrap();
        {
            let db = high.db().unwrap();
            assert_eq!(db.query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
            let files: Vec<String> = db.prepare("PRAGMA database_list").unwrap()
                .query_map([], |r| r.get(2)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            assert!(files.iter().all(String::is_empty), "no backing database files");
        }
        let public = doc("a", "Shared", "落花流水 river", Public);
        let secret = doc("b", "Hidden", "秘密 river", Restricted);
        high.replace(&[public.clone(), secret.clone()]).unwrap();
        low.replace(&[public.clone()]).unwrap();
        // Identical (title, body) is stored once and referenced by both views.
        assert_eq!(postings(&high), (2, 3));
        // A view only matches through its own documents, even for shared terms.
        assert_eq!(hits(&low, "river", Restricted), vec!["a"]);
        assert_eq!(hits(&low, "秘密", Restricted), Vec::<String>::new());
        assert_eq!(hits(&high, "river", Restricted), vec!["b", "a"]);
        assert_eq!(hits(&high, "river", Public), vec!["a"]);
        // Literal substring across scripts, including a single scalar.
        assert_eq!(hits(&high, "花流", Restricted), vec!["a"]);
        assert_eq!(hits(&high, "密", Restricted), vec!["b"]);
        // A hidden-only change rotates only the view that sees it.
        let before = low.generation().to_owned();
        let changed = doc("b", "Hidden", "改写 ocean", Restricted);
        let g = high.generation().to_owned();
        high.apply(&g, &[changed], &[]).unwrap();
        assert_eq!(low.generation(), before);
        assert_ne!(high.generation(), g);
        // Old terms of a replaced or deleted body no longer match anywhere.
        assert_eq!(hits(&high, "秘密", Restricted), Vec::<String>::new());
        assert_eq!(hits(&high, "ocean", Restricted), vec!["b"]);
        assert_eq!(postings(&high), (2, 3));
        let g = high.generation().to_owned();
        high.apply(&g, &[], &["a".into()]).unwrap();
        // Still referenced by the low view: kept and matchable there only.
        assert_eq!(hits(&low, "落花", Restricted), vec!["a"]);
        assert_eq!(hits(&high, "落花", Restricted), Vec::<String>::new());
        assert_eq!(postings(&high), (2, 2));
        // A failed batch rolls back documents and reference counts together.
        let g = low.generation().to_owned();
        assert!(low.apply(&g, &[doc("x", "X", "fresh", Public), doc("x", "X", "dup", Public)], &[]).is_err());
        assert_eq!(postings(&high), (2, 2));
        assert_eq!(hits(&low, "fresh", Public), Vec::<String>::new());
        // Dropping a view releases what only it referenced.
        drop(low);
        assert_eq!(postings(&high), (1, 1));
        assert_eq!(hits(&high, "ocean", Restricted), vec!["b"]);
        high.replace(&[]).unwrap();
        assert_eq!(postings(&high), (0, 0));
        assert_eq!(hits(&high, "ocean", Restricted), Vec::<String>::new());
    }
}
