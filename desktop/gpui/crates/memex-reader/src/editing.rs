//! Reader-owned Markdown draft and revision-bound manual marks.
use super::*;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use memex_client::reading::{MarkdownEdit, ReadingAnnotations};

#[derive(Default)]
pub(super) struct Editing {
    pub visible: bool,
    pub input: Option<Entity<TextareaState>>,
    pub subscription: Option<Subscription>,
    pub baseline: String,
    pub revision: String,
    pub pending: Option<MarkdownEdit>,
    pub saving: bool,
    pub message: Option<String>,
    /// A book: the one chapter of one side the draft holds (never the whole item).
    pub chapter: Option<ChapterTarget>,
    /// The chapter save whose outcome is not known yet; saving again replays it.
    pub chapter_pending: Option<memex_client::book::ChapterEdit>,
    /// The chapter changed elsewhere: the draft stays, nothing was saved.
    pub stale: bool,
    /// Walks: the next chapter save runs, then its reply is treated as lost.
    #[cfg(feature = "dev-snapshot")]
    pub lose_next_reply: bool,
}

/// Which chapter of which side of the open book the editor holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChapterTarget {
    /// Index into the book's entries.
    pub at: usize,
    pub side: &'static str,
    pub key: String,
    pub title: String,
    /// SHA-256 of the chapter text the draft started from.
    pub sha256: String,
}

/// Request ids of chapter saves are kept with the repair ids, under this prefix.
fn edit_key(e: &memex_client::book::ChapterEdit) -> String {
    format!("edit|{}|{}|{}|{}|{}", e.item_id, e.revision_id, e.side, e.chapter, memex_client::book::sha256_hex(&e.markdown))
}

impl Reader {
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_editor(&self) -> Option<Entity<TextareaState>> { self.editing.input.clone() }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_selection_points(&self, term: &str) -> Option<(Point<Pixels>, Point<Pixels>)> { crate::selection::selection_points(term) }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_annotations(&self) -> Option<ReadingAnnotations> { self.annotations.clone() }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_navigation(&self) -> (Option<Bounds<Pixels>>, f32) { (self.rail_bounds.get(), self.nav_index) }
    pub fn blocks_navigation(&self, cx: &App) -> bool {
        self.editing.saving || self.editing.pending.is_some() || self.editing.chapter_pending.is_some() || self.draft_dirty(cx) || self.annotation_saving
    }
    /// Walks: a one-line summary of the chapter editor.
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_chapter_editor(&self, cx: &App) -> String {
        let e = &self.editing;
        format!("visible={} target={:?} dirty={} saving={} pending={:?} stale={} button={} message={:?}",
            e.visible, e.chapter.as_ref().map(|c| format!("{}:{}", c.side, c.key)), self.draft_dirty(cx), e.saving,
            e.chapter_pending.as_ref().map(|p| p.request_id.clone()), e.stale, self.edit_button_enabled(), e.message)
    }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_lose_next_edit_reply(&mut self) { self.editing.lose_next_reply = true; }
    /// The chapter on screen when a single side of it can be edited: 原文 or 译文,
    /// that side has the chapter, and its text is loaded.
    pub(super) fn chapter_target(&self) -> Option<ChapterTarget> {
        let book = self.model.book()?;
        if self.chapter_loading || !self.chapter_edit_supported { return None; }
        let (side, text, index) = match self.model.mode() {
            Mode::Original => ("original", book.original.as_ref()?, book.entries.get(book.at)?.original?),
            Mode::Translation => ("translation", book.translation.as_ref()?, book.entries.get(book.at)?.translation?),
            Mode::Bilingual => return None,
        };
        let chapter = book.doc.edition(side)?.chapters.get(index)?;
        Some(ChapterTarget { at: book.at, side, key: chapter.key.clone(), title: chapter.title.trim().to_string(), sha256: memex_client::book::sha256_hex(text) })
    }
    /// 编辑 Markdown: ordinary items need `markdown_edit_v1`; a book needs
    /// `reader_chapter_edit_v1` and one side of a chapter on screen.
    pub(super) fn edit_button_enabled(&self) -> bool {
        if self.loading { return false; }
        if self.model.book().is_some() { self.editing.visible || self.chapter_target().is_some() } else { self.edit_allowed }
    }
    /// Why a book's 编辑 Markdown is off (only when the agent offers chapter edits).
    pub(super) fn edit_button_hint(&self) -> Option<&'static str> {
        let book = self.model.book()?;
        if !self.chapter_edit_supported || self.edit_button_enabled() { return None; }
        let missing = match self.model.mode() {
            Mode::Bilingual => return Some("切到原文或译文后编辑本章"),
            Mode::Original => book.entries.get(book.at).is_none_or(|e| e.original.is_none()),
            Mode::Translation => book.entries.get(book.at).is_none_or(|e| e.translation.is_none()),
        };
        missing.then_some("这一侧没有这一章，不能编辑")
    }
    fn draft_dirty(&self, cx: &App) -> bool {
        self.editing.input.as_ref().is_some_and(|s| s.read(cx).value().as_ref() != self.editing.baseline)
    }
    pub fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.visible { self.editing.visible = false; cx.notify(); return; }
        if self.model.book().is_some() { return self.open_chapter_editor(window, cx); }
        if !self.edit_allowed || self.loading { return; }
        let Some(detail) = self.model.detail() else { return; };
        if self.model.stats().is_some_and(|s| !s.complete()) {
            self.editing.message = Some("正文尚未载入完整，不能用片段覆盖全文。".into());
            cx.notify(); return;
        }
        if self.editing.input.is_none() {
            let source = detail.canonical.markdown.clone();
            self.editing.baseline = source.clone();
            self.editing.revision = detail.canonical.revision_id.clone();
            let input = cx.new(|cx| TextareaState::new(window, cx).default_value(source).soft_wrap(true).rows(12));
            self.editing.subscription = Some(cx.subscribe(&input, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) { cx.notify(); }
            }));
            self.editing.input = Some(input);
        }
        self.editing.visible = true;
        self.insights.view = View::Text;
        self.selected.borrow_mut().clear();
        cx.notify();
    }
    /// The editor of a book holds exactly the chapter of the side on screen.
    fn open_chapter_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading { return; }
        let Some(target) = self.chapter_target() else { return };
        // A clean draft of another chapter/side/revision is replaced; a dirty one
        // cannot exist here (navigation is blocked while it is dirty).
        let current = self.model.book().map(|b| b.doc.revision_id.clone()).unwrap_or_default();
        if self.editing.input.is_some() && (self.editing.chapter.as_ref() != Some(&target) || self.editing.revision != current) && !self.blocks_navigation(cx) {
            self.editing = Default::default();
        }
        if self.editing.input.is_none() {
            let Some(book) = self.model.book() else { return };
            let source = if target.side == "original" { book.original.clone() } else { book.translation.clone() }.unwrap_or_default();
            self.editing.baseline = source.clone();
            self.editing.revision = book.doc.revision_id.clone();
            let input = cx.new(|cx| TextareaState::new(window, cx).default_value(source).soft_wrap(true).rows(12));
            self.editing.subscription = Some(cx.subscribe(&input, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) { cx.notify(); }
            }));
            self.editing.input = Some(input);
            self.editing.chapter = Some(target);
            self.editing.message = None;
        }
        self.editing.visible = true;
        self.insights.view = View::Text;
        self.selected.borrow_mut().clear();
        cx.notify();
    }
    /// Saves the chapter draft: the request id is stored before anything is
    /// sent, so a lost reply is checked with the same id (same upload or the
    /// committed receipt), never a second revision.
    fn save_chapter(&mut self, cx: &mut Context<Self>) {
        if self.editing.saving || !self.chapter_edit_supported { return; }
        let (Some(target), Some(input), Some(book)) = (self.editing.chapter.clone(), self.editing.input.as_ref(), self.model.book()) else { return };
        let request = match self.editing.chapter_pending.clone() {
            Some(pending) => pending,
            None => {
                let mut edit = memex_client::book::ChapterEdit {
                    item_id: book.doc.item_id.clone(), revision_id: self.editing.revision.clone(), side: target.side.into(), chapter: target.key.clone(),
                    expected_chapter_sha256: target.sha256.clone(), request_id: String::new(), markdown: input.read(cx).value().to_string(),
                };
                edit.request_id = self.repair_ids.get(&edit_key(&edit)).cloned().unwrap_or_else(|| format!("mac-{}", memex_client::biblio::new_request_id()));
                edit
            }
        };
        let key = edit_key(&request);
        self.repair_ids.insert(key.clone(), request.request_id.clone());
        self.save_repair_ids();
        self.editing.chapter_pending = Some(request.clone());
        self.editing.saving = true;
        self.editing.stale = false;
        self.editing.message = None;
        let client = self.client.clone();
        let sent = request.clone();
        #[cfg(feature = "dev-snapshot")]
        let lose = std::mem::take(&mut self.editing.lose_next_reply);
        #[cfg(not(feature = "dev-snapshot"))]
        let lose = false;
        let task = cx.background_executor().spawn(async move {
            let result = client.reader_edit_chapter(&sent);
            if lose { Err(memex_client::ClientError::Timeout) } else { result }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                use memex_client::ClientError;
                this.editing.saving = false;
                let definitive = |e: &ClientError| matches!(e, ClientError::Unsupported) || matches!(e, ClientError::Daemon(c) if matches!(c.as_str(),
                    "invalid_reader_edit" | "reader_edit_too_large" | "reader_edit_not_found" | "reader_edit_upload_conflict" | "request_id_conflict"
                    | "library_resource_not_found" | "item_not_found" | "item_revision_limit_exceeded" | "permission_denied"
                    | "reader_chapter_not_found" | "reader_side_not_found" | "stale_book_manifest"));
                match result {
                    Ok(receipt) => {
                        this.repair_ids.remove(&key);
                        this.save_repair_ids();
                        this.editing.chapter_pending = None;
                        if !receipt.changed {
                            this.editing.baseline = request.markdown;
                            this.editing.message = Some(tx("没有改动：这一章与当前版本相同，没有新建版本。").into());
                        } else {
                            // A new revision: reread the book and return to this chapter.
                            // Both revisions stay readable: offer 查看本次修改 (old vs new).
                            this.compare_offer = this.revision_text_supported.then(|| crate::compare::CompareSpec {
                                item_id: request.item_id.clone(), target: memex_client::book::RevisionTarget::Chapter { side: request.side.clone(), chapter: request.chapter.clone() },
                                old_revision: request.revision_id.clone(), new_revision: receipt.revision_id.clone(), title: target.title.clone(),
                            });
                            this.editing = Default::default();
                            this.editing.revision = receipt.revision_id;
                            this.editing.message = Some(tx("已保存这一章，新版本已建立，旧版本保留。改过的这一章需要重新整理段落对照。").into());
                            this.after_repair = Some(target.at);
                            this.reload(cx);
                            cx.emit(ReaderEvent::Saved);
                        }
                    }
                    Err(e) if e.is_stale_revision() => {
                        this.repair_ids.remove(&key);
                        this.save_repair_ids();
                        this.editing.chapter_pending = None;
                        this.editing.stale = true;
                        this.editing.message = Some(tx("这一章已在别处更新，没有保存。草稿还在：可以先复制草稿，再重新载入本章。").into());
                    }
                    Err(ClientError::Daemon(c)) if c == "reader_job_in_progress" => {
                        // Nothing written; the same request is replayed once the job ends.
                        this.editing.message = Some(tx("这本书有一章正在整理，暂时不能保存。草稿保留，整理结束后再保存。").into());
                    }
                    Err(e) if definitive(&e) => {
                        this.repair_ids.remove(&key);
                        this.save_repair_ids();
                        this.editing.chapter_pending = None;
                        this.editing.message = Some(e.message());
                    }
                    Err(e) => this.editing.message = Some(txf("保存结果未知（{0}）：可能已经保存，也可能没有。草稿和这次请求都保留；点「核实保存结果」会用同一请求核对，不会重复建立版本。", &[&e.message()]).to_string()),
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }
    /// 对比修改 of two revisions of one target, in place of the text. Without
    /// `reader_revision_text_v1` nothing opens.
    pub fn open_revision_compare(&mut self, spec: crate::compare::CompareSpec, cx: &mut Context<Self>) {
        if !self.revision_text_supported || self.model.active_item() != Some(spec.item_id.as_str()) { return; }
        let client = self.client.clone();
        let compare = cx.new(|cx| crate::compare::RevisionCompare::new(client, spec, cx));
        let sub = cx.subscribe(&compare, |this: &mut Self, _, event, cx| match event {
            crate::compare::CompareEvent::Close => { this.compare = None; cx.notify(); }
        });
        self.compare = Some((compare, sub));
        self.editing.visible = false;
        cx.notify();
    }
    /// 查看本次修改: the last chapter save, its base revision against the new one.
    pub fn view_last_change(&mut self, cx: &mut Context<Self>) {
        if let Some(spec) = self.compare_offer.clone() { self.open_revision_compare(spec, cx); }
    }
    pub fn close_revision_compare(&mut self, cx: &mut Context<Self>) {
        self.compare = None;
        // 查看修改's proof list belongs to the comparison it explains.
        self.fmt.review = None;
        cx.notify();
    }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_compare(&self) -> Option<Entity<crate::compare::RevisionCompare>> { self.compare.as_ref().map(|(c, _)| c.clone()) }
    #[cfg(feature = "dev-snapshot")]
    pub fn dev_compare_offer(&self) -> Option<crate::compare::CompareSpec> { self.compare_offer.clone() }
    /// After a conflict: drops the draft and rereads the book at this chapter.
    pub fn reload_chapter(&mut self, cx: &mut Context<Self>) {
        if self.editing.saving || self.editing.chapter_pending.is_some() { return; }
        let Some(at) = self.editing.chapter.as_ref().map(|c| c.at) else { return };
        self.editing = Default::default();
        self.after_repair = Some(at);
        self.reload(cx);
    }
    pub fn copy_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.editing.input { cx.write_to_clipboard(ClipboardItem::new_string(input.read(cx).value().to_string())); }
    }
    pub fn save_markdown(&mut self, cx: &mut Context<Self>) {
        if self.editing.chapter.is_some() { return self.save_chapter(cx); }
        if self.editing.saving || !self.edit_allowed { return; }
        let Some(input) = self.editing.input.as_ref() else { return; };
        let Some(item_id) = self.model.active_item().map(str::to_owned) else { return; };
        if !self.draft_dirty(cx) && self.editing.pending.is_none() { return; }
        let request = self.editing.pending.clone().unwrap_or_else(|| MarkdownEdit {
            item_id, expected_revision_id: self.editing.revision.clone(),
            request_id: memex_client::biblio::new_request_id(), markdown: input.read(cx).value().to_string(),
        });
        self.editing.pending = Some(request.clone());
        self.editing.saving = true;
        self.editing.message = None;
        let client = self.client.clone();
        let sent = request.clone();
        let task = cx.background_executor().spawn(async move { client.edit_markdown(sent) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.editing.saving = false;
                match result {
                    Ok(saved) if saved.item_id == request.item_id && saved.previous_revision_id.as_deref() == Some(&request.expected_revision_id) => {
                        this.editing.baseline = request.markdown;
                        this.editing.revision = saved.revision_id;
                        this.editing.pending = None;
                        this.editing.message = Some("已保存新版本；原始资料和旧版本保留。旧版划线不会猜测迁移到新版。".into());
                        this.fetch(cx);
                        cx.emit(ReaderEvent::Saved);
                    }
                    Ok(_) => this.editing.message = Some("保存回执不匹配，结果未确认；再次保存会核实同一请求。".into()),
                    Err(error) => {
                        let unknown = !matches!(&error, memex_client::ClientError::Daemon(c) if matches!(c.as_str(),
                            "invalid_markdown_edit" | "stale_item_revision" | "request_id_conflict" |
                            "library_resource_not_found" | "item_revision_limit_exceeded" | "permission_denied"));
                        if !unknown { this.editing.pending = None; }
                        this.editing.message = Some(if unknown { "保存结果未确认，草稿仍在。再次保存会核实原请求，不会重复创建版本。".into() } else { error.message() });
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }
    pub fn discard_markdown(&mut self, cx: &mut Context<Self>) {
        if self.editing.saving || self.editing.pending.is_some() || self.editing.chapter_pending.is_some() { return; }
        // A chapter draft only drops: the book text on screen is unchanged.
        if self.editing.chapter.is_some() { self.editing = Editing::default(); cx.notify(); return; }
        self.editing = Editing::default();
        self.fetch(cx);
    }
    pub(super) fn editor_panel(&self) -> AnyElement {
        match &self.editing.input {
            Some(input) => div().flex_1().min_h_0().size_full().child(Textarea::new(input).size_full()).into_any_element(),
            None => div().into_any_element(),
        }
    }
    pub(super) fn reading_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let dirty = self.draft_dirty(cx);
        let e = &self.editing;
        let pending = e.pending.is_some() || e.chapter_pending.is_some();
        let save = if e.saving { "保存中…" } else if e.chapter_pending.is_some() { "核实保存结果" } else { "保存" };
        let editing_line = e.chapter.as_ref().map(|c| txf("正在编辑：{0} · {1}", &[&tx(if c.side == "original" { "原文" } else { "译文" }), &c.title]).to_string());
        h_flex().gap_1().flex_wrap().items_center()
            .when_some(editing_line, |d, line| d.child(div().text_xs().font_weight(FontWeight::MEDIUM).mr_1().child(line)))
            .when(self.editing.input.is_some(), |d| d
                .child(Button::new("save-markdown").small().primary().label(tx(save))
                    // After a conflict the same base can only fail again: copy, then reload.
                    .disabled(e.saving || e.stale || (!dirty && !pending)).on_click(cx.listener(|r, _, _, cx| r.save_markdown(cx))))
                .when(!e.stale, |d| d.child(Button::new("discard-markdown").small().ghost().label("放弃修改").disabled(e.saving || pending)
                    .on_click(cx.listener(|r, _, _, cx| r.discard_markdown(cx)))))
                .when(e.stale, |d| d
                    .child(Button::new("copy-draft").small().outline().label(tx("复制草稿")).on_click(cx.listener(|r, _, _, cx| r.copy_draft(cx))))
                    .child(Button::new("reload-chapter").small().outline().label(tx("放弃草稿并重新载入")).on_click(cx.listener(|r, _, _, cx| r.reload_chapter(cx)))))
                .child(div().text_xs().child(if dirty { "未保存" } else { "已保存" })))
            .when_some(self.compare_offer.as_ref().filter(|o| !self.editing.visible && self.compare.is_none() && self.model.active_item() == Some(o.item_id.as_str())), |d, _| d
                .child(Button::new("view-last-change").small().outline().label(tx("查看本次修改")).on_click(cx.listener(|r, _, _, cx| r.view_last_change(cx)))))
            .when(!self.editing.visible, |d| d
                .child(Button::new("copy-selection").small().ghost().label("复制选中").on_click(cx.listener(|r, _, _, cx| r.copy_selection(cx))))
                .child(Button::new("underline-selection").small().ghost().label("划线").disabled(!self.annotations_allowed || self.annotation_saving || self.model.book().is_some())
                    .on_click(cx.listener(|r, _, _, cx| r.mark_selection(false, cx))))
                .child(Button::new("remove-underline").small().ghost().label("取消划线").disabled(!self.annotations_allowed || self.annotation_saving || self.model.book().is_some())
                    .on_click(cx.listener(|r, _, _, cx| r.mark_selection(true, cx)))))
            .into_any_element()
    }
    pub(super) fn load_annotations(&mut self, cx: &mut Context<Self>) {
        if !self.annotations_allowed { return; }
        let Some(detail) = self.model.detail() else { return; };
        let (item, revision) = (detail.canonical.item_id.clone(), detail.canonical.revision_id.clone());
        let expected = ReadingAnnotations::empty(item.clone(), revision.clone(), &detail.canonical.markdown);
        let client = self.client.clone();
        let ticket = self.ticket;
        let task = cx.background_executor().spawn(async move { client.reading_annotations(item, revision) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if ticket != this.ticket { return; }
                match result {
                    Ok(marks) if marks.item_id == expected.item_id && marks.revision_id == expected.revision_id && marks.markdown_sha256 == expected.markdown_sha256 => this.annotations = Some(marks),
                    _ => this.annotation_notice = Some("划线读取失败；正文仍可阅读，刷新后重试。".into()),
                }
                cx.notify();
            }).ok();
        }).detach();
    }
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.selected.borrow();
        let mut parts: Vec<_> = selection.values().collect();
        parts.sort_by_key(|p| p.order);
        let text = parts.into_iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n");
        if !text.is_empty() { cx.write_to_clipboard(ClipboardItem::new_string(text)); }
    }
    pub fn mark_selection(&mut self, remove: bool, cx: &mut Context<Self>) {
        if self.annotation_saving { return; }
        let parts = self.selected.borrow().values().cloned().collect::<Vec<_>>();
        if parts.is_empty() || parts.iter().any(|p| p.translation || p.source.is_none()) {
            self.annotation_notice = Some("请在原文中选中文字后划线；译文没有原文坐标映射，不会混用。".into());
            cx.notify(); return;
        }
        let Some(current) = self.annotations.clone() else { self.load_annotations(cx); return; };
        let start = parts.iter().filter_map(|p| p.source.as_ref().map(|r| r.start)).min().unwrap();
        let end = parts.iter().filter_map(|p| p.source.as_ref().map(|r| r.end)).max().unwrap();
        let mut next = current.clone();
        if remove { next.unmark(start..end); } else { next.mark(start..end); }
        self.annotation_saving = true;
        self.annotation_notice = Some("正在保存划线…".into());
        let client = self.client.clone();
        let ticket = self.ticket;
        let task = cx.background_executor().spawn(async move { client.save_annotations(current.version(), next) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.annotation_saving = false;
                if this.ticket != ticket { return; }
                match result {
                    Ok(marks) => { this.annotations = Some(marks); this.annotation_notice = Some("划线已保存".into()); }
                    Err(error) => { this.annotation_notice = Some(format!("划线未确认：{}。请刷新核对后再操作。", error.message())); }
                }
                cx.notify();
            }).ok();
        }).detach();
        cx.notify();
    }
}
