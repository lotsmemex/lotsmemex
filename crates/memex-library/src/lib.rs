//! Verified Store-to-query synchronization, independent of UI and transports.
//!
//! Store-to-query synchronization is incremental: the Store's writer-lock
//! generation token says whether anything changed, and per-item head stamps say
//! which items to read again. Out-of-band edits that bypass the Store writer are
//! not detected until the next Store write or a new process.

use anyhow::{Context, Result};
use memex_core::Sensitivity;
pub use memex_query::{GroupFacet, Query, QueryPage, SortField};
use memex_query::{GroupRef, IndexedDocument, ItemSummary, QueryIndex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

/// Bounded original-text entry, independent of generated analysis. Further
/// pages use the existing revision-bound context_read byte-range contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryEntry {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub manual_tags: Vec<String>,
    pub source_url: String,
    pub sensitivity: Sensitivity,
    pub coordinate_space: String,
    pub content_sha256: String,
    pub total_bytes: usize,
    pub markdown: String,
    pub next_byte: Option<usize>,
}

pub fn read_entry(
    library: impl AsRef<std::path::Path>,
    item_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<LibraryEntry> {
    let pair = memex_store::with_consistent_snapshot(library, |root| {
        memex_store::read_item_at_sensitivity(root, item_id, max_sensitivity)
    })
    .map_err(|_| anyhow::anyhow!("library_read_failed"))?;
    let (item, revision) = pair
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
    let text = &revision.content_markdown;
    let mut end = text.len().min(16 * 1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(LibraryEntry {
        item_id: item.item_id,
        revision_id: revision.revision_id,
        title: revision.title,
        manual_tags: revision
            .metadata
            .manual_tags
            .map(|tags| tags.value)
            .unwrap_or_default(),
        source_url: revision.source_url,
        sensitivity: item.sensitivity.max(revision.sensitivity),
        coordinate_space: "revision_markdown_utf8_v1".into(),
        content_sha256: format!("sha256:{:x}", Sha256::digest(text)),
        total_bytes: text.len(),
        markdown: text[..end].to_owned(),
        next_byte: (end < text.len()).then_some(end),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryQueryRequest {
    pub query: Query,
    /// Local owners may narrow their read. Remote adapters MUST impose the
    /// authenticated session's ceiling in addition to this requested ceiling.
    pub max_sensitivity: Sensitivity,
    pub offset: usize,
    pub limit: usize,
    pub expected_view: Option<String>,
}

pub struct LibraryQuery {
    library: PathBuf,
    // At most four lazy derived views of the same canonical Store, sharing one
    // set of full-text postings. Hidden-only edits must not change a less
    // privileged caller's generation/view tokens.
    visible: BTreeMap<Sensitivity, VisibleIndex>,
    /// Store write token (`catalog/generation`) the item cache was refreshed at.
    generation: Option<String>,
    /// One entry per non-trashed item: its head stamp and index summary (no body).
    items: BTreeMap<String, CachedItem>,
    /// Bumped on every item-cache refresh; a view synced at this value is current.
    refreshes: u64,
}

struct CachedItem {
    stamp: memex_store::ItemChangeStamp,
    /// `group_ids` is empty here; memberships are applied per ceiling.
    summary: ItemSummary,
    content_sha256: [u8; 32],
}

struct VisibleIndex {
    index: QueryIndex,
    fingerprints: BTreeMap<String, [u8; 32]>,
    groups_fingerprint: Option<[u8; 32]>,
    tree: Option<memex_store::GroupTreeSnapshot>,
    item_groups: BTreeMap<String, Vec<String>>,
    installed: bool,
    synced_refresh: Option<u64>,
}

impl LibraryQuery {
    /// One instance belongs to one library and one synchronous caller. A new
    /// instance rebuilds its memory index; views from an old instance expire.
    pub fn new(library: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            library: library.into(),
            visible: BTreeMap::new(),
            generation: None,
            items: BTreeMap::new(),
            refreshes: 0,
        })
    }

    pub fn query(&mut self, request: &LibraryQueryRequest) -> Result<QueryPage> {
        self.synchronize(request.max_sensitivity)?;
        let visible = &self.visible[&request.max_sensitivity];
        let mut page = visible.index.query(
            &request.query,
            request.max_sensitivity,
            request.offset,
            request.limit,
            request.expected_view.as_deref(),
        )?;
        for item in &mut page.items {
            item.group_ids = visible
                .item_groups
                .get(&item.item_id)
                .cloned()
                .unwrap_or_default();
        }
        Ok(page)
    }

    pub fn group_tree(
        &mut self,
        ceiling: Sensitivity,
        offset: usize,
        limit: usize,
        expected_view: Option<&str>,
    ) -> Result<memex_store::GroupTreePage> {
        if limit == 0
            || limit > memex_store::MAX_GROUP_TREE_PAGE
            || (offset > 0 && expected_view.is_none())
        {
            anyhow::bail!("invalid_group_window");
        }
        self.synchronize(ceiling)?;
        self.visible[&ceiling]
            .tree
            .as_ref()
            .expect("synchronized tree")
            .page(offset, limit, expected_view)
    }

    /// Freshness: every Store writer replaces `catalog/generation` before it releases the
    /// writer lock, and this runs under the shared lock. An unchanged token means no
    /// item, trash marker or group changed, so a warm query reads one small file. After a
    /// write, only items whose head stamp (`lstat`) changed are read again.
    fn synchronize(&mut self, ceiling: Sensitivity) -> Result<()> {
        let library = self.library.clone();
        let result = memex_store::with_consistent_snapshot(&library, |root| {
            self.synchronize_locked(root, ceiling)
        });
        if result.is_err() {
            // A failed canonical verification invalidates BOTH navigation and search.
            self.visible.clear();
            self.items.clear();
            self.generation = None;
            anyhow::bail!("library_query_failed");
        }
        Ok(())
    }

    fn synchronize_locked(&mut self, root: &std::path::Path, ceiling: Sensitivity) -> Result<()> {
        let generation = memex_store::catalog_generation(root)?;
        let refreshing = generation.is_none() || generation != self.generation;
        let mut refresh = Vec::new();
        if refreshing {
            let mut previous = std::mem::take(&mut self.items);
            self.generation = None;
            for (item_id, stamp) in memex_store::item_change_stamps(root)? {
                match previous.remove(&item_id) {
                    Some(cached) if cached.stamp == stamp => {
                        self.items.insert(item_id, cached);
                    }
                    _ => refresh.push((item_id, stamp)),
                }
            }
            self.refreshes += 1;
        }
        if !self.visible.contains_key(&ceiling) {
            // One copy of the full-text postings for every ceiling; each view
            // keeps its own documents, groups and generation.
            let index = match self.visible.values().next() {
                Some(other) => other.index.new_shared()?,
                None => QueryIndex::new()?,
            };
            self.visible.insert(ceiling, VisibleIndex {
                index,
                fingerprints: BTreeMap::new(),
                groups_fingerprint: None,
                tree: None,
                item_groups: BTreeMap::new(),
                installed: false,
                synced_refresh: None,
            });
        }
        let Self { visible: views, items, refreshes, generation: published, .. } = self;
        let visible = views.get_mut(&ceiling).expect("installed visible index");
        if visible.installed && visible.synced_refresh == Some(*refreshes) {
            *published = generation;
            return Ok(());
        }
        let groups = memex_store::read_visible_groups(root, ceiling)?;
        let shown = |id: &str, cached: &CachedItem| {
            cached.summary.sensitivity <= ceiling && !groups.trashed_item_ids.contains(id)
        };
        let full = !visible.installed;
        // Unchanged items whose document this view still lacks (a new view, or one
        // that missed an earlier refresh). Changed items are judged once verified.
        let mut jobs: Vec<Job> = refresh.into_iter().map(|(id, stamp)| Job::Verify(id, stamp)).collect();
        for (item_id, cached) in items.iter() {
            if shown(item_id, cached) && (full || visible.fingerprints.get(item_id) != Some(&fingerprint(cached)?)) {
                jobs.push(Job::Body(item_id.clone()));
            }
        }
        // Documents stream into one open transaction: nothing is visible before it
        // commits, a failure anywhere rolls it back, and at most the transient
        // budget of verified bodies waits between the workers and the index.
        let VisibleIndex { index, fingerprints: old, .. } = visible;
        let mut fingerprints = BTreeMap::new();
        let feed = |writer: &mut memex_query::IndexWriter<'_>| -> Result<()> {
            let verified = stream_documents(root, &jobs, &*items, &|id, cached| {
                Ok(shown(id, cached) && (full || old.get(id) != Some(&fingerprint(cached)?)))
            }, |document| writer.upsert(&document))?;
            items.extend(verified);
            for (item_id, cached) in items.iter() {
                if shown(item_id, cached) {
                    fingerprints.insert(item_id.clone(), fingerprint(cached)?);
                }
            }
            if !full {
                for id in old.keys().filter(|id| !fingerprints.contains_key(*id)) {
                    writer.remove(id)?;
                }
            }
            Ok(())
        };
        if full {
            index.replace_streaming(feed)?;
        } else {
            let expected = index.generation().to_owned();
            index.apply_streaming(&expected, feed)?;
        }
        let visible_ids: std::collections::BTreeSet<&str> = fingerprints.keys().map(String::as_str).collect();
        // Only authorized groups and authorized items enter the projection;
        // hidden members change neither counts, facets nor group_ids.
        let memberships: Vec<(String, String)> = groups
            .memberships
            .iter()
            .filter(|membership| visible_ids.contains(membership.item_id.as_str()))
            .map(|membership| (membership.group_id.clone(), membership.item_id.clone()))
            .collect();
        let mut group_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (group_id, item_id) in &memberships {
            group_ids
                .entry(item_id.clone())
                .or_default()
                .push(group_id.clone());
        }
        for ids in group_ids.values_mut() {
            ids.sort();
        }
        let tree =
            memex_store::GroupTreeSnapshot::from_verified_items(&groups, &visible_ids, ceiling)?;
        let group_refs: Vec<GroupRef> = groups
            .nodes
            .iter()
            .map(|node| GroupRef {
                group_id: node.group_id.clone(),
                name: node.name.clone(),
                kind: node.kind.as_str().into(),
                parent_id: node.parent_id.clone(),
            })
            .collect();
        let groups_fingerprint: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&(&group_refs, &memberships))?).into();
        if visible.groups_fingerprint != Some(groups_fingerprint) {
            visible.index.replace_groups(&group_refs, &memberships)?;
        }
        visible.tree = Some(tree);
        visible.item_groups = group_ids;
        visible.groups_fingerprint = Some(groups_fingerprint);
        visible.fingerprints = fingerprints;
        visible.installed = true;
        visible.synced_refresh = Some(*refreshes);
        *published = generation;
        Ok(())
    }
}

enum Job {
    /// A changed item: full canonical verification, then its document if needed.
    Verify(String, memex_store::ItemChangeStamp),
    /// An unchanged, already verified item whose document this view lacks.
    Body(String),
}

/// Bytes of verified bodies allowed to wait for the index writer. One body larger
/// than this still passes alone, so the bound is this plus one body per worker.
const STREAM_BUDGET: usize = 32 * 1024 * 1024;
const WORKERS: usize = 8;

/// Verifies and reads `jobs` on workers inside the caller's Store snapshot and
/// hands each needed document to `sink` on this thread. Returns the refreshed
/// cache entries only after every worker has joined without error.
fn stream_documents(
    root: &std::path::Path,
    jobs: &[Job],
    items: &BTreeMap<String, CachedItem>,
    needed: &(dyn Fn(&str, &CachedItem) -> Result<bool> + Sync),
    mut sink: impl FnMut(IndexedDocument) -> Result<()>,
) -> Result<Vec<(String, CachedItem)>> {
    use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Condvar, Mutex};
    struct Budget { state: Mutex<(usize, bool)>, ready: Condvar }
    impl Budget {
        /// False once aborted; waits while queued bytes would exceed the budget.
        fn acquire(&self, n: usize) -> bool {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            while !s.1 && s.0 > 0 && s.0 + n > STREAM_BUDGET {
                s = self.ready.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.1 { return false; }
            s.0 += n;
            true
        }
        fn release(&self, n: usize) {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).0 -= n;
            self.ready.notify_all();
        }
        fn abort(&self) {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
            self.ready.notify_all();
        }
    }
    let budget = Budget { state: Mutex::new((0, false)), ready: Condvar::new() };
    let next = AtomicUsize::new(0);
    let produce = |id: &str, job: &Job| -> Result<(Option<(String, CachedItem)>, Option<IndexedDocument>)> {
        Ok(match job {
            Job::Verify(item_id, stamp) => {
                let (item, revision) = memex_store::read_item(root, item_id)?;
                let added_at = memex_store::verified_item_added_at(root, &item)?;
                let origin = memex_store::item_origin(root, item_id)?;
                let cached = cached_item(stamp.clone(), item, &revision, added_at, origin);
                let document = needed(id, &cached)?.then(|| IndexedDocument {
                    summary: cached.summary.clone(),
                    content_markdown: revision.content_markdown,
                });
                (Some((item_id.clone(), cached)), document)
            }
            Job::Body(item_id) => {
                let cached = items.get(item_id).context("library verification worker failed")?;
                let content_markdown = memex_store::read_item(root, item_id)?.1.content_markdown;
                (None, Some(IndexedDocument { summary: cached.summary.clone(), content_markdown }))
            }
        })
    };
    std::thread::scope(|scope| -> Result<Vec<_>> {
        let (send, receive) = mpsc::channel();
        let workers: Vec<_> = (0..WORKERS.min(jobs.len()))
            .map(|_| {
                let (send, budget, next, produce) = (send.clone(), &budget, &next, &produce);
                scope.spawn(move || -> Result<()> {
                    loop {
                        let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else { return Ok(()) };
                        let id = match job { Job::Verify(id, _) | Job::Body(id) => id.as_str() };
                        let output = match produce(id, job) {
                            Ok(output) => output,
                            Err(error) => { budget.abort(); return Err(error); }
                        };
                        let bytes = output.1.as_ref().map_or(0, |d| d.content_markdown.len());
                        if !budget.acquire(bytes) { return Ok(()); }
                        if send.send((output, bytes)).is_err() { budget.release(bytes); return Ok(()); }
                    }
                })
            })
            .collect();
        drop(send);
        let mut verified = Vec::new();
        let mut failure = None;
        for ((cached, document), bytes) in receive.iter() {
            verified.extend(cached);
            let written = document.map_or(Ok(()), &mut sink);
            budget.release(bytes);
            if let Err(error) = written {
                failure = Some(error);
                budget.abort();
                break;
            }
        }
        drop(receive);
        // Join every worker before reporting, also on failure.
        let mut joined = Ok(());
        for worker in workers {
            let result = worker.join().map_err(|_| anyhow::anyhow!("library verification worker failed")).and_then(|r| r);
            if joined.is_ok() { joined = result; }
        }
        if let Some(error) = failure { return Err(error); }
        joined?;
        Ok(verified)
    })
}

fn fingerprint(cached: &CachedItem) -> Result<[u8; 32]> {
    // Membership changes update the relational projection and page summaries,
    // not the original body or its FTS document.
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&cached.summary)?);
    hash.update([0]);
    hash.update(cached.content_sha256);
    Ok(hash.finalize().into())
}

fn cached_item(
    stamp: memex_store::ItemChangeStamp,
    item: memex_core::Item,
    revision: &memex_core::ItemRevision,
    added_at: Option<String>,
    origin: Option<memex_core::ItemOrigin>,
) -> CachedItem {
    // Origin groups web sources without making each article a facet.
    // Native Zotero records likewise share a source facet per library group;
    // their full item locator remains unchanged in the canonical revision.
    let mut source = url::Url::parse(&revision.source_url)
        .ok()
        .filter(|url| url.has_host())
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|| revision.source_url.clone());
    if let Some((group, key)) = revision
        .source_url
        .strip_prefix("urn:zotero:group:")
        .and_then(|value| value.split_once(":item:"))
    {
        if !group.is_empty()
            && group.bytes().all(|b| b.is_ascii_digit())
            && !key.is_empty()
            && key.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            source = format!("urn:zotero:group:{group}");
        }
    }
    // Match the existing Store metadata projection's timestamp meaning.
    let captured_at = revision
        .provenance
        .iter()
        .find(|activity| activity.activity_type == "translate")
        .map(|activity| activity.occurred_at.clone());
    CachedItem {
        stamp,
        content_sha256: Sha256::digest(revision.content_markdown.as_bytes()).into(),
        summary: ItemSummary {
            item_id: item.item_id,
            revision_id: revision.revision_id.clone(),
            title: revision.title.clone(),
            source,
            item_type: item.item_type,
            captured_at,
            published_at: revision.metadata.published_day(),
            duration_seconds: revision.metadata.duration_seconds.as_ref().map(|v| v.value).filter(|v| *v > 0),
            added_at,
            origin,
            dates: Default::default(),
            sensitivity: item.sensitivity.max(revision.sensitivity),
            group_ids: Vec::new(),
            translated_title: None,
            title_source: None,
        },
    }
}
