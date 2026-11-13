//! 补全预览 sheet (md1 panel C): opening a lookup for the item's own DOI/ISBN,
//! the candidate table, and apply. State and rules are in `lookup`; this file
//! only drives requests and draws.

use crate::biblio_view::{BiblioEditor, BiblioEvent, CEILING};
use crate::lookup::{field_label, note_text, value_summary, ApplyOutcome, LookupSheet};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use memex_client::biblio::{added_at_local, identifier_label, provider_label};
use memex_client::{tx, txf};

/// Long values (abstracts) are cut in the table; the applied value is whole.
fn clip(s: String, max: usize) -> String {
    if s.chars().count() > max { format!("{}…", s.chars().take(max).collect::<String>()) } else { s }
}

impl BiblioEditor {
    /// Explicit lookup of this item's own identifier, at the loaded revision.
    pub fn open_lookup(&mut self, kind: &str, cx: &mut Context<Self>) {
        let (Some(item_id), Some(rev)) = (self.item_id.clone(), self.revision_id.clone()) else { return };
        let Some(source) = self.caps.lookup_source(kind).cloned() else { return };
        let Some(value) = self.original().and_then(|b| b.identifier(kind)).filter(|i| i.is_valid()).map(|i| i.display()) else { return };
        self.sheet = Some(LookupSheet::loading(kind, &source.provider));
        cx.notify();
        let ticket = self.ticket;
        let client = self.client.clone();
        let kind = kind.to_string();
        let task = cx.background_executor().spawn(async move { client.lookup_preview(&item_id, &rev, &kind, &value, CEILING) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                if let Some(sheet) = this.sheet.as_mut() {
                    sheet.set_result(result);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn close_lookup(&mut self, cx: &mut Context<Self>) {
        if self.sheet.as_ref().is_some_and(|s| s.applying) {
            return;
        }
        self.sheet = None;
        cx.notify();
    }

    pub fn toggle_candidate(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.toggle(id);
        }
        cx.notify();
    }

    /// Sends `preview_id` + chosen ids; the agent applies its frozen values.
    pub fn apply_lookup(&mut self, cx: &mut Context<Self>) {
        let Some(rev) = self.revision_id.clone() else { return };
        let Some(sheet) = self.sheet.as_mut() else { return };
        let Some(edit) = sheet.start_apply(&rev) else { return };
        let count = edit.selected_candidate_ids.len();
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
                let Some(sheet) = this.sheet.as_mut() else { return };
                match sheet.finish_apply(sent, result) {
                    ApplyOutcome::Applied { revision_id } => {
                        this.sheet = None;
                        cx.emit(BiblioEvent::Saved { revision_id, note: Some(txf("已应用 {0} 项补全", &[&count])) });
                    }
                    ApplyOutcome::Conflict(message) => {
                        this.sheet = None;
                        cx.emit(BiblioEvent::Conflict(message));
                    }
                    ApplyOutcome::Stay => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(sheet) = self.sheet.clone() else { return div().into_any_element() };
        let muted = theme.muted_foreground;
        let provider = provider_label(&sheet.provider).to_string();
        let kind = identifier_label(&sheet.kind);
        let synthetic = self.client.mode().is_fixture();
        let viewport = window.viewport_size();
        let width = px(600.).min(viewport.width - px(48.));

        let header = h_flex()
            .gap_2()
            .items_center()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).font_family(memex_client::reading_family()).child(txf("按 {0} 补全 · 来自 {1}", &[&kind, &provider])))
            .when(synthetic, |d| d.child(div().px_2().rounded(px(6.)).bg(theme.accent).text_xs().text_color(theme.primary).child(tx("合成数据"))))
            .child(div().flex_1())
            .child(Button::new("lookup-close").ghost().xsmall().icon(IconName::Close).disabled(sheet.applying).on_click(cx.listener(|this, _, _, cx| this.close_lookup(cx))));

        let mut body = v_flex().gap_2().px_4().py_3();
        if let Some(p) = &sheet.preview {
            let fetched = added_at_local(Some(&p.fetched_at)).unwrap_or_default();
            body = body.child(div().text_xs().text_color(muted).child(txf("{0} {1} · 查询于 {2}", &[&kind, &p.identifier.value, &fetched])));
            if !p.notes.is_empty() {
                let mut notes = v_flex().gap_1();
                for n in &p.notes {
                    notes = notes.child(h_flex().gap_1().items_start().child(Icon::new(IconName::Info).xsmall().text_color(theme.primary)).child(div().flex_1().min_w_0().child(note_text(n))));
                }
                body = body.child(div().px_2().py_1().rounded(px(6.)).bg(theme.accent.opacity(0.6)).text_xs().text_color(muted).child(notes));
            }
        }
        if sheet.loading {
            body = body.child(div().py_6().text_color(muted).child(txf("正在向 {0} 查询…", &[&provider])));
        }
        if let Some(e) = &sheet.error {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .p_2()
                    .rounded(px(6.))
                    .bg(theme.warning.opacity(0.12))
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                    .child(div().flex_1().min_w_0().child(e.clone())),
            );
        }
        // English labels ("Institutions", "Differs; current kept…") need more room.
        let en = memex_client::i18n::language() == memex_client::Lang::En;
        let (label_w, use_w) = if en { (92., 112.) } else { (64., 84.) };
        if let Some(p) = &sheet.preview {
            let cell = |w: Option<f32>| match w {
                Some(w) => div().w(px(w)).flex_none(),
                None => div().flex_1().min_w_0(),
            };
            let mut table = v_flex().border_1().border_color(theme.border).rounded(px(6.)).text_sm().child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .bg(theme.muted.opacity(0.5))
                    .text_xs()
                    .text_color(muted)
                    .child(cell(Some(label_w)).child(tx("字段")))
                    .child(cell(None).child(tx("现有")))
                    .child(cell(None).child(tx("查到的")))
                    .child(cell(Some(use_w)).child(tx("选用"))),
            );
            let unmet = sheet.unmet();
            for (ix, c) in p.candidates.iter().enumerate() {
                let selected = sheet.is_selected(&c.candidate_id);
                let id = c.candidate_id.clone();
                let handle = cx.entity();
                let choose = if c.unchanged {
                    div().text_xs().text_color(muted).child(tx("一致")).into_any_element()
                } else {
                    v_flex()
                        .gap_1()
                        .child(
                            Checkbox::new(("candidate", ix))
                                .checked(selected)
                                .disabled(sheet.applying || sheet.expired)
                                .on_click(move |_, _, cx| handle.update(cx, |e, cx| e.toggle_candidate(&id, cx))),
                        )
                        .when(c.conflict, |d| d.child(div().text_xs().text_color(theme.warning).child(tx("与现有不同，默认保留现有"))))
                        .into_any_element()
                };
                let missing: Vec<String> = unmet.iter().filter(|(f, _)| *f == c.field && selected).map(|(_, m)| field_label(m)).collect();
                let needs = (!c.requires.is_empty()).then(|| {
                    let names: Vec<String> = c.requires.iter().map(|r| field_label(r)).collect();
                    let text = if missing.is_empty() { txf("需同时选用：{0}", &[&names.join("、")]) } else { txf("必须同时选用“{0}”才能应用", &[&missing.join("、")]) };
                    div().text_xs().text_color(if missing.is_empty() { muted } else { theme.danger }).child(text)
                });
                table = table.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py(px(6.))
                        .items_start()
                        .border_t_1()
                        .border_color(theme.border)
                        .when(selected, |d| d.bg(theme.accent.opacity(0.35)))
                        .child(cell(Some(label_w)).text_color(muted).child(field_label(&c.field)))
                        .child(cell(None).text_xs().text_color(if c.current_value.is_none() { muted } else { theme.foreground }).child(clip(value_summary(&c.field, c.current_value.as_ref()), 90)))
                        .child(cell(None).text_xs().child(v_flex().gap_1().child(clip(value_summary(&c.field, Some(&c.proposed_value)), 90)).children(needs)))
                        .child(cell(Some(use_w)).child(choose)),
                );
            }
            body = body.child(table);
        }

        let footer = h_flex()
            .gap_3()
            .items_center()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child(txf("只向 {0} 发送了此条目的 {1}，没有发送其他内容。", &[&provider, &kind]))
                    .child(tx("只填写你勾选的字段，原始收录内容不改")),
            )
            .child(Button::new("lookup-cancel").label(tx("取消")).disabled(sheet.applying).on_click(cx.listener(|this, _, _, cx| this.close_lookup(cx))))
            .when(sheet.expired || (sheet.preview.is_none() && !sheet.loading), |d| {
                let kind = sheet.kind.clone();
                d.child(Button::new("lookup-again").label(tx("重新查询")).on_click(cx.listener(move |this, _, _, cx| this.open_lookup(&kind, cx))))
            })
            .when(sheet.preview.is_some() && !sheet.expired, |d| {
                d.child(
                    Button::new("lookup-apply")
                        .primary()
                        .label(if sheet.applying { tx("正在应用…").to_string() } else { txf("应用 {0} 项", &[&sheet.count()]) })
                        .disabled(!sheet.can_apply())
                        .on_click(cx.listener(|this, _, _, cx| this.apply_lookup(cx))),
                )
            });

        let panel = v_flex()
            .id("lookup-sheet")
            .w(width)
            .max_h(viewport.height - px(80.))
            .rounded(px(10.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .text_color(theme.foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(div().id("lookup-body").flex_1().min_h_0().overflow_y_scroll().child(body))
            .child(footer);
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("lookup-backdrop")
                    .occlude()
                    .w(viewport.width)
                    .h(viewport.height)
                    .bg(gpui_kit::black().opacity(0.28))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(panel),
            ),
        )
        .with_priority(2)
        .into_any_element()
    }
}

impl BiblioEditor {
    /// Development/test hook: the first candidate id for each of `fields`.
    pub fn dev_candidates(&self, fields: &[&str]) -> Vec<String> {
        let Some(p) = self.sheet.as_ref().and_then(|s| s.preview.as_ref()) else { return vec![] };
        fields.iter().filter_map(|f| p.candidates.iter().find(|c| c.field == *f).map(|c| c.candidate_id.clone())).collect()
    }
}
