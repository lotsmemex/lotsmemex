//! Bibliographic fields in the inspector (md1 panels A and B) as their own
//! entity. Editing goes through `bibliographic_metadata_edit` and only when the
//! agent advertises `bibliographic_metadata_v2`; otherwise nothing is read or
//! sent and a note says so. The 补全预览 sheet lives in `lookup_view`.
//!
//! Interface to `Metadata`: `fetch` (background), `set_loaded`, `clear`,
//! `save`, `reset`, `is_dirty`, `blocks_navigation`, `locked`, and
//! `BiblioEvent`s for outcomes that affect the whole record.

use crate::biblio::{field_of, BiblioDraft, CreatorInput, ROLES};
use crate::lookup::{field_label, LookupSheet};
use crate::view::{field, minutes_field, value_box};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::biblio::{identifier_label, IdentifierDuplicate, IdentifierQuery};
use memex_client::{tx, txf, Bibliographic, Capabilities, Client, ClientError, DatePrecision, Lang, Sensitivity};
use std::collections::HashMap;

/// Read ceiling for the owner's own inspector.
pub const CEILING: Sensitivity = Sensitivity::Restricted;

/// Shown when the backend has no bibliographic read (a key, via `relabel`).
pub const NO_BIBLIO: &str = "当前代理未提供书目字段（作者、日期、DOI 等）";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiblioEvent {
    /// A new revision exists (an edit or a lookup apply); reload the record.
    Saved { revision_id: String, note: Option<String> },
    /// `stale_item_revision`: lock like a stale title save; Cancel reloads.
    Conflict(String),
    /// The agent found nothing to change; reload quietly.
    Unchanged,
    /// Field errors are shown inline; this is the status line for them.
    Rejected(String),
    /// 放大阅读: show the AI 纪要 in the reader's full reading area.
    ReadMinutes,
    /// 生成 beside 未生成 (manual minutes, `minutes_request_v1`).
    GenerateMinutes,
    /// The save failed without a field (transport, server state).
    Failed(String),
}

/// What `fetch` found for one revision.
pub struct Loaded {
    caps: Result<Capabilities, ClientError>,
    biblio: Option<Result<Bibliographic, ClientError>>,
    duplicates: Vec<IdentifierDuplicate>,
}

/// Blocking reads for the background task: capabilities first, and v2
/// commands only when advertised.
pub fn fetch(client: &Client, item_id: &str, revision_id: &str) -> Loaded {
    let caps = client.capabilities();
    let biblio = caps.as_ref().ok().filter(|c| c.bibliographic).map(|_| client.bibliographic(item_id, revision_id, CEILING));
    let duplicates = match &biblio {
        Some(Ok(b)) => {
            let ids: Vec<IdentifierQuery> = b
                .identifiers
                .iter()
                .filter(|i| matches!(i.kind.as_str(), "doi" | "isbn") && i.is_valid())
                .map(|i| IdentifierQuery { kind: i.kind.clone(), value: i.display() })
                .collect();
            if ids.is_empty() { vec![] } else { client.identifier_duplicates(ids, Some(item_id.into()), CEILING).unwrap_or_default() }
        }
        _ => vec![],
    };
    Loaded { caps, biblio, duplicates }
}

pub struct BiblioEditor {
    pub(crate) client: Client,
    pub(crate) caps: Capabilities,
    pub(crate) item_id: Option<String>,
    pub(crate) revision_id: Option<String>,
    pub(crate) draft: Option<BiblioDraft>,
    note: Option<String>,
    duplicates: Vec<IdentifierDuplicate>,
    /// Set by `Metadata` while the record is saving or in conflict.
    pub locked: bool,
    saving: bool,
    pub(crate) sheet: Option<LookupSheet>,
    inputs: HashMap<String, Entity<InputState>>,
    input_lang: Lang,
    sync: bool,
    subscriptions: Vec<Subscription>,
    pub(crate) ticket: u64,
    /// AI 纪要 of the same revision (from the analysis read), shown where the
    /// abstract used to be. `None`: the agent does not report minutes.
    pub minutes: Option<memex_client::AiMinutes>,
    /// The URL host last set as the site-name placeholder, per input.
    site_placeholder: Option<(EntityId, String)>,
}

impl EventEmitter<BiblioEvent> for BiblioEditor {}

impl BiblioEditor {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            caps: Capabilities::default(),
            item_id: None,
            revision_id: None,
            draft: None,
            note: None,
            duplicates: vec![],
            locked: false,
            saving: false,
            sheet: None,
            inputs: HashMap::new(),
            input_lang: memex_client::i18n::language(),
            sync: false,
            subscriptions: vec![],
            ticket: 0,
            minutes: None,
            site_placeholder: None,
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.ticket += 1;
        self.item_id = None;
        self.revision_id = None;
        self.draft = None;
        self.note = None;
        self.duplicates.clear();
        self.saving = false;
        self.sheet = None;
        self.minutes = None;
        cx.notify();
    }

    pub fn set_loaded(&mut self, item_id: &str, revision_id: &str, loaded: Loaded, cx: &mut Context<Self>) {
        self.clear(cx);
        self.item_id = Some(item_id.into());
        self.revision_id = Some(revision_id.into());
        match loaded.caps {
            Ok(caps) => self.caps = caps,
            Err(e) => self.note = Some(txf("书目字段读取失败：{0}", &[&e.message()])),
        }
        match loaded.biblio {
            Some(Ok(b)) => self.draft = Some(BiblioDraft::new(b)),
            Some(Err(ClientError::Unsupported)) => self.note = Some(NO_BIBLIO.into()),
            Some(Err(e)) => self.note = Some(txf("书目字段读取失败：{0}", &[&e.message()])),
            None if self.note.is_none() => self.note = Some(NO_BIBLIO.into()),
            None => {}
        }
        self.duplicates = loaded.duplicates;
        self.sync = true;
        cx.notify();
    }

    pub fn original(&self) -> Option<&Bibliographic> {
        self.draft.as_ref().map(BiblioDraft::original)
    }

    /// Why bibliographic fields are absent, if they are.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.draft.as_ref().is_some_and(BiblioDraft::is_dirty)
    }

    pub fn is_saving(&self) -> bool {
        self.saving || self.sheet.as_ref().is_some_and(|s| s.applying)
    }

    pub fn blocks_navigation(&self) -> bool {
        self.is_dirty() || self.is_saving()
    }

    fn editable(&self) -> bool {
        self.caps.bibliographic && self.draft.is_some() && !self.locked && !self.is_saving()
    }

    /// Discards local edits and errors.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = &mut self.draft {
            d.reset();
            self.sync = true;
        }
        cx.notify();
    }

    /// Starts the edit for the draft against `expected_revision_id`. `allow_reuse`
    /// is false when the title/tag save just produced that revision. Returns
    /// false when there was nothing to send (or a local field error).
    pub fn save(&mut self, expected_revision_id: String, allow_reuse: bool, cx: &mut Context<Self>) -> bool {
        if self.saving || !self.caps.bibliographic {
            return false;
        }
        let (Some(item_id), Some(draft)) = (self.item_id.clone(), self.draft.as_mut()) else { return false };
        let edit = match draft.build_edit(&item_id, &expected_revision_id, allow_reuse) {
            Ok(Some(edit)) => edit,
            Ok(None) => return false,
            Err(()) => {
                cx.emit(BiblioEvent::Rejected(tx("有字段需要修改，见标出的位置").into()));
                cx.notify();
                return false;
            }
        };
        self.saving = true;
        cx.notify();
        let ticket = self.ticket;
        let client = self.client.clone();
        let sent = edit.clone();
        let task = cx.background_executor().spawn(async move { client.bibliographic_edit(edit, CEILING) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                this.saving = false;
                this.revision_id = Some(expected_revision_id);
                let event = this.finish_save(sent, result);
                cx.emit(event);
                cx.notify();
            })
            .ok();
        })
        .detach();
        true
    }

    fn finish_save(&mut self, sent: memex_client::biblio::MetadataEdit, result: Result<memex_client::ItemUpdate, ClientError>) -> BiblioEvent {
        let Some(draft) = self.draft.as_mut() else { return BiblioEvent::Unchanged };
        finish_edit(draft, sent, result)
    }

    // ------------------------------------------------------------ inputs

    /// The input for `key`, created on first use with the draft's value.
    fn input(&mut self, key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(state) = self.inputs.get(key) {
            return state.clone();
        }
        let value = self.draft_value(key);
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
        state.update(cx, |s, cx| s.set_value(value, window, cx));
        let k = key.to_string();
        self.subscriptions.push(cx.subscribe_in(&state, window, move |this, state, event, window, cx| match event {
            InputEvent::Change => {
                let value = state.read(cx).value().to_string();
                this.input_changed(&k, value, cx);
            }
            InputEvent::PressEnter { .. } if k == "keyword:new" => {
                let value = state.read(cx).value().to_string();
                this.add_keyword(&value, cx);
                state.update(cx, |s, cx| s.set_value("", window, cx));
            }
            _ => {}
        }));
        self.inputs.insert(key.to_string(), state.clone());
        state
    }

    fn draft_value(&self, key: &str) -> String {
        let Some(d) = &self.draft else { return String::new() };
        if let Some((i, part)) = creator_key(key) {
            return d.inputs.creators.get(i).map(|c| match part {
                "family" => c.family.clone(),
                "given" => c.given.clone(),
                _ => c.literal.clone(),
            }).unwrap_or_default();
        }
        if key == "keyword:new" {
            return String::new();
        }
        d.inputs.get(key).to_string()
    }

    fn input_changed(&mut self, key: &str, value: String, cx: &mut Context<Self>) {
        if !self.editable() || key == "keyword:new" || self.draft_value(key) == value {
            return;
        }
        let Some(d) = self.draft.as_mut() else { return };
        if let Some((i, part)) = creator_key(key) {
            if let Some(c) = d.inputs.creators.get_mut(i) {
                match part {
                    "family" => c.family = value,
                    "given" => c.given = value,
                    _ => c.literal = value,
                }
            }
        } else {
            d.inputs.text.insert(key.to_string(), value);
        }
        d.touched(key);
        cx.notify();
    }

    /// Development/test hook: types `value` into the input `key`.
    pub fn dev_set_input(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        self.input_changed(key, value.to_string(), cx);
        self.sync = true;
    }

    fn add_keyword(&mut self, value: &str, cx: &mut Context<Self>) {
        let value = value.trim();
        if !self.editable() || value.is_empty() {
            return;
        }
        if let Some(d) = self.draft.as_mut() {
            if !d.inputs.keywords.iter().any(|k| k == value) {
                d.inputs.keywords.push(value.to_string());
                d.touched("keywords");
            }
        }
        cx.notify();
    }

    fn update_draft(&mut self, key: &str, f: impl FnOnce(&mut crate::biblio::Inputs), cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if let Some(d) = self.draft.as_mut() {
            f(&mut d.inputs);
            d.touched(key);
            self.sync = true;
        }
        cx.notify();
    }

    pub fn set_precision(&mut self, p: DatePrecision, cx: &mut Context<Self>) {
        self.update_draft("precision", |i| {
            i.precision = Some(p);
            let cut = match p {
                DatePrecision::Year => 4,
                DatePrecision::Month => 7,
                _ => usize::MAX,
            };
            if let Some(date) = i.text.get_mut("date") {
                if date.len() > cut && date.is_char_boundary(cut) {
                    date.truncate(cut);
                }
            }
        }, cx);
    }

    pub fn add_creator(&mut self, cx: &mut Context<Self>) {
        self.update_draft("creators", |i| i.creators.push(CreatorInput::new_person()), cx);
    }

    pub fn remove_creator(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.update_draft("creators", |i| {
            if ix < i.creators.len() {
                i.creators.remove(ix);
            }
        }, cx);
    }

    fn set_role(&mut self, ix: usize, role: &str, cx: &mut Context<Self>) {
        self.update_draft("creators", |i| {
            if let Some(c) = i.creators.get_mut(ix) {
                c.role = role.to_string();
            }
        }, cx);
    }

    /// Person ⇄ one literal name. Nothing is split: a literal becomes the family
    /// input as a whole, a person's parts are joined into the literal.
    fn set_person(&mut self, ix: usize, person: bool, cx: &mut Context<Self>) {
        self.update_draft("creators", |i| {
            let Some(c) = i.creators.get_mut(ix) else { return };
            if c.person == person {
                return;
            }
            if person {
                if c.family.trim().is_empty() && c.given.trim().is_empty() {
                    c.family = c.literal.clone();
                }
            } else if c.literal.trim().is_empty() {
                let (f, g) = (c.family.trim(), c.given.trim());
                c.literal = if f.is_ascii() && g.is_ascii() { format!("{g} {f}").trim().to_string() } else { format!("{f}{g}") };
            }
            c.person = person;
        }, cx);
    }

    fn remove_keyword(&mut self, k: &str, cx: &mut Context<Self>) {
        let k = k.to_string();
        self.update_draft("keywords", move |i| i.keywords.retain(|x| *x != k), cx);
    }
}

/// Maps one edit outcome onto the draft: success commits, an unknown outcome
/// keeps the request for an identical retry, field rejections go inline.
pub fn finish_edit(draft: &mut BiblioDraft, sent: memex_client::biblio::MetadataEdit, result: Result<memex_client::ItemUpdate, ClientError>) -> BiblioEvent {
    match result {
        Ok(receipt) => {
            draft.commit();
            BiblioEvent::Saved { revision_id: receipt.revision_id, note: None }
        }
        Err(e) if e.is_stale_revision() => {
            draft.set_pending(None);
            BiblioEvent::Conflict(e.message())
        }
        Err(ClientError::Daemon(code)) if code == "metadata_unchanged" => {
            draft.set_pending(None);
            BiblioEvent::Unchanged
        }
        Err(e) if e.field_error().is_some() => {
            draft.set_pending(None);
            let (field, reason) = e.field_error().unwrap_or_default();
            draft.set_errors(field, reason);
            BiblioEvent::Rejected(tx("有字段未被接受，见标出的位置").into())
        }
        Err(e) if e.is_transport() => {
            draft.set_pending(Some(sent));
            BiblioEvent::Failed(txf("保存结果未知：{0}。再次保存会重试同一请求，不会重复写入。", &[&e.message()]))
        }
        Err(e) => {
            draft.set_pending(None);
            BiblioEvent::Failed(txf("保存失败：{0}", &[&e.message()]))
        }
    }
}

fn creator_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("creator:")?;
    let (i, part) = rest.split_once(':')?;
    Some((i.parse().ok()?, part))
}

pub fn role_label(role: &str) -> String {
    match role {
        "author" => tx("作者").into(),
        "editor" => tx("编者").into(),
        "translator" => tx("译者").into(),
        "contributor" => tx("贡献者").into(),
        other => other.to_string(),
    }
}

fn precision_label(p: DatePrecision) -> &'static str {
    match p {
        DatePrecision::Year => tx("精确到年"),
        DatePrecision::Month => tx("精确到月"),
        DatePrecision::Day => tx("精确到日"),
        DatePrecision::Unparsed => tx("未解析"),
    }
}

/// A rejected value in words, next to its field.
pub fn reason_text(field: &str, reason: &str) -> String {
    if let Some(missing) = reason.strip_prefix("requires:") {
        return txf("需要同时提供“{0}”", &[&field_label(missing)]);
    }
    tx(match (field, reason) {
        ("number_of_pages", _) => "页数须为正整数",
        (_, "checksum") => "校验位不正确",
        (_, "syntax") => "格式不正确",
        (_, "prefix") => "前缀不正确",
        (_, "length") => "位数不正确",
        (_, "duplicate_or_empty") => "标识符重复或为空",
        (_, "precision_mismatch") => "日期与所选精度不一致",
        (_, "format") => "日期格式不正确，请按所选精度填写",
        (_, "missing_value") => "请填写日期",
        (_, "unparsed_without_raw") => "未解析的日期需要填写原文",
        (_, "readonly") => "此字段只读",
        (_, "unknown_field") => "代理不认识此字段",
        _ => "此值未被接受",
    })
    .into()
}

fn error_line(text: String, cx: &App) -> Div {
    h_flex().gap_1().text_xs().text_color(cx.theme().danger).child(Icon::new(IconName::CircleAlert).xsmall()).child(text)
}

impl Render for BiblioEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = memex_client::i18n::language();
        if self.input_lang != lang {
            // Placeholders are set at creation; recreate in the new language.
            self.input_lang = lang;
            self.inputs.clear();
            self.subscriptions.clear();
        }
        if self.sync {
            self.sync = false;
            let keys: Vec<String> = self.inputs.keys().cloned().collect();
            for key in keys {
                let value = self.draft_value(&key);
                if let Some(state) = self.inputs.get(&key).cloned() {
                    if state.read(cx).value() != value.as_str() {
                        state.update(cx, |s, cx| s.set_value(value, window, cx));
                    }
                }
            }
        }
        let theme = cx.theme().clone();
        let Some(draft) = self.draft.clone() else {
            return v_flex().into_any_element();
        };
        let editable = self.editable();
        let b = draft.original().clone();
        let inputs = draft.inputs.clone();
        let err = |field: &str, cx: &App| draft.error(field).map(|r| error_line(reason_text(field, r), cx));
        let muted = theme.muted_foreground;
        let mut rows: Vec<AnyElement> = vec![];

        // Lookup (panel C entry): only for a locally valid DOI/ISBN whose source
        // the agent advertises.
        let mut lookups = h_flex().gap_2().justify_end();
        let mut any_lookup = false;
        for kind in ["doi", "isbn"] {
            if self.caps.lookup_source(kind).is_some() && b.identifier(kind).is_some_and(|i| i.is_valid()) {
                any_lookup = true;
                let blocked = !editable || draft.is_dirty();
                lookups = lookups.child(
                    Button::new(SharedString::from(format!("lookup-{kind}")))
                        .outline()
                        .small()
                        .label(txf("按 {0} 补全", &[&identifier_label(kind)]))
                        .disabled(blocked)
                        .when(blocked && draft.is_dirty(), |b| b.tooltip(tx("请先保存或取消修改")))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_lookup(kind, cx))),
                );
            }
        }
        if any_lookup {
            rows.push(lookups.into_any_element());
        }

        // Creators: one row each, never split.
        let n = inputs.creators.len();
        for (ix, c) in inputs.creators.iter().enumerate() {
            let first_of_role = ix == 0 || inputs.creators[ix - 1].role != c.role;
            let label = if first_of_role { role_label(&c.role) } else { String::new() };
            let mut names = h_flex().flex_1().min_w_0().gap_1();
            if c.person {
                let f = self.input(&format!("creator:{ix}:family"), tx("姓"), window, cx);
                let g = self.input(&format!("creator:{ix}:given"), tx("名"), window, cx);
                names = names.child(div().flex_1().min_w(px(48.)).child(Input::new(&f).small().disabled(!editable))).child(div().flex_1().min_w(px(48.)).child(Input::new(&g).small().disabled(!editable)));
            } else {
                let l = self.input(&format!("creator:{ix}:literal"), tx("机构或完整姓名"), window, cx);
                names = names.child(div().flex_1().min_w_0().child(Input::new(&l).small().disabled(!editable)));
            }
            let handle = cx.entity();
            let (role, person) = (c.role.clone(), c.person);
            let menu = Button::new(("creator-role", ix))
                .ghost()
                .xsmall()
                .label(role_label(&c.role))
                .dropdown_caret(true)
                .disabled(!editable)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for r in ROLES {
                        let h = handle.clone();
                        menu = menu.item(PopupMenuItem::new(role_label(r)).checked(role == r).on_click(move |_, _, cx| h.update(cx, |e, cx| e.set_role(ix, r, cx))));
                    }
                    menu = menu.separator();
                    let h = handle.clone();
                    menu = menu.item(PopupMenuItem::new(tx("个人（姓、名分开）")).checked(person).on_click(move |_, _, cx| h.update(cx, |e, cx| e.set_person(ix, true, cx))));
                    let h = handle.clone();
                    menu.item(PopupMenuItem::new(tx("机构或整名（不拆分）")).checked(!person).on_click(move |_, _, cx| h.update(cx, |e, cx| e.set_person(ix, false, cx))))
                });
            let remove = Button::new(("creator-remove", ix))
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Minus).text_color(theme.primary))
                .tooltip(tx("移除"))
                .disabled(!editable)
                .on_click(cx.listener(move |this, _, _, cx| this.remove_creator(ix, cx)));
            rows.push(field(&label, h_flex().gap_1().items_center().child(names).child(menu).when(editable, |d| d.child(remove)), cx).into_any_element());
        }
        let add = Button::new("creator-add")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::Plus).text_color(theme.primary))
            .label(tx("添加作者"))
            .disabled(!editable)
            .on_click(cx.listener(|this, _, _, cx| this.add_creator(cx)));
        rows.push(
            field(if n == 0 { tx("作者") } else { "" }, v_flex().gap_1().when(editable, |d| d.child(div().child(add))).when(n == 0 && !editable, |d| d.child("—")).children(err("creators", cx)), cx)
                .into_any_element(),
        );

        // Type-specific fields.
        let text = |this: &mut Self, key: &str, label: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let state = this.input(key, placeholder, window, cx);
            let e = draft.error(field_of(key)).map(|r| error_line(reason_text(field_of(key), r), cx));
            field(label, v_flex().gap_1().child(Input::new(&state).small().disabled(!editable)).children(e), cx).into_any_element()
        };
        let date = {
            let precision = inputs.precision.unwrap_or(DatePrecision::Day);
            let (key, placeholder) = match precision {
                DatePrecision::Year => ("date", "YYYY"),
                DatePrecision::Month => ("date", "YYYY-MM"),
                DatePrecision::Day => ("date", "YYYY-MM-DD"),
                DatePrecision::Unparsed => ("date_raw", tx("原文，如 2019年春季")),
            };
            let state = self.input(key, placeholder, window, cx);
            let handle = cx.entity();
            let menu = Button::new("date-precision")
                .ghost()
                .xsmall()
                .label(precision_label(precision))
                .dropdown_caret(true)
                .disabled(!editable)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for p in [DatePrecision::Year, DatePrecision::Month, DatePrecision::Day, DatePrecision::Unparsed] {
                        let h = handle.clone();
                        menu = menu.item(PopupMenuItem::new(precision_label(p)).checked(p == precision).on_click(move |_, _, cx| h.update(cx, |e, cx| e.set_precision(p, cx))));
                    }
                    menu
                });
            field(
                tx("发布日期"),
                v_flex().gap_1().child(h_flex().gap_1().items_center().child(div().flex_1().min_w_0().child(Input::new(&state).small().disabled(!editable))).child(menu)).children(err("published_date", cx)),
                cx,
            )
            .into_any_element()
        };
        let pair = |this: &mut Self, (k1, l1): (&str, &str), (k2, l2): (&str, &str), window: &mut Window, cx: &mut Context<Self>| {
            let a = this.input(k1, "", window, cx);
            let c = this.input(k2, "", window, cx);
            let e = draft.error(k1).map(|r| error_line(reason_text(k1, r), cx)).or_else(|| draft.error(k2).map(|r| error_line(reason_text(k2, r), cx)));
            field(
                l1,
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().min_w(px(40.)).child(Input::new(&a).small().disabled(!editable)))
                            .child(div().flex_none().text_color(muted).child(l2.to_string()))
                            .child(div().flex_1().min_w(px(40.)).child(Input::new(&c).small().disabled(!editable))),
                    )
                    .children(e),
                cx,
            )
            .into_any_element()
        };
        // 时长 below 发布日期: read-only, from the current revision; only when known.
        let duration = memex_client::duration_label(b.duration_seconds).map(|t| field(tx("时长"), value_box(Some(t), cx), cx).into_any_element());
        let mut id_kinds: Vec<&str> = vec![];
        match b.item_type.as_str() {
            "journal_article" => {
                rows.push(text(self, "journal_title", tx("期刊"), "", window, cx));
                // Pages get their own row: ranges and e-locators can be long.
                rows.push(pair(self, ("volume", tx("卷")), ("issue", tx("期")), window, cx));
                rows.push(text(self, "pages", tx("页"), "", window, cx));
                rows.push(date);
                rows.extend(duration);
                id_kinds.extend(["doi", "issn"]);
                if b.publisher.is_some() {
                    rows.push(text(self, "publisher", tx("出版社"), "", window, cx));
                }
            }
            "book" => {
                rows.push(text(self, "publisher", tx("出版社"), "", window, cx));
                rows.push(text(self, "publication_place", tx("出版地"), "", window, cx));
                rows.push(date);
                rows.extend(duration);
                rows.push(text(self, "edition", tx("版次"), "", window, cx));
                rows.push(text(self, "number_of_pages", tx("页数"), "", window, cx));
                id_kinds.extend(["isbn", "doi"]);
            }
            "webpage" => {
                // An empty site name shows the URL host as a placeholder (never saved).
                let host = b.link_host().unwrap_or_default();
                let state = self.input("site_name", "", window, cx);
                if self.site_placeholder.as_ref() != Some(&(state.entity_id(), host.clone())) {
                    self.site_placeholder = Some((state.entity_id(), host.clone()));
                    state.update(cx, |s, cx| s.set_placeholder(host, window, cx));
                }
                rows.push(text(self, "site_name", tx("网站"), "", window, cx));
                rows.push(date);
                rows.extend(duration);
            }
            // Other types keep whatever they carry, without suggested empty fields.
            _ => {
                rows.push(date);
                rows.extend(duration);
                id_kinds.extend(["doi", "isbn", "issn"].into_iter().filter(|k| b.identifier(k).is_some()));
            }
        }
        // 网址: the canonical (else source) URL, opened in the system browser.
        if let Some(url) = b.link().map(str::to_string) {
            let shown = url.split_once("://").map(|(_, r)| r).unwrap_or(&url).trim_end_matches('/').to_string();
            let open = url.clone();
            rows.push(
                field(
                    tx("网址"),
                    h_flex()
                        .id("source-url")
                        .gap_1()
                        .items_center()
                        .min_w_0()
                        .pt(px(2.))
                        .cursor_pointer()
                        .text_color(theme.primary)
                        .child(div().min_w_0().truncate().child(shown))
                        .child(Icon::new(IconName::ExternalLink).xsmall().flex_none())
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(url.clone()).build(window, cx))
                        .on_click(move |_, _, cx| cx.open_url(&open)),
                    cx,
                )
                .into_any_element(),
            );
        }
        for kind in &id_kinds {
            let state = self.input(kind, "", window, cx);
            let stored = b.identifier(kind);
            let unchanged = inputs.get(kind) == stored.map(|i| i.display()).unwrap_or_default();
            let badge = stored.filter(|_| unchanged).and_then(|i| i.validation.as_ref()).map(|v| {
                let ok = v.status == "valid";
                h_flex()
                    .flex_none()
                    .gap_1()
                    .text_xs()
                    .text_color(if ok { gpui_kit::hsla(0.38, 0.45, 0.42, 1.) } else { theme.warning })
                    .child(Icon::new(if ok { IconName::CircleCheck } else { IconName::TriangleAlert }).xsmall())
                    .child(if ok { tx("校验通过").to_string() } else { tx("未通过校验").to_string() })
            });
            rows.push(field(&identifier_label(kind), h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&state).small().disabled(!editable))).children(badge), cx).into_any_element());
        }
        if let Some(e) = err("identifiers", cx) {
            rows.push(field("", e, cx).into_any_element());
        }
        if !self.duplicates.is_empty() {
            let kinds: Vec<String> = self.duplicates.iter().map(|d| identifier_label(&d.kind)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            rows.push(
                field(
                    "",
                    h_flex()
                        .gap_1()
                        .items_start()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::Info).xsmall())
                        .child(div().flex_1().min_w_0().child(txf("另有 {0} 个条目使用相同的 {1}（未合并）", &[&self.duplicates.len(), &kinds.join("/")]))),
                    cx,
                )
                .into_any_element(),
            );
        }
        if b.item_type == "book" || b.series_title.is_some() {
            rows.push(pair(self, ("series_title", tx("丛书")), ("series_number", "#"), window, cx));
        }
        rows.push(text(self, "language", tx("语言"), tx("如 zh、en"), window, cx));

        let mut keywords = h_flex().flex_wrap().gap_1().items_center();
        // Values a model provided carry a subtle AI mark (manual tags never do).
        let model_keywords = b.from_model("keywords");
        for k in &inputs.keywords {
            let kw = k.clone();
            let ai = model_keywords && b.keywords.contains(k);
            keywords = keywords.child(
                h_flex()
                    .id(SharedString::from(format!("kw-{k}")))
                    .gap_1()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(theme.secondary)
                    .when(ai, |d| d.border_1().border_color(theme.primary.opacity(0.25)))
                    .text_xs()
                    .when(ai, |d| {
                        d.child(div().text_size(px(9.)).font_weight(FontWeight::SEMIBOLD).text_color(theme.primary).child("AI"))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tx("由模型补全的关键词")).build(window, cx))
                    })
                    .child(k.clone())
                    .when(editable, |d| d.child(div().id(SharedString::from(format!("kw-x-{k}"))).child(Icon::new(IconName::Close).xsmall()).on_click(cx.listener(move |this, _, _, cx| this.remove_keyword(&kw, cx))))),
            );
        }
        if editable {
            let state = self.input("keyword:new", tx("添加关键词"), window, cx);
            keywords = keywords.child(div().w(px(110.)).child(Input::new(&state).xsmall()));
        } else if inputs.keywords.is_empty() {
            keywords = keywords.child(div().text_color(muted).child("—"));
        }
        rows.push(field(tx("关键词"), v_flex().gap_1().child(keywords).children(err("keywords", cx)), cx).into_any_element());
        // AI 纪要 takes the abstract's old place; the abstract follows it.
        let not_requested = self.minutes.as_ref().is_some_and(|m| m.state == memex_client::MinutesState::NotRequested);
        let generate = (self.caps.minutes_request && not_requested).then(|| {
            Button::new("minutes-generate")
                .outline()
                .xsmall()
                .icon(IconName::BookOpen)
                .label(tx("生成"))
                .tooltip(tx("生成这篇的 AI 纪要"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(BiblioEvent::GenerateMinutes)))
                .into_any_element()
        });
        if let Some(m) = minutes_field(self.minutes.as_ref(), generate, cx) {
            // Above the text, so a long 纪要 never pushes it out of sight.
            if self.minutes.as_ref().and_then(|m| m.text()).is_some() {
                let open = Button::new("minutes-open").ghost().xsmall().icon(IconName::Maximize).label(tx("放大阅读")).on_click(cx.listener(|_, _, _, cx| cx.emit(BiblioEvent::ReadMinutes)));
                rows.push(field("", open, cx).into_any_element());
            }
            rows.push(m);
        }
        if let Some(a) = &b.abstract_text {
            rows.push(field(tx("文献摘要"), div().text_color(theme.foreground).child(a.clone()), cx).into_any_element());
        }
        // Errors for fields without an input of their own.
        let shown = ["creators", "identifiers", "published_date", "keywords", "journal_title", "publisher", "volume", "issue", "pages", "publication_place", "edition", "number_of_pages", "series_title", "series_number", "language", "site_name"];
        for (f, r) in draft.errors().iter().filter(|(f, _)| !shown.contains(&f.as_str())) {
            rows.push(field("", error_line(format!("{}：{}", field_label(f), reason_text(f, r)), cx), cx).into_any_element());
        }
        let sheet = self.sheet.is_some().then(|| self.render_sheet(window, cx));
        v_flex().gap_3().children(rows).children(sheet).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{fetch, finish_edit, reason_text, BiblioEvent, CEILING};
    use crate::biblio::BiblioDraft;
    use memex_client::{fixture::FixtureBackend, Client, ClientError};

    #[test]
    fn stale_maps_to_conflict_field_errors_go_inline_and_unknown_outcomes_keep_the_request() {
        let fixture = FixtureBackend::new(8);
        let client = Client::new(fixture.clone());
        let id = "item_fixture_000001";
        let rev = client.item_read(id).unwrap().revision_id;
        let mut d = BiblioDraft::new(client.bibliographic(id, &rev, CEILING).unwrap());
        // Bad ISBN checksum: rejected by the writer, shown next to identifiers.
        d.inputs.text.insert("isbn".into(), "978-7-5086-3381-5".into());
        let edit = d.build_edit(id, &rev, true).unwrap().unwrap();
        let e = finish_edit(&mut d, edit.clone(), client.bibliographic_edit(edit, CEILING));
        assert!(matches!(e, BiblioEvent::Rejected(_)));
        assert_eq!(d.error("identifiers"), Some("checksum"));
        assert!(reason_text("identifiers", "checksum").contains("校验位"));
        // Lost reply: the retry reuses the request id and the agent applies once.
        d.inputs.text.insert("isbn".into(), String::new());
        d.inputs.text.insert("volume".into(), "186".into());
        let edit = d.build_edit(id, &rev, true).unwrap().unwrap();
        assert!(matches!(finish_edit(&mut d, edit.clone(), Err(ClientError::Timeout)), BiblioEvent::Failed(_)));
        let retry = d.build_edit(id, &rev, true).unwrap().unwrap();
        assert_eq!(retry.request_id, edit.request_id);
        let saved = finish_edit(&mut d, retry.clone(), client.bibliographic_edit(retry, CEILING));
        let BiblioEvent::Saved { revision_id, .. } = saved else { panic!("{saved:?}") };
        assert!(!d.is_dirty());
        // Someone else edits: the next save is a conflict, not a rebase.
        let mut d = BiblioDraft::new(client.bibliographic(id, &revision_id, CEILING).unwrap());
        fixture.simulate_external_edit(id);
        d.inputs.text.insert("issue".into(), "9".into());
        let edit = d.build_edit(id, &revision_id, true).unwrap().unwrap();
        assert!(matches!(finish_edit(&mut d, edit.clone(), client.bibliographic_edit(edit, CEILING)), BiblioEvent::Conflict(_)));
    }

    #[test]
    fn without_the_capability_nothing_is_read() {
        let fixture = FixtureBackend::new(8);
        let mut controls = fixture.controls();
        controls.legacy_agent = true;
        fixture.set_controls(controls);
        let client = Client::new(fixture);
        let loaded = fetch(&client, "item_fixture_000001", "rev");
        assert!(loaded.biblio.is_none() && loaded.duplicates.is_empty());
        assert!(!loaded.caps.unwrap().bibliographic);
    }
}
