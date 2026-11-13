//! Folder tree (groups_v1): the 分组 section of the sidebar, its inline editing
//! (new group, new subfolder, rename), the breadcrumb above a folder's list and
//! the item menu (folders, other libraries, trash). Reads the `Library` model; sends `set_filter` and group
//! edits back to it. Counts follow the include-subfolders toggle (direct
//! `item_count` off, `subtree_item_count` on). Each row's ⋯ (on hover, always on
//! the selected row) opens the same menu as a right click; a double click on an
//! editable folder renames it in place. 删除… (`groups_v1.node_delete` only)
//! confirms the server's preview in place of the row; nothing moves.

use crate::edit::Draft;
use crate::model::{Filter, LoadState};
use crate::view::{Library, LibraryEvent, ItemDrag, SelectAllNodes, ClearNodeSelection, RenameSelectedNode, DeleteSelectedNodes};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::base::Disableable as _;
use gpui_kit::*;
use memex_client::{relabel, tx, txf, GroupEditOp, GroupNode};
use std::collections::HashSet;

#[derive(Clone)]
pub(crate) struct RootDrag {
    id: String,
    name: String,
    pub(crate) ids: Vec<String>,
    sortable: bool,
    versions: Option<std::collections::BTreeMap<String, String>>,
}

impl Render for RootDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().px_3().py_2().rounded_md().bg(cx.theme().sidebar)
            .text_color(cx.theme().foreground).child(if self.versions.is_some() {
                txf("{0} 个文件夹 · ⌘ 移动 / 松开映射", &[&self.ids.len()])
            } else { self.name.clone() })
    }
}

pub struct FolderTree {
    library: Entity<Library>,
    /// Expanded folder ids. Roots open the first time the tree arrives.
    expanded: HashSet<String>,
    roots_opened: bool,
    /// The inline name field and what it is for.
    draft: Option<(Draft, Entity<InputState>)>,
    /// Right-click menu on a folder: (folder id, window position).
    menu: Option<(String, Point<Pixels>)>,
    picked: Option<Vec<String>>,
    anchor: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl FolderTree {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&library, |this, library, cx| {
            if !this.roots_opened {
                let roots: Vec<String> = library.read(cx).model.groups().iter().filter(|g| g.parent_id.is_none()).map(|g| g.id.clone()).collect();
                if !roots.is_empty() {
                    this.roots_opened = true;
                    this.expanded.extend(roots);
                }
            }
            cx.notify();
        });
        let edited = cx.subscribe(&library, |this, _, event: &LibraryEvent, cx| {
            if *event == LibraryEvent::GroupsEdited {
                if let Some((Draft::NewChild { parent_id }, _)) = &this.draft {
                    this.expanded.insert(parent_id.clone());
                }
                this.draft = None;
                cx.notify();
            }
        });
        Self { library, expanded: HashSet::new(), roots_opened: false, draft: None, menu: None, picked: None, anchor: None, focus: cx.focus_handle(), _subscriptions: vec![observe, edited] }
    }

    fn visible_nodes(&self, cx: &App) -> Vec<String> {
        fn visit(parent: Option<&str>, groups: &[GroupNode], open: &HashSet<String>, out: &mut Vec<String>) {
            for node in groups.iter().filter(|g| g.parent_id.as_deref() == parent) {
                out.push(node.id.clone());
                if open.contains(&node.id) { visit(Some(&node.id), groups, open, out); }
            }
        }
        let mut out = vec![];
        visit(None, self.library.read(cx).model.groups(), &self.expanded, &mut out);
        out
    }

    pub fn selection(&self, cx: &App) -> Vec<String> {
        self.picked.clone().unwrap_or_else(|| match self.library.read(cx).model.filter() {
            Filter::Group(id) if !self.library.read(cx).show_trash => vec![id],
            _ => vec![],
        }).into_iter().filter(|id| self.library.read(cx).model.group(id).is_some()).collect()
    }

    pub fn click_node(&mut self, id: &str, modifiers: Modifiers, cx: &mut Context<Self>) {
        let mut selected = self.selection(cx);
        if modifiers.shift {
            let visible = self.visible_nodes(cx);
            let anchor = self.anchor.as_deref().or_else(|| selected.first().map(String::as_str));
            if let (Some(a), Some(b)) = (anchor.and_then(|id| visible.iter().position(|v| v == id)), visible.iter().position(|v| v == id)) {
                self.picked = Some(visible[a.min(b)..=a.max(b)].to_vec());
            }
        } else if modifiers.alt || modifiers.platform {
            if let Some(i) = selected.iter().position(|v| v == id) { selected.remove(i); } else { selected.push(id.into()); }
            self.picked = Some(selected);
            if self.anchor.is_none() { self.anchor = Some(id.into()); }
        } else {
            self.picked = Some(vec![id.into()]);
            self.anchor = Some(id.into());
            self.library.update(cx, |l, cx| l.navigate(Filter::Group(id.into()), cx));
        }
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.picked = Some(self.visible_nodes(cx));
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.picked = Some(vec![]);
        self.menu = None;
        self.cancel(cx);
    }

    fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Single-line Input propagates Return after emitting PressEnter. Do not
        // replace the field before its queued submit reads the edited value.
        if self.draft.is_some() { return; }
        let ids = self.selection(cx);
        if ids.len() == 1 && self.library.read(cx).model.group_writes()
            && self.library.read(cx).model.group(&ids[0]).is_some_and(|g| !g.read_only)
        { self.start_edit(Draft::Rename { id: ids[0].clone() }, window, cx); }
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.draft.is_some() { return; }
        let ids = self.selection(cx);
        self.library.update(cx, |l, cx| l.delete_nodes(&ids, cx));
    }

    /// Expands every ancestor of `id` so the folder is visible.
    pub fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        let path: Vec<String> = self.library.read(cx).model.group_path(id).iter().map(|n| n.id.clone()).collect();
        for ancestor in &path[..path.len().saturating_sub(1)] {
            self.expanded.insert(ancestor.clone());
        }
        cx.notify();
    }

    fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    /// Opens the inline name field (new group, new subfolder or rename).
    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        cx.notify();
    }

    /// The folder whose name field is open for renaming.
    pub fn renaming(&self) -> Option<&str> {
        match &self.draft { Some((Draft::Rename { id }, _)) => Some(id), _ => None }
    }

    /// The folder whose menu is open.
    pub fn menu_for(&self) -> Option<&str> {
        self.menu.as_ref().map(|(id, _)| id.as_str())
    }

    /// The right-click menu of folder `id` at a window position (walks use it too).
    pub fn open_menu(&mut self, id: &str, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((id.to_string(), position));
        cx.notify();
    }

    pub fn start_edit(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let initial = match &draft {
            Draft::Rename { id } => self.library.read(cx).model.group(id).map(|g| g.name.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        if let Draft::NewChild { parent_id } = &draft {
            self.expanded.insert(parent_id.clone());
        }
        let placeholder = match draft {
            Draft::NewRoot => tx("分组名称"),
            Draft::NewChild { .. } => tx("子文件夹名称"),
            Draft::Rename { .. } => tx("新名称"),
        };
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial));
        let sub = cx.subscribe_in(&state, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        });
        state.update(cx, |s, cx| s.focus(window, cx));
        self._subscriptions.push(sub);
        self.library.update(cx, |l, _| l.editor.clear_message());
        self.draft = Some((draft, state));
        cx.notify();
    }

    /// Sends the field's name; the field closes when the agent confirms.
    pub fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((draft, state)) = &self.draft else { return };
        let name = state.read(cx).value().to_string();
        let Some(op) = draft.op(&name) else { return };
        self.library.update(cx, |l, cx| l.edit_groups(op, cx));
    }

    /// Development walk: types `name` into the open field and submits it.
    pub fn submit_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, state)) = &self.draft {
            state.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        }
        self.submit(window, cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        self.library.update(cx, |l, _| l.editor.clear_message());
        cx.notify();
    }

    fn name_field(&self, state: &Entity<InputState>, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.library.read(cx).editor.in_flight();
        h_flex()
            .gap_1()
            .pl(px(2. + depth as f32 * 14. + 14.))
            .pr_1()
            .py(px(2.))
            .child(div().flex_1().min_w_0().child(Input::new(state).xsmall()))
            .child(Button::new("group-name-ok").xsmall().primary().icon(IconName::Check).loading(busy).tooltip(tx("确定")).on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))))
            .child(Button::new("group-name-cancel").xsmall().ghost().icon(IconName::Close).tooltip(tx("取消")).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .into_any_element()
    }

    /// One folder row and, when expanded, its children (depth-first).
    #[allow(clippy::too_many_arguments)]
    fn tree_rows(&self, node: &GroupNode, depth: usize, groups: &[GroupNode], active: Option<&str>, count: &dyn Fn(&GroupNode) -> usize, rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let theme = cx.theme().clone();
        let children: Vec<&GroupNode> = groups.iter().filter(|g| g.parent_id.as_deref() == Some(node.id.as_str())).collect();
        let has_trash = node.parent_id.is_none() && self.library.read(cx).group_trash_available;
        let open = self.expanded.contains(&node.id);
        let is_active = self.selection(cx).contains(&node.id);
        let icon_color = if is_active { theme.sidebar_primary } else { theme.muted_foreground };
        if let Some((Draft::Rename { id }, state)) = &self.draft {
            if *id == node.id {
                let state = state.clone();
                rows.push(self.name_field(&state, depth, cx));
                if open {
                    for child in children {
                        self.tree_rows(child, depth + 1, groups, active, count, rows, cx);
                    }
                }
                return;
            }
        }
        let icon: AnyElement = match node.kind.as_str() {
            // An imported Zotero library: the "Z" badge of the design.
            "zotero_group" => div()
                .size(px(15.))
                .flex_none()
                .rounded(px(3.))
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .flex()
                .items_center()
                .justify_center()
                .child("Z")
                .into_any_element(),
            "user" => Icon::new(IconName::CircleUser).small().text_color(icon_color).into_any_element(),
            _ => Icon::new(if open && !children.is_empty() { IconName::FolderOpen } else { IconName::Folder }).small().text_color(icon_color).into_any_element(),
        };
        let toggle_id = node.id.clone();
        let disclosure = div()
            .id(SharedString::from(format!("disclose-{}", node.id)))
            .w(px(14.))
            .flex_none()
            .when(!children.is_empty() || has_trash, |d| {
                d.child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle(&toggle_id, cx);
                    }))
            });
        let pick = node.id.clone();
        let menu_id = node.id.clone();
        let more_id = node.id.clone();
        let editable = self.library.read(cx).model.group_writes() && !node.read_only;
        let row_group = SharedString::from(format!("group-row-{}", node.id));
        let more = div()
            .id(SharedString::from(format!("group-more-{}", node.id)))
            .flex_none()
            .px(px(3.))
            .rounded(px(4.))
            .text_color(theme.muted_foreground)
            .hover(|d| d.bg(theme.sidebar_accent).text_color(theme.foreground))
            .when(!is_active, |d| d.opacity(0.).group_hover(row_group.clone(), |s| s.opacity(1.)))
            .child(Icon::new(IconName::Ellipsis).xsmall())
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tx("更多操作")).build(window, cx))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.open_menu(&more_id, e.position, cx);
            }));
        let pinned = node.parent_id.is_none() && node.kind == "user" && node.name.eq_ignore_ascii_case("Main");
        let sortable = node.parent_id.is_none() && !pinned && self.library.read(cx).model.group_reorder;
        let selected = self.selection(cx);
        let ids = if selected.contains(&node.id) { selected } else { vec![node.id.clone()] };
        let versions = self.library.read(cx).model.folder_versions(&ids);
        let drag = RootDrag { id: node.id.clone(), name: node.name.clone(),
            ids, sortable, versions };
        let before = node.id.clone();
        let destination = node.id.clone();
        rows.push(
            h_flex()
                .id(SharedString::from(format!("group-{}", node.id)))
                .debug_selector({ let id = node.id.clone(); move || format!("group-{id}") })
                .group(row_group)
                .gap_1()
                .pl(px(2. + depth as f32 * 14.))
                .pr_2()
                .py_1()
                .rounded(theme.radius)
                .when(is_active, |d| d.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                .when(!is_active, |d| d.hover(|d| d.bg(theme.sidebar_accent.opacity(0.5))))
                .child(disclosure)
                .child(div().w(px(18.)).flex_none().flex().justify_center().child(icon))
                .child(div().flex_1().min_w_0().truncate().child(node.name.clone()))
                .when(pinned, |d| d.child(div().text_xs().text_color(theme.muted_foreground).child("⌖")))
                .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(count(node).to_string()))
                .child(more)
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .drag_over::<ItemDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                .on_drop(cx.listener(move |this, drag: &ItemDrag, window, cx| {
                    let moving = window.modifiers().platform;
                    this.library.update(cx, |l, cx| l.drop_items(drag, &destination, moving, false, cx));
                }))
                .drag_over::<RootDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                .on_drop(cx.listener(move |this, drag: &RootDrag, window, cx| {
                    if let Some(versions) = &drag.versions {
                        this.expanded.insert(before.clone());
                        this.library.update(cx, |l, cx| l.drop_folders(&drag.ids, versions, &before, window.modifiers().platform, cx));
                    } else if sortable && drag.sortable && drag.ids.len() == 1 && drag.id != before {
                            this.library.update(cx, |library, cx| library.edit_groups(GroupEditOp::ReorderRoot {
                                group_id: drag.id.clone(), before_id: Some(before.clone()),
                            }, cx));
                    }
                }))
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    if e.click_count() >= 2 && editable {
                        this.start_edit(Draft::Rename { id: pick.clone() }, window, cx);
                        return;
                    }
                    this.click_node(&pick, e.modifiers(), cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.menu = Some((menu_id.clone(), e.position));
                        cx.notify();
                    }),
                )
                // Files from Finder dropped here import into this library / folder.
                .map(|row| crate::import_view::file_drop_target(row, self.library.clone(), crate::import::DropSpot::Group(node.id.clone())))
                .into_any_element(),
        );
        if open {
            for child in children {
                self.tree_rows(child, depth + 1, groups, active, count, rows, cx);
            }
            if has_trash {
                let root = node.id.clone();
                let drop_root = root.clone();
                let selected = self.library.read(cx).show_trash && self.library.read(cx).trash_group.as_deref() == Some(root.as_str());
                rows.push(h_flex().id(SharedString::from(format!("trash-{root}"))).debug_selector({ let root = root.clone(); move || format!("trash-{root}") }).gap_2().pl(px(32.)).pr_2().py_1()
                    .rounded(theme.radius).when(selected, |d| d.bg(theme.sidebar_accent))
                    .hover(|d| d.bg(theme.sidebar_accent.opacity(0.5)))
                    .child(crate::trash::trash_icon().small()).child(tx("垃圾桶"))
                    .drag_over::<ItemDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                    .on_drop(cx.listener(move |this, drag: &ItemDrag, _, cx| {
                        this.library.update(cx, |l, cx| l.drop_items(drag, &drop_root, false, true, cx));
                    }))
                    .drag_over::<RootDrag>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                    .on_drop(cx.listener(|this, drag: &RootDrag, _, cx| this.library.update(cx, |l, cx| l.delete_nodes(&drag.ids, cx))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library.update(cx, |l, cx| { l.show_group_trash(root.clone(), cx); l.report_active_group(None, cx); });
                    })).into_any_element());
            }
        }
        if let Some(flow) = self.library.read(cx).delete.clone().filter(|f| f.group_id() == node.id) {
            rows.push(self.delete_row(&flow, depth, cx));
        }
        if let Some((Draft::NewChild { parent_id }, state)) = &self.draft {
            if *parent_id == node.id {
                let state = state.clone();
                rows.push(self.name_field(&state, depth + 1, cx));
            }
        }
    }

    /// The deletion confirmation under a folder's row: only the preview's numbers.
    fn delete_row(&self, flow: &crate::view::DeleteFlow, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        use crate::view::DeleteFlow;
        let theme = cx.theme().clone();
        let card = v_flex().id("group-delete-confirm").gap_1().ml(px(2. + depth as f32 * 14. + 14.)).mr_1().my_1().px_2().py(px(6.))
            .rounded(px(6.)).border_1().border_color(theme.danger.opacity(0.35)).bg(theme.danger.opacity(0.06)).text_xs();
        let cancel = Button::new("group-delete-cancel").xsmall().ghost().label(tx("取消")).on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |l, cx| l.cancel_delete(cx))));
        match flow {
            DeleteFlow::Previewing { notice, .. } => card
                .when_some(notice.clone(), |d, n| d.child(div().text_color(theme.warning).child(n)))
                .child(div().text_color(theme.muted_foreground).child(tx("正在读取将要删除的内容…")))
                .child(h_flex().justify_end().child(cancel)).into_any_element(),
            DeleteFlow::Failed { message, .. } => card
                .child(div().text_color(theme.warning).child(relabel(message)))
                .child(h_flex().justify_end().child(Button::new("group-delete-close").xsmall().ghost().label(tx("关闭")).on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |l, cx| l.cancel_delete(cx)))))).into_any_element(),
            DeleteFlow::Confirm { preview, .. } | DeleteFlow::Deleting { preview } => {
                let busy = matches!(flow, DeleteFlow::Deleting { .. });
                let notice = match flow { DeleteFlow::Confirm { notice, .. } => notice.clone(), _ => None };
                let text = txf("删除「{0}」？将移除 {1} 个文件夹、{2} 条归档关系，涉及 {3} 个条目；条目本身不会被删除，可在“已删除的库”中恢复。", &[&preview.name, &preview.affected_node_count, &preview.affected_membership_count, &preview.distinct_item_count]);
                card.when_some(notice, |d, n| d.child(div().text_color(theme.warning).child(n)))
                    .child(div().text_color(theme.foreground).child(text))
                    .child(h_flex().gap_1().justify_end()
                        .child(cancel.disabled(busy))
                        .child(Button::new("group-delete-ok").xsmall().danger().label(tx("删除")).loading(busy).disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |l, cx| l.confirm_delete(cx))))))
                    .into_any_element()
            }
        }
    }

    fn folder_menu(&self, id: &str, position: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let library = self.library.read(cx);
        let editable = library.model.group_writes() && library.model.group(id).is_some_and(|g| !g.read_only);
        let path = library.model.group_path(id).iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(" / ");
        let format_batch = library.format_batch;
        let deletable = library.can_delete(id);
        let delete_id = id.to_string();
        let group_id = id.to_string();
        let name = library.model.group(id).map(|g| g.name.clone()).unwrap_or_default();
        let item = |key: &'static str, label: &'static str, draft: Draft, cx: &mut Context<Self>| {
            h_flex()
                .id(key)
                .px_2()
                .py_1()
                .rounded(px(5.))
                .hover(|d| d.bg(theme.muted))
                .child(tx(label))
                .on_click(cx.listener(move |this, _, window, cx| this.start_edit(draft.clone(), window, cx)))
        };
        let menu = v_flex()
            .w(px(150.))
            .occlude()
            .p_1()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .text_sm()
            .child(h_flex().id("menu-copy-path").px_2().py_1().rounded(px(5.))
                .hover(|d| d.bg(theme.muted))
                .child(tx("复制文库路径"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                    this.menu = None;
                    cx.notify();
                })))
            .when(format_batch, |d| d.child(h_flex().id("menu-format-batch").px_2().py_1().rounded(px(5.))
                .hover(|d| d.bg(theme.muted))
                .child(tx("批量格式整理"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu = None;
                    let event = LibraryEvent::FormatBatch { group_id: group_id.clone(), name: name.clone() };
                    this.library.update(cx, |_, cx| cx.emit(event));
                    cx.notify();
                }))))
            .when(editable, |d| d
                .child(item("menu-new-child", "新建子文件夹", Draft::NewChild { parent_id: id.to_string() }, cx))
                .child(item("menu-rename", "重命名", Draft::Rename { id: id.to_string() }, cx)))
            .when(deletable, |d| d.child(h_flex().id("menu-delete").px_2().py_1().rounded(px(5.))
                .text_color(theme.danger)
                .hover(|d| d.bg(theme.muted))
                .child(tx("删除…"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu = None;
                    let id = delete_id.clone();
                    this.library.update(cx, |l, cx| l.begin_delete(id, None, cx));
                    cx.notify();
                }))))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        deferred(anchored().position(position).snap_to_window().child(menu)).with_priority(1).into_any_element()
    }
}

impl Render for FolderTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let library = self.library.read(cx);
        let groups = library.model.groups().to_vec();
        let include = library.model.include_descendants();
        let supported = library.model.groups_supported();
        let writes = library.model.group_writes();
        let reorder = library.model.group_reorder;
        let message = library.editor.message().map(relabel);
        let retry_transfer = library.editor.pending_transfer().is_some();
        let active = match library.model.filter() {
            Filter::Group(id) if !library.show_trash => Some(id),
            _ => None,
        };
        let header = h_flex()
            .pt_3()
            .pb_1()
            .px_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child(tx("分组")))
            .when(writes && !groups.is_empty(), |d| {
                d.child(
                    Button::new("group-new-root")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip(tx("新建分组"))
                        .on_click(cx.listener(|this, _, window, cx| this.start_edit(Draft::NewRoot, window, cx))),
                )
            });
        let mut rows = vec![];
        for root in groups.iter().filter(|g| g.parent_id.is_none()) {
            self.tree_rows(root, 0, &groups, active.as_deref(), &|n| if include { n.subtree_item_count } else { n.item_count }, &mut rows, cx);
        }
        let new_root = match &self.draft {
            Some((Draft::NewRoot, state)) => Some(state.clone()),
            _ => None,
        };
        let quiet = |text: &'static str| div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(tx(text));
        v_flex()
            .key_context("MemexFolders").track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectAllNodes, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &ClearNodeSelection, _, cx| this.clear_selection(cx)))
            .on_action(cx.listener(|this, _: &RenameSelectedNode, window, cx| this.rename_selected(window, cx)))
            .on_action(cx.listener(|this, _: &DeleteSelectedNodes, _, cx| this.delete_selected(cx)))
            .child(header)
            .children(rows)
            .when(reorder, |d| d.child(div().id("root-order-end").h(px(12.))
                .drag_over::<RootDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
                .on_drop(cx.listener(|this, drag: &RootDrag, _, cx| {
                    if !drag.sortable || drag.ids.len() != 1 { return; }
                    this.library.update(cx, |library, cx| library.edit_groups(GroupEditOp::ReorderRoot {
                        group_id: drag.id.clone(), before_id: None,
                    }, cx));
                }))))
            .when_some(new_root, |d, state| d.child(self.name_field(&state, 0, cx)))
            .when(groups.is_empty() && self.draft.is_none(), |d| match supported {
                Some(false) => d.child(quiet("当前代理未提供分组")),
                None => d.child(quiet("载入中…")),
                Some(true) => d.child(quiet("还没有分组")).when(writes, |d| {
                    d.child(
                        h_flex()
                            .id("group-new-first")
                            .gap_1()
                            .mx_1()
                            .px_1()
                            .py_1()
                            .rounded(theme.radius)
                            .text_xs()
                            .text_color(theme.primary)
                            .hover(|d| d.bg(theme.sidebar_accent.opacity(0.5)))
                            .child(Icon::new(IconName::Plus).xsmall())
                            .child(tx("新建分组"))
                            .on_click(cx.listener(|this, _, window, cx| this.start_edit(Draft::NewRoot, window, cx))),
                    )
                }),
            })
            .when_some(message, |d, m| d.child(div().px_2().py_1().text_xs().text_color(theme.warning).child(m)))
            .when(retry_transfer, |d| d.child(Button::new("retry-folder-transfer").debug_selector(|| "retry-folder-transfer".into()).xsmall().label(tx("重试原拖放请求"))
                .on_click(cx.listener(|this, _, _, cx| this.library.update(cx, |l, cx| l.retry_folder_transfer(cx))))))
            .when_some(self.menu.clone(), |d, (id, position)| d.child(self.folder_menu(&id, position, cx)))
    }
}

/// Breadcrumb and item count above the list when a folder is selected; each
/// ancestor can be clicked to go up.
pub fn folder_header(library: &Entity<Library>, cx: &App) -> Option<AnyElement> {
    let l = library.read(cx);
    let Filter::Group(id) = l.model.filter() else { return None };
    let path: Vec<GroupNode> = l.model.group_path(&id).into_iter().cloned().collect();
    if path.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let total = l.model.len();
    let ready = l.model.state == LoadState::Ready;
    let count = if l.model.include_descendants() { txf("{0} 条条目（含子文件夹）", &[&total]) } else { txf("{0} 条直接条目", &[&total]) };
    let mut crumbs = h_flex().gap_1().min_w_0().overflow_hidden().text_sm();
    let last = path.len() - 1;
    for (i, node) in path.iter().enumerate() {
        if i > 0 {
            crumbs = crumbs.child(Icon::new(IconName::ChevronRight).xsmall().text_color(theme.muted_foreground));
        }
        let target = node.id.clone();
        let handle = library.clone();
        crumbs = crumbs.child(
            div()
                .id(("crumb", i))
                .flex_none()
                .when(i == last, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground))
                .when(i != last, |d| d.text_color(theme.muted_foreground).hover(|d| d.text_color(theme.foreground)))
                .child(node.name.clone())
                .when(i != last, |d| {
                    d.on_click(move |_, _, cx| {
                        let target = target.clone();
                        handle.update(cx, |l, cx| l.navigate(Filter::Group(target), cx));
                    })
                }),
        );
    }
    Some(
        v_flex()
            .px_3()
            .py_2()
            .gap(px(2.))
            .border_b_1()
            .border_color(theme.border)
            .child(crumbs)
            .child(div().text_xs().text_color(theme.muted_foreground).child(if ready { count } else { tx("载入中…").to_string() }))
            .into_any_element(),
    )
}

/// The item menu of a right-clicked row (the whole selection). Frequent
/// first: 移动到文件夹… / 添加到文件夹… within this library; then, less often,
/// 映射到其他库… (the same item in another library) and 复制到其他库…
/// (disabled: no agent operation yet); then 移入本库垃圾桶. A chosen action
/// lists its folders. Rendered by the list and table views.
pub fn group_picker(library: &Entity<Library>, cx: &App) -> Option<AnyElement> {
    use crate::view::PickMode;
    let l = library.read(cx);
    let picker = l.picker.clone()?;
    let theme = cx.theme().clone();
    let count = l.model.selection_len();
    let loading = l.model.selection_loading();
    let close = {
        let handle = library.clone();
        move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| handle.update(cx, |l, cx| l.close_picker(cx))
    };
    let busy = l.member_busy || l.editor.in_flight();
    let action = |id: &'static str, label: &'static str, mode: PickMode| {
        let handle = library.clone();
        h_flex().id(id).debug_selector(move || id.to_string()).px_2().py_1().gap_2().rounded(px(5.)).hover(|d| d.bg(theme.muted)).child(tx(label))
            .when(!busy, |d| d.on_click(move |_, _, cx| handle.update(cx, |l, cx| l.choose_picker_mode(mode, cx))))
    };
    let muted = |text: String| div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(text);
    let rule = || div().mx_1().my_1().h(px(1.)).bg(theme.border);
    let mut menu = v_flex().id("item-menu").occlude().w(px(250.)).max_h(px(380.)).p_1().rounded(px(8.)).border_1().border_color(theme.border).bg(theme.popover).shadow_md().text_sm().on_mouse_down_out(close);
    if count > 1 {
        menu = menu.child(muted(txf("已选 {0} 条", &[&count])).id("picker-count"));
    }
    match picker.choosing {
        _ if loading => menu = menu.child(muted(tx("正在载入所选条目…").into())),
        None => {
            let in_folder = l.current_folder().is_some();
            if l.model.group_writes() {
                if l.move_source().is_some() {
                    menu = menu.child(action("picker-move", "移动到文件夹…", PickMode::MoveToFolder));
                } else if in_folder && l.model.include_descendants() {
                    menu = menu.child(muted(tx("关闭“含子文件夹条目”后可移动到文件夹").into()));
                }
                menu = menu.child(action("picker-add", "添加到文件夹…", PickMode::AddToFolder));
                if in_folder {
                    // 复制到其他库… makes an independent item: an agent operation
                    // (`item_copy_v1`) without a frozen request yet. Shown, never
                    // imitated by a mapping.
                    menu = menu.child(rule()).child(action("picker-map", "映射到其他库…", PickMode::MapToLibrary)).child(
                        h_flex().id("picker-copy").px_2().py_1().gap_2().text_color(theme.muted_foreground)
                            .child(div().flex_1().child(tx("复制到其他库…")))
                            .child(div().text_xs().child(tx("需要代理支持"))),
                    );
                } else if l.model.filter() == Filter::Inbox && l.model.unfiled_supported == Some(true) {
                    menu = menu.child(muted(tx("添加到任一库或文件夹后，条目即离开 Inbox").into()));
                }
            }
            if l.group_trash_available {
                menu = menu.child(rule());
                if l.current_root().is_some() && !l.trash_busy {
                    let handle = library.clone();
                    let label = if count > 1 { txf("移入本库垃圾桶（{0} 条）", &[&count]) } else { tx("移入本库垃圾桶").into() };
                    menu = menu.child(h_flex().id("item-delete").px_2().py_2().gap_2().rounded(px(5.))
                        .hover(|d| d.bg(theme.muted)).child(crate::trash::trash_icon().small()).child(label)
                        .on_click(move |_, _, cx| handle.update(cx, |l, cx| if l.model.selection_len() > 1 { l.delete_selection(cx) } else { let id = l.model.selection().first().cloned(); if let Some(id) = id { l.delete_from_group(&id, cx) } })));
                } else if l.current_root().is_none() {
                    menu = menu.child(muted(tx("进入一个库后可删除条目").into()));
                }
            }
        }
        Some(mode) => {
            let (title, empty) = match mode {
                PickMode::MoveToFolder => ("移动到文件夹：加入所选文件夹，并从当前文件夹移出", "当前库没有其他文件夹"),
                PickMode::AddToFolder => ("添加到文件夹：同时保留在原处", if l.current_folder().is_some() { "当前库没有其他文件夹" } else { "还没有分组" }),
                PickMode::MapToLibrary => ("映射到其他库：同一条目，两个库都显示", "没有其他库"),
            };
            menu = menu.child(muted(tx(title).into()));
            let folders = l.pick_targets(mode);
            if folders.is_empty() {
                menu = menu.child(muted(tx(empty).into()));
            }
            let mut list = v_flex().id("picker-folders").overflow_y_scroll();
            for (i, (depth, folder)) in folders.into_iter().enumerate() {
                let handle = library.clone();
                let group_id = folder.id.clone();
                list = list.child(
                    h_flex()
                        .id(("picker-folder", i))
                        .debug_selector({ let id = folder.id.clone(); move || format!("picker-target-{id}") })
                        .gap_1()
                        .pl(px(8. + depth as f32 * 12.))
                        .pr_2()
                        .py_1()
                        .rounded(px(5.))
                        .hover(|d| d.bg(theme.muted))
                        .child(Icon::new(IconName::Folder).xsmall().text_color(theme.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().child(folder.name.clone()))
                        .on_click(move |_, _, cx| {
                            let group_id = group_id.clone();
                            handle.update(cx, |l, cx| match mode {
                                PickMode::MoveToFolder => l.move_selection_to(group_id, cx),
                                PickMode::AddToFolder | PickMode::MapToLibrary => l.add_selection_to(group_id, cx),
                            });
                        }),
                );
            }
            menu = menu.child(list);
        }
    }
    Some(deferred(anchored().position(picker.position).snap_to_window().child(menu)).with_priority(1).into_any_element())
}
