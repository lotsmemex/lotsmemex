//! 回收站 (`dedup_trash_v1`): the items the owner's dedup run moved out of the
//! library, each with 恢复. There is no delete-forever. `Trash` is the state
//! (pure apart from the blocking `load` / `restore` helpers run off the UI
//! thread); `TrashView` renders it in place of the item list.

use crate::view::Library;
use gpui_kit::component::{
    button::Button,
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::trash::{TrashEntry, TrashRestore};
use memex_client::{relabel, tx, txf, Client, ClientError};
use std::collections::HashMap;

/// Lucide `trash` (ISC), served by the shell's asset source: the bundled icon
/// set has none.
pub const TRASH_ICON: &str = "icons/memex-trash.svg";

pub fn trash_icon() -> Icon {
    Icon::empty().path(TRASH_ICON)
}

/// Kept-copy titles read at most per listing (the rest show their id).
const KEPT_TITLE_READS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TrashState {
    /// The agent has no `dedup_trash_v1` (or it is not known yet): nothing shown.
    #[default]
    Unsupported,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct Trash {
    pub state: TrashState,
    pub entries: Vec<TrashEntry>,
    /// Titles of the copies that stayed in the library, by item id.
    pub kept_titles: HashMap<String, String>,
    /// The item being restored.
    pub restoring: Option<String>,
    /// Outcome of the last restore, shown above the list.
    pub message: Option<(bool, String)>,
}

/// A listing: the entries and the kept copies' titles.
pub type Listing = (Vec<TrashEntry>, HashMap<String, String>);

/// Reads the whole trash and the kept copies' titles (blocking; background only).
pub fn load(client: &Client) -> Result<Listing, ClientError> {
    let entries = client.trash_all()?;
    let mut titles = HashMap::new();
    for kept in entries.iter().filter_map(|e| e.kept_item_id.as_ref()) {
        if titles.len() >= KEPT_TITLE_READS {
            break;
        }
        if !titles.contains_key(kept) {
            if let Ok(item) = client.item_read(kept) {
                titles.insert(kept.clone(), item.title);
            }
        }
    }
    Ok((entries, titles))
}

impl Trash {
    /// The 回收站 entry is shown.
    pub fn visible(&self) -> bool {
        self.state != TrashState::Unsupported
    }

    /// Count on the sidebar entry; none when empty or unknown.
    pub fn badge(&self) -> Option<usize> {
        (self.state == TrashState::Ready && !self.entries.is_empty()).then_some(self.entries.len())
    }

    pub fn begin_load(&mut self) {
        if self.state != TrashState::Ready {
            self.state = TrashState::Loading;
        }
    }

    pub fn on_load(&mut self, result: Result<Listing, ClientError>) {
        match result {
            Ok((entries, titles)) => {
                self.entries = entries;
                self.kept_titles = titles;
                self.state = TrashState::Ready;
            }
            Err(ClientError::Unsupported) => *self = Trash::default(),
            // Keep a list already shown on a transient failure.
            Err(e) if self.state == TrashState::Ready => self.message = Some((false, txf("无法读取回收站：{0}", &[&e.message()]))),
            Err(e) => self.state = TrashState::Failed(e.message()),
        }
    }

    /// Starts restoring `item_id`; `false` while another restore runs.
    pub fn begin_restore(&mut self, item_id: &str) -> bool {
        if self.restoring.is_some() || !self.entries.iter().any(|e| e.item_id == item_id) {
            return false;
        }
        self.restoring = Some(item_id.to_string());
        self.message = None;
        true
    }

    /// The restore answer. `true`: the library must be read again.
    pub fn on_restore(&mut self, item_id: &str, result: Result<TrashRestore, ClientError>) -> bool {
        self.restoring = None;
        let title = self.entries.iter().find(|e| e.item_id == item_id).map(|e| e.title.clone()).unwrap_or_else(|| item_id.to_string());
        match result {
            Ok(r) if r.restored => {
                self.entries.retain(|e| e.item_id != item_id);
                self.message = Some((true, txf("已恢复：{0}", &[&title])));
                true
            }
            // Already back (e.g. restored elsewhere): the list is out of date.
            Err(ClientError::Daemon(code)) if code == "item_not_trashed" => {
                self.entries.retain(|e| e.item_id != item_id);
                self.message = Some((false, ClientError::Daemon(code).message()));
                true
            }
            Ok(_) => {
                self.message = Some((false, txf("恢复失败：{0}", &[&tx("代理未说明原因")])));
                false
            }
            Err(e) => {
                self.message = Some((false, txf("恢复失败：{0}", &[&e.message()])));
                false
            }
        }
    }

    pub fn reason(&self, entry: &TrashEntry) -> String {
        entry.reason_label(entry.kept_item_id.as_ref().and_then(|k| self.kept_titles.get(k)).map(String::as_str))
    }
}

/// The trash list, shown instead of the items while 回收站 is selected.
pub struct TrashView {
    library: Entity<Library>,
}

impl TrashView {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self { library }
    }
}

impl Render for TrashView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let library = self.library.read(cx);
        let trash = library.trash.clone();
        let group_name = library.trash_group.as_ref().and_then(|id| library.model.group(id)).map(|g| g.name.clone());
        let title = group_name.as_ref().map(|name| txf("{0} · 垃圾桶", &[name])).unwrap_or_else(|| tx("回收站").into());
        let header = v_flex()
            .gap_1()
            .px_4()
            .pt_4()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(trash_icon().small().text_color(theme.primary))
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
                    .when_some(trash.badge(), |d, n| d.child(div().text_sm().text_color(theme.muted_foreground).child(n.to_string()))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tx(if group_name.is_some() { "恢复后回到本库原目录，其他库不受影响。" } else { "回收站中的条目无法打开阅读；恢复后回到资料库即可阅读。" })));
        let mut body = v_flex().id("trash-list").flex_1().min_h_0().overflow_y_scroll().px_4().py_3().gap_2();
        if let Some((ok, text)) = &trash.message {
            body = body.child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .text_sm()
                    .bg(if *ok { theme.primary.opacity(0.08) } else { theme.warning.opacity(0.12) })
                    .child(Icon::new(if *ok { IconName::CircleCheck } else { IconName::TriangleAlert }).small().text_color(if *ok { theme.primary } else { theme.warning }))
                    .child(relabel(text)),
            );
        }
        let muted = |text: String| div().flex_1().flex().items_center().justify_center().py_8().text_sm().text_color(theme.muted_foreground).child(text);
        match &trash.state {
            TrashState::Loading => body = body.child(muted(tx("正在读取回收站…").into())),
            TrashState::Failed(m) => body = body.child(muted(txf("无法读取回收站：{0}", &[&relabel(m)]))),
            TrashState::Ready if trash.entries.is_empty() => body = body.child(muted(tx("回收站是空的").into())),
            _ => {}
        }
        if trash.state == TrashState::Ready {
            for (i, e) in trash.entries.iter().enumerate() {
                let restoring = trash.restoring.as_deref() == Some(e.item_id.as_str());
                let busy = trash.restoring.is_some();
                let id = e.item_id.clone();
                let library = self.library.clone();
                let host = e.source_host().unwrap_or_else(|| tx("来源未记录").into());
                body = body.child(
                    h_flex()
                        .id(("trash-row", i))
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Icon::new(IconName::FileText).small().text_color(theme.muted_foreground))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .child(div().text_sm().truncate().child(e.title.clone()))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(div().flex_none().child(host))
                                        .child("·")
                                        .child(div().flex_none().child(format!("{} {}", tx("移入时间"), e.trashed_label())))
                                        .child("·")
                                        .child(div().min_w_0().truncate().text_color(theme.primary).child(trash.reason(e))),
                                ),
                        )
                        .child(
                            Button::new(("trash-restore", i))
                                .small()
                                .outline()
                                .icon(IconName::Undo2)
                                .label(if restoring { tx("正在恢复…") } else { tx("恢复") })
                                .loading(restoring)
                                .disabled(busy)
                                .on_click(move |_, _, cx| library.update(cx, |l, cx| l.restore_from_trash(&id, cx))),
                        ),
                );
            }
        }
        v_flex().size_full().bg(theme.background).child(header).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{load, Trash, TrashState};
    use memex_client::{Client, ClientError};
    use memex_client::fixture::{FixtureBackend, FixtureControls};
    use memex_client::{QuerySpec, Sensitivity};

    #[test]
    fn restore_then_reload_brings_the_item_back_and_the_trash_shrinks() {
        let fixture = FixtureBackend::new(30);
        let client = Client::new(fixture.clone());
        let mut trash = Trash::default();
        trash.begin_load();
        trash.on_load(load(&client));
        assert!(trash.visible());
        assert_eq!(trash.badge(), Some(3));
        let first = trash.entries[0].clone();
        // The kept copy's title is shown, not its id.
        let kept = client.item_read(first.kept_item_id.as_deref().unwrap()).unwrap().title;
        assert!(trash.reason(&first).contains(&kept));
        let total = |c: &Client| c.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap().total;
        let before = total(&client);
        assert!(trash.begin_restore(&first.item_id));
        assert!(!trash.begin_restore(&trash.entries[1].item_id.clone()), "one restore at a time");
        let reload = trash.on_restore(&first.item_id, client.trash_restore(&first.item_id));
        assert!(reload);
        assert!(matches!(&trash.message, Some((true, _))));
        assert_eq!(total(&client), before + 1, "the library has the item again");
        trash.on_load(load(&client));
        assert_eq!(trash.badge(), Some(2));
        assert!(client.item_read(&first.item_id).is_ok());
        // An agent without the capability: the entry disappears, nothing shown.
        fixture.set_controls(FixtureControls { no_trash: true, ..Default::default() });
        let client = Client::new(fixture);
        trash.on_load(load(&client));
        assert!(!trash.visible() && trash.badge().is_none());
    }

    #[test]
    fn a_failed_restore_is_reported_and_keeps_the_entry() {
        let mut trash = Trash { state: TrashState::Ready, entries: vec![serde_json::from_str(r#"{"item_id":"a","revision_id":"r","title":"T","source_url":"","trashed_at":"","reason":"duplicate"}"#).unwrap()], ..Default::default() };
        assert!(trash.begin_restore("a"));
        assert!(!trash.on_restore("a", Err(ClientError::Unavailable)));
        assert_eq!(trash.entries.len(), 1);
        assert!(matches!(&trash.message, Some((false, _))));
        assert!(trash.restoring.is_none());
    }
}
