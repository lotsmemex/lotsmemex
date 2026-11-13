//! Pure library state: query options, the active view, a sparse window cache,
//! facets, selection and load state. No GPUI types, so every rule here is unit
//! tested with the synthetic fixture.
//!
//! Two backend paths share one model:
//! - `Query`: the agent's `library_query` views. Rows are fetched on demand for the
//!   visible range; a changed library rejects the view and the model restarts from
//!   offset zero with a fresh view, keeping the selected item id.
//! - `Legacy`: agents without `library_query` (the 916 baseline). `library_list`
//!   pages are appended as they arrive and are shown immediately; filtering and
//!   sorting apply to what has loaded, and the UI says so until loading finishes.

use memex_client::{relabel, source_label, tx, txf, Client, ClientError, GroupNode, GroupEditOp, GroupScope, ItemOrigin, LibraryItem, QueryItem, QuerySpec, QueryWindow, Sensitivity, SortKey};
use std::collections::BTreeMap;

pub const WINDOW: usize = 100;

/// No text, source, type or folder filter: the first window's facets already
/// cover the whole authorized library (sort does not change counts).
pub fn is_unfiltered(spec: &QuerySpec) -> bool {
    spec.text.is_empty() && spec.source.is_none() && spec.item_type.is_none() && spec.group.is_none() && spec.origin.is_none() && !spec.unfiled
}

/// First window of `spec` plus the whole-library browse facets. Unfiltered queries
/// reuse the first window for the facets instead of a second full query; filtered
/// queries keep the separate browse query (facets of the filtered result are not
/// the sidebar's library-wide counts). Returns (browse, first).
pub fn load_first(client: &Client, spec: &QuerySpec, ceiling: Sensitivity) -> (Result<QueryWindow, ClientError>, Result<QueryWindow, ClientError>) {
    let first = client.library_query(spec, ceiling, 0, WINDOW, None);
    let browse = match &first {
        Ok(w) if is_unfiltered(spec) => Ok(w.clone()),
        Ok(_) => client.library_query(&QuerySpec::default(), ceiling, 0, 1, None),
        Err(e) => Err(e.clone()),
    };
    (browse, first)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    /// Chinese display title from the agent (`title` stays the original; sorting
    /// and searching use the original, as the agent does).
    pub translated_title: Option<String>,
    /// Value used by the `source` filter (an origin for queries, the site name for legacy).
    pub source_key: Option<String>,
    pub source_label: String,
    pub item_type: String,
    pub captured_at: Option<String>,
    /// Classic-table bibliographic columns; `None` when the backend has none.
    /// Authors: first author and whether more follow ("et al." is added on render).
    pub authors: Option<(String, bool)>,
    pub year: Option<String>,
    pub container: Option<String>,
    /// 采集日期 (`added_at`, RFC3339 UTC); `None` is "not recorded". Never the
    /// capture time.
    pub added_at: Option<String>,
    /// 发布日期: the agent's `published_at` (`YYYY-MM-DD`, day precision only);
    /// `None` is unknown, never replaced by a capture date.
    pub published_at: Option<String>,
    /// 时长 in whole seconds (`duration_seconds`); `None` is unknown.
    pub duration_seconds: Option<u32>,
}

/// Tooltip text for a list/table row: its full title, once the row has loaded.
pub fn row_tooltip(row: Option<&Row>) -> Option<String> {
    row.filter(|r| !r.title.trim().is_empty()).map(Row::display_title)
}

fn dash(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "—".into())
}

impl Row {
    /// `译名 ｜ Original`, or the original alone.
    pub fn display_title(&self) -> String {
        memex_client::display_title(&self.title, self.translated_title.as_deref())
    }
    pub fn authors_label(&self) -> String {
        match &self.authors {
            Some((name, true)) => txf("{0} 等", &[name]),
            Some((name, false)) => name.clone(),
            None => "—".into(),
        }
    }
    pub fn year_label(&self) -> String {
        dash(&self.year)
    }
    pub fn container_label(&self) -> String {
        dash(&self.container)
    }
    /// Local date of first inclusion, or "未记录".
    pub fn added_label(&self) -> String {
        self.added_local_date().unwrap_or_else(|| tx("未记录").into())
    }
    pub fn added_local_date(&self) -> Option<String> {
        memex_client::biblio::added_at_local(self.added_at.as_deref()).map(|t| t[..10].to_string())
    }
    /// 发布日期 column: the publication day, or "—".
    pub fn published_label(&self) -> String {
        dash(&self.published_at)
    }
    /// 时长 column: `h:mm:ss` / `mm:ss`, or "—".
    pub fn duration_label(&self) -> String {
        memex_client::duration_label(self.duration_seconds).unwrap_or_else(|| "—".into())
    }
    /// The reading list's trailing date: the publication day and, when known, the
    /// duration (`2026-10-30 · 1:00:26`). Without a publication day the duration
    /// alone, else the inclusion date labelled 采集 so it is never read as published.
    pub fn list_meta(&self) -> Option<String> {
        let duration = memex_client::duration_label(self.duration_seconds);
        match (self.published_at.clone(), duration) {
            (Some(d), Some(t)) => Some(format!("{d} · {t}")),
            (Some(d), None) => Some(d),
            (None, Some(t)) => Some(t),
            (None, None) => self.added_local_date().map(|d| txf("采集 {0}", &[&d])),
        }
    }
}

impl From<&QueryItem> for Row {
    fn from(item: &QueryItem) -> Self {
        Row {
            item_id: item.item_id.clone(),
            revision_id: item.revision_id.clone(),
            title: item.title.clone(),
            translated_title: item.translated_title.clone(),
            source_key: item.source.clone(),
            source_label: item.source_label(),
            item_type: item.item_type.clone(),
            captured_at: item.captured_at.clone(),
            authors: item.bibliographic.as_ref().and_then(|b| b.first_author()),
            year: item.bibliographic.as_ref().and_then(|b| b.published_date.as_ref()).and_then(|d| d.year()),
            container: item.bibliographic.as_ref().and_then(|b| b.container()),
            added_at: item.added_at.clone().or_else(|| item.bibliographic.as_ref().and_then(|b| b.added_at.clone())),
            published_at: item.published_at.clone(),
            duration_seconds: item.duration_seconds,
        }
    }
}

impl From<&LibraryItem> for Row {
    fn from(item: &LibraryItem) -> Self {
        Row {
            item_id: item.item_id.clone(),
            revision_id: item.revision_id.clone(),
            title: item.title.clone(),
            translated_title: None,
            source_key: item.site_name.clone(),
            source_label: item.source().to_string(),
            item_type: item.item_type.clone(),
            captured_at: item.captured_at.clone(),
            authors: None,
            year: None,
            container: None,
            added_at: None,
            published_at: item.published_at.clone(),
            duration_seconds: item.duration_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetEntry {
    pub key: String,
    pub label: String,
    pub count: usize,
}

/// The reading list's 排序 menu (same options and order as the phone).
/// 最近加入资料库 is the `added_at` candidate (unknown times last; an older
/// agent refuses it and the list returns to 抓取时间). 原始发表日期 has no
/// agent order yet. Neither tells an owner's capture from an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSort {
    /// 最近加入资料库 (`added_at`, newest first; unknown last).
    AddedToLibrary,
    /// 抓取时间 (`captured_at`, newest first); the default.
    Captured,
    /// 原始发表日期 (bibliographic date, newest first). Needs agent support.
    Published,
    /// 标题 (A→Z).
    Title,
    /// 来源 (A→Z).
    Source,
}

impl ListSort {
    pub const ALL: [ListSort; 5] = [ListSort::AddedToLibrary, ListSort::Captured, ListSort::Published, ListSort::Title, ListSort::Source];

    /// Chinese key; shown through `tx`.
    pub fn label(self) -> &'static str {
        match self {
            ListSort::AddedToLibrary => "最近加入资料库",
            ListSort::Captured => "抓取时间",
            ListSort::Published => "原始发表日期",
            ListSort::Title => "标题",
            ListSort::Source => "来源",
        }
    }

    /// The `library_query` order, or `None` when the agent cannot sort this way.
    pub fn query(self) -> Option<(SortKey, bool)> {
        match self {
            ListSort::Captured => Some((SortKey::CapturedAt, true)),
            ListSort::Title => Some((SortKey::Title, false)),
            ListSort::Source => Some((SortKey::Source, false)),
            ListSort::AddedToLibrary => Some((SortKey::AddedAt, true)),
            ListSort::Published => None,
        }
    }

    /// The menu entry for an order (the classic table may set others: none matches).
    pub fn of(sort: (SortKey, bool)) -> Option<ListSort> {
        ListSort::ALL.into_iter().find(|s| s.query() == Some(sort))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facets {
    pub total: usize,
    pub sources: Vec<FacetEntry>,
    pub item_types: Vec<FacetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    Undetermined,
    Query,
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Inbox, the default entry for captures and imports: items in no library or
    /// folder (`unfiled`) where the agent can tell, else everything, newest first.
    Inbox,
    All,
    Source(String),
    ItemType(String),
    /// A folder (root group or collection), by id.
    Group(String),
}

#[derive(Debug, Clone)]
pub struct LibraryModel {
    pub spec: QuerySpec,
    pub path: Path,
    pub state: LoadState,
    view: Option<String>,
    total: usize,
    rows: BTreeMap<usize, Row>,
    pending: Vec<usize>,
    facets: Facets,
    legacy: Vec<LibraryItem>,
    legacy_complete: bool,
    /// The primary selection: the range anchor and the item the reader shows.
    selected: Option<String>,
    /// The multi-selection (Shift/Option/⌘ clicks, or the row a context menu
    /// was opened on); None follows `selected`, Some(empty) explicitly deselects.
    picked: Option<Vec<String>>,
    /// A Shift range (inclusive indices) whose rows are still loading; its ids
    /// are filled in as windows arrive.
    picked_range: Option<(usize, usize)>,
    /// Folder nodes as listed by the backend; empty when it has no groups.
    groups: Vec<GroupNode>,
    /// `None` until known; `Some(false)` when the agent has no folder tree.
    groups_supported: Option<bool>,
    /// `group_edit` is available.
    group_writes: bool,
    pub group_reorder: bool,
    pub group_transfer_folders: bool,
    /// The toolbar's "include subfolder items" view switch; sent explicitly with
    /// every folder query and kept while no folder is selected.
    include_descendants: bool,
    /// The agent refused `sort: "added_at"` (older than that contract): the
    /// order is offered as unavailable and the list went back to 抓取时间.
    pub added_at_rejected: bool,
    /// Lightweight daemon capability; never infer origin from a row's source URL.
    pub origin_supported: Option<bool>,
    /// Inbox is selected (with or without `unfiled` on the wire).
    inbox: bool,
    /// `groups_v1.query_unfiled`; `None` until the capabilities were read.
    pub unfiled_supported: Option<bool>,
    /// Items in no folder, as the agent counted them; `None` when it cannot tell
    /// or has not answered (never guessed).
    pub inbox_count: Option<usize>,
}

impl Default for LibraryModel {
    fn default() -> Self {
        Self {
            spec: QuerySpec::default(),
            path: Path::Undetermined,
            state: LoadState::Loading,
            view: None,
            total: 0,
            rows: BTreeMap::new(),
            pending: vec![],
            facets: Facets::default(),
            legacy: vec![],
            legacy_complete: false,
            selected: None,
            picked: None,
            picked_range: None,
            groups: vec![],
            groups_supported: None,
            group_writes: false,
            group_reorder: false,
            group_transfer_folders: false,
            include_descendants: false,
            added_at_rejected: false,
            origin_supported: None,
            inbox: false,
            unfiled_supported: None,
            inbox_count: None,
        }
    }
}

impl LibraryModel {
    pub fn filter(&self) -> Filter {
        if self.inbox {
            return Filter::Inbox;
        }
        match (&self.spec.source, &self.spec.item_type, &self.spec.group) {
            (Some(s), _, _) => Filter::Source(s.clone()),
            (_, Some(t), _) => Filter::ItemType(t.clone()),
            (_, _, Some(g)) => Filter::Group(g.group_id.clone()),
            _ => Filter::All,
        }
    }

    pub fn groups(&self) -> &[GroupNode] {
        &self.groups
    }

    pub fn set_groups(&mut self, groups: Vec<GroupNode>) {
        self.groups = groups;
    }

    /// A freshly read tree and whether this agent accepts edits.
    pub fn set_group_tree(&mut self, nodes: Vec<GroupNode>, writes: bool) {
        self.groups = nodes;
        self.groups_supported = Some(true);
        self.group_writes = writes;
    }

    /// The agent has no folder tree (an older agent).
    pub fn set_groups_unsupported(&mut self) {
        self.groups.clear();
        self.groups_supported = Some(false);
        self.group_writes = false;
        self.group_reorder = false;
        self.group_transfer_folders = false;
    }

    pub fn groups_supported(&self) -> Option<bool> {
        self.groups_supported
    }

    pub fn group_writes(&self) -> bool {
        self.group_writes
    }

    /// Freeze the complete source subtrees and ancestor versions. A newly added
    /// child, changed membership, or reparent must invalidate a pending drag.
    pub fn folder_versions(&self, ids: &[String]) -> Option<BTreeMap<String, String>> {
        if ids.is_empty() || ids.len() > memex_client::MAX_GROUP_EDIT_ITEMS { return None; }
        let nodes: BTreeMap<_, _> = self.groups.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for n in &self.groups {
            if let Some(parent) = &n.parent_id { children.entry(parent).or_default().push(&n.id); }
        }
        let mut versions = BTreeMap::new();
        let mut stack: Vec<&str> = ids.iter().map(String::as_str).collect();
        for id in ids {
            let mut node = *nodes.get(id.as_str())?;
            node.parent_id.as_ref()?; // library roots cannot become folders
            while let Some(parent) = &node.parent_id {
                node = *nodes.get(parent.as_str())?;
                if versions.insert(node.id.clone(), node.version.clone()).is_some() { break; }
            }
        }
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) { continue; }
            let node = nodes.get(id)?;
            versions.insert(node.id.clone(), node.version.clone());
            stack.extend(children.get(id).into_iter().flatten().copied());
        }
        Some(versions)
    }

    /// The version a `group_edit` names: the target node's (the parent's for a
    /// new subfolder); none for a new root group.
    pub fn version_for(&self, op: &GroupEditOp) -> Option<String> {
        let target = match op {
            GroupEditOp::CreateRoot { .. } => return None,
            GroupEditOp::ReorderRoot { group_id, .. } => group_id,
            GroupEditOp::TransferFolders { target_parent_id, .. } => target_parent_id,
            GroupEditOp::CreateFolder { parent_id: id, .. } | GroupEditOp::Rename { id, .. } | GroupEditOp::AddMembers { group_id: id, .. } | GroupEditOp::RemoveMembers { group_id: id, .. } | GroupEditOp::MoveMembers { target_group_id: id, .. } => id,
        };
        Some(self.group(target).map(|g| g.version.clone()).unwrap_or_default())
    }

    /// Folders in tree order (preorder) with their depth, for pickers.
    pub fn groups_in_order(&self) -> Vec<(usize, &GroupNode)> {
        fn visit<'a>(all: &'a [GroupNode], parent: Option<&str>, depth: usize, out: &mut Vec<(usize, &'a GroupNode)>) {
            for node in all.iter().filter(|g| g.parent_id.as_deref() == parent) {
                if out.iter().any(|(_, n)| n.id == node.id) {
                    continue; // a cycle is invalid data; never loop
                }
                out.push((depth, node));
                visit(all, Some(&node.id), depth + 1, out);
            }
        }
        let mut out = vec![];
        visit(&self.groups, None, 0, &mut out);
        out
    }

    /// Writable folders a selection in `current` can be filed into, in tree
    /// order with depth. Inside a library (`current` is one of its folders):
    /// `same_library` lists that library's root and folders except `current`
    /// (移动到文件夹… / 添加到文件夹…), otherwise every other library
    /// (映射到其他库…). With no current folder (Inbox, 全部条目, a filter):
    /// every writable folder.
    pub fn pick_targets(&self, current: Option<&str>, same_library: bool) -> Vec<(usize, GroupNode)> {
        let root = current.and_then(|c| self.group(c)).map(|g| g.root_group_id.clone());
        self.groups_in_order()
            .into_iter()
            .filter(|(_, g)| !g.read_only)
            .filter(|(_, g)| match &root {
                None => true,
                Some(r) if same_library => &g.root_group_id == r && Some(g.id.as_str()) != current,
                Some(r) => &g.root_group_id != r,
            })
            .map(|(d, g)| (d, g.clone()))
            .collect()
    }

    pub fn group(&self, id: &str) -> Option<&GroupNode> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Root-first ancestor chain ending with `id` (breadcrumb).
    pub fn group_path(&self, id: &str) -> Vec<&GroupNode> {
        let mut path = vec![];
        let mut current = self.group(id);
        while let Some(node) = current {
            if path.iter().any(|n: &&GroupNode| n.id == node.id) {
                break; // a cycle is invalid data; never loop
            }
            path.push(node);
            current = node.parent_id.as_deref().and_then(|p| self.group(p));
        }
        path.reverse();
        path
    }

    pub fn include_descendants(&self) -> bool {
        self.include_descendants
    }

    /// Navigation count shown for a folder under the current view switch.
    pub fn group_count(&self, node: &GroupNode) -> usize {
        if self.include_descendants { node.subtree_item_count } else { node.item_count }
    }

    /// Flips the view switch; a selected folder is queried again with it.
    /// Returns whether the view must reload.
    pub fn set_include_descendants(&mut self, on: bool) -> bool {
        self.include_descendants = on;
        match &mut self.spec.group {
            Some(g) if g.include_descendants != on => {
                g.include_descendants = on;
                self.clear_picked();
                self.restart();
                true
            }
            _ => false,
        }
    }

    /// Changing filter, search or sort discards the view (it binds all options).
    pub fn set_filter(&mut self, filter: Filter) {
        self.inbox = filter == Filter::Inbox;
        // Sent only to an agent that advertises it (the load checks again).
        self.spec.unfiled = self.inbox && self.unfiled_supported != Some(false);
        if self.inbox {
            // Newest captures and imports first (抓取时间 where 采集日期 is refused).
            let (key, descending) = if self.added_at_rejected { ListSort::Captured } else { ListSort::AddedToLibrary }.query().unwrap_or((SortKey::CapturedAt, true));
            self.spec.sort = key;
            self.spec.descending = descending;
        }
        let (source, item_type, group) = match filter {
            Filter::Inbox | Filter::All => (None, None, None),
            Filter::Source(s) => (Some(s), None, None),
            Filter::ItemType(t) => (None, Some(t), None),
            Filter::Group(id) => (None, None, Some(GroupScope { group_id: id, include_descendants: self.include_descendants })),
        };
        self.spec.source = source;
        self.spec.item_type = item_type;
        if group.is_some() {
            // Folder navigation shows that folder's contents, regardless of
            // a capture/import filter chosen in the previous location.
            self.spec.origin = None;
        }
        self.spec.group = group;
        self.clear_picked();
        self.restart();
    }

    pub fn set_search(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text != self.spec.text {
            self.spec.text = text;
            self.clear_picked();
            self.restart();
        }
    }

    pub fn set_origin(&mut self, origin: Option<ItemOrigin>) {
        if self.origin_supported == Some(true) {
            self.spec.origin = origin;
            self.clear_picked();
            self.restart();
        }
    }

    /// The list's current order (the classic table's column sorts included).
    pub fn sort(&self) -> (SortKey, bool) {
        (self.spec.sort, self.spec.descending)
    }

    pub fn set_sort(&mut self, key: SortKey, descending: bool) {
        self.spec.sort = key;
        self.spec.descending = descending;
        self.clear_picked();
        self.restart();
    }

    /// An agent refused the `added_at` order: back to 抓取时间, once. `true`
    /// when that happened (the caller reloads); any other failure is not this.
    pub fn on_sort_refused(&mut self, error: &ClientError) -> bool {
        let refused = matches!(error, ClientError::Unsupported) || matches!(error, ClientError::Daemon(c) if c == "invalid_request");
        if !refused || self.spec.sort != SortKey::AddedAt || self.added_at_rejected {
            return false;
        }
        self.added_at_rejected = true;
        let (key, descending) = ListSort::Captured.query().unwrap_or((SortKey::CapturedAt, true));
        self.set_sort(key, descending);
        true
    }

    /// Forget the view and rows (after a stale view, refresh, or option change).
    /// Picked ids stay (a refresh is not a new list); a loading range does not.
    pub fn restart(&mut self) {
        self.view = None;
        self.picked_range = None;
        self.rows.clear();
        self.pending.clear();
        self.state = LoadState::Loading;
        if self.path == Path::Legacy {
            self.rebuild_legacy();
        }
    }

    /// Restart a legacy listing from its first page.
    pub fn reset_legacy(&mut self) {
        self.legacy.clear();
        self.legacy_complete = false;
        self.restart();
    }

    pub fn view(&self) -> Option<String> {
        self.view.clone()
    }

    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub fn row(&self, ix: usize) -> Option<&Row> {
        self.rows.get(&ix)
    }

    pub fn facets(&self) -> &Facets {
        &self.facets
    }

    pub fn set_browse_facets(&mut self, window: &QueryWindow) {
        let entry = |f: &memex_client::Facet, label: String| FacetEntry { key: f.value.clone(), label, count: f.count };
        self.facets = Facets {
            total: window.total,
            sources: window.sources.iter().map(|f| entry(f, source_label(Some(&f.value)))).collect(),
            item_types: window.item_types.iter().map(|f| entry(f, f.value.clone())).collect(),
        };
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// A plain click (or the shell's commit): this one item, nothing else.
    pub fn select(&mut self, item_id: Option<String>) {
        self.selected = item_id;
        self.clear_picked();
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_deref()?;
        self.index_of(id)
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().find(|(_, r)| r.item_id == id).map(|(ix, _)| *ix)
    }

    /// Back to the primary selection alone (an option change, a new folder).
    pub fn clear_picked(&mut self) {
        self.picked = None;
        self.picked_range = None;
    }

    /// Deselect without closing the reader or losing the Shift anchor.
    pub fn clear_selection(&mut self) {
        self.set_picked(vec![]);
    }

    /// Select the entire query, loading missing windows before actions are allowed.
    pub fn select_all(&mut self) -> std::ops::Range<usize> {
        if self.total == 0 {
            self.clear_selection();
        } else {
            self.picked_range = Some((0, self.total - 1));
            self.resolve_range();
        }
        0..self.total
    }

    /// The items acted on together: the multi-selection, else the primary one.
    pub fn selection(&self) -> Vec<String> {
        self.picked.clone().unwrap_or_else(|| self.selected.iter().cloned().collect())
    }

    pub fn selection_len(&self) -> usize {
        self.picked.as_ref().map_or(usize::from(self.selected.is_some()), Vec::len)
    }

    /// Whether `id` is highlighted as selected.
    pub fn is_selected(&self, id: &str) -> bool {
        self.picked.as_ref().map_or(self.selected.as_deref() == Some(id), |set| set.iter().any(|p| p == id))
    }

    /// A Shift range still has rows loading; actions wait for it.
    pub fn selection_loading(&self) -> bool {
        self.picked_range.is_some()
    }

    /// Option/⌘+click: adds or removes one item; the open item and the anchor
    /// stay. The primary item starts the set when it is in this list. Removing
    /// the last picked item leaves an explicitly empty selection.
    pub fn toggle_picked(&mut self, item_id: &str) {
        self.picked_range = None;
        if self.picked.is_none() {
            self.picked = Some(self.selected.clone().filter(|p| self.index_of(p).is_some()).into_iter().collect());
        }
        let picked = self.picked.as_mut().unwrap();
        match picked.iter().position(|p| p == item_id) {
            Some(i) => { picked.remove(i); }
            None => picked.push(item_id.to_string()),
        }
    }

    /// Shift+click on row `ix`: every row from the anchor (the primary item)
    /// to `ix`, in list order, replacing the set. Rows not loaded yet are taken
    /// by index and filled in when their window arrives; the returned range is
    /// what must load. `None` when the anchor is not among the loaded rows (the
    /// caller treats the click as a plain one).
    pub fn extend_picked(&mut self, ix: usize) -> Option<std::ops::Range<usize>> {
        let anchor = self.selected_index()?;
        let (lo, hi) = (anchor.min(ix), anchor.max(ix).min(self.total.saturating_sub(1)));
        self.picked_range = Some((lo, hi));
        self.resolve_range();
        Some(lo..hi + 1)
    }

    fn resolve_range(&mut self) {
        let Some((lo, hi)) = self.picked_range else { return };
        self.picked = Some((lo..=hi).filter_map(|i| self.rows.get(&i)).map(|r| r.item_id.clone()).collect());
        if (lo..=hi).all(|i| self.rows.contains_key(&i)) {
            self.picked_range = None;
        }
    }

    /// Right-click on `item_id`: inside the selection the menu acts on all of
    /// it; outside, the selection becomes that row alone (the open item stays).
    pub fn context_selection(&mut self, item_id: &str) -> Vec<String> {
        if !self.is_selected(item_id) {
            self.picked = Some(vec![item_id.to_string()]);
            self.picked_range = None;
        }
        self.selection()
    }

    /// After a partial failure: these items stay selected.
    pub fn set_picked(&mut self, ids: Vec<String>) {
        self.picked = Some(ids);
        self.picked_range = None;
    }

    /// Window offsets that must be fetched to show `range` and are not in flight.
    pub fn missing_windows(&mut self, range: std::ops::Range<usize>) -> Vec<usize> {
        if self.path != Path::Query || self.view.is_none() {
            return vec![];
        }
        let end = range.end.min(self.total);
        let mut out = vec![];
        let mut offset = (range.start / WINDOW) * WINDOW;
        while offset < end {
            let complete = (offset..(offset + WINDOW).min(self.total)).all(|i| self.rows.contains_key(&i));
            if !complete && !self.pending.contains(&offset) {
                self.pending.push(offset);
                out.push(offset);
            }
            offset += WINDOW;
        }
        out
    }

    /// Applies a query window. Returns false (and changes nothing) when the window
    /// belongs to another view, so rows from different views never mix.
    pub fn apply_window(&mut self, window: &QueryWindow) -> bool {
        self.path = Path::Query;
        match &self.view {
            None if window.offset == 0 => {
                self.view = Some(window.view.clone());
                self.total = window.total;
                self.rows.clear();
            }
            Some(v) if *v == window.view => {}
            _ => return false,
        }
        self.pending.retain(|o| *o != window.offset);
        for (i, item) in window.items.iter().enumerate() {
            self.rows.insert(window.offset + i, Row::from(item));
        }
        self.resolve_range();
        self.state = LoadState::Ready;
        true
    }

    pub fn window_failed(&mut self, offset: usize) {
        self.pending.retain(|o| *o != offset);
    }

    /// The agent has no `library_query`: switch to `library_list` paging.
    pub fn use_legacy(&mut self) {
        self.path = Path::Legacy;
        self.reset_legacy();
    }

    pub fn legacy_complete(&self) -> bool {
        self.legacy_complete
    }

    pub fn legacy_loaded(&self) -> usize {
        self.legacy.len()
    }

    /// Appends one `library_list` page and re-derives rows and facets.
    pub fn apply_legacy_page(&mut self, items: Vec<LibraryItem>, complete: bool) {
        self.path = Path::Legacy;
        self.legacy.extend(items);
        self.legacy_complete = complete;
        self.state = LoadState::Ready;
        self.rebuild_legacy();
    }

    fn rebuild_legacy(&mut self) {
        let needle = self.spec.text.to_lowercase();
        let mut rows: Vec<Row> = self
            .legacy
            .iter()
            .filter(|i| needle.is_empty() || i.title.to_lowercase().contains(&needle))
            .filter(|i| self.spec.source.as_ref().is_none_or(|s| i.site_name.as_ref() == Some(s)))
            .filter(|i| self.spec.item_type.as_ref().is_none_or(|t| &i.item_type == t))
            .map(Row::from)
            .collect();
        let key = |r: &Row| -> Option<String> {
            match self.spec.sort {
                SortKey::Title => Some(r.title.clone()),
                SortKey::Source => r.source_key.clone(),
                SortKey::CapturedAt => r.captured_at.clone(),
                SortKey::ItemType => Some(r.item_type.clone()),
                SortKey::AddedAt => r.added_at.clone(),
            }
        };
        let descending = self.spec.descending;
        rows.sort_by(|a, b| {
            let order = match (key(a), key(b)) {
                (Some(x), Some(y)) => if descending { y.cmp(&x) } else { x.cmp(&y) },
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            };
            order.then_with(|| a.item_id.cmp(&b.item_id))
        });
        self.total = rows.len();
        self.rows = rows.into_iter().enumerate().collect();
        let mut sources = BTreeMap::<String, usize>::new();
        let mut types = BTreeMap::<String, usize>::new();
        for item in &self.legacy {
            *sources.entry(item.source().to_string()).or_default() += 1;
            *types.entry(item.item_type.clone()).or_default() += 1;
        }
        let entries = |m: BTreeMap<String, usize>| m.into_iter().map(|(k, c)| FacetEntry { label: k.clone(), key: k, count: c }).collect();
        self.facets = Facets { total: self.legacy.len(), sources: entries(sources), item_types: entries(types) };
    }

    pub fn fail(&mut self, message: String) {
        self.state = LoadState::Failed(message);
    }

    /// A whole listing failed: drop the rows so the list shows the error, not
    /// placeholders for rows that will never load.
    pub fn fail_reload(&mut self, message: String) {
        self.total = 0;
        self.rows.clear();
        self.fail(message);
    }

    /// The muted line above an Inbox that cannot tell filed items from others.
    pub fn inbox_notice(&self) -> Option<&'static str> {
        (self.inbox && self.unfiled_supported == Some(false)).then_some("这台 Mac 的 {0} 还不能区分已归档条目，Inbox 暂按采集时间显示全部条目")
    }

    /// The ids of `id` and every folder below it.
    pub fn subtree_ids(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = self.group(id).map(|g| vec![g.id.clone()]).unwrap_or_default();
        let mut i = 0;
        while i < out.len() {
            let parent = out[i].clone();
            let more: Vec<String> = self.groups.iter().filter(|g| g.parent_id.as_deref() == Some(parent.as_str()) && !out.contains(&g.id)).map(|g| g.id.clone()).collect();
            out.extend(more);
            i += 1;
        }
        out
    }

    /// Empty-state copy for the list area, `None` when rows exist.
    pub fn empty_message(&self) -> Option<String> {
        match &self.state {
            LoadState::Loading if self.total == 0 => Some(tx("正在载入资料库…").into()),
            LoadState::Failed(message) if self.total == 0 => Some(txf("资料库不可用\n{0}", &[&relabel(message)])),
            _ if self.total > 0 => None,
            _ if self.facets.total == 0 && matches!(self.filter(), Filter::All | Filter::Inbox) && self.spec.text.is_empty() => Some(tx("资料库还是空的").into()),
            _ if self.inbox && self.spec.unfiled && self.spec.text.is_empty() && self.spec.origin.is_none() => Some(tx("Inbox 里没有未归档的条目").into()),
            _ => Some(tx("没有符合当前筛选的条目").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Backend, BackendMode, Command, Response};
    use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};

    struct Counting(FixtureBackend, Arc<AtomicUsize>);
    impl Backend for Counting {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode {
            self.0.mode()
        }
    }

    #[test]
    fn unfiltered_load_is_one_query_and_filtered_keeps_library_wide_facets() {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = Client::new(Counting(FixtureBackend::new(300), calls.clone()));
        // Default sort and a non-default sort are both unfiltered.
        for spec in [QuerySpec::default(), QuerySpec { sort: SortKey::Title, descending: false, ..QuerySpec::default() }] {
            calls.store(0, Ordering::SeqCst);
            let (browse, first) = load_first(&client, &spec, Sensitivity::Restricted);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "unfiltered load must not repeat the full query");
            let (b, f) = (browse.unwrap(), first.unwrap());
            assert_eq!((b.total, f.items.len()), (300, WINDOW));
            assert_eq!(b.item_types, f.item_types);
        }
        // A filter: first window is filtered, browse facets still count the whole library.
        let full = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap();
        let src = full.sources[0].clone();
        let spec = QuerySpec { source: Some(src.value.clone()), ..QuerySpec::default() };
        calls.store(0, Ordering::SeqCst);
        let (browse, first) = load_first(&client, &spec, Sensitivity::Restricted);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (b, f) = (browse.unwrap(), first.unwrap());
        assert_eq!(f.total, src.count);
        assert!(f.total < 300);
        assert_eq!(b.total, 300);
        assert_eq!(b.sources, full.sources);
        // Origin changes list results, but the sidebar still describes the entire library.
        let imported = QuerySpec { origin: Some(ItemOrigin::Import), ..QuerySpec::default() };
        let (b, f) = load_first(&client, &imported, Sensitivity::Restricted);
        assert_eq!((b.unwrap().total, f.unwrap().total), (300, 100));
        // A failed first window is reported once, without a second query.
        let offline = FixtureBackend::new(10);
        let mut c = offline.controls();
        c.offline = true;
        offline.set_controls(c);
        let calls2 = Arc::new(AtomicUsize::new(0));
        let oc = Client::new(Counting(offline, calls2.clone()));
        let (b, f) = load_first(&oc, &spec, Sensitivity::Restricted);
        assert!(b.is_err() && f.is_err());
        assert_eq!(calls2.load(Ordering::SeqCst), 1);
    }

    fn first(client: &Client, model: &mut LibraryModel) -> QueryWindow {
        let w = client.library_query(&model.spec, Sensitivity::Restricted, 0, WINDOW, None).unwrap();
        assert!(model.apply_window(&w));
        w
    }

    fn ids(model: &LibraryModel, range: std::ops::RangeInclusive<usize>) -> Vec<String> {
        range.map(|i| model.row(i).unwrap().item_id.clone()).collect()
    }

    #[test]
    fn shift_ranges_run_from_the_open_item_both_ways_and_option_toggles() {
        let client = Client::new(FixtureBackend::new(40));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let open = model.row(5).unwrap().item_id.clone();
        model.select(Some(open.clone()));
        assert_eq!((model.selection(), model.selection_len()), (vec![open.clone()], 1));
        // Down: 5..=9 in list order; the open item (and anchor) stays.
        assert_eq!(model.extend_picked(9), Some(5..10));
        assert_eq!(model.selection(), ids(&model, 5..=9));
        assert_eq!(model.selected(), Some(open.as_str()));
        // Up from the same anchor replaces the range: 2..=5.
        model.extend_picked(2);
        assert_eq!(model.selection(), ids(&model, 2..=5));
        assert!(!model.is_selected(&model.row(6).unwrap().item_id.clone()));
        // Option: one more far away, then one of the range off again.
        let far = model.row(20).unwrap().item_id.clone();
        let three = model.row(3).unwrap().item_id.clone();
        model.toggle_picked(&far);
        model.toggle_picked(&three);
        assert_eq!(model.selection_len(), 4);
        assert!(model.is_selected(&far) && !model.is_selected(&three));
        // From the primary item alone, Option adds to it rather than replacing it.
        model.select(Some(open.clone()));
        model.toggle_picked(&far);
        assert_eq!(model.selection(), vec![open.clone(), far.clone()]);
        // Removing everything deselects without closing the reader.
        model.toggle_picked(&far);
        model.toggle_picked(&open);
        assert!(model.selection().is_empty());
        assert_eq!(model.selected(), Some(open.as_str()));
        // A plain click (the shell's commit) is that item alone.
        model.extend_picked(8);
        let other = model.row(12).unwrap().item_id.clone();
        model.select(Some(other.clone()));
        assert_eq!(model.selection(), vec![other]);
    }

    #[test]
    fn a_shift_range_into_unloaded_rows_waits_for_their_window() {
        let client = Client::new(FixtureBackend::new(300));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let open = model.row(90).unwrap().item_id.clone();
        model.select(Some(open));
        // Row 250 was never loaded: the range is taken by index and loads first.
        assert_eq!(model.extend_picked(250), Some(90..251));
        assert!(model.selection_loading());
        assert_eq!(model.selection_len(), 10, "only rows loaded so far");
        assert_eq!(model.missing_windows(90..251), vec![100, 200]);
        for offset in [100, 200] {
            let w = client.library_query(&model.spec, Sensitivity::Restricted, offset, WINDOW, model.view()).unwrap();
            assert!(model.apply_window(&w));
        }
        assert!(!model.selection_loading());
        assert_eq!(model.selection(), ids(&model, 90..=250));
        // Without the anchor among the rows (another list), Shift is a plain click.
        model.select(Some("item_elsewhere".into()));
        assert_eq!(model.extend_picked(3), None);
        assert_eq!(model.selection(), vec!["item_elsewhere".to_string()]);
    }

    #[test]
    fn select_all_waits_for_every_window_and_cancel_keeps_only_the_reader_anchor() {
        let client = Client::new(FixtureBackend::new(257));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let anchor = model.row(13).unwrap().item_id.clone();
        model.select(Some(anchor.clone()));
        assert_eq!(model.select_all(), 0..257);
        assert!(model.selection_loading());
        assert_eq!(model.missing_windows(0..257), vec![100, 200]);
        for offset in [200, 100] {
            let w = client.library_query(&model.spec, Sensitivity::Restricted, offset, WINDOW, model.view()).unwrap();
            assert!(model.apply_window(&w));
        }
        assert!(!model.selection_loading());
        assert_eq!(model.selection(), ids(&model, 0..=256));
        model.clear_selection();
        assert_eq!(model.selection_len(), 0);
        assert!(!model.is_selected(&anchor));
        assert_eq!(model.selected(), Some(anchor.as_str()));
        model.restart();
        first(&client, &mut model);
        assert_eq!(model.selection_len(), 0, "refresh must not resurrect a cancelled selection");
    }

    #[test]
    fn options_and_folders_clear_the_multi_selection_but_a_refresh_keeps_it() {
        let client = Client::new(FixtureBackend::new(60));
        let mut model = LibraryModel::default();
        model.set_group_tree(client.groups().unwrap(), true);
        first(&client, &mut model);
        let open = model.row(0).unwrap().item_id.clone();
        let pick = |m: &mut LibraryModel| {
            m.select(Some(open.clone()));
            m.extend_picked(4);
            assert_eq!(m.selection_len(), 5);
        };
        pick(&mut model);
        model.restart();
        first(&client, &mut model);
        assert_eq!(model.selection_len(), 5, "a reload of the same list keeps it");
        model.set_search("x");
        assert_eq!(model.selection(), vec![open.clone()]);
        model.set_search("");
        first(&client, &mut model);
        pick(&mut model);
        model.set_filter(Filter::Group("grp_c_bias".into()));
        assert_eq!(model.selection(), vec![open.clone()], "the open item stays; the set goes");
        model.set_filter(Filter::All);
        first(&client, &mut model);
        pick(&mut model);
        model.set_sort(SortKey::Title, false);
        assert_eq!(model.selection_len(), 1);
    }

    #[test]
    fn right_click_inside_acts_on_the_set_and_outside_resets_it() {
        let client = Client::new(FixtureBackend::new(30));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let open = model.row(1).unwrap().item_id.clone();
        model.select(Some(open.clone()));
        model.extend_picked(3);
        let set = ids(&model, 1..=3);
        assert_eq!(model.context_selection(&set[2]), set);
        // Outside: that row alone, and the open item is unchanged.
        let outside = model.row(10).unwrap().item_id.clone();
        assert_eq!(model.context_selection(&outside), vec![outside.clone()]);
        assert_eq!(model.selection(), vec![outside.clone()]);
        assert!(!model.is_selected(&open));
        assert_eq!(model.selected(), Some(open.as_str()));
        // With nothing picked, the open item's own row is the selection.
        model.select(Some(open.clone()));
        assert_eq!(model.context_selection(&open), vec![open]);
    }

    #[test]
    fn folder_targets_stay_in_this_library_and_mapping_lists_only_the_others() {
        let client = Client::new(FixtureBackend::new(30));
        let mut model = LibraryModel::default();
        model.set_group_tree(client.groups().unwrap(), true);
        let ids = |v: Vec<(usize, GroupNode)>| v.into_iter().map(|(_, g)| g.id).collect::<Vec<_>>();
        // 移动到 / 添加到文件夹: the library's root and folders, never the current one.
        let same = ids(model.pick_targets(Some("grp_c_sleep"), true));
        assert_eq!(same, ["grp_z_psych", "grp_c_cog", "grp_c_attention", "grp_c_bias", "grp_c_confirm", "grp_c_anchor"]);
        // 映射到其他库: every other library, none of this one.
        let other = ids(model.pick_targets(Some("grp_c_sleep"), false));
        assert!(other.contains(&"grp_z_ai".to_string()) && other.contains(&"grp_c_week".to_string()));
        assert!(other.iter().all(|id| model.group(id).unwrap().root_group_id != "grp_z_psych"));
        // Outside a library (Inbox, filters): any folder.
        assert_eq!(model.pick_targets(None, true).len(), model.groups().len());
    }

    #[test]
    fn fetches_only_windows_covering_the_visible_range() {
        let client = Client::new(FixtureBackend::new(1000));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        assert_eq!(model.len(), 1000);
        assert!(model.row(0).is_some() && model.row(WINDOW).is_none());
        // Scrolling to rows 450..480 needs exactly the window at 400.
        assert_eq!(model.missing_windows(450..480), vec![400]);
        // The same request is not issued twice while in flight.
        assert!(model.missing_windows(450..480).is_empty());
        let w = client.library_query(&model.spec, Sensitivity::Restricted, 400, WINDOW, model.view()).unwrap();
        assert!(model.apply_window(&w));
        assert!(model.row(450).is_some() && model.row(300).is_none());
    }

    #[test]
    fn stale_view_windows_never_mix_and_selection_survives_restart() {
        let fixture = FixtureBackend::new(300);
        let client = Client::new(fixture.clone());
        let mut model = LibraryModel::default();
        let w = first(&client, &mut model);
        let chosen = w.items[3].item_id.clone();
        model.select(Some(chosen.clone()));
        // A window from another view is refused.
        let mut foreign = w.clone();
        foreign.view = "view_other".into();
        foreign.offset = 100;
        assert!(!model.apply_window(&foreign));
        // An edit elsewhere makes the old view stale; restart refetches offset 0.
        fixture.simulate_external_edit(&w.items[10].item_id);
        assert!(client.library_query(&model.spec, Sensitivity::Restricted, 100, WINDOW, model.view()).unwrap_err().is_stale_view());
        model.restart();
        first(&client, &mut model);
        assert_eq!(model.selected(), Some(chosen.as_str()));
        assert_eq!(model.selected_index(), Some(3));
    }

    #[test]
    fn option_changes_restart_and_empty_states_differ() {
        let client = Client::new(FixtureBackend::new(40));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        model.set_search("不存在的词🌹");
        assert_eq!(model.view(), None);
        first(&client, &mut model);
        assert!(model.is_empty());
        let browse = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap();
        model.set_browse_facets(&browse);
        assert_eq!(model.empty_message().as_deref(), Some("没有符合当前筛选的条目"));
        let none = Client::new(FixtureBackend::new(0));
        let mut empty = LibraryModel::default();
        empty.apply_window(&none.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap());
        assert_eq!(empty.empty_message().as_deref(), Some("资料库还是空的"));
        // A failed reload after rows were shown reports the failure, not placeholders.
        model.restart();
        model.fail_reload("无法连接本机代理".into());
        assert_eq!(model.len(), 0);
        assert_eq!(model.empty_message().as_deref(), Some("资料库不可用\n无法连接本机代理"));
    }

    #[test]
    fn folder_filter_follows_the_view_switch_and_counts_switch_with_it() {
        let client = Client::new(FixtureBackend::new(128));
        let mut model = LibraryModel::default();
        model.set_group_tree(client.groups().unwrap(), true);
        let bias = model.group("grp_c_bias").unwrap().clone();
        assert_eq!(model.group_path("grp_c_anchor").iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["心理学研究组", "认知", "认知偏差", "锚定效应"]);
        // Off by default: direct members and item_count.
        assert_eq!(model.group_count(&bias), bias.item_count);
        model.origin_supported = Some(true);
        model.set_origin(Some(ItemOrigin::Capture));
        model.set_filter(Filter::Group(bias.id.clone()));
        assert_eq!(model.spec.origin, None, "opening a folder must not hide its imported items");
        assert!(!is_unfiltered(&model.spec));
        first(&client, &mut model);
        assert_eq!(model.len(), bias.item_count);
        // On: the query is restarted with include_descendants and the count is the subtree union.
        assert!(model.set_include_descendants(true));
        assert_eq!(model.view(), None);
        first(&client, &mut model);
        assert_eq!((model.len(), model.group_count(&bias)), (bias.subtree_item_count, bias.subtree_item_count));
        // Without a folder the switch only changes counts; the next folder uses it.
        model.set_filter(Filter::All);
        assert!(!model.set_include_descendants(false));
        assert!(!model.set_include_descendants(true) && model.spec.group.is_none());
        model.set_filter(Filter::Group("grp_c_cog".into()));
        assert_eq!(model.spec.group.as_ref().map(|g| g.include_descendants), Some(true));
        // An explicit origin choice within this folder is still respected.
        model.set_origin(Some(ItemOrigin::Import));
        assert_eq!(model.spec.origin, Some(ItemOrigin::Import));
    }

    #[test]
    fn rows_show_added_at_only_and_carry_their_full_title_for_the_tooltip() {
        let client = Client::new(FixtureBackend::new(12));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let rows: Vec<Row> = (0..model.len()).filter_map(|i| model.row(i).cloned()).collect();
        let missing = rows.iter().find(|r| r.item_id == "item_fixture_000004").unwrap();
        // Not recorded stays not recorded, although the item has a capture time.
        assert!(missing.captured_at.is_some());
        assert_eq!(missing.added_label(), "未记录");
        let present = rows.iter().find(|r| r.item_id == "item_fixture_000001").unwrap();
        assert_eq!(present.added_label().len(), 10);
        // The tooltip is the whole title, however long; a row still loading has none.
        let long = rows.iter().find(|r| r.item_id == "item_fixture_000000").unwrap();
        assert_eq!(row_tooltip(Some(long)).as_deref(), Some(long.title.as_str()));
        assert!(long.title.chars().count() > 30);
        assert_eq!(row_tooltip(None), None);
    }

    #[test]
    fn rows_show_publication_day_and_duration_from_their_own_fields() {
        let client = Client::new(FixtureBackend::new(40));
        let mut model = LibraryModel::default();
        first(&client, &mut model);
        let rows: Vec<Row> = (0..model.len()).filter_map(|i| model.row(i).cloned()).collect();
        let row = |id: &str| rows.iter().find(|r| r.item_id == id).unwrap().clone();
        let podcast = row(memex_client::fixture::PODCAST_ITEM);
        assert_eq!((podcast.published_label(), podcast.duration_label()), ("2026-10-30".into(), "1:00:26".into()));
        assert_eq!(podcast.list_meta().as_deref(), Some("2026-10-30 · 1:00:26"));
        // Duration only (an unparsed date): the duration alone, no stand-in date.
        let short = row("item_fixture_000004");
        assert_eq!((short.published_label(), short.duration_label(), short.list_meta()), ("—".into(), "00:59".into(), Some("00:59".into())));
        // A year-only date and no duration: dashes; the list falls back to the labelled 采集 date.
        let year_only = row("item_fixture_000005");
        assert_eq!((year_only.published_label(), year_only.duration_label()), ("—".into(), "—".into()));
        assert_eq!(year_only.list_meta(), year_only.added_local_date().map(|d| format!("采集 {d}")));
        let day = row("item_fixture_000001");
        assert_eq!(day.published_at.as_ref().map(String::len), Some(10));
        assert_eq!(day.list_meta(), day.published_at.clone());
        // Legacy pages carry the same fields.
        let mut legacy = LibraryModel::default();
        legacy.use_legacy();
        let page = client.library_page(None).unwrap();
        legacy.apply_legacy_page(page.items, page.next_cursor.is_none());
        let r = (0..legacy.len()).filter_map(|i| legacy.row(i)).find(|r| r.item_id == memex_client::fixture::PODCAST_ITEM).unwrap();
        assert_eq!((r.published_at.as_deref(), r.duration_seconds), (Some("2026-10-30"), Some(3626)));
        // 原始发表日期 stays unavailable: no `published_at` sort is negotiated.
        assert_eq!(ListSort::Published.query(), None);
    }

    #[test]
    fn legacy_pages_show_immediately_and_filter_loaded_items() {
        let client = Client::new(FixtureBackend::new(120));
        let mut model = LibraryModel::default();
        model.use_legacy();
        let page = client.library_page(None).unwrap();
        model.apply_legacy_page(page.items, page.next_cursor.is_none());
        assert_eq!(model.len(), 50);
        assert!(!model.legacy_complete());
        model.set_filter(Filter::ItemType("论文".into()));
        assert!(model.len() > 0 && model.len() < 50);
        assert!((0..model.len()).all(|i| model.row(i).unwrap().item_type == "论文"));
    }

    #[test]
    fn list_sort_maps_only_what_the_agent_can_sort_and_defaults_to_capture_time() {
        assert_eq!(ListSort::of((QuerySpec::default().sort, QuerySpec::default().descending)), Some(ListSort::Captured));
        assert_eq!(ListSort::AddedToLibrary.query(), Some((SortKey::AddedAt, true)));
        assert_eq!(ListSort::Published.query(), None);
        for s in [ListSort::AddedToLibrary, ListSort::Captured, ListSort::Title, ListSort::Source] {
            assert_eq!(ListSort::of(s.query().unwrap()), Some(s));
        }
        assert_eq!(ListSort::of((SortKey::ItemType, false)), None, "a table-only order has no menu entry");
        let mut m = LibraryModel::default();
        m.set_sort(SortKey::Title, false);
        assert_eq!(ListSort::of(m.sort()), Some(ListSort::Title));
    }

    #[test]
    fn a_refused_added_at_order_goes_back_to_capture_time_once_and_rows_carry_titles() {
        let mut m = LibraryModel::default();
        m.set_sort(SortKey::AddedAt, true);
        assert!(!m.on_sort_refused(&ClientError::Timeout), "a timeout is not a refusal");
        assert!(m.on_sort_refused(&ClientError::Unsupported));
        assert_eq!((m.sort(), m.added_at_rejected), ((SortKey::CapturedAt, true), true));
        assert!(!m.on_sort_refused(&ClientError::Unsupported), "only for the added_at order");
        // Rows keep the original title for sorting and show the translation first.
        let fixture = memex_client::fixture::FixtureBackend::new(30);
        let client = Client::new(fixture);
        let w = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 30, None).unwrap();
        let row = w.items.iter().find(|i| i.item_id == memex_client::fixture::TITLE_ONLY_ITEM).map(Row::from).unwrap();
        assert_eq!(row.title, "Attention Is Not All You Need: Revisiting Recurrent Memory");
        assert_eq!(row.display_title(), "注意力并非全部：重新审视循环记忆 ｜ Attention Is Not All You Need: Revisiting Recurrent Memory");
        assert_eq!(row_tooltip(Some(&row)), Some(row.display_title()));
        assert_eq!(w.items.iter().find(|i| i.item_id == memex_client::fixture::TITLE_ONLY_ITEM).unwrap().title_source.as_deref(), Some("title_only"));
    }
}
