//! GPUI views of the library module. `Library` owns the model and all backend
//! requests; the sidebar, the classic table and the compact list are separate
//! views that only read it and send intents back. The shell decides whether a
//! requested selection is committed (navigation guard), so selection changes leave
//! this module only as `LibraryEvent`s.

use crate::edit::{GroupEditor, Outcome};
use crate::model::{row_tooltip, Filter, LibraryModel, LoadState, Path, Row, WINDOW};
pub use crate::tree::{folder_header, group_picker, FolderTree};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    input::{Input, InputEvent, InputState},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::base::Disableable as _;
use memex_client::{item_type_label, relabel, source_display, tx, txf, Client, ClientError, GroupEditOp, ItemOrigin, Lang, Sensitivity, SortKey};
use std::ops::Range;

/// The desktop is the local owner's own view of its library.
pub const OWNER_CEILING: Sensitivity = Sensitivity::Restricted;

actions!(memex_library, [DeleteSelectedItem, SelectAllItems, ClearItemSelection, RenameSelectedItem, SelectAllNodes, ClearNodeSelection, RenameSelectedNode, DeleteSelectedNodes]);

/// Key context of the item list / table. Text fields bind Backspace themselves,
/// deeper in their own context, so typing there never reaches this.
pub const ITEMS_CONTEXT: &str = "MemexItems";

/// Backspace (the Mac delete key), forward Delete and ⌘⌫ move the selected
/// item to its library's trash, as 移入本库垃圾桶 does.
pub fn key_bindings() -> Vec<KeyBinding> {
    let mut keys: Vec<_> = ["backspace", "delete", "cmd-backspace"].into_iter().map(|k| KeyBinding::new(k, DeleteSelectedItem, Some(ITEMS_CONTEXT))).collect();
    keys.extend([
        KeyBinding::new("cmd-a", SelectAllItems, Some(ITEMS_CONTEXT)),
        KeyBinding::new("escape", ClearItemSelection, Some(ITEMS_CONTEXT)),
        KeyBinding::new("escape", ClearItemSelection, Some("MemexItems > DataTable")),
        KeyBinding::new("cmd-shift-a", ClearItemSelection, Some(ITEMS_CONTEXT)),
        KeyBinding::new("enter", RenameSelectedItem, Some(ITEMS_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAllNodes, Some("MemexFolders")),
        KeyBinding::new("escape", ClearNodeSelection, Some("MemexFolders")),
        KeyBinding::new("cmd-shift-a", ClearNodeSelection, Some("MemexFolders")),
        KeyBinding::new("enter", RenameSelectedNode, Some("MemexFolders")),
    ]);
    keys.extend(["backspace", "delete", "cmd-backspace"].into_iter().map(|k| KeyBinding::new(k, DeleteSelectedNodes, Some("MemexFolders"))));
    keys
}

/// Internal drags freeze the items and their source view, independently of Finder drops.
#[derive(Clone)]
pub struct ItemDrag {
    pub rows: Vec<Row>,
    source: Option<String>,
    generation: u64,
}

impl Render for ItemDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().px_3().py_2().rounded_md().bg(cx.theme().popover).text_color(cx.theme().foreground)
            .child(txf("{0} 条 · ⌘ 移动 / 松开映射", &[&self.rows.len()]))
    }
}

struct ItemRename {
    row: Row,
    input: Entity<InputState>,
    saving: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    /// The user picked an item; the shell commits it with `Library::commit_selection`.
    SelectionRequested(String),
    /// Open in a new reader tab (double click / Enter).
    OpenRequested(String),
    /// Return on exactly one selected item; the shell guards unsaved edits first.
    RenameRequested(String),
    ItemRenamed(String),
    /// A group edit was applied (the tree is being reloaded).
    GroupsEdited,
    /// 批量格式整理 of this folder (`reader_content_format_batch_v1`).
    FormatBatch { group_id: String, name: String },
    /// Files from Finder were dropped on a sidebar row (the shell imports them).
    FilesDropped { paths: Vec<std::path::PathBuf>, spot: crate::import::DropSpot },
}

/// 删除… of one library / folder (`groups_v1.node_delete`), shown at its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteFlow {
    /// Reading the server's preview of what would be removed.
    Previewing { group_id: String, notice: Option<String> },
    /// The preview, waiting for 删除 / 取消; `notice` says why it is shown again.
    Confirm { preview: memex_client::group_delete::DeletePreview, notice: Option<String> },
    Deleting { preview: memex_client::group_delete::DeletePreview },
    Failed { group_id: String, message: String },
}

impl DeleteFlow {
    pub fn group_id(&self) -> &str {
        match self {
            DeleteFlow::Previewing { group_id, .. } | DeleteFlow::Failed { group_id, .. } => group_id,
            DeleteFlow::Confirm { preview, .. } | DeleteFlow::Deleting { preview } => &preview.group_id,
        }
    }
}

/// A rejected confirmation (expired, or the subtree changed since the preview).
fn confirmation_rejected(e: &ClientError) -> bool {
    matches!(e, ClientError::Daemon(c) if matches!(c.as_str(), "group_delete_preview_not_found" | "group_delete_preview_expired" | "stale_group_delete_preview"))
}

/// A restore refused because an ancestor is itself still deleted (the list only
/// shows outstanding deletions, so the shared conflict code means that here).
fn parent_inactive(e: &ClientError) -> bool {
    matches!(e, ClientError::Daemon(c) if c == "group_delete_restore_conflict")
}

/// What the folder list of the item menu does with the selection. A library
/// is a root group; its folders are below it. One item may sit in several
/// folders of a library, so moving between folders is the common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// 移动到文件夹…: to another folder of this library: added there, then
    /// removed from the current folder.
    MoveToFolder,
    /// 添加到文件夹…: also filed in another folder of this library (outside a
    /// library: in any folder); it stays where it is.
    AddToFolder,
    /// 映射到其他库…: the same item (not a copy) also shown in another library.
    /// 复制到其他库… (an independent copy) needs an agent operation that does
    /// not exist yet, so it is offered disabled, never imitated by a mapping.
    MapToLibrary,
}

/// The item menu of a right-clicked row. Its actions apply to the whole
/// selection (`LibraryModel::selection`), which the right-click settled.
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub item_id: String,
    pub position: Point<Pixels>,
    /// Showing the folder list (after one of the filing actions was chosen).
    pub choosing: Option<PickMode>,
}

pub struct Library {
    client: Client,
    pub model: LibraryModel,
    pub editor: GroupEditor,
    pub picker: Option<Picker>,
    pub search: Entity<InputState>,
    /// 回收站 (`dedup_trash_v1`).
    pub trash: crate::trash::Trash,
    /// The trash list is shown instead of the items.
    pub show_trash: bool,
    pub group_trash_available: bool,
    /// The agent formats whole folders (`reader_content_format_batch_v1`).
    pub format_batch: bool,
    pub trash_group: Option<String>,
    pub trash_busy: bool,
    pub trash_notice: Option<String>,
    /// 加入库… / 移入库… of a selection is running (one at a time).
    pub member_busy: bool,
    /// The last filing result, shown in the status line.
    pub member_notice: Option<String>,
    /// A 移动到文件夹… stopped at an unknown outcome: 重试 resends the same requests.
    pub move_plan: Option<crate::edit::MovePlan>,
    /// The agent lists `move_members` in `groups_v1.write_ops` (atomic folder move).
    pub move_atomic: bool,
    /// Why the order just changed by itself (an agent refused 最近加入资料库).
    pub sort_notice: Option<String>,
    /// `groups_v1.node_delete`: 删除… and 已删除的库 are offered.
    pub node_delete: bool,
    pub delete: Option<DeleteFlow>,
    delete_queue: Vec<String>,
    rename: Option<ItemRename>,
    /// Outstanding deletions (已删除的库), as last listed.
    pub deletions: Vec<memex_client::group_delete::Deletion>,
    pub restoring: Option<String>,
    pub deletion_notice: Option<String>,
    /// Delete / restore request ids, written before each send so an unknown
    /// outcome is retried with the same id (`group-delete-requests.json`).
    delete_requests: std::collections::HashMap<String, String>,
    delete_store: Option<std::path::PathBuf>,
    deletions_ticket: u64,
    /// The last listing failure was `library_loading` (busy, not bad data).
    pub load_busy: bool,
    trash_ticket: u64,
    /// Focused by a click in the item list / table; the delete keys act only then.
    pub items_focus: FocusHandle,
    generation: u64,
    /// Language the search placeholder was last set in.
    placeholder_lang: Lang,
    /// Explicit sidebar choices so far; a report sees whether it is still the latest.
    active_group_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The latest report; the next one runs after it (never two at once).
    active_group_task: Option<Task<()>>,
    /// The sidebar row a file drag from Finder is over (for the drop hint).
    pub drop_hover: Option<crate::import::DropSpot>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LibraryEvent> for Library {}

impl Library {
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tx("搜索标题")));
        let subscription = cx.subscribe_in(&search, window, |this, state, event, _window, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                this.model.set_search(&text);
                this.reload(cx);
            }
        });
        let mut model = LibraryModel::default();
        // Inbox is where captures and imports arrive: selected at start.
        model.set_filter(Filter::Inbox);
        let mut this = Self { client, model, editor: GroupEditor::default(), picker: None, search, trash: Default::default(), show_trash: false, group_trash_available: false, format_batch: false, trash_group: None, trash_busy: false, trash_notice: None, member_busy: false, member_notice: None, move_plan: None, move_atomic: false, sort_notice: None, node_delete: false, delete: None, delete_queue: vec![], rename: None, deletions: vec![], restoring: None, deletion_notice: None, delete_requests: Default::default(), delete_store: None, deletions_ticket: 0, load_busy: false, trash_ticket: 0, items_focus: cx.focus_handle(), generation: 0, placeholder_lang: memex_client::i18n::language(), active_group_seq: Default::default(), active_group_task: None, drop_hover: None, _subscriptions: vec![subscription] };
        this.reload(cx);
        this
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Where pending delete / restore request ids are kept (beside the settings).
    pub fn set_request_store(&mut self, path: Option<std::path::PathBuf>) {
        self.delete_requests = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        self.delete_store = path;
    }

    fn save_delete_requests(&self) {
        if let (Some(path), Ok(bytes)) = (&self.delete_store, serde_json::to_vec(&self.delete_requests)) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// The id for `key`, persisted before it is sent.
    fn request_for(&mut self, key: String) -> String {
        if let Some(id) = self.delete_requests.get(&key) { return id.clone(); }
        let id = memex_client::biblio::new_request_id();
        self.delete_requests.insert(key, id.clone());
        self.save_delete_requests();
        id
    }

    fn forget_request(&mut self, key: &str) {
        if self.delete_requests.remove(key).is_some() { self.save_delete_requests(); }
    }

    /// 删除… is offered for `id`: the agent has node deletion and the node is writable.
    pub fn can_delete(&self, id: &str) -> bool {
        self.node_delete && self.model.group(id).is_some_and(|g| !g.read_only)
    }

    /// Every selected subtree uses the existing preview/confirmation/restore flow.
    /// Descendants of selected ancestors are not deleted a second time.
    pub fn delete_nodes(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if self.delete.is_some() { return; }
        self.delete_queue = ids.iter().filter(|id| self.can_delete(id)
            && !self.model.group_path(id).iter().any(|g| &g.id != *id && ids.contains(&g.id)))
            .cloned().collect();
        self.next_node_delete(cx);
    }

    fn next_node_delete(&mut self, cx: &mut Context<Self>) {
        if !self.delete_queue.is_empty() {
            let id = self.delete_queue.remove(0);
            self.begin_delete(id, None, cx);
        }
    }

    /// 删除…: reads the server's preview first; nothing is deleted yet.
    pub fn begin_delete(&mut self, group_id: String, notice: Option<String>, cx: &mut Context<Self>) {
        if !self.can_delete(&group_id) || matches!(self.delete, Some(DeleteFlow::Deleting { .. })) { return; }
        self.delete = Some(DeleteFlow::Previewing { group_id: group_id.clone(), notice: notice.clone() });
        cx.notify();
        let client = self.client.clone();
        let id = group_id.clone();
        let task = cx.background_executor().spawn(async move { client.group_delete_preview(&id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.delete.as_ref().map(DeleteFlow::group_id) != Some(group_id.as_str()) { return; }
                this.delete = Some(match result {
                    Ok(preview) => DeleteFlow::Confirm { preview, notice },
                    Err(e) => DeleteFlow::Failed { group_id, message: e.message() },
                });
                cx.notify();
            }).ok();
        }).detach();
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.delete, Some(DeleteFlow::Deleting { .. })) {
            self.delete = None;
            self.delete_queue.clear();
            cx.notify();
        }
    }

    /// 删除 in the confirmation: one `group_delete` with the preview's token and a
    /// request id written beforehand. Afterwards the tree, Inbox and deletion list
    /// are read again; the reply's `outcome` is never applied by itself.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(DeleteFlow::Confirm { preview, .. }) = self.delete.clone() else { return };
        let key = format!("delete|{}|{}", preview.group_id, preview.confirmation_token);
        let request_id = self.request_for(key.clone());
        let gone = self.model.subtree_ids(&preview.group_id);
        self.delete = Some(DeleteFlow::Deleting { preview: preview.clone() });
        cx.notify();
        let client = self.client.clone();
        let p = preview.clone();
        let task = cx.background_executor().spawn(async move { client.group_delete(&p.group_id, &p.confirmation_token, &request_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.forget_request(&key);
                        this.delete = None;
                        let shown = matches!(this.model.filter(), Filter::Group(id) if gone.contains(&id)) || this.trash_group.as_ref().is_some_and(|t| gone.contains(t));
                        if shown {
                            this.show_trash = false;
                            this.trash_group = None;
                            this.model.set_filter(Filter::Inbox);
                        }
                        cx.emit(LibraryEvent::GroupsEdited);
                        this.reload(cx);
                        this.next_node_delete(cx);
                    }
                    // Not known to have happened: 删除 again resends this same request.
                    Err(e) if e.is_transport() || e.is_busy() => {
                        this.delete = Some(DeleteFlow::Confirm { preview, notice: Some(txf("删除结果未知：{0}。再次点击删除会重试同一请求，不会重复删除。", &[&e.message()])) });
                    }
                    Err(e) if confirmation_rejected(&e) => {
                        this.forget_request(&key);
                        this.delete = None;
                        this.begin_delete(preview.group_id.clone(), Some(tx("内容已变化，请重新确认").into()), cx);
                        this.reload(cx);
                    }
                    Err(e) => {
                        this.forget_request(&key);
                        this.delete = Some(DeleteFlow::Failed { group_id: preview.group_id.clone(), message: e.message() });
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// 已删除的库: every outstanding deletion (nothing without node deletion).
    pub fn load_deletions(&mut self, cx: &mut Context<Self>) {
        self.deletions_ticket += 1;
        let ticket = self.deletions_ticket;
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.group_delete_list() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.deletions_ticket != ticket { return; }
                match result {
                    Ok(list) => { this.node_delete = true; this.deletions = list; }
                    Err(ClientError::Unsupported) => { this.node_delete = false; this.deletions.clear(); this.delete = None; }
                    Err(e) => this.deletion_notice = Some(e.message()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// 恢复 of one deletion; afterwards everything is read again.
    pub fn restore_deletion(&mut self, deletion_id: String, cx: &mut Context<Self>) {
        if self.restoring.is_some() || !self.node_delete { return; }
        let key = format!("restore|{deletion_id}");
        let request_id = self.request_for(key.clone());
        self.restoring = Some(deletion_id.clone());
        self.deletion_notice = None;
        cx.notify();
        let client = self.client.clone();
        let id = deletion_id.clone();
        let task = cx.background_executor().spawn(async move { client.group_delete_restore(&id, &request_id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.restoring = None;
                match result {
                    Ok(()) => {
                        this.forget_request(&key);
                        cx.emit(LibraryEvent::GroupsEdited);
                        this.reload(cx);
                    }
                    Err(e) if e.is_transport() || e.is_busy() => {
                        this.deletion_notice = Some(txf("恢复结果未知：{0}。再次点击恢复会重试同一请求。", &[&e.message()]));
                    }
                    Err(e) => {
                        this.forget_request(&key);
                        this.deletion_notice = Some(if parent_inactive(&e) { tx("请先恢复上级").into() } else { e.message() });
                        this.load_deletions(cx);
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Cheap change check (one-row query) used when the window regains focus: captures made
    /// elsewhere (e.g. the Chrome extension) appear without a manual refresh, while an
    /// unchanged library keeps its scroll position and loaded rows.
    pub fn refresh_if_changed(&mut self, cx: &mut Context<Self>) {
        if self.model.path != Path::Query || self.model.state != LoadState::Ready {
            return;
        }
        let ticket = self.generation;
        let (total, top) = (self.model.len(), self.model.row(0).map(|r| r.item_id.clone()));
        let client = self.client.clone();
        let spec = self.model.spec.clone();
        let task = cx.background_executor().spawn(async move { client.library_query(&spec, OWNER_CEILING, 0, 1, None) });
        cx.spawn(async move |this, cx| {
            let Ok(window) = task.await else { return };
            this.update(cx, |this, cx| {
                let changed = window.total != total || window.items.first().map(|i| &i.item_id) != top.as_ref();
                if this.generation == ticket && changed {
                    this.reload(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Restarts the current query from offset zero (also after a stale view).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_busy = false;
        let ticket = self.next_generation();
        if self.model.path == Path::Legacy {
            self.model.reset_legacy();
            self.load_legacy_page(None, ticket, cx);
            cx.notify();
            return;
        }
        self.model.restart();
        cx.notify();
        self.load_groups(ticket, cx);
        self.load_trash(cx);
        self.load_deletions(cx);
        let client = self.client.clone();
        let mut spec = self.model.spec.clone();
        let inbox = self.model.filter() == Filter::Inbox;
        let task = cx.background_executor().spawn(async move {
            let supported = match client.agent_version() {
                Ok(v) => v.query_origin,
                Err(ClientError::Unsupported) => false,
                Err(e) => return (None, None, None, spec, (Err(e.clone()), Err(e))),
            };
            if !supported { spec.origin = None; }
            // `unfiled` only where advertised; the Inbox then shows everything.
            let unfiled = match client.capabilities() {
                Ok(c) => c.group_query_unfiled,
                Err(e) => return (Some(supported), None, None, spec, (Err(e.clone()), Err(e))),
            };
            spec.unfiled = inbox && unfiled;
            let pages = crate::model::load_first(&client, &spec, OWNER_CEILING);
            // The Inbox count: the agent's own total of unfiled items, else nothing.
            let plain = memex_client::QuerySpec { unfiled: true, ..Default::default() };
            let count = match &pages.1 {
                _ if !unfiled => None,
                Ok(w) if spec.text.is_empty() && spec.origin.is_none() && spec.unfiled => Some(w.total),
                _ => client.library_query(&plain, OWNER_CEILING, 0, 1, None).ok().map(|w| w.total),
            };
            (Some(supported), Some(unfiled), count, spec, pages)
        });
        cx.spawn(async move |this, cx| {
            let (supported, unfiled, inbox_count, spec, (facets, first)) = task.await;
            this.update(cx, |this, cx| {
                if this.generation != ticket {
                    return;
                }
                if supported.is_some() { this.model.origin_supported = supported; }
                if unfiled.is_some() {
                    this.model.unfiled_supported = unfiled;
                    this.model.inbox_count = inbox_count;
                }
                this.model.spec = spec;
                match first {
                    Ok(window) => {
                        if let Ok(f) = facets {
                            this.model.set_browse_facets(&f);
                        }
                        this.model.apply_window(&window);
                    }
                    // An agent before `added_at`: back to 抓取时间 (said in the menu), not legacy.
                    Err(e) if this.model.on_sort_refused(&e) => {
                        this.sort_notice = Some(tx("当前代理还不支持按加入时间排序，已改回抓取时间").into());
                        this.reload(cx);
                        return;
                    }
                    Err(ClientError::Unsupported) => {
                        this.model.use_legacy();
                        this.load_legacy_page(None, ticket, cx);
                    }
                    Err(e) => {
                        // Busy after the bounded retries: the status line offers 重试.
                        this.load_busy = e.is_busy();
                        this.model.fail_reload(e.message());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the trash (and whether the agent has one) for the sidebar count and list.
    pub fn load_trash(&mut self, cx: &mut Context<Self>) {
        self.trash_ticket += 1;
        let ticket = self.trash_ticket;
        self.trash.begin_load();
        let client = self.client.clone();
        let group = self.trash_group.clone();
        let task = cx.background_executor().spawn(async move {
            match group {
                Some(id) => client.group_trash_all(&id).map(|items| (items, Default::default())),
                None => crate::trash::load(&client),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.trash_ticket != ticket {
                    return;
                }
                this.trash.on_load(result);
                if !this.trash.visible() {
                    this.show_trash = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows the trash list (or the items again).
    pub fn set_show_trash(&mut self, show: bool, cx: &mut Context<Self>) {
        self.trash_group = None;
        self.show_trash = show && self.trash.visible();
        if self.show_trash {
            self.trash.message = None;
            self.load_trash(cx);
        }
        cx.notify();
    }

    pub fn show_group_trash(&mut self, root: String, cx: &mut Context<Self>) {
        if !self.group_trash_available { return; }
        self.trash_group = Some(root);
        self.trash = Default::default();
        self.show_trash = true;
        self.load_trash(cx);
        cx.notify();
    }

    pub fn current_root(&self) -> Option<String> {
        let Filter::Group(id) = self.model.filter() else { return None };
        self.model.group(&id).map(|g| g.root_group_id.clone())
    }

    pub fn delete_from_group(&mut self, item_id: &str, cx: &mut Context<Self>) {
        self.delete_item(item_id, None, cx);
    }

    /// The delete keys on the selected item: the same trash move as the menu,
    /// then the next row (or the previous one at the end) is selected. Where
    /// the menu offers no deletion this does nothing, or says where it works.
    pub fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.show_trash || self.trash_busy || !self.group_trash_available { return; }
        if self.model.selection_len() > 1 {
            if self.current_root().is_none() {
                self.trash_notice = Some(tx("进入一个库后可删除条目").into());
                cx.notify();
                return;
            }
            self.delete_selection(cx);
            return;
        }
        let (Some(id), Some(ix)) = (self.model.selection().first().cloned(), self.model.selected_index()) else { return };
        if self.current_root().is_none() {
            self.trash_notice = Some(tx("进入一个库后可删除条目").into());
            cx.notify();
            return;
        }
        let next = self.model.row(ix + 1).or_else(|| ix.checked_sub(1).and_then(|i| self.model.row(i))).map(|r| r.item_id.clone());
        self.delete_item(&id, next, cx);
    }

    /// 移入本库垃圾桶 of several items: one move per item at its own revision.
    /// The items that were not confirmed stay selected; when the open item
    /// went, the first row after the selection is opened.
    pub fn delete_selection(&mut self, cx: &mut Context<Self>) {
        if self.trash_busy || !self.group_trash_available || self.model.selection_loading() { return; }
        let ids = self.model.selection();
        if ids.len() <= 1 {
            if let Some(id) = ids.first() { self.delete_item(&id.clone(), None, cx); }
            return;
        }
        let Some(root) = self.current_root() else { return };
        let rows: Vec<Row> = ids.iter().filter_map(|id| self.row_for(id)).collect();
        let last = rows.iter().filter_map(|r| (0..self.model.len()).find(|i| self.model.row(*i).is_some_and(|x| x.item_id == r.item_id))).max();
        let first = rows.iter().filter_map(|r| (0..self.model.len()).find(|i| self.model.row(*i).is_some_and(|x| x.item_id == r.item_id))).min();
        let outside = |i: usize| self.model.row(i).filter(|r| !ids.contains(&r.item_id)).map(|r| r.item_id.clone());
        let next = last.and_then(|l| outside(l + 1)).or_else(|| first.and_then(|f| f.checked_sub(1)).and_then(outside));
        self.close_picker(cx);
        self.trash_busy = true;
        self.member_notice = None;
        self.trash_notice = Some(tx("正在移入本库垃圾桶…").into());
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            rows.into_iter().map(|row| {
                let result = client.group_trash_edit(&root, &row.item_id, &row.revision_id, &memex_client::biblio::new_request_id(), None);
                (row.item_id, result)
            }).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            this.update(cx, |this, cx| {
                this.trash_busy = false;
                let failed: Vec<String> = results.iter().filter(|(_, r)| r.is_err()).map(|(id, _)| id.clone()).collect();
                let moved = results.len() - failed.len();
                this.trash_notice = Some(match results.iter().find_map(|(_, r)| r.as_ref().err()) {
                    None => txf("已移入本库垃圾桶 {0} 条；其他库不受影响", &[&moved]),
                    Some(e) => txf("已移入本库垃圾桶 {0} 条；{1} 条未确认，仍保持选中，请查看本库垃圾桶：{2}", &[&moved, &failed.len(), &e.message()]),
                });
                let primary_gone = this.model.selected().is_some_and(|p| results.iter().any(|(id, r)| id == p && r.is_ok()));
                this.model.set_picked(failed);
                if let Some(next) = next.filter(|_| primary_gone) { this.request_select(next, cx); }
                this.reload(cx);
            }).ok();
        }).detach();
        cx.notify();
    }

    fn delete_item(&mut self, item_id: &str, then_select: Option<String>, cx: &mut Context<Self>) {
        if self.trash_busy || !self.group_trash_available { return; }
        let (Some(root), Some(row)) = (self.current_root(), self.row_for(item_id)) else { return };
        self.close_picker(cx);
        self.trash_busy = true;
        self.member_notice = None;
        self.trash_notice = Some(tx("正在移入本库垃圾桶…").into());
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            client.group_trash_edit(&root, &row.item_id, &row.revision_id, &memex_client::biblio::new_request_id(), None)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.trash_busy = false;
                let deleted = result.is_ok();
                this.trash_notice = Some(match result {
                    Ok(()) => tx("已移入本库垃圾桶；其他库不受影响").into(),
                    Err(e) => txf("删除未确认，请查看本库垃圾桶：{0}", &[&e.message()]),
                });
                // Asked while the rows still hold its title; the shell's guard applies.
                if let Some(next) = then_select.filter(|_| deleted) { this.request_select(next, cx); }
                this.reload(cx);
            }).ok();
        }).detach();
        cx.notify();
    }

    /// 恢复: puts the item back, then reads the library and the trash again.
    pub fn restore_from_trash(&mut self, item_id: &str, cx: &mut Context<Self>) {
        if !self.trash.begin_restore(item_id) {
            return;
        }
        cx.notify();
        let client = self.client.clone();
        let id = item_id.to_string();
        let group = self.trash_group.clone();
        let expected_group = group.clone();
        let ticket = self.trash_ticket;
        let entry = self.trash.entries.iter().find(|e| e.item_id == id).cloned();
        let task = cx.background_executor().spawn(async move {
            if let Some(group) = group {
                let entry = entry.ok_or(ClientError::InvalidFrame)?;
                let plan = entry.plan_id.as_deref().ok_or(ClientError::InvalidFrame)?;
                client.group_trash_edit(&group, &id, &entry.revision_id, &memex_client::biblio::new_request_id(), Some(plan))?;
                Ok(memex_client::trash::TrashRestore { item_id: id, revision_id: entry.revision_id, restored: true })
            } else { client.trash_restore(&id) }
        });
        let id = item_id.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.trash_group != expected_group || this.trash_ticket != ticket { return; }
                if this.trash.on_restore(&id, result) {
                    // The trash was read before the reload asks again; keep the message.
                    let message = this.trash.message.clone();
                    this.reload(cx);
                    this.trash.message = message;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Folder nodes for the sidebar. An agent without `groups_v1` answers
    /// unsupported and the section says so.
    fn load_groups(&mut self, ticket: u64, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.groups().map(|tree| (tree, client.capabilities().map(|c| (c.group_writes, c.group_reorder, c.group_trash, c.features.iter().any(|f| f == memex_client::format::FORMAT_FEATURE) && c.features.iter().any(|f| f == memex_client::format::FORMAT_BATCH_FEATURE), c.group_move_members, c.group_transfer_folders)).unwrap_or_default())) });
        cx.spawn(async move |this, cx| {
            let groups = task.await;
            this.update(cx, |this, cx| {
                if this.generation != ticket {
                    return;
                }
                // Keep the last tree on a transient failure; drop it when unsupported.
                match groups {
                    Ok((tree, (writes, reorder, trash, format_batch, move_atomic, transfer_folders))) => {
                        this.model.set_group_tree(tree, writes);
                        this.move_atomic = move_atomic;
                        this.model.group_reorder = reorder;
                        this.model.group_transfer_folders = transfer_folders;
                        this.group_trash_available = trash;
                        this.format_batch = format_batch;
                    }
                    Err(ClientError::Unsupported) => { this.model.set_groups_unsupported(); this.group_trash_available = false; this.format_batch = false; this.move_atomic = false; }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One `group_edit` at the node's current version. Success or a stale tree
    /// reloads; an unknown outcome keeps the request for a retry.
    pub fn edit_groups(&mut self, op: GroupEditOp, cx: &mut Context<Self>) {
        let version = self.model.version_for(&op);
        let Some(request) = self.editor.prepare(version, op) else { cx.notify(); return };
        self.send_group_edit(request, cx);
    }

    pub fn retry_folder_transfer(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.editor.pending_transfer() {
            if let Some(request) = self.editor.prepare(p.expected_version, p.op) { self.send_group_edit(request, cx); }
        }
    }

    fn send_group_edit(&mut self, request: crate::edit::Pending, cx: &mut Context<Self>) {
        let transfer = match &request.op { GroupEditOp::TransferFolders { moving, .. } => Some(*moving), _ => None };
        cx.notify();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.group_edit(&request.request_id, request.expected_version.as_deref(), request.op) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let outcome = this.editor.finish(result);
                if let Some(moving) = transfer {
                    this.member_notice = Some(tx(match outcome {
                        Outcome::Done if moving => "文件夹已移动",
                        Outcome::Done => "目录快照已映射；后续新增成员不会自动同步",
                        Outcome::Unknown => "文件夹拖放结果未知，请重试原请求",
                        Outcome::Stale => "目录已变化，本次拖放未提交，请重新拖动",
                        Outcome::Rejected => "文件夹拖放未完成，请查看左栏错误提示",
                    }).into());
                }
                match outcome {
                    Outcome::Done => {
                        cx.emit(LibraryEvent::GroupsEdited);
                        this.reload(cx);
                    }
                    Outcome::Stale => this.reload(cx),
                    Outcome::Unknown | Outcome::Rejected => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The folder (or library root) being shown, if any.
    pub fn current_folder(&self) -> Option<String> {
        let Filter::Group(id) = self.model.filter() else { return None };
        self.model.group(&id).map(|g| g.id.clone())
    }

    /// The folder 移动到文件夹… removes from: an explicit folder (not a library
    /// root) being shown, writable, and listed by direct members only (with
    /// subfolder items included, a row may not be a member of this folder at
    /// all). Inbox, 全部条目, filters and recursive views have none: they only
    /// offer 添加到文件夹…, so a move never empties every location; filing an
    /// Inbox item anywhere already takes it out of Inbox (`groups_v1` unfiled).
    pub fn move_source(&self) -> Option<String> {
        let id = self.current_folder()?;
        let node = self.model.group(&id)?;
        (self.model.group_writes() && !node.read_only && node.parent_id.is_some() && !self.model.include_descendants()).then_some(id)
    }

    /// The folders `mode` offers for the current location.
    pub fn pick_targets(&self, mode: PickMode) -> Vec<(usize, memex_client::GroupNode)> {
        match mode {
            // Folder to folder: the library root is not a move endpoint.
            PickMode::MoveToFolder => match self.move_source() {
                Some(source) => self.model.pick_targets(Some(&source), true).into_iter().filter(|(_, g)| g.parent_id.is_some()).collect(),
                None => vec![],
            },
            PickMode::AddToFolder => self.model.pick_targets(self.current_folder().as_deref(), true),
            PickMode::MapToLibrary => match self.current_folder() {
                Some(current) => self.model.pick_targets(Some(&current), false),
                None => vec![],
            },
        }
    }

    /// 添加到文件夹… / 映射到其他库… of the selection to `group_id`: it also
    /// appears there (the same item) and stays where it is. Batched by the
    /// agent's per-request limit.
    pub fn add_selection_to(&mut self, group_id: String, cx: &mut Context<Self>) {
        let ids = self.model.selection();
        if ids.is_empty() || self.member_busy || self.model.selection_loading() { return; }
        let name = self.model.group(&group_id).map(|g| g.name.clone()).unwrap_or_default();
        let target_root = self.model.group(&group_id).map(|g| g.root_group_id.clone());
        let mapped = self.current_root().is_some_and(|r| target_root.as_deref() != Some(r.as_str()));
        let version = self.model.group(&group_id).map(|g| g.version.clone());
        self.close_picker(cx);
        self.member_busy = true;
        self.trash_notice = None;
        self.member_notice = Some(tx("正在添加…").into());
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { crate::edit::add_members(&client, &group_id, version, &ids) });
        let total = self.model.selection_len();
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                use crate::edit::MemberOutcome;
                this.member_busy = false;
                this.member_notice = Some(match &outcome {
                    MemberOutcome::Done { changed: true } if mapped => txf("已映射到「{0}」：{1} 条，两个库中是同一条目", &[&name, &total]),
                    MemberOutcome::Done { changed: true } => txf("已添加到「{0}」：{1} 条，原处保留", &[&name, &total]),
                    MemberOutcome::Done { changed: false } => tx("没有变化：名称相同，或条目已在这个分组中").into(),
                    MemberOutcome::Failed { done: 0, error, .. } => txf("未能加入「{0}」：{1}", &[&name, &error.message()]),
                    MemberOutcome::Failed { done, error, .. } => txf("已加入「{0}」{1}/{2} 条，其余未加入：{3}", &[&name, done, &total, &error.message()]),
                });
                if matches!(outcome, MemberOutcome::Done { .. } | MemberOutcome::Failed { done: 1.., .. }) {
                    cx.emit(LibraryEvent::GroupsEdited);
                }
                this.reload(cx);
            }).ok();
        }).detach();
        cx.notify();
    }

    /// 移动到文件夹… of the selection, within this library, as a frozen plan
    /// (`edit::MovePlan`): atomic where the agent lists `move_members`, else
    /// added to the target first and removed from this folder only after
    /// every add was confirmed. The same move after an unknown outcome
    /// resumes the same requests.
    pub fn move_selection_to(&mut self, target: String, cx: &mut Context<Self>) {
        let Some(source) = self.move_source() else { return };
        let ids = self.model.selection();
        let allowed = self.pick_targets(PickMode::MoveToFolder).iter().any(|(_, g)| g.id == target);
        if ids.is_empty() || !allowed || self.member_busy || self.model.selection_loading() { return; }
        let plan = match self.move_plan.take() {
            Some(plan) if plan.is_same(&source, &target, &ids) => plan,
            _ => {
                let version = |id: &str| self.model.group(id).map(|g| g.version.clone());
                crate::edit::MovePlan::new(source.clone(), version(&source), target.clone(), version(&target), &ids, self.move_atomic)
            }
        };
        self.close_picker(cx);
        self.run_move(plan, cx);
    }

    /// 重试 of a move whose outcome was unknown: the same requests again.
    pub fn resume_move(&mut self, cx: &mut Context<Self>) {
        if self.member_busy { return; }
        if let Some(plan) = self.move_plan.take() { self.run_move(plan, cx); }
    }

    fn run_move(&mut self, mut plan: crate::edit::MovePlan, cx: &mut Context<Self>) {
        let name = self.model.group(&plan.target).map(|g| g.name.clone()).unwrap_or_default();
        let total = plan.len();
        self.member_busy = true;
        self.trash_notice = None;
        self.member_notice = Some(tx("正在移动到文件夹…").into());
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move {
            let outcome = plan.run(&client);
            (plan, outcome)
        });
        cx.spawn(async move |this, cx| {
            let (plan, outcome) = task.await;
            this.update(cx, |this, cx| {
                use crate::edit::{MoveOutcome, MoveStage};
                this.member_busy = false;
                this.member_notice = Some(match &outcome {
                    MoveOutcome::Moved => txf("已移动到「{0}」：{1} 条", &[&name, &total]),
                    MoveOutcome::Failed { stage, added, moved, unknown, error } => {
                        let e = error.message();
                        match (stage, unknown) {
                            (MoveStage::Add, true) => txf("添加到「{0}」的结果未知，尚未移出来源；重试会沿用原请求：{1}", &[&name, &e]),
                            (MoveStage::Add, false) if *added == 0 => txf("未能移动到「{0}」，来源未改动：{1}", &[&name, &e]),
                            (MoveStage::Add, false) => txf("已添加到「{0}」{1}/{2} 条，尚未移出来源：{3}", &[&name, added, &total, &e]),
                            (MoveStage::Remove, true) => txf("已添加到「{0}」；移出来源的结果未知，重试会沿用原请求：{1}", &[&name, &e]),
                            (MoveStage::Remove, false) if *moved == 0 => txf("已添加到目标「{0}」，尚未移出来源：{1}", &[&name, &e]),
                            (MoveStage::Remove, false) => txf("已添加到目标「{0}」，{1}/{2} 条已移出来源，其余尚未移出：{3}", &[&name, moved, &total, &e]),
                            (MoveStage::Atomic, true) => txf("移动到「{0}」的结果未知；重试会沿用原请求：{1}", &[&name, &e]),
                            (MoveStage::Atomic, false) if *moved == 0 => txf("未能移动到「{0}」，来源未改动：{1}", &[&name, &e]),
                            (MoveStage::Atomic, false) => txf("已移动 {1}/{2} 条到「{0}」，其余未移动：{3}", &[&name, moved, &total, &e]),
                        }
                    }
                });
                match outcome {
                    // Moved items left this folder.
                    MoveOutcome::Moved => this.model.clear_picked(),
                    // Kept for an identical retry; the selection stays too.
                    MoveOutcome::Failed { unknown: true, .. } => this.move_plan = Some(plan),
                    MoveOutcome::Failed { .. } => {}
                }
                cx.emit(LibraryEvent::GroupsEdited);
                this.reload(cx);
            }).ok();
        }).detach();
        cx.notify();
    }

    /// Right-click on an item row: the menu for the selection. A row outside
    /// the selection becomes the selection first (the open item stays).
    pub fn open_picker(&mut self, item_id: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.model.context_selection(&item_id);
        if self.model.group_writes() || self.group_trash_available {
            self.picker = Some(Picker { item_id, position, choosing: None });
        }
        cx.notify();
    }

    /// The folder list for 添加到文件夹….
    pub fn choose_picker(&mut self, cx: &mut Context<Self>) {
        self.choose_picker_mode(PickMode::AddToFolder, cx);
    }

    pub fn choose_picker_mode(&mut self, mode: PickMode, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.picker {
            p.choosing = Some(mode);
            cx.notify();
        }
    }

    pub fn select_all_items(&mut self, cx: &mut Context<Self>) {
        let range = self.model.select_all();
        self.ensure_rows(range, cx);
        cx.notify();
    }

    pub fn clear_item_selection(&mut self, cx: &mut Context<Self>) {
        if self.rename.as_ref().is_some_and(|r| r.saving) { return; }
        self.rename = None;
        self.picker = None;
        self.model.clear_selection();
        cx.notify();
    }

    pub fn request_rename(&mut self, cx: &mut Context<Self>) {
        if self.show_trash || self.model.selection_loading() || self.model.selection_len() != 1 { return; }
        cx.emit(LibraryEvent::RenameRequested(self.model.selection()[0].clone()));
    }

    pub fn renaming_item(&self) -> Option<&str> {
        self.rename.as_ref().map(|r| r.row.item_id.as_str())
    }

    pub fn start_item_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.as_ref().is_some_and(|r| r.saving) { return; }
        let Some(row) = self.row_for(id) else { return; };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(row.title.clone()));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) { this.submit_item_rename(cx); }
        }).detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        self.rename = Some(ItemRename { row, input, saving: false, error: None });
        cx.notify();
    }

    pub fn submit_item_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(rename) = &self.rename {
            rename.input.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        }
        self.submit_item_rename(cx);
    }

    fn submit_item_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = &mut self.rename else { return; };
        if rename.saving { return; }
        let title = rename.input.read(cx).value().trim().to_string();
        if title.is_empty() { return; }
        rename.saving = true;
        rename.error = None;
        let row = rename.row.clone();
        let id = row.item_id.clone();
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { crate::edit::rename_item(&client, &row, &title) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.rename = None;
                        cx.emit(LibraryEvent::ItemRenamed(id));
                        this.reload(cx);
                    }
                    Err(e) => if let Some(rename) = &mut this.rename {
                        rename.saving = false;
                        rename.error = Some(e.message());
                    },
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }

    pub fn drop_folders(&mut self, ids: &[String], versions: &std::collections::BTreeMap<String, String>, target: &str, moving: bool, cx: &mut Context<Self>) {
        if self.editor.in_flight() || self.member_busy || self.trash_busy || self.show_trash { return; }
        let error = if !self.model.group_transfer_folders {
            Some("当前代理不支持文件夹拖放")
        } else if self.model.folder_versions(ids).as_ref() != Some(versions) {
            Some("目录已变化，请重新拖动")
        } else if self.model.group(target).is_none_or(|g| g.read_only)
            || self.model.group_path(target).iter().any(|g| ids.contains(&g.id)) {
            Some("不能放到自身、子文件夹或只读目录")
        } else { None };
        if let Some(error) = error {
            self.member_notice = Some(tx(error).into());
            cx.notify();
            return;
        }
        self.member_notice = Some(tx(if moving { "正在移动文件夹…" } else { "正在映射目录快照；后续新增成员不会自动同步" }).into());
        self.edit_groups(GroupEditOp::TransferFolders { source_ids: ids.to_vec(), target_parent_id: target.into(), expected_versions: versions.clone(), moving }, cx);
    }

    pub fn item_drag(&self, id: &str) -> Option<ItemDrag> {
        if self.show_trash || self.model.selection_loading() { return None; }
        let ids = if self.model.is_selected(id) { self.model.selection() } else { vec![id.to_string()] };
        let rows: Vec<_> = ids.iter().filter_map(|id| self.row_for(id)).collect();
        (rows.len() == ids.len() && !rows.is_empty()).then(|| ItemDrag { rows, source: self.current_folder(), generation: self.generation })
    }

    /// Modifier is sampled at drop, not drag start. Never silently map a refused move.
    pub fn drop_items(&mut self, drag: &ItemDrag, target: &str, moving: bool, trash: bool, cx: &mut Context<Self>) {
        if self.member_busy || self.trash_busy || self.rename.is_some() { return; }
        if self.generation != drag.generation || self.current_folder() != drag.source
            || drag.rows.iter().any(|r| self.row_for(&r.item_id).is_none_or(|now| now.revision_id != r.revision_id))
        {
            self.member_notice = Some(tx("列表已变化，请重新拖动").into());
            cx.notify();
            return;
        }
        if trash {
            if self.current_root().as_deref() != Some(target) || !self.group_trash_available { return; }
            self.model.set_picked(drag.rows.iter().map(|r| r.item_id.clone()).collect());
            self.delete_selection(cx);
        } else {
            let allowed = if moving { self.pick_targets(PickMode::MoveToFolder) } else { self.model.pick_targets(None, true) };
            if !allowed.iter().any(|(_, g)| g.id == target && !g.read_only) {
                self.member_notice = Some(tx("不能放到这里；⌘ 移动需要同库的明确来源文件夹").into());
                cx.notify();
                return;
            }
            self.model.set_picked(drag.rows.iter().map(|r| r.item_id.clone()).collect());
            if moving { self.move_selection_to(target.to_string(), cx); }
            else { self.add_selection_to(target.to_string(), cx); }
        }
    }

    /// A row click. Shift: the range from the open item; Option or ⌘: add or
    /// remove this row. Neither changes the open item. A plain click selects
    /// this row alone and opens it (as before).
    pub fn click_row(&mut self, ix: usize, item_id: String, modifiers: Modifiers, cx: &mut Context<Self>) {
        if modifiers.shift {
            if let Some(range) = self.model.extend_picked(ix) {
                self.ensure_rows(range, cx);
                cx.notify();
                return;
            }
        } else if modifiers.alt || modifiers.platform {
            self.model.toggle_picked(&item_id);
            cx.notify();
            return;
        }
        self.plain_click(item_id, cx);
    }

    /// A plain click: on the open item it drops the rest of the selection;
    /// elsewhere it asks the shell to open that item.
    pub fn plain_click(&mut self, item_id: String, cx: &mut Context<Self>) {
        if self.model.selected() == Some(item_id.as_str()) {
            self.model.clear_picked();
            cx.notify();
        } else {
            self.request_select(item_id, cx);
        }
    }

    pub fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    /// The toolbar's "include subfolder items" switch.
    pub fn set_include_descendants(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.model.set_include_descendants(on) {
            self.reload(cx);
        } else {
            cx.notify();
        }
    }

    fn load_legacy_page(&mut self, cursor: Option<String>, ticket: u64, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let task = cx.background_executor().spawn(async move { client.library_page(cursor) });
        cx.spawn(async move |this, cx| {
            let page = task.await;
            this.update(cx, |this, cx| {
                if this.generation != ticket {
                    return;
                }
                match page {
                    Ok(page) => {
                        let next = page.next_cursor.clone();
                        this.model.apply_legacy_page(page.items, next.is_none());
                        if next.is_some() {
                            this.load_legacy_page(next, ticket, cx);
                        }
                    }
                    Err(e) => this.model.fail(e.message()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ensures rows for `range` are loaded (virtual table/list windows).
    pub fn ensure_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let offsets = self.model.missing_windows(range);
        for offset in offsets {
            let ticket = self.generation;
            let client = self.client.clone();
            let spec = self.model.spec.clone();
            let view = self.model.view();
            let task = cx.background_executor().spawn(async move { client.library_query(&spec, OWNER_CEILING, offset, WINDOW, view) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    if this.generation != ticket {
                        return;
                    }
                    match result {
                        Ok(window) => {
                            this.model.apply_window(&window);
                        }
                        Err(e) if e.is_stale_view() => this.reload(cx),
                        Err(e) => {
                            this.model.window_failed(offset);
                            this.load_busy = e.is_busy();
                            this.model.fail(e.message());
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.show_trash = false;
        self.trash_group = None;
        self.trash_notice = None;
        // An unresolved move keeps its notice and 重试 wherever the user goes.
        if self.move_plan.is_none() { self.member_notice = None; }
        self.model.set_filter(filter);
        self.reload(cx);
    }

    /// A file drag is (`inside`) or is no longer over the sidebar row `spot`.
    pub fn set_drop_hover(&mut self, spot: crate::import::DropSpot, inside: bool, cx: &mut Context<Self>) {
        if inside && self.drop_hover.as_ref() != Some(&spot) {
            self.drop_hover = Some(spot);
            cx.notify();
        } else if !inside && self.drop_hover.as_ref() == Some(&spot) {
            self.drop_hover = None;
            cx.notify();
        }
    }

    /// The user clicked this place in the sidebar: show it, and tell the agent
    /// where captures not sent to the Inbox go. Only explicit clicks come here;
    /// start-up, restores, refreshes and reloads use `set_filter` and say nothing.
    pub fn navigate(&mut self, filter: Filter, cx: &mut Context<Self>) {
        let group_id = match &filter { Filter::Group(id) => Some(id.clone()), _ => None };
        self.set_filter(filter, cx);
        self.report_active_group(group_id, cx);
    }

    /// Reports one explicit choice (`None`: a virtual view) in the background,
    /// with a fresh request id. Reports run one after another; one superseded
    /// by a newer click before it is sent is dropped, never queued again. The
    /// result is ignored: navigation never waits on it.
    pub fn report_active_group(&mut self, group_id: Option<String>, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        let seq = self.active_group_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let latest = self.active_group_seq.clone();
        let client = self.client.clone();
        let request_id = memex_client::biblio::new_request_id();
        let previous = self.active_group_task.take();
        self.active_group_task = Some(cx.background_executor().spawn(async move {
            if let Some(previous) = previous { previous.await; }
            let superseded = || latest.load(Ordering::SeqCst) != seq;
            if superseded() { return; }
            let _ = client.report_active_group(&request_id, group_id.as_deref(), superseded);
        }));
    }

    pub fn set_sort(&mut self, key: SortKey, descending: bool, cx: &mut Context<Self>) {
        self.sort_notice = None;
        self.model.set_sort(key, descending);
        self.reload(cx);
    }

    pub fn set_origin(&mut self, origin: Option<ItemOrigin>, cx: &mut Context<Self>) {
        self.model.set_origin(origin);
        self.reload(cx);
    }

    pub fn request_select(&mut self, item_id: String, cx: &mut Context<Self>) {
        if self.model.selected() != Some(item_id.as_str()) {
            cx.emit(LibraryEvent::SelectionRequested(item_id));
        }
    }

    pub fn commit_selection(&mut self, item_id: Option<String>, cx: &mut Context<Self>) {
        self.model.select(item_id);
        cx.notify();
    }

    /// After an interface language switch: the placeholder, then a re-render (column
    /// headers follow on the table refresh). A stored failure is reloaded so its
    /// text, which may be formatted, comes back in the new language.
    pub fn relocalize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lang = memex_client::i18n::language();
        if self.placeholder_lang != lang {
            self.placeholder_lang = lang;
            self.search.update(cx, |s, cx| s.set_placeholder(tx("搜索标题"), window, cx));
        }
        if matches!(self.model.state, LoadState::Failed(_)) {
            self.reload(cx);
        } else {
            cx.notify();
        }
    }

    /// Item title and display fields for an id among loaded rows.
    pub fn row_for(&self, item_id: &str) -> Option<Row> {
        (0..self.model.len()).filter_map(|i| self.model.row(i)).find(|r| r.item_id == item_id).cloned()
    }

    pub fn status_line(&self) -> Option<String> {
        if let Some(notice) = &self.member_notice { return Some(notice.clone()); }
        if let Some(notice) = &self.trash_notice { return Some(notice.clone()); }
        match (&self.model.path, &self.model.state) {
            (Path::Legacy, LoadState::Failed(m)) => Some(txf("代理未提供新检索，基础列表也读取失败：{0}", &[&relabel(m)])),
            (_, LoadState::Failed(m)) => Some(relabel(m)),
            (Path::Legacy, _) if !self.model.legacy_complete() => {
                Some(txf("代理未提供新检索，改用基础列表：已载入 {0} 条，筛选仅作用于已载入部分…", &[&self.model.legacy_loaded()]))
            }
            (Path::Legacy, _) => Some(tx("代理未提供新检索，改用基础列表：搜索只匹配标题").into()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- sidebar

pub struct LibrarySidebar {
    library: Entity<Library>,
    /// The 分组 section (`tree`): folders under their root groups.
    tree: Entity<FolderTree>,
    /// Folded sections (来源, 类型, 其他来源), this session.
    folded: std::collections::HashSet<&'static str>,
    /// Source groups opened to show their sub-sites, this session.
    open_groups: std::collections::HashSet<String>,
    /// 已删除的库 is unfolded.
    pub deletions_open: bool,
}

impl LibrarySidebar {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        let tree = cx.new(|cx| FolderTree::new(library.clone(), cx));
        // 其他来源 starts folded: single-item sources are the long tail.
        Self { library, tree, folded: ["其他来源"].into_iter().collect(), open_groups: Default::default(), deletions_open: false }
    }

    pub fn toggle_section(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.folded.remove(key) {
            self.folded.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.open_groups.remove(key) {
            self.open_groups.insert(key.to_string());
        }
        cx.notify();
    }

    /// Expands every ancestor of `id` so the folder is visible.
    pub fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        self.tree.update(cx, |t, cx| t.reveal(id, cx));
    }

    pub fn tree(&self) -> Entity<FolderTree> {
        self.tree.clone()
    }
}

fn sidebar_row(
    id: impl Into<ElementId>,
    icon: IconName,
    label: String,
    count: impl Into<Option<usize>>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let count: Option<usize> = count.into();
    let theme = cx.theme();
    h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .when(active, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
        .when(!active, |d| d.hover(|d| d.bg(theme.sidebar_accent.opacity(0.5))))
        .child(Icon::new(icon).small().text_color(if active { theme.sidebar_primary } else { theme.muted_foreground }))
        .child(div().flex_1().truncate().child(label))
        .when_some(count, |d, n| d.child(div().text_xs().text_color(theme.muted_foreground).child(n.to_string())))
}

fn section(title: &str, cx: &App) -> Div {
    div().pt_3().pb_1().px_2().text_xs().text_color(cx.theme().muted_foreground).child(title.to_string())
}

/// A foldable section title: chevron, title, and (folded) the item count.
fn fold_section(key: &'static str, folded: bool, count: usize, cx: &mut Context<LibrarySidebar>) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(SharedString::from(format!("fold-{key}")))
        .pt_3()
        .pb_1()
        .px_2()
        .gap_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .rounded(theme.radius)
        .hover(|d| d.text_color(theme.foreground))
        .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall())
        .child(div().flex_1().child(tx(key)))
        .when(folded, |d| d.child(count.to_string()))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(key, cx)))
}

impl Render for LibrarySidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let library = self.library.read(cx);
        let facets = library.model.facets().clone();
        let filter = library.model.filter();
        let inbox_count = library.model.inbox_count.filter(|_| library.model.unfiled_supported == Some(true));
        let (node_delete, deletions, restoring, deletion_notice) = (library.node_delete, library.deletions.clone(), library.restoring.clone(), library.deletion_notice.clone());
        let show_trash = library.show_trash && library.trash_group.is_none();
        let trash = library.trash.visible().then(|| if library.trash_group.is_none() { library.trash.badge() } else { None });
        let pick = |filter: Filter| {
            let library = self.library.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                library.update(cx, |l, cx| l.navigate(filter.clone(), cx));
            }
        };
        let mut col = v_flex()
            .id("library-navigation")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px_2()
            .text_sm()
            // Inbox first: where captures and imports arrive. Its count only when
            // the agent itself counted the unfiled items.
            .child(div().pt_2().child(sidebar_row("inbox", IconName::Inbox, tx("Inbox").into(), inbox_count, filter == Filter::Inbox && !library.show_trash, cx)
                .tooltip(|window, cx| Tooltip::new(tx("尚未归入任何库或文件夹的条目")).build(window, cx))
                .debug_selector(|| "sidebar-inbox".into())
                .on_click(pick(Filter::Inbox))
                .map(|row| crate::import_view::file_drop_target(row, self.library.clone(), crate::import::DropSpot::Inbox))))
            .child(section(tx("资料库"), cx))
            .child(sidebar_row("all", IconName::GalleryVerticalEnd, tx("全部条目").into(), facets.total, filter == Filter::All && !library.show_trash, cx).debug_selector(|| "sidebar-all".into()).on_click(pick(Filter::All)));
        // Always shown: the tree, its empty state, or why there is none.
        col = col.child(self.tree.clone());
        let mut sources = v_flex()
            .id("library-sources")
            .h(relative(0.25))
            .flex_none()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px_2()
            .text_sm()
            .border_t_1()
            .border_color(cx.theme().sidebar_border);
        if !facets.sources.is_empty() {
            // Grouped by site (36氪 holds www./m./… sub-sites); one-item sources under 其他来源.
            let tree = crate::sources::group_sources(&facets.sources);
            let folded = self.folded.contains("来源");
            sources = sources.child(fold_section("来源", folded, facets.sources.iter().map(|f| f.count).sum(), cx));
            if !folded {
                let member = |i: usize, f: &crate::model::FacetEntry, cx: &mut Context<Self>| {
                    let active = filter == Filter::Source(f.key.clone());
                    sidebar_row(("src", i), IconName::Globe, source_display(&f.label).to_string(), f.count, active, cx).pl_6().text_xs().on_click(pick(Filter::Source(f.key.clone())))
                };
                let mut n = 0;
                for (g_ix, g) in tree.groups.iter().enumerate() {
                    if let [only] = g.members.as_slice() {
                        // One origin: the group row filters directly.
                        let active = filter == Filter::Source(only.key.clone());
                        sources = sources.child(sidebar_row(("site", g_ix), IconName::Globe, g.name.clone(), g.count, active, cx).on_click(pick(Filter::Source(only.key.clone()))));
                        continue;
                    }
                    let open = self.open_groups.contains(&g.key);
                    let key = g.key.clone();
                    let inside = g.members.iter().any(|m| filter == Filter::Source(m.key.clone()));
                    sources = sources.child(
                        sidebar_row(("site", g_ix), if open { IconName::ChevronDown } else { IconName::ChevronRight }, g.name.clone(), g.count, inside && !open, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&key, cx))),
                    );
                    if open {
                        for m in &g.members {
                            n += 1;
                            sources = sources.child(member(n, m, cx));
                        }
                    }
                }
                if !tree.others.is_empty() {
                    let open = !self.folded.contains("其他来源");
                    sources = sources.child(
                        sidebar_row("others", if open { IconName::ChevronDown } else { IconName::ChevronRight }, txf("其他来源 · {0}", &[&tree.others.len()]), tree.others_count(), false, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_section("其他来源", cx))),
                    );
                    if open {
                        for m in &tree.others {
                            n += 1;
                            sources = sources.child(member(n, m, cx));
                        }
                    }
                }
            }
        }
        if !facets.item_types.is_empty() {
            let folded = self.folded.contains("类型");
            col = col.child(fold_section("类型", folded, facets.item_types.iter().map(|f| f.count).sum(), cx));
            for (i, f) in facets.item_types.iter().enumerate().filter(|_| !folded) {
                let active = filter == Filter::ItemType(f.key.clone());
                col = col.child(sidebar_row(("type", i), IconName::FileText, item_type_label(&f.label), f.count, active, cx).on_click(pick(Filter::ItemType(f.key.clone()))));
            }
        }
        // 已删除的库: reachable even when every root (Main included) was deleted.
        if node_delete {
            let theme = cx.theme().clone();
            let open = self.deletions_open;
            let notice = deletion_notice;
            col = col.child(div().mt_3().pt_2().border_t_1().border_color(theme.sidebar_border).child(
                h_flex().id("deleted-libraries").gap_2().px_2().py_1().rounded(theme.radius)
                    .hover(|d| d.bg(theme.sidebar_accent.opacity(0.5)))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().truncate().child(tx("已删除的库")))
                    .when(!deletions.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).child(deletions.len().to_string())))
                    .on_click(cx.listener(|this, _, _, cx| { this.deletions_open = !this.deletions_open; cx.notify(); })),
            ));
            if open {
                if deletions.is_empty() {
                    col = col.child(div().pl_8().py_1().text_xs().text_color(theme.muted_foreground).child(tx("没有已删除的库")));
                }
                for (i, d) in deletions.iter().enumerate() {
                    let library = self.library.clone();
                    let id = d.deletion_id.clone();
                    let date = d.deleted_at.get(..10).unwrap_or("").to_string();
                    col = col.child(h_flex().id(("deletion", i)).gap_1().pl_8().pr_1().py(px(3.)).text_xs()
                        .child(v_flex().flex_1().min_w_0().child(div().truncate().text_sm().child(d.name.clone())).child(div().text_color(theme.muted_foreground).child(date)))
                        .child(Button::new(("deletion-restore", i)).xsmall().outline().label(tx("恢复"))
                            .loading(restoring.as_deref() == Some(id.as_str())).disabled(restoring.is_some())
                            .on_click(move |_, _, cx| library.update(cx, |l, cx| l.restore_deletion(id.clone(), cx)))));
                }
                if let Some(n) = notice {
                    col = col.child(div().id("deletion-notice").pl_8().pr_2().py_1().text_xs().text_color(theme.warning).child(n));
                }
            }
        }
        // 回收站 last, only when the agent has one; the count only when non-zero.
        if let Some(badge) = trash {
            let library = self.library.clone();
            let node_drop = library.clone();
            let item_drop = library.clone();
            let theme = cx.theme();
            col = col.child(div().mt_3().mb_2().pt_2().border_t_1().border_color(theme.sidebar_border).child(
                h_flex()
                    .id("trash")
                    .debug_selector(|| "trash".into())
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .when(show_trash, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                    .when(!show_trash, |d| d.hover(|d| d.bg(theme.sidebar_accent.opacity(0.5))))
                    .child(crate::trash::trash_icon().small().text_color(if show_trash { theme.sidebar_primary } else { theme.muted_foreground }))
                    .child(div().flex_1().truncate().child(tx("回收站")))
                    .when_some(badge, |d, n| d.child(div().text_xs().text_color(theme.muted_foreground).child(n.to_string())))
                    .drag_over::<crate::tree::RootDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                    .on_drop(move |drag: &crate::tree::RootDrag, _, cx| node_drop.update(cx, |l, cx| l.delete_nodes(&drag.ids, cx)))
                    .drag_over::<ItemDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                    .on_drop(move |drag: &ItemDrag, _, cx| item_drop.update(cx, |l, cx| {
                        if let Some(root) = l.current_root() { l.drop_items(drag, &root, false, true, cx); }
                    }))
                    .on_click(move |_, _, cx| library.update(cx, |l, cx| { l.set_show_trash(true, cx); l.report_active_group(None, cx); })),
            ));
        }
        v_flex().id("library-sidebar").size_full().overflow_hidden()
            .child(col)
            .when(!facets.sources.is_empty(), |d| d.child(sources))
    }
}

/// 排序 for the reading list: the five orders of the phone; the two the agent
/// cannot sort by yet are shown disabled with the reason.
fn sort_menu(library: &Entity<Library>, cx: &App) -> AnyElement {
    use crate::model::ListSort;
    let current = ListSort::of(library.read(cx).model.sort());
    let handle = library.clone();
    Button::new("list-sort")
        .ghost()
        .xsmall()
        .dropdown_caret(true)
        .label(current.map(|s| tx(s.label()).to_string()).unwrap_or_else(|| tx("自定义").to_string()))
        .tooltip(tx("排序"))
        .dropdown_menu(move |menu, _, cx| {
            let mut menu = menu.label(tx("排序"));
            for s in ListSort::ALL {
                let h = handle.clone();
                let refused = s == ListSort::AddedToLibrary && handle.read(cx).model.added_at_rejected;
                let available = s.query().is_some() && !refused;
                let label = if available { tx(s.label()).to_string() } else { txf("{0}（需代理支持）", &[&tx(s.label())]) };
                menu = menu.item(PopupMenuItem::new(label).checked(current == Some(s)).disabled(!available).on_click(move |_, _, cx| {
                    if let Some((key, descending)) = s.query() {
                        h.update(cx, |l, cx| l.set_sort(key, descending, cx));
                    }
                }));
            }
            menu
        })
        .into_any_element()
}

/// Above an Inbox that cannot tell filed items apart: what it shows instead.
fn inbox_note(library: &Entity<Library>, cx: &App) -> Option<AnyElement> {
    let note = library.read(cx).model.inbox_notice()?;
    Some(div().id("inbox-note").px_3().pb_1().text_xs().text_color(cx.theme().muted_foreground).child(txf(note, &[&memex_client::BRAND_NAME])).into_any_element())
}

fn origin_control(library: &Entity<Library>, cx: &App) -> Option<AnyElement> {
    let model = &library.read(cx).model;
    if model.origin_supported != Some(true) { return None; }
    let selected = model.spec.origin;
    Some(h_flex().px_2().pb_1().gap_1().children(
        [(Some(ItemOrigin::Capture), "我抓取的"), (None, "全部"), (Some(ItemOrigin::Import), "导入的")]
            .into_iter().enumerate().map(|(i, (origin, label))| {
                let handle = library.clone();
                Button::new(("query-origin", i)).ghost().xsmall().label(tx(label))
                    .selected(selected == origin)
                    .on_click(move |_, _, cx| handle.update(cx, |l, cx| l.set_origin(origin, cx)))
            })
    ).into_any_element())
}

// ---------------------------------------------------------------- classic table

pub struct ItemTableDelegate {
    library: WeakEntity<Library>,
    /// Every column of `COLUMNS`, with its sort state.
    columns: Vec<Column>,
    /// Indices into `COLUMNS` of the columns shown, in order.
    shown: Vec<usize>,
}

/// Author/year/container have no backend sort key yet, so they are not
/// sortable. 采集日期 is `added_at` only (never `captured_at`). 发布日期 is the
/// agent's `published_at` (day precision only, never a capture date) and 时长 its
/// `duration_seconds`; neither is sortable: this client reads no `query_sorts`
/// advertisement, and a `published_at` sort needs it together with
/// `query_date_policy` (it orders by the effective date, not this column).
const COLUMNS: [(&str, &str, f32, Option<SortKey>); 9] = [
    ("title", "标题", 290., Some(SortKey::Title)),
    ("authors", "作者", 110., None),
    ("year", "年份", 52., None),
    ("container", "期刊或出版社", 140., None),
    ("item_type", "类型", 76., Some(SortKey::ItemType)),
    ("source", "来源", 108., Some(SortKey::Source)),
    ("added_at", "采集日期", 104., Some(SortKey::AddedAt)),
    ("published_at", "发布日期", 96., None),
    ("duration", "时长", 72., None),
];

/// Columns the header's context menu shows or hides (shown by default).
const OPTIONAL_COLUMNS: [usize; 2] = [7, 8];

impl ItemTableDelegate {
    fn key(&self, col_ix: usize) -> &'static str {
        COLUMNS[self.shown[col_ix]].0
    }
    /// Shows or hides an optional column, keeping the `COLUMNS` order.
    pub fn toggle_column(&mut self, ix: usize) {
        if !OPTIONAL_COLUMNS.contains(&ix) { return; }
        if let Some(at) = self.shown.iter().position(|&i| i == ix) {
            self.shown.remove(at);
        } else {
            self.shown.push(ix);
            self.shown.sort_unstable();
        }
    }
    pub fn shown_keys(&self) -> Vec<&'static str> {
        self.shown.iter().map(|&i| COLUMNS[i].0).collect()
    }
}

impl ItemTableDelegate {
    fn with<R>(&self, cx: &App, f: impl FnOnce(&Library) -> R) -> Option<R> {
        self.library.upgrade().map(|l| f(l.read(cx)))
    }
}

impl TableDelegate for ItemTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.shown.len()
    }
    fn rows_count(&self, cx: &App) -> usize {
        self.with(cx, |l| l.model.len()).unwrap_or(0)
    }
    /// Header names follow the interface language on every table refresh.
    fn column(&self, col_ix: usize, _: &App) -> Column {
        let ix = self.shown[col_ix];
        let mut c = self.columns[ix].clone();
        c.name = tx(COLUMNS[ix].1).into();
        c
    }
    /// Right-clicking a header lists the optional columns to show or hide.
    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let table = cx.entity().downgrade();
        let shown = self.shown.clone();
        div().id(("th", col_ix)).size_full().child(tx(COLUMNS[self.shown[col_ix]].1)).context_menu(move |menu, _, _| {
            let mut menu = menu.label(tx("显示列"));
            for ix in OPTIONAL_COLUMNS {
                let t = table.clone();
                menu = menu.item(PopupMenuItem::new(tx(COLUMNS[ix].1)).checked(shown.contains(&ix)).on_click(move |_, _, cx| {
                    t.update(cx, |t, cx| { t.delegate_mut().toggle_column(ix); t.refresh(cx); cx.notify(); }).ok();
                }));
            }
            menu
        })
    }
    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        let target = self.shown[col_ix];
        let Some(key) = COLUMNS[target].3 else { return };
        let descending = matches!(sort, ColumnSort::Descending);
        for (i, c) in self.columns.iter_mut().enumerate() {
            *c = c.clone().sort(if i == target { sort } else { ColumnSort::Default });
        }
        if let Some(library) = self.library.upgrade() {
            library.update(cx, |l, cx| l.set_sort(key, descending, cx));
        }
    }
    /// Hovering a row shows its full title after the normal tooltip delay.
    fn render_tr(&mut self, row_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let title = self.with(cx, |l| row_tooltip(l.model.row(row_ix))).flatten();
        let item = self.with(cx, |l| l.model.row(row_ix).map(|r| r.item_id.clone())).flatten();
        let picked = item.as_ref().is_some_and(|id| self.with(cx, |l| l.model.is_selected(id)).unwrap_or(false));
        // The table paints its own row; the other selected rows look the same.
        let active = if cx.theme().list.active_highlight { cx.theme().tokens.table_active } else { cx.theme().tokens.accent };
        let library = self.library.clone();
        let table = cx.entity().downgrade();
        let drag = item.as_ref().and_then(|id| self.with(cx, |l| l.item_drag(id)).flatten());
        div()
            .id(("row", row_ix))
            .debug_selector(move || format!("item-row-{row_ix}"))
            .when_some(drag, |d, drag| d.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
            .when(picked, |d| d.bg(active))
            .when_some(title, |d, t| d.tooltip(move |window, cx| Tooltip::new(t.clone()).build(window, cx)))
            .when_some(item, |d, id| {
                let (l2, l3, id2, id3) = (library.clone(), library.clone(), id.clone(), id.clone());
                // Shift / Option / ⌘ clicks are the selection's, not the table's
                // (which would move its single selection and open the item).
                d.capture_any_mouse_down(move |e, window, cx| {
                    let m = e.modifiers;
                    if e.button != MouseButton::Left || !(m.shift || m.alt || m.platform) { return; }
                    cx.stop_propagation();
                    if let Some(t) = table.upgrade() {
                        let focus = t.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                    if let Some(l) = l2.upgrade() {
                        l.update(cx, |l, cx| l.click_row(row_ix, id2.clone(), m, cx));
                    }
                })
                .on_click(move |e, _, cx| {
                    // Before the table's own click: a plain click on the open item.
                    if e.click_count() == 1 {
                        if let Some(l) = l3.upgrade() {
                            l.update(cx, |l, cx| if l.model.selected() == Some(id3.as_str()) { l.plain_click(id3.clone(), cx) });
                        }
                    }
                })
                .on_mouse_down(MouseButton::Right, move |e, _, cx| {
                    if let Some(l) = library.upgrade() {
                        l.update(cx, |l, cx| l.open_picker(id.clone(), e.position, cx));
                    }
                })
            })
    }
    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let row = self.with(cx, |l| l.model.row(row_ix).cloned()).flatten();
        let muted = cx.theme().muted_foreground;
        let Some(row) = row else {
            return div().text_color(muted).child(if col_ix == 0 { tx("载入中…") } else { "" }).into_any_element();
        };
        match self.key(col_ix) {
            "title" => h_flex().gap_2().overflow_hidden().child(Icon::new(IconName::FileText).small().text_color(muted)).child(div().min_w_0().truncate().child(title_text(&row, muted))).into_any_element(),
            "year" => div().child(row.year_label()).into_any_element(),
            "added_at" => div().when(row.added_at.is_none(), |d| d.text_color(muted)).child(row.added_label()).into_any_element(),
            "published_at" => div().when(row.published_at.is_none(), |d| d.text_color(muted)).child(row.published_label()).into_any_element(),
            "duration" => div().when(row.duration_seconds.is_none(), |d| d.text_color(muted)).child(row.duration_label()).into_any_element(),
            key => div().truncate().child(cell_text(&row, key)).into_any_element(),
        }
    }
    fn cell_label(&self, row_ix: usize, col_ix: usize, cx: &App) -> Option<SharedString> {
        let row = self.with(cx, |l| l.model.row(row_ix).cloned()).flatten()?;
        Some(cell_text(&row, self.key(col_ix)).into())
    }
    fn render_empty(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let message = self.with(cx, |l| l.model.empty_message()).flatten().unwrap_or_default();
        v_flex().size_full().items_center().justify_center().text_color(cx.theme().muted_foreground).child(message)
    }
    fn visible_rows_changed(&mut self, visible_range: Range<usize>, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if let Some(library) = self.library.upgrade() {
            library.update(cx, |l, cx| l.ensure_rows(visible_range, cx));
        }
    }
}

/// A row's title: the translation first, then a muted 「｜」 and the original.
fn title_text(row: &Row, muted: Hsla) -> StyledText {
    let text = row.display_title();
    let lead = memex_client::title_parts(&row.title, row.translated_title.as_deref()).0.map_or(text.len(), str::len);
    let runs = if lead < text.len() { vec![(lead..text.len(), HighlightStyle { color: Some(muted), ..Default::default() })] } else { vec![] };
    StyledText::new(text).with_highlights(runs)
}

pub fn cell_text(row: &Row, key: &str) -> String {
    match key {
        "title" => row.display_title(),
        "authors" => row.authors_label(),
        "year" => row.year_label(),
        "container" => row.container_label(),
        // Known type names follow the interface language; others stay verbatim.
        "item_type" => item_type_label(&row.item_type),
        "source" => source_display(&row.source_label).to_string(),
        "published_at" => row.published_label(),
        "duration" => row.duration_label(),
        _ => row.added_label(),
    }
}

pub struct LibraryTable {
    library: Entity<Library>,
    pub table: Entity<TableState<ItemTableDelegate>>,
}

impl LibraryTable {
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let columns = COLUMNS
            .iter()
            .map(|(key, name, width, sort_key)| {
                let c = Column::new(*key, tx(name)).width(px(*width));
                match sort_key {
                    Some(_) => c.sortable(),
                    None => c,
                }
            })
            .collect();
        let delegate = ItemTableDelegate { library: library.downgrade(), columns, shown: (0..COLUMNS.len()).collect() };
        let table = cx.new(|cx| TableState::new(delegate, window, cx).row_selectable(true).col_movable(false));
        cx.subscribe(&table, |this, table, event: &TableEvent, cx| {
            let pick = |ix: usize, cx: &mut Context<Self>| this.library.read(cx).model.row(ix).map(|r| r.item_id.clone());
            match event {
                TableEvent::SelectRow(ix) => {
                    if let Some(id) = pick(*ix, cx) {
                        this.library.update(cx, |l, cx| l.request_select(id, cx));
                    }
                }
                TableEvent::DoubleClickedRow(ix) => {
                    if let Some(id) = pick(*ix, cx) {
                        this.library.update(cx, |_, cx| cx.emit(LibraryEvent::OpenRequested(id)));
                    }
                }
                _ => {}
            }
            let _ = table;
        })
        .detach();
        cx.observe(&library, |this, library, cx| {
            // The table's own highlight is the open item, while it is selected.
            let model = &library.read(cx).model;
            let selected = model.selected_index().filter(|_| model.selected().is_some_and(|id| model.is_selected(id)));
            this.table.update(cx, |t, cx| {
                match selected {
                    Some(ix) if t.selected_row() != Some(ix) => t.set_selected_row(ix, cx),
                    None => t.clear_selection(cx),
                    _ => {}
                }
                t.refresh(cx);
            });
            cx.notify();
        })
        .detach();
        Self { library, table }
    }
}

impl Render for LibraryTable {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.library.read(cx).status_line();
        let header = folder_header(&self.library, cx);
        let picker = group_picker(&self.library, cx);
        v_flex()
            .size_full()
            .when_some(origin_control(&self.library, cx), |d, control| d.child(control))
            .when_some(inbox_note(&self.library, cx), |d, n| d.child(div().pt_1().child(n)))
            .when_some(header, |d, h| d.child(h))
            .child(div().flex_1().min_h_0().size_full().key_context(ITEMS_CONTEXT)
                .on_action(cx.listener(|this, _: &DeleteSelectedItem, _, cx| this.library.update(cx, |l, cx| l.delete_selected(cx))))
                .on_action(cx.listener(|this, _: &SelectAllItems, _, cx| this.library.update(cx, |l, cx| l.select_all_items(cx))))
                .on_action(cx.listener(|this, _: &ClearItemSelection, _, cx| this.library.update(cx, |l, cx| l.clear_item_selection(cx))))
                .on_action(cx.listener(|this, _: &RenameSelectedItem, _, cx| this.library.update(cx, |l, cx| l.request_rename(cx))))
                .child(DataTable::new(&self.table).stripe(true).bordered(false).scrollbar_visible(true, false)))
            .when_some(item_name_field(&self.library, cx), |d, field| d.child(field))
            .when_some(status, |d, s| d.child(status_row(&self.library, s, cx)))
            .when_some(picker, |d, p| d.child(p))
    }
}

fn item_name_field(library: &Entity<Library>, cx: &App) -> Option<AnyElement> {
    let rename = library.read(cx).rename.as_ref()?;
    let (save, cancel) = (library.clone(), library.clone());
    Some(v_flex().id("item-rename").px_3().py_2().gap_1()
        .child(h_flex().gap_1().child(div().flex_1().child(Input::new(&rename.input).small()))
            .child(Button::new("item-rename-save").small().primary().label(tx("确定")).disabled(rename.saving)
                .on_click(move |_, _, cx| save.update(cx, |l, cx| l.submit_item_rename(cx))))
            .child(Button::new("item-rename-cancel").small().ghost().label(tx("取消")).disabled(rename.saving)
                .on_click(move |_, _, cx| cancel.update(cx, |l, cx| { l.rename = None; cx.notify(); }))))
        .when_some(rename.error.clone(), |d, error| d.child(div().text_xs().text_color(cx.theme().warning).child(error)))
        .into_any_element())
}

/// The status line; when the agent stayed busy, a calm 重试 that reloads the listing.
fn status_row<T>(library: &Entity<Library>, status: String, cx: &mut Context<T>) -> Div {
    let busy = matches!(library.read(cx).model.state, LoadState::Failed(_)) && library.read(cx).load_busy;
    let resume = library.read(cx).move_plan.is_some() && !library.read(cx).member_busy;
    let (handle, h2) = (library.clone(), library.clone());
    h_flex().gap_2().items_center().px_3().py_1().text_xs().text_color(cx.theme().muted_foreground).child(div().flex_1().min_w_0().child(status))
        .when(busy, |d| d.child(Button::new("library-busy-retry").xsmall().outline().label(tx("重试")).on_click(move |_, _, cx| handle.update(cx, |l, cx| l.reload(cx)))))
        .when(resume, |d| d.child(Button::new("library-move-retry").xsmall().outline().label(tx("重试")).on_click(move |_, _, cx| h2.update(cx, |l, cx| l.resume_move(cx)))))
}

// ---------------------------------------------------------------- compact list

pub struct LibraryList {
    library: Entity<Library>,
    scroll: UniformListScrollHandle,
}

impl LibraryList {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self { library, scroll: UniformListScrollHandle::new() }
    }

    /// Scrolls to the last row; rows load by window as they come into view.
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn at_end(&self) -> Option<bool> {
        self.scroll.is_scrolled_to_end()
    }
}

impl Render for LibraryList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let library = self.library.read(cx);
        let count = library.model.len();
        let empty = library.model.empty_message();
        let search = library.search.clone();
        let status = library.status_line();
        let handle = self.library.clone();
        let header = folder_header(&self.library, cx);
        let picker = group_picker(&self.library, cx);
        let sort_notice = library.sort_notice.clone();
        let focus = library.items_focus.clone();
        let list_focus = focus.clone();
        v_flex()
            .size_full()
            .child(h_flex().p_2().gap_1().child(div().flex_1().min_w_0().child(Input::new(&search).small().prefix(Icon::new(IconName::Search).small()).cleanable(true))).child(sort_menu(&self.library, cx)))
            .when_some(origin_control(&self.library, cx), |d, control| d.child(control))
            .when_some(sort_notice, |d, n| d.child(div().px_3().pb_1().text_xs().text_color(cx.theme().muted_foreground).child(n)))
            .when_some(inbox_note(&self.library, cx), |d, n| d.child(n))
            .when_some(header, |d, h| d.child(h))
            .child(div().flex_1().min_h_0().flex().flex_col().key_context(ITEMS_CONTEXT).track_focus(&list_focus)
                .on_action(cx.listener(|this, _: &DeleteSelectedItem, _, cx| this.library.update(cx, |l, cx| l.delete_selected(cx))))
                .on_action(cx.listener(|this, _: &SelectAllItems, _, cx| this.library.update(cx, |l, cx| l.select_all_items(cx))))
                .on_action(cx.listener(|this, _: &ClearItemSelection, _, cx| this.library.update(cx, |l, cx| l.clear_item_selection(cx))))
                .on_action(cx.listener(|this, _: &RenameSelectedItem, _, cx| this.library.update(cx, |l, cx| l.request_rename(cx))))
                .child(match empty {
                Some(message) => div().flex_1().flex().items_center().justify_center().text_sm().text_color(cx.theme().muted_foreground).child(message).into_any_element(),
                None => uniform_list("library-list", count, move |range, _window, cx| {
                    handle.update(cx, |l, cx| l.ensure_rows(range.clone(), cx));
                    let library = handle.read(cx);
                    let theme = cx.theme().clone();
                    range
                        .map(|ix| {
                            let row = library.model.row(ix).cloned();
                            let active = row.as_ref().is_some_and(|r| library.model.is_selected(&r.item_id));
                            let id = row.as_ref().map(|r| r.item_id.clone());
                            let tip = row_tooltip(row.as_ref());
                            let h = handle.clone();
                            let h2 = handle.clone();
                            let focus = focus.clone();
                            let id2 = id.clone();
                            let drag = id.as_ref().and_then(|id| library.item_drag(id));
                            h_flex()
                                .id(("row", ix))
                                .debug_selector(move || format!("item-row-{ix}"))
                                .h(px(30.))
                                .px_3()
                                .gap_2()
                                .text_sm()
                                .when(active, |d| d.bg(theme.list_active).text_color(theme.primary))
                                .when(!active, |d| d.hover(|d| d.bg(theme.list_hover)))
                                .child(Icon::new(IconName::FileText).small().text_color(theme.muted_foreground))
                                .w_full()
                                .overflow_hidden()
                                .child(div().flex_1().min_w_0().truncate().child(match row.as_ref() {
                                    Some(r) => title_text(r, theme.muted_foreground).into_any_element(),
                                    None => tx("载入中…").into_any_element(),
                                }))
                                .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(row.as_ref().and_then(Row::list_meta).unwrap_or_default()))
                                .when_some(tip, |d, t| d.tooltip(move |window, cx| Tooltip::new(t.clone()).build(window, cx)))
                                .when_some(drag, |d, drag| d.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
                                .on_mouse_down(MouseButton::Right, move |e, _, cx| {
                                    if let Some(id) = id2.clone() {
                                        h2.update(cx, |l, cx| l.open_picker(id, e.position, cx));
                                    }
                                })
                                .on_click(move |event, window, cx| {
                                    window.focus(&focus, cx);
                                    if let Some(id) = id.clone() {
                                        let modifiers = event.modifiers();
                                        h.update(cx, |l, cx| {
                                            if event.click_count() >= 2 && !(modifiers.shift || modifiers.alt || modifiers.platform) {
                                                cx.emit(LibraryEvent::OpenRequested(id));
                                            } else {
                                                l.click_row(ix, id, modifiers, cx);
                                            }
                                        });
                                    }
                                })
                        })
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element(),
            }))
            .when_some(item_name_field(&self.library, cx), |d, field| d.child(field))
            .when_some(status, |d, s| d.child(status_row(&self.library, s, cx)))
            .when_some(picker, |d, p| d.child(p))
    }
}
