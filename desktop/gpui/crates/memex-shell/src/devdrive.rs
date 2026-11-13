//! Development-only scripted walk (feature `dev-snapshot`); fixture states, or an
//! isolated synthetic agent when `MEMEX_AGENT_SOCKET` is set.
//! Drives the live window through the review states with the shell's own
//! intents and saves GPUI's Metal render of each frame. It exercises the app's
//! state transitions, not OS input: keyboard, IME and VoiceOver need a person.

use super::*;
use memex_library::model::Filter;
use std::{fmt::Write as _, path::PathBuf, time::Duration};

pub fn start(window: &mut Window, cx: &mut Context<Shell>) {
    let Some(dir) = std::env::var_os("MEMEX_DESKTOP_SNAPSHOT_DIR").map(PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).ok();
    cx.spawn_in(window, async move |this, cx| {
        let mut log = String::new();
        let executor = cx.background_executor().clone();
        let wait = |ms| executor.timer(Duration::from_millis(ms));
        // A real (isolated) agent answers in seconds, not milliseconds.
        let agent = this.read_with(cx, |s, _| s.fixture.is_none()).unwrap_or(false);
        let settle = if agent { 5000 } else { 1200 };
        wait(if agent { 8000 } else { 2500 }).await;

        macro_rules! step {
            ($name:expr, $body:expr) => {{
                this.update_in(cx, |this, window, cx| {
                    #[allow(clippy::redundant_closure_call)]
                    ($body)(this, window, cx);
                    window.refresh();
                })
                .ok();
                wait(settle).await;
                // Hidden windows have no display-link frames. Let after-layout
                // observers settle before capturing (e.g. the reading percentage).
                cx.update(|window, cx| { window.draw(cx).clear(cx); }).ok();
                // Visible-image entities are first created by this layout;
                // pump occluded test frames through IO, GPUI decode and upload.
                if matches!(std::env::var("MEMEX_DESKTOP_WALK").as_deref(), Ok("images" | "cover")) {
                    for _ in 0..30 {
                        wait(100).await;
                        cx.update(|window, cx| { window.draw(cx).clear(cx); }).ok();
                    }
                } else {
                    wait(50).await;
                }
                let path = dir.join(format!("{}.png", $name));
                // Draw now: an occluded window gets no display-link frames, and the
                // capture would repeat the last presented scene. Twice, because some
                // elements (table headers) lay out from the previous frame's bounds.
                let saved = cx
                    .update(|window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|img| img.save(&path).map_err(|e| e.to_string()))
                    })
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                this.update_in(cx, |this, _window, cx| {
                    let selected = this.metadata.read(cx).item_id().unwrap_or("-").to_string();
                    let _ = writeln!(
                        log,
                        "{}\tview={:?}\tdark={}\tselected={}\tconversation={}\tdirty_block={}\tnotice={:?}\tsaved={:?}\trows={}\trev={:?}\ttitle={:?}\tlist_state={:?}\tskipped_highlights={}",
                        $name,
                        this.prefs.view,
                        this.prefs.appearance.is_dark(this.system_dark),
                        selected,
                        this.chat.read(cx).model.has_conversation(),
                        this.metadata.read(cx).blocks_navigation(cx),
                        this.notice,
                        saved.err(),
                        this.library.read(cx).model.len(),
                        this.reader.read(cx).model.detail().map(|d| d.canonical.revision_id.clone()),
                        this.reader.read(cx).model.detail().map(|d| d.canonical.title.clone()),
                        this.library.read(cx).model.state,
                        this.reader.read(cx).skipped_highlights(),
                    );
                })
                .ok();
            }};
        }
        // Synthetic inputs typed by the walk follow the interface language under review.
        let en = i18n::language() == Lang::En;
        let row_id = |this: &Shell, ix: usize, cx: &App| this.library.read(cx).model.row(ix).map(|r| r.item_id.clone()).unwrap_or_default();

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("folder-drag") {
            assert!(!agent && std::env::var_os("MEMEX_AGENT_SOCKET").is_none());
            assert!(std::env::var("MEMEX_DESKTOP_STATE").unwrap_or_default().starts_with("/private/tmp/"));
            step!("f00-start", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let fixture = this.fixture.as_ref().unwrap();
                fixture.set_controls(FixtureControls { transfer_folders_op: true, ..fixture.controls() });
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.refresh_capabilities(cx);
                this.library.update(cx, |l, cx| l.reload(cx));
            });
            step!("f01-selected-folders-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.sidebar.read(cx).tree().update(cx, |t, cx| {
                    t.click_node("grp_c_cog", Modifiers::default(), cx);
                    t.click_node("grp_c_sleep", Modifiers { alt: true, ..Default::default() }, cx);
                    assert_eq!(t.selection(cx).len(), 2);
                });
            });
            step!("f02-mapped-snapshot-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| {
                    let ids = vec!["grp_c_cog".into(), "grp_c_sleep".into()];
                    let versions = l.model.folder_versions(&ids).unwrap();
                    l.drop_folders(&ids, &versions, "grp_u_mine", false, cx);
                });
            });
            step!("f03-command-move-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let mapped = this.library.read(cx).model.groups().iter().find(|g| g.name == "认知" && g.id != "grp_c_cog").unwrap().id.clone();
                this.sidebar.read(cx).tree().update(cx, |t, cx| t.reveal(&mapped, cx));
                this.set_appearance(Appearance::Dark, cx);
                this.library.update(cx, |l, cx| {
                    assert_eq!(l.model.groups().len(), 21);
                    let ids = vec!["grp_c_sleep".into()];
                    let versions = l.model.folder_versions(&ids).unwrap();
                    l.drop_folders(&ids, &versions, "grp_c_multi", true, cx);
                });
            });
            step!("f03b-moved-folder-visible-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.sidebar.read(cx).tree().update(cx, |t, cx| {
                    t.reveal("grp_c_sleep", cx);
                    t.click_node("grp_c_sleep", Modifiers::default(), cx);
                });
            });
            step!("f04-unknown-keeps-original-request", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                assert_eq!(this.library.read(cx).model.group("grp_c_sleep").unwrap().parent_id.as_deref(), Some("grp_c_multi"));
                let fixture = this.fixture.as_ref().unwrap();
                fixture.set_controls(FixtureControls { folder_commit_error: true, ..fixture.controls() });
                this.library.update(cx, |l, cx| {
                    let ids = vec!["grp_c_sleep".into()];
                    let versions = l.model.folder_versions(&ids).unwrap();
                    l.drop_folders(&ids, &versions, "grp_u_mine", false, cx);
                });
            });
            step!("f05-replayed-once-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                assert!(this.library.read(cx).editor.pending_transfer().is_some());
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.retry_folder_transfer(cx));
            });
            assert!(this.read_with(cx, |s, cx| s.library.read(cx).editor.pending_transfer().is_none() && s.library.read(cx).model.groups().len() == 22).unwrap());
            let _ = writeln!(log, "folder-drag\tsynthetic=true\tproduction_socket=false\tinput-paths=headless-ui-tests\tfinal_nodes=22\treplay=once");
            std::fs::write(dir.join("states.tsv"), log).unwrap();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("interaction") {
            assert!(!agent && std::env::var_os("MEMEX_AGENT_SOCKET").is_none());
            assert!(std::env::var("MEMEX_DESKTOP_STATE").unwrap_or_default().starts_with("/private/tmp/"));
            step!("x01-multiple-items", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let fixture = this.fixture.as_ref().unwrap();
                fixture.set_controls(FixtureControls { group_trash: true, move_members_op: true, ..fixture.controls() });
                this.refresh_capabilities(cx);
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.library.update(cx, |l, cx| { l.set_include_descendants(false, cx); l.set_filter(Filter::Group("grp_c_sleep".into()), cx); });
            });
            step!("x02-selected-menu", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = row_id(this, 0, cx);
                this.select(id.clone(), false, cx);
                this.library.update(cx, |l, cx| {
                    l.model.extend_picked(3);
                    assert_eq!(l.model.selection_len(), 4);
                    l.open_picker(id, point(px(1200.), px(225.)), cx);
                });
            });
            step!("x03-selected-destinations", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| { l.choose_picker_mode(memex_library::view::PickMode::MoveToFolder, cx); assert_eq!(l.model.selection_len(), 4); });
            });
            step!("x04-item-rename", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let id = row_id(this, 0, cx);
                this.library.update(cx, |l, cx| { l.close_picker(cx); l.commit_selection(Some(id.clone()), cx); l.start_item_rename(&id, window, cx); });
            });
            step!("x05-tree-selection-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.clear_item_selection(cx));
                this.set_appearance(Appearance::Dark, cx);
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| {
                    t.click_node("grp_c_cog", Modifiers::default(), cx);
                    t.click_node("grp_c_sleep", Modifiers { shift: true, ..Default::default() }, cx);
                    assert_eq!(t.selection(cx).len(), 2);
                });
            });
            step!("x06-classic-multiple-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Classic, cx);
                this.library.update(cx, |l, cx| { l.set_filter(Filter::Group("grp_c_sleep".into()), cx); });
            });
            step!("x07-classic-mapping-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = row_id(this, 0, cx);
                this.select(id.clone(), false, cx);
                this.library.update(cx, |l, cx| { l.model.extend_picked(2); l.open_picker(id, point(px(480.), px(220.)), cx); });
            });
            step!("x08-recoverable-folder-delete", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| { l.close_picker(cx); l.delete_nodes(&["grp_c_sleep".into()], cx); });
            });
            step!("x09-items-in-library-trash", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| {
                    l.cancel_delete(cx);
                    let id = l.model.row(0).unwrap().item_id.clone();
                    l.model.select(Some(id.clone())); l.model.extend_picked(1);
                    let drag = l.item_drag(&id).unwrap();
                    l.drop_items(&drag, "grp_z_psych", false, true, cx);
                });
            });
            step!("x10-restore-items", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.show_group_trash("grp_z_psych".into(), cx));
            });
            let _ = writeln!(log, "interaction\tsynthetic=true\tproduction_socket=false\tinput-paths=headless-ui-tests");
            std::fs::write(dir.join("states.tsv"), log).unwrap();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("images") {
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            assert!(agent && socket.starts_with("/private/tmp/memex-mobile-") && socket.ends_with("/agent.sock"));
            let id = std::env::var("MEMEX_DEV_IMAGE_ITEM").expect("synthetic image item");
            step!("i01-local-image-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(id.clone(), false, cx);
            });
            step!("i02-inline-image-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.scroll_rows(4, cx));
            });
            step!("i03-missing-image-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.scroll_to_end(cx));
            });
            std::fs::write(dir.join("states.tsv"), log).unwrap();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("library-trash") {
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "library-trash requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let root = this.read_with(cx, |s, cx| s.library.read(cx).model.groups().iter()
                .find(|g| g.name == "Main").map(|g| g.id.clone())).unwrap().expect("synthetic Main");
            step!("t01-library-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group(root.clone()), cx));
                this.sidebar.update(cx, |s, cx| s.reveal(&root, cx));
            });
            let id = this.read_with(cx, |s, cx| row_id(s, 0, cx)).unwrap();
            assert!(!id.is_empty());
            step!("t02-deleted", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.delete_from_group(&id, cx));
            });
            assert_eq!(this.read_with(cx, |s, cx| s.library.read(cx).model.len()).unwrap(), 0);
            step!("t03-trash-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.show_group_trash(root.clone(), cx));
            });
            assert_eq!(this.read_with(cx, |s, cx| s.library.read(cx).trash.entries.len()).unwrap(), 1);
            step!("t04-trash-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("t05-restored", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.restore_from_trash(&id, cx));
            });
            assert!(this.read_with(cx, |s, cx| s.library.read(cx).trash.entries.is_empty()).unwrap());
            step!("t06-library-restored", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group(root.clone()), cx));
            });
            assert_eq!(this.read_with(cx, |s, cx| s.library.read(cx).model.len()).unwrap(), 1);
            std::fs::write(dir.join("states.tsv"), log).unwrap();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("folder-filter") {
            if agent {
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            step!("f01-default-all", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
            });
            step!("f02-explicit-captures", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.set_origin(Some(memex_client::ItemOrigin::Capture), cx));
            });
            step!("f03-folder-all", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_bias".into()), cx));
            });
            step!("f04-explicit-imports", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.set_origin(Some(memex_client::ItemOrigin::Import), cx));
            });
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // The 采集 card under the item list on synthetic captures only: collapsed
        // and expanded, light and dark.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("capture-card") {
            if agent {
                std::fs::write(dir.join("states.tsv"), "capture-card is fixture-only; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let now = memex_library::import_view::now_seconds();
            this.update_in(cx, |this, _, cx| {
                this.fixture.as_ref().unwrap().set_capture_jobs(memex_client::capture_jobs::synthetic_capture_jobs(now));
                this.importer.update(cx, |i, cx| i.poll_capture(cx));
            }).ok();
            let set = |collapsed: bool, dark: bool| move |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                this.importer.update(cx, |i, cx| { i.collapsed = collapsed; cx.notify(); });
            };
            step!("cc01-collapsed-light", set(true, false));
            step!("cc02-expanded-light", set(false, false));
            step!("cc03-collapsed-dark", set(true, true));
            step!("cc04-expanded-dark", set(false, true));
            let _ = writeln!(log, "capture_calls={}", this.read_with(cx, |s, _| s.fixture.as_ref().unwrap().capture_job_calls()).unwrap_or(0));
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("reader-controls") {
            assert!(!agent, "reader-controls refuses a real agent");
            step!("rc00-loaded", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                s.set_view(ViewMode::Reading, cx); s.set_appearance(Appearance::Light, cx);
                s.set_conversation_collapsed(true, cx);
                s.select(memex_client::fixture::SHOWCASE_ITEM.into(), false, cx);
            });
            step!("rc01-status-points", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                s.reader.update(cx, |r, cx| {
                    let mut d = r.model.detail().unwrap().clone();
                    d.canonical.title = "合成验收：五十条重点可连续拨动".into();
                    d.canonical.markdown = (1..=50).map(|i| format!("第 {i} 条重点：保留原始版本和引用坐标，阅读时可以连续浏览所有标注。")).collect::<Vec<_>>().join("\n\n");
                    let items = (0..50).map(|i| serde_json::json!({"block_index":i,"quote":format!("第 {} 条重点",i+1),"style":"highlight"})).collect::<Vec<_>>();
                    let data = serde_json::from_value(serde_json::json!({
                        "highlights":{"state":"succeeded","items":items},
                        "quotes":{"state":"running","items":[]},
                        "fact_check":{"state":"failed","reason":"provider_timeout","items":[]}
                    })).unwrap();
                    r.insights.reset(&d.canonical.item_id, Some(data));
                    r.translate.reset(&d.canonical.item_id, memex_client::AnalysisState::NotRequested, None,
                        Some(memex_client::AiMinutes { state: memex_client::MinutesState::NotRequested, reason: None, text: None }), true);
                    r.model.load(d); r.restore_mode(memex_reader::model::Mode::Original, cx);
                    r.set_emphasis(true, cx); cx.notify();
                });
            });
            let rail = this.read_with(cx, |s,cx|s.reader.read(cx).dev_navigation().0.unwrap()).unwrap();
            let mid = point(rail.left()+px(6.),rail.top()+rail.size.height*0.5);
            step!("rc02-hover-middle", |_: &mut Shell, w: &mut Window, cx: &mut Context<Shell>| {
                w.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent {position:mid,pressed_button:None,modifiers:Modifiers::default()}),cx);
            });
            let middle=this.read_with(cx,|s,cx|s.reader.read(cx).dev_navigation().1).unwrap();
            assert!(middle>10. && middle<40., "hover maps to the middle of all 50 marks");
            for (name, dy, expected) in [("rc03-wheel-last",-5000.,49.),("rc04-wheel-first",5000.,0.)] {
                step!(name, |_: &mut Shell,w:&mut Window,cx:&mut Context<Shell>| {
                    w.dispatch_event(PlatformInput::Scroll(ScrollEvent {position:point(mid.x-px(80.),mid.y),delta:ScrollDelta::Pixels(point(px(0.),px(dy))),modifiers:Modifiers::default(),touch_phase:TouchPhase::Moved,source:ScrollSource::default()}),cx);
                });
                assert_eq!(this.read_with(cx,|s,cx|s.reader.read(cx).dev_navigation().1).unwrap(),expected);
            }
            step!("rc05-dark-dial", |s:&mut Shell,_:&mut Window,cx:&mut Context<Shell>| s.set_appearance(Appearance::Dark,cx));
            std::fs::write(dir.join("states.tsv"),log).unwrap();
            cx.update(|_,cx|cx.quit()).ok(); return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("reader-editing") {
            assert!(!agent, "reader-editing refuses a real agent");
            const BODY: &str = "# 阅读与编辑验收\n\n第一处 **重复文字**；第二处 [重复文字](https://example.invalid)。中文🌹 坐标不能混淆。\n\n这是下一段，保留 Markdown 格式和原文版本。\n\n## 第二节\n\n阅读态可以划线，编辑态保存新版本。";
            step!("re01-original", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                s.set_view(ViewMode::Reading, cx); s.set_appearance(Appearance::Light, cx);
                s.select("item_fixture_000005".into(), false, cx);
                s.reader.update(cx, |r, cx| r.restore_mode(memex_reader::model::Mode::Original, cx));
            });
            step!("re02-editor-dirty", |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                s.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                let editor = s.reader.read(cx).dev_editor().expect("editor enabled");
                editor.update(cx, |e, cx| e.replace_all(BODY, window, cx));
            });
            this.update_in(cx, |s, _, cx| {
                assert!(!s.allow_close(cx), "dirty draft blocks close");
                s.select("item_fixture_000008".into(), false, cx);
                assert_eq!(s.reader.read(cx).model.active_item(), Some("item_fixture_000005"));
                s.notice = None;
            }).unwrap();
            step!("re03-saved-reading", |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                s.reader.update(cx, |r, cx| { r.save_markdown(cx); r.toggle_editor(window, cx); });
            });
            this.read_with(cx, |s, cx| {
                assert_eq!(s.reader.read(cx).model.detail().unwrap().canonical.markdown, BODY);
                assert!(!s.reader.read(cx).blocks_navigation(cx));
            }).unwrap();
            let (from, to) = this.read_with(cx, |s, cx| s.reader.read(cx).dev_selection_points("重复文字")).unwrap().expect("text glyph geometry");
            cx.update(|window, cx| {
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: from, pressed_button: None, modifiers: Modifiers::default() }), cx);
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: from, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }), cx);
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: to, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }), cx);
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: to, modifiers: Modifiers::default(), click_count: 1 }), cx);
            }).unwrap();
            step!("re04-selected", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            this.update_in(cx, |s, window, cx| {
                s.reader.update(cx, |r, cx| r.copy_selection(cx));
                assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()), Some("重复文字".into()));
                // A toolbar mouse-down clears GPUI's window selection. The completed
                // source-bound payload must remain available to the ensuing action.
                gpui_kit::base::TextSelection::clear(window, cx);
            }).unwrap();
            step!("re05-underlined", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| s.reader.update(cx, |r, cx| r.mark_selection(false, cx)));
            this.read_with(cx, |s, cx| {
                let marks = s.reader.read(cx).dev_annotations().unwrap();
                assert_eq!(marks.underlines, vec![memex_client::reading::Underline { start: BODY.rfind("重复文字").unwrap(), end: BODY.rfind("重复文字").unwrap() + "重复文字".len() }]);
            }).unwrap();
            step!("re06-dark-reading", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| s.set_appearance(Appearance::Dark, cx));
            step!("re07-editor-save-failed", |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                s.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                let editor = s.reader.read(cx).dev_editor().unwrap();
                editor.update(cx, |e, cx| e.replace_all("", window, cx));
                s.reader.update(cx, |r, cx| r.save_markdown(cx));
            });
            assert!(this.read_with(cx, |s, cx| s.reader.read(cx).blocks_navigation(cx)).unwrap());
            step!("re08-discarded", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| s.reader.update(cx, |r, cx| r.discard_markdown(cx)));
            assert!(!this.read_with(cx, |s, cx| s.reader.read(cx).blocks_navigation(cx)).unwrap());
            let long = format!("# 合成长文首端\n\n{}\n\n## 合成长文末端\n\n超过单帧限制，末端也必须完整。", (0..3000).map(|i| format!("第{i}段：中文🌹阅读资料、原始来源与版本保留；长文应该可以完整阅读、编辑和定位。\n\n")).collect::<String>());
            assert!(long.len() > 256 * 1024);
            step!("re09-long-saved", |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                s.set_appearance(Appearance::Light, cx);
                s.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                let editor = s.reader.read(cx).dev_editor().unwrap();
                editor.update(cx, |e, cx| e.replace_all(long.clone(), window, cx));
                s.reader.update(cx, |r, cx| { r.save_markdown(cx); r.toggle_editor(window, cx); });
            });
            assert_eq!(this.read_with(cx, |s, cx| s.reader.read(cx).model.detail().unwrap().canonical.markdown.len()).unwrap(), long.len());
            step!("re10-long-tail", |s: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| s.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            std::fs::write(dir.join("states.tsv"), log).unwrap();
            cx.update(|_, cx| cx.quit()).ok(); return;
        }

        // This review is fixture-only, including its partial-text states. Never
        // fall through to the agent walk's editing actions for this selector.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("reading-stats") {
            if agent {
                std::fs::write(dir.join("states.tsv"), "reading-stats requires the synthetic fixture; refused agent\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            use memex_client::reading_stats::{sample, CountMethod, StatsPrefs};
            use memex_reader::model::Mode;
            step!("rs01-complete-top", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.set_appearance(Appearance::Light, cx);
                this.set_reading_stats(StatsPrefs::default(), cx);
                this.select("item_fixture_000005".into(), false, cx);
            });
            step!("rs02-original-long-top", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| {
                    if let Some(mut d) = r.model.detail().cloned() {
                        d.canonical.markdown = (0..60).map(|i| format!("## Synthetic section {i}\n\n{}\n\n", sample())).collect();
                        r.model.load(d);
                        r.restore_mode(Mode::Original, cx);
                    }
                });
            });
            let rail = this.read_with(cx, |s, cx| s.reader.read(cx).progress_rail_bounds()).unwrap().expect("visible reading rail without AI marks");
            cx.update(|window, cx| {
                let from = point(rail.center().x, rail.top() + px(2.));
                let to = point(rail.left() - px(40.), rail.bottom() + px(12.));
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: from, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }), cx);
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: to, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }), cx);
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: to, modifiers: Modifiers::default(), click_count: 1 }), cx);
            }).unwrap();
            step!("rs02b-rail-drag-outside-to-end", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            assert_eq!(this.read_with(cx, |s, cx| s.reader.read(cx).reading_position()).unwrap(), 100, "dragging outside the rail reaches the end");
            step!("rs03-middle", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_rows(60, cx)));
            step!("rs04-end", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            step!("rs05-chars-markup-dark-narrow", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_reading_stats(StatsPrefs { method: CountMethod::Chars, markup: true, zh: 600, en: 300 }, cx);
                this.set_appearance(Appearance::Dark, cx);
                window.resize(size(px(960.), px(640.)));
            });
            step!("rs06-partial-known", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_reading_stats(StatsPrefs::default(), cx);
                this.reader.update(cx, |r, cx| {
                    if let Some(mut d) = r.model.detail().cloned() {
                        let total = d.canonical.markdown.len();
                        d.canonical.markdown = format!("# Synthetic partial text\n\n{}", sample());
                        r.model.load_partial(d, Some(total));
                        r.restore_mode(Mode::Original, cx);
                    }
                });
            });
            step!("rs07-partial-unknown", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| {
                    if let Some(d) = r.model.detail().cloned() {
                        r.model.load_partial(d, None);
                        r.restore_mode(Mode::Original, cx);
                    }
                });
            });
            step!("rs08-bilingual", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.reload(cx); r.restore_mode(Mode::Bilingual, cx); });
            });
            step!("rs09-translation", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.restore_mode(Mode::Translation, cx)));
            // The actual settings intents emit to the shell, persist only the
            // caller's isolated prefs file, and update the reader immediately.
            for (name, prefs, dark) in [
                ("rs10-settings-mixed", StatsPrefs::default(), false),
                ("rs11-settings-chars", StatsPrefs { method: CountMethod::Chars, markup: true, zh: 800, en: 150 }, false),
                ("rs12-settings-dark", StatsPrefs { method: CountMethod::Chars, markup: true, zh: 800, en: 150 }, true),
            ] {
                this.update_in(cx, |s, window, cx| {
                    s.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    s.open_settings(window, cx);
                    if let Some((_, settings)) = &s.settings {
                        settings.update(cx, |settings, cx| {
                            settings.page = memex_settings::view::Page::Appearance;
                            settings.choose_reading_stats(prefs, cx);
                        });
                    }
                }).ok();
                wait(settle).await;
                let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
                if let Some(h) = handle {
                    let saved = cx.update_window(h, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(dir.join(format!("{name}.png"))).map_err(|e| e.to_string()))
                    });
                    let applied = this.read_with(cx, |s, cx| s.prefs.reading_stats == prefs && s.reader.read(cx).model.stats_prefs() == prefs).unwrap_or(false);
                    let _ = writeln!(log, "{name}\tapplied={applied}\tsaved={:?}", saved.map_err(|e| e.to_string()).and_then(|r| r).err());
                }
            }
            step!("rs13-reader-settings-applied", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Owner Markdown edit of one book chapter against the backend's synthetic
        // paragraph fixture (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM, isolated only):
        // `MEMEX_DESKTOP_WALK=chapteredit`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("chapteredit") {
            use memex_reader::model::{Mode, Paired};
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "chapteredit requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let sha = |s: &str| memex_client::book::sha256_hex(s)[..12].to_string();
            let summary = |s: &Shell, cx: &App| {
                let r = s.reader.read(cx);
                let b = r.model.book();
                let alignment = b.map(|b| match &b.alignment { Paired::Index(a) => format!("{}:{:?}:{}", if a.is_missing() { "missing" } else { "index" }, a.status, a.pairs.len()), other => format!("{other:?}") });
                format!("rev={:?}\tat={:?}\tmode={:?}\talignment={:?}\toriginal_sha={:?}\ttranslation_sha={:?}\tcomposer={:?}\tmeta_draft={}\tnotice={:?}\teditor[{}]",
                    b.map(|b| b.doc.revision_id.clone()), b.map(|b| b.at), r.model.mode(), alignment,
                    b.and_then(|b| b.original.as_deref().map(sha)), b.and_then(|b| b.translation.as_deref().map(sha)),
                    s.chat.read(cx).composer().read(cx).value().to_string(), s.metadata.read(cx).blocks_navigation(cx), s.notice, r.dev_chapter_editor(cx))
            };
            let draft = |s: &Shell, cx: &App| s.reader.read(cx).dev_editor().map(|e| e.read(cx).value().to_string()).unwrap_or_default();
            macro_rules! note { ($tag:expr) => {{ let line = this.read_with(cx, |s, cx| summary(s, cx)).unwrap_or_default(); let _ = writeln!(log, "{}\t{}", $tag, line); }}; }
            step!("c01-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(item.clone(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            wait(1500).await;
            step!("c01-original-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("c01");
            let first = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| (b.doc.revision_id.clone(), b.translation.clone()))).ok().flatten();
            // Unrelated drafts that must survive every chapter save.
            step!("c02-editor-open-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：编辑本章期间应保留", window, cx));
                let title = this.reader.read(cx).model.detail().map(|d| format!("{}（未保存标题）", d.canonical.title)).unwrap_or_default();
                this.metadata.update(cx, |m, cx| m.edit_title(&title, cx));
                this.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
            });
            note!("c02");
            step!("c03-dirty-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                if let Some(e) = this.reader.read(cx).dev_editor() {
                    let next = e.read(cx).value().replacen("Two birds.", "Two swallows.", 1);
                    e.update(cx, |e, cx| e.set_value(next, window, cx));
                }
            });
            // A dirty chapter draft keeps its chapter and side.
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| { r.go_chapter(1, cx); r.set_mode(Mode::Translation, cx); })).ok();
            wait(1000).await;
            note!("c03-after-blocked-navigation");
            step!("c04-unknown-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_conversation_collapsed(false, cx);
                this.reader.update(cx, |r, cx| { r.dev_lose_next_edit_reply(); r.save_markdown(cx); });
            });
            note!("c04");
            // 核实保存结果: the same request id returns the committed receipt.
            step!("c05-saved-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_conversation_collapsed(true, cx);
                this.reader.update(cx, |r, cx| r.save_markdown(cx));
            });
            wait(3000).await;
            step!("c05-saved-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("c05");
            step!("c06-bilingual-missing-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); }));
            note!("c06");
            let second = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| (b.doc.revision_id.clone(), b.translation.clone()))).ok().flatten();
            let _ = writeln!(log, "c06-check\trevision_moved={}\ttranslation_unchanged={}", first.as_ref().map(|f| &f.0) != second.as_ref().map(|s| &s.0), first.as_ref().map(|f| &f.1) == second.as_ref().map(|s| &s.1));
            step!("c07-unchanged-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.toggle_editor(window, cx); r.save_markdown(cx); }));
            note!("c07");
            step!("c08-stale-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                if let Some(e) = this.reader.read(cx).dev_editor() {
                    let next = e.read(cx).value().replacen("Three trees.", "Three oaks.", 1);
                    e.update(cx, |e, cx| e.set_value(next, window, cx));
                }
            });
            // A second writer commits a translation edit over the same socket.
            let rev = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| b.doc.revision_id.clone())).ok().flatten().unwrap_or_default();
            let (sock, it) = (socket.clone(), item.clone());
            let other = executor.spawn(async move {
                let client = memex_client::Client::new(memex_client::AgentBackend::new(sock.into()));
                let doc = client.reader_toc(&it, &rev)?;
                let text = client.reader_chapter(&doc, "translation", "chapter-1")?;
                client.reader_edit_chapter(&memex_client::book::ChapterEdit {
                    item_id: it.clone(), revision_id: rev.clone(), side: "translation".into(), chapter: "chapter-1".into(),
                    expected_chapter_sha256: memex_client::book::sha256_hex(&text), request_id: "walk-second-writer-1".into(), markdown: text.replacen("清晨。", "清晨时分。", 1),
                })
            }).await;
            let _ = writeln!(log, "c08-second-writer\t{:?}", other.map(|r| (r.revision_id, r.side, r.changed)));
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.save_markdown(cx))).ok();
            step!("c08-stale-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("c08");
            let kept = this.read_with(cx, |s, cx| draft(s, cx).contains("Three oaks.")).unwrap_or(false);
            let _ = writeln!(log, "c08-check\tdraft_kept={kept}");
            step!("c09-stale-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("c10-reloaded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.reload_chapter(cx));
            });
            wait(2000).await;
            step!("c10-reloaded-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("c10");
            // The original-only appendix offers no translation-side edit.
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| { r.go_chapter(1, cx); })).ok();
            wait(2500).await;
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx))).ok();
            wait(500).await;
            note!("c11-appendix-translation");
            // 译文 of chapter 1, a multi-chunk CJK save through the UI; the book is
            // reread at the new revision and shows exactly the saved chapter.
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.go_chapter(0, cx))).ok();
            wait(2500).await;
            let long = format!("\n{}\n", "合成读者沿着晨光前行，鸟和树都在。🙂".repeat(400));
            step!("c12-translation-long-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                if let Some(e) = this.reader.read(cx).dev_editor() {
                    let next = format!("{}{long}", e.read(cx).value());
                    e.update(cx, |e, cx| e.set_value(next, window, cx));
                }
            });
            let saved = this.read_with(cx, |s, cx| draft(s, cx)).unwrap_or_default();
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.save_markdown(cx))).ok();
            wait(4000).await;
            step!("c13-translation-long-saved-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("c13");
            let back = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().and_then(|b| b.translation.clone())).ok().flatten().unwrap_or_default();
            let _ = writeln!(log, "c13-check	bytes={}	readback_equal={}", saved.len(), back == saved);
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 整理本章段落 against the backend's synthetic repair fixture over its socket
        // (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM): `MEMEX_DESKTOP_WALK=repairjoint`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("repairjoint") {
            use memex_reader::model::Mode;
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let outcome = std::env::var("MEMEX_WALK_OUTCOME").unwrap_or_default();
            step!(format!("j01-{outcome}-missing-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(item.clone(), false, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); });
            });
            wait(1500).await;
            step!(format!("j01-{outcome}-missing-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!(format!("j02-{outcome}-running-light"), |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：整理期间应保留", window, cx));
                this.reader.update(cx, |r, cx| { r.ask_repair(cx); r.start_repair(cx); });
            });
            wait(5000).await;
            step!(format!("j03-{outcome}-terminal-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let s = this.read_with(cx, |s, cx| (s.reader.read(cx).model.book().map(|b| (b.doc.revision_id.clone(), b.at)), s.chat.read(cx).composer().read(cx).value().to_string())).ok();
            let _ = writeln!(log, "j03\toutcome={outcome}\tbook,composer={s:?}");
            // 查询原任务: replays the same request id; the original job comes back.
            step!(format!("j04-{outcome}-after-query-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.start_repair(cx)));
            wait(3000).await;
            step!(format!("j04-{outcome}-after-query-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // The delete keys against the synthetic Store's per-library trash, typed
        // through the window's keymap and focus: `MEMEX_DESKTOP_WALK=deletekey`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("deletekey") {
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "deletekey requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let root = this.read_with(cx, |s, cx| s.library.read(cx).model.groups().iter()
                .find(|g| g.name == "Main").map(|g| g.id.clone())).unwrap().expect("synthetic Main");
            // Typed from outside any entity update, as the platform delivers keys.
            macro_rules! press {
                ($($k:expr),+) => {{
                    $( cx.update(|window, cx| { window.dispatch_keystroke(gpui_kit::Keystroke::parse($k).unwrap(), cx); }).ok(); wait(200).await; )+
                }};
            }
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| { let l = s.library.read(cx); (l.model.len(), l.model.selected().map(str::to_string), l.model.selected_index(), l.trash_notice.clone(), s.chat.read(cx).composer().read(cx).value().to_string()) }).ok();
                    let _ = writeln!(log, "{}\trows,selected,index,notice,composer={s:?}", $name);
                }};
            }
            let open_main = |this: &mut Shell, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group(root.clone()), cx));
                this.sidebar.update(cx, |s, cx| s.reveal(&root, cx));
            };
            step!("d01-library-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                open_main(this, cx);
            });
            let id = this.read_with(cx, |s, cx| row_id(s, 0, cx)).unwrap();
            step!("d01-library-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(id.clone(), false, cx));
            note!("d01");
            // A text field focused: Backspace edits its text, the item stays.
            step!("d02-composer-backspace-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_conversation_collapsed(false, cx));
            step!("d02-composer-backspace-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| { c.set_value("草稿abc", window, cx); c.focus(window, cx); });
            });
            press!("backspace");
            step!("d02-composer-backspace-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("d02");
            let search = this.read_with(cx, |s, cx| s.library.read(cx).search.clone()).unwrap();
            step!("d03-search-backspace-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let _ = this;
                search.update(cx, |s, cx| { s.set_value("Syn", window, cx); s.focus(window, cx); });
            });
            press!("backspace");
            step!("d03-search-backspace-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let typed = this.read_with(cx, |_, cx| search.read(cx).value().to_string()).unwrap_or_default();
            note!("d03");
            let _ = writeln!(log, "d03-search\tvalue={typed:?}");
            // The item list focused (as a click on a row does): Backspace trashes the selection.
            step!("d04-list-backspace-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                search.update(cx, |s, cx| s.set_value("", window, cx));
                this.library.update(cx, |l, cx| { l.model.set_search(""); l.reload(cx); });
            });
            note!("d04-before");
            step!("d04-list-backspace-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let focus = this.library.read(cx).items_focus.clone();
                window.focus(&focus, cx);
            });
            press!("backspace");
            step!("d04-list-backspace-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("d04");
            step!("d05-trash-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.library.update(cx, |l, cx| l.show_group_trash(root.clone(), cx)));
            let _ = writeln!(log, "d05-trash\tentries={:?}", this.read_with(cx, |s, cx| s.library.read(cx).trash.entries.iter().map(|e| e.item_id.clone()).collect::<Vec<_>>()).ok());
            // Classic table focused: forward Delete does the same.
            step!("d06-restored-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.restore_from_trash(&id, cx));
                this.set_appearance(Appearance::Dark, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            step!("d06-restored-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { open_main(this, cx); });
            step!("d06-restored-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(id.clone(), false, cx));
            note!("d06");
            step!("d07-table-delete-dark", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let focus = this.table.read(cx).table.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            });
            press!("delete");
            step!("d07-table-delete-dark", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("d07");
            step!("d08-restored-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.show_group_trash(root.clone(), cx));
            });
            step!("d08-restored-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.library.update(cx, |l, cx| l.restore_from_trash(&id, cx)));
            step!("d08-restored-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| open_main(this, cx));
            note!("d08");
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 对比修改 of two revisions read whole through `revision_chunk`, against the
        // backend's synthetic paragraph fixture over its socket (MEMEX_AGENT_SOCKET
        // + MEMEX_WALK_ITEM, isolated only): `MEMEX_DESKTOP_WALK=revisiondiff`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("revisiondiff") {
            use memex_reader::compare::CompareSpec;
            use memex_reader::model::Mode;
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "revisiondiff requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let lang_start = this.read_with(cx, |s, _| s.prefs.language).unwrap_or_default();
            macro_rules! note {
                ($tag:expr) => {{
                    let line = this.read_with(cx, |s, cx| {
                        let r = s.reader.read(cx);
                        format!("rev={:?}\toffer={:?}\tcompare={:?}\teditor[{}]", r.model.book().map(|b| b.doc.revision_id.clone()).or_else(|| r.model.detail().map(|d| d.canonical.revision_id.clone())),
                            r.dev_compare_offer().map(|o| (o.target, o.old_revision, o.new_revision)), r.dev_compare().map(|c| c.read(cx).summary()), r.dev_chapter_editor(cx))
                    }).unwrap_or_default();
                    let _ = writeln!(log, "{}\t{}", $tag, line);
                }};
            }
            // Waits until the comparison is no longer loading (bounded).
            macro_rules! settle_compare {
                () => {{
                    for _ in 0..60 {
                        let busy = this.read_with(cx, |s, cx| s.reader.read(cx).dev_compare().is_some_and(|c| c.read(cx).summary().starts_with("loading"))).unwrap_or(false);
                        if !busy { break; }
                        wait(250).await;
                    }
                }};
            }
            let edit = |s: &mut Shell, window: &mut Window, cx: &mut Context<Shell>, f: &dyn Fn(&str) -> String| {
                if let Some(e) = s.reader.read(cx).dev_editor() {
                    let next = f(&e.read(cx).value());
                    e.update(cx, |e, cx| e.set_value(next, window, cx));
                }
            };
            step!("r01-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(item.clone(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            wait(1500).await;
            note!("r01");
            // Original chapter 1: one line rewritten in place, one deleted, one added.
            step!("r02-edited-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                edit(this, window, cx, &|t| t.replacen("One morning.", "One quiet morning.", 1).replacen("Two birds.", "Two swallows sat on the fence.", 1)
                    .replacen("Three trees.\n\n", "", 1).replacen("Unknown.\n", "Unknown.\n\nA new closing line.\n", 1));
            });
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.save_markdown(cx))).ok();
            wait(4000).await;
            step!("r03-saved-offer-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("r03");
            step!("r04-compare-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.view_last_change(cx)));
            settle_compare!();
            step!("r04-compare-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("r04");
            step!("r05-compare-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("r06-compare-en-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_language(LanguagePref::En, cx);
            });
            this.update(cx, |s, cx| { s.set_language(lang_start, cx); s.reader.update(cx, |r, cx| r.close_revision_compare(cx)); }).ok();
            // Translation chapter 1: a long CJK chapter (several 8 KiB chunks), then a
            // second edit inside it, so long unchanged runs fold between the changes.
            let long: String = (1..=320).map(|n| format!("第{n}段：合成读者沿着晨光前行，鸟和树都在，港湾很安静。\n\n")).collect();
            step!("r07-translation-long-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx));
                this.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                edit(this, window, cx, &|t| format!("{t}{long}"));
            });
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.save_markdown(cx))).ok();
            wait(5000).await;
            note!("r07");
            step!("r08-translation-second-edit-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.toggle_editor(window, cx));
                edit(this, window, cx, &|t| t.replacen("第40段：合成读者沿着晨光前行，鸟和树都在，港湾很安静。", "第40段：合成读者沿着暮色前行，鸟和树都在，港湾依旧很安静。", 1)
                    .replacen("第150段：合成读者沿着晨光前行，鸟和树都在，港湾很安静。\n\n", "", 1)
                    .replacen("第260段：合成读者沿着晨光前行，鸟和树都在，港湾很安静。\n\n", "第260段：合成读者沿着晨光前行，鸟和树都在，港湾很安静。\n\n新增段落：守灯人把天气记进日志。\n\n", 1));
            });
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.save_markdown(cx))).ok();
            wait(5000).await;
            note!("r08");
            step!("r09-cjk-compare-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.view_last_change(cx)));
            settle_compare!();
            step!("r09-cjk-compare-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("r09");
            step!("r10-cjk-compare-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("r11-cjk-expanded-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(c) = this.reader.read(cx).dev_compare() { c.update(cx, |c, cx| c.expand(1, cx)); }
            });
            note!("r11");
            // A whole-chain comparison: the base of the long edit against the latest.
            let book_revs = this.read_with(cx, |s, cx| s.reader.read(cx).dev_compare_offer()).ok().flatten();
            // The first long save's base is the revision before r07; find it in the log line.
            let first_base = log.lines().find(|l| l.starts_with("r03")).and_then(|l| l.split("rev=Some(\"").nth(1)).and_then(|s| s.split('"').next()).map(str::to_string);
            if let (Some(spec), Some(base)) = (book_revs.clone(), first_base) {
                step!("r12-cjk-base-to-latest-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_appearance(Appearance::Light, cx);
                    this.reader.update(cx, |r, cx| r.open_revision_compare(CompareSpec { old_revision: base.clone(), ..spec.clone() }, cx));
                });
                settle_compare!();
                step!("r12-cjk-base-to-latest-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("r12");
            }
            // A revision that is not on this item's chain: reported, nothing compared.
            if let Some(spec) = book_revs.clone() {
                let mut bogus = spec.old_revision.clone();
                let last = bogus.pop().map(|c| if c == '0' { '1' } else { '0' }).unwrap_or('0');
                bogus.push(last);
                step!("r13-unreadable-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_appearance(Appearance::Light, cx);
                    this.reader.update(cx, |r, cx| r.open_revision_compare(CompareSpec { old_revision: bogus.clone(), ..spec.clone() }, cx));
                });
                settle_compare!();
                step!("r13-unreadable-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("r13");
                step!("r14-unreadable-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            }
            // An ordinary item's body: an owner Markdown edit over the same socket,
            // then its old and new body side by side.
            let body_item = this.read_with(cx, |s, cx| (0..s.library.read(cx).model.len()).map(|i| row_id(s, i, cx)).find(|id| !id.is_empty() && *id != item)).ok().flatten();
            let client = this.read_with(cx, |s, _| s.client.clone()).unwrap();
            if let Some(body_item) = body_item {
                let (c, id) = (client.clone(), body_item.clone());
                let saved = cx.background_executor().spawn(async move {
                    let detail = c.item_detail(&id)?;
                    let old = detail.canonical.markdown.clone();
                    let mut lines: Vec<String> = old.lines().map(str::to_string).collect();
                    let texts: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].trim().chars().count() > 8).collect();
                    if let Some(&i) = texts.first() { lines[i] = format!("{}（已修订）", lines[i].replacen(' ', "  ", 1)); }
                    if let Some(&i) = texts.get(1) { lines[i].clear(); }
                    lines.push(String::new());
                    lines.push("合成新增：这一段在本次编辑中加入。".into());
                    let markdown = lines.join("\n") + "\n";
                    let update = c.edit_markdown(memex_client::reading::MarkdownEdit { item_id: id.clone(), expected_revision_id: detail.canonical.revision_id.clone(), request_id: memex_client::biblio::new_request_id(), markdown })?;
                    Ok::<_, memex_client::ClientError>((detail.canonical.revision_id, update.revision_id, detail.canonical.title))
                }).await;
                let _ = writeln!(log, "body-edit\t{body_item}\t{saved:?}");
                if let Ok((old, new, title)) = saved {
                    step!("r15-body-compare-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        this.set_appearance(Appearance::Light, cx);
                        this.select(body_item.clone(), false, cx);
                    });
                    wait(1500).await;
                    step!("r15-body-compare-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        let spec = CompareSpec { item_id: body_item.clone(), target: memex_client::book::RevisionTarget::Body {}, old_revision: old.clone(), new_revision: new.clone(), title: title.clone() };
                        this.reader.update(cx, |r, cx| r.open_revision_compare(spec, cx));
                    });
                    settle_compare!();
                    step!("r15-body-compare-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    note!("r15");
                    step!("r16-body-compare-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                    // Opening the book again closes the body comparison (it belongs to its item).
                    step!("r17-back-to-book-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        this.set_appearance(Appearance::Light, cx);
                        this.select(item.clone(), false, cx);
                    });
                    note!("r17");
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // A title edit (metadata-only revision) keeps paragraph pairing and a
        // completed AI chapter translation, against the backend's synthetic
        // fixture over its socket (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM,
        // MEMEX_WALK_CASE=alignment|translation): `MEMEX_DESKTOP_WALK=renameinherit`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("renameinherit") {
            use memex_client::book::ReaderRequest;
            use memex_reader::model::{Mode, Paired};
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "renameinherit requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let case = std::env::var("MEMEX_WALK_CASE").unwrap_or_default();
            let summary = |s: &Shell, cx: &App| {
                let r = s.reader.read(cx);
                let b = r.model.book();
                let alignment = b.map(|b| match &b.alignment { Paired::Index(a) => format!("{}:{:?}:{}:{}", if a.is_missing() { "missing" } else { "index" }, a.status, a.pairs.len(), &a.version[..a.version.len().min(12)]), other => format!("{other:?}") });
                format!("rev={:?}\tat={:?}\tmode={:?}\ttitle={:?}\talignment={:?}\tnotice={:?}\tcomposer={:?}\ttr[{}]\teditor[{}]",
                    b.map(|b| b.doc.revision_id.clone()), b.map(|b| b.at), r.model.mode(), r.model.detail().map(|d| d.canonical.title.clone()), alignment, r.model.notice(),
                    s.chat.read(cx).composer().read(cx).value().to_string(), r.dev_chapter_translation(), r.dev_chapter_editor(cx))
            };
            macro_rules! note { ($tag:expr) => {{ let line = this.read_with(cx, |s, cx| summary(s, cx)).unwrap_or_default(); let _ = writeln!(log, "{}\t{}", $tag, line); }}; }
            let rev = |s: &Shell, cx: &App| s.reader.read(cx).model.book().map(|b| b.doc.revision_id.clone()).unwrap_or_default();
            let raw = |command: memex_client::Command, sock: String| {
                use memex_client::Backend as _;
                memex_client::AgentBackend::new(sock.into()).send(&command).map(|r| r.reader.is_some())
            };
            macro_rules! rename {
                ($name:expr, $title:expr) => {{
                    step!($name, |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        let t: String = $title.to_string();
                        this.metadata.update(cx, |m, cx| { m.edit_title(&t, cx); m.save(cx); });
                    });
                    wait(5000).await;
                    step!($name, |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    note!($name);
                }};
            }
            step!("n01-open-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：改标题期间应保留", window, cx));
                this.select(item.clone(), false, cx);
            });
            wait(2000).await;
            if case == "alignment" {
                step!("n01-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); }));
                note!("n01");
                let (r0, v0) = this.read_with(cx, |s, cx| (rev(s, cx), s.reader.read(cx).model.book().and_then(|b| match &b.alignment { Paired::Index(a) => Some(a.version.clone()), _ => None }).unwrap_or_default())).unwrap();
                rename!("n02-renamed-once-light", "Synthetic paragraph book（改名一次）");
                rename!("n03-renamed-twice-light", "Synthetic paragraph book（改名两次）");
                step!("n04-renamed-twice-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                step!("n05-renamed-parallel-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.set_book_parallel(true, cx)); });
                // The original revision's alignment version, read at the current revision: rejected.
                let r2 = this.read_with(cx, |s, cx| rev(s, cx)).unwrap();
                let (sock, it) = (socket.clone(), item.clone());
                let (r2c, v0c) = (r2.clone(), v0.clone());
                let old_page = executor.spawn(async move {
                    let at = |revision: String, version: String| memex_client::Command::Reader { max_sensitivity: memex_client::Sensitivity::Restricted,
                        request: ReaderRequest::Alignment { item_id: it.clone(), revision_id: revision, chapter: "chapter-1".into(), offset: 0, limit: 128, expected_version: version } };
                    (raw(at(r2c.clone(), v0c.clone()), sock.clone()), raw(at(r0.clone(), "any".into()), sock.clone()), raw(at(r2c, "any".into()), sock))
                }).await;
                let _ = writeln!(log, "n05-old-version\t(new_rev+old_version, old_rev, new_rev+any)={old_page:?}");
                // A dirty chapter draft across a rename: the draft and composer stay.
                step!("n06-draft-dirty-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                    this.reader.update(cx, |r, cx| { r.set_book_parallel(false, cx); r.set_mode(Mode::Original, cx); r.toggle_editor(window, cx); });
                    if let Some(e) = this.reader.read(cx).dev_editor() { let next = e.read(cx).value().replacen("Two birds.", "Two herons.", 1); e.update(cx, |e, cx| e.set_value(next, window, cx)); }
                });
                rename!("n07-renamed-with-draft-light", "Synthetic paragraph book（改名三次）");
                let kept = this.read_with(cx, |s, cx| s.reader.read(cx).dev_editor().is_some_and(|e| e.read(cx).value().contains("Two herons."))).unwrap_or(false);
                let _ = writeln!(log, "n07-check\tdraft_kept={kept}");
                step!("n08-draft-save-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.save_markdown(cx)));
                wait(3000).await;
                step!("n08-draft-save-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("n08");
                let kept = this.read_with(cx, |s, cx| s.reader.read(cx).dev_editor().is_some_and(|e| e.read(cx).value().contains("Two herons."))).unwrap_or(false);
                let _ = writeln!(log, "n08-check\tdraft_kept={kept}");
            } else {
                // The English appendix: translate it once (the only dispatch).
                step!("n01-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.go_chapter(1, cx); }));
                wait(1500).await;
                this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.ask_chapter_translation(cx))).ok();
                wait(2500).await;
                this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.confirm_chapter_translation(cx))).ok();
                wait(7000).await;
                step!("n01-translated-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx)));
                note!("n01");
                let previews = |s: &Shell, cx: &App| s.reader.read(cx).dev_chapter_translation_previews();
                let last = this.read_with(cx, |s, cx| s.reader.read(cx).dev_chapter_translation_last()).ok().flatten();
                let client = this.read_with(cx, |s, _| s.client.clone()).unwrap();
                let dispatched = |c: memex_client::Client, it: String, last: Option<(String, String, String)>| async move {
                    match last { Some((_, _, job)) => c.reader_translate_status(&it, &job).ok().and_then(|j| j.usage).map(|u| u.requests_dispatched), None => None }
                };
                let d0 = dispatched(client.clone(), item.clone(), last.clone()).await;
                let p0 = this.read_with(cx, |s, cx| previews(s, cx)).unwrap_or(0);
                // The result's metadata at the first revision (for the stale checks below).
                let (r0, c) = (this.read_with(cx, |s, cx| rev(s, cx)).unwrap(), client.clone());
                let it = item.clone();
                let meta0 = executor.spawn(async move { let doc = c.reader_toc(&it, &r0)?; c.reader_translate_preview(&doc, "appendix").map(|p| p.translation) }).await;
                let _ = writeln!(log, "n01-meta\t{:?}", meta0.as_ref().map(|m| m.as_ref().map(|m| (m.revision_id.clone(), m.version[..12].to_string(), m.bytes))));
                rename!("n02-renamed-once-light", "Synthetic translation book（改名一次）");
                rename!("n03-renamed-twice-light", "Synthetic translation book（改名两次）");
                step!("n04-renamed-twice-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                step!("n05-renamed-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx)); });
                note!("n05");
                // 翻译本章 again: the existing result, no new dispatch.
                step!("n06-ask-again-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.ask_chapter_translation(cx); }));
                wait(2000).await;
                step!("n06-ask-again-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("n06");
                let p1 = this.read_with(cx, |s, cx| previews(s, cx)).unwrap_or(0);
                let d1 = dispatched(client.clone(), item.clone(), last.clone()).await;
                let _ = writeln!(log, "n06-check\tpreviews_before_rename={p0}\tpreviews_after={p1}\trequests_dispatched_before={d0:?}\tafter={d1:?}");
                // Old chunk versions: the first revision's meta, and its version at the current revision.
                if let Ok(Some(m0)) = meta0 {
                    let r2 = this.read_with(cx, |s, cx| rev(s, cx)).unwrap();
                    let c = client.clone();
                    let res = executor.spawn(async move {
                        let at_new = memex_client::book::TranslationMeta { revision_id: r2.clone(), ..m0.clone() };
                        (c.reader_translation_text(&m0).map(|t| t.len()), c.reader_translation_text(&at_new).map(|t| t.len()))
                    }).await;
                    let _ = writeln!(log, "n07-old-chunks\t(old_rev_meta, new_rev_old_version)={res:?}");
                }
                let d2 = dispatched(client.clone(), item.clone(), last.clone()).await;
                let _ = writeln!(log, "n07-check\trequests_dispatched={d2:?}");
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 「Mac 正忙，稍后重试」 against the synthetic fixture behind a proxy that answers
        // library_loading for library_query and chapter chunks until `<snapshot dir>/free`
        // exists (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM, isolated only): `MEMEX_DESKTOP_WALK=busy`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("busy") {
            use memex_reader::model::Mode;
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "busy requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let draft = "草稿：Mac 忙时应保留";
            this.update_in(cx, |this, window, cx| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value(draft, window, cx));
                this.select(item.clone(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            }).ok();
            // Every busy read is retried 6 times (~16.5 s) before the busy line shows.
            wait(26000).await;
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| {
                        let l = s.library.read(cx);
                        (l.load_busy, l.status_line(), s.reader.read(cx).dev_busy(), s.chat.read(cx).composer().read(cx).value().to_string())
                    }).ok();
                    let _ = writeln!(log, "{}\t{s:?}", $name);
                }};
            }
            step!("m01-busy-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m01");
            step!("m02-busy-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            std::fs::write(dir.join("free"), "").ok();
            step!("m03-retry-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.reload(cx));
                this.reader.update(cx, |r, cx| r.retry_busy(cx));
            });
            wait(4000).await;
            step!("m03-retry-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m03");
            // Previews that meet library_loading are sent once: the busy line and 重试 (the user's).
            let tr = |s: &Shell, cx: &App| format!("tr[{}]\tfmt[{}]", s.reader.read(cx).dev_chapter_translation(), s.reader.read(cx).dev_format());
            macro_rules! note_tr { ($name:expr) => {{ let line = this.read_with(cx, |s, cx| tr(s, cx)).unwrap_or_default(); let _ = writeln!(log, "{}\t{}", $name, line); }}; }
            step!("m04-preview-busy-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.go_chapter(1, cx); }));
            wait(2000).await;
            std::fs::write(dir.join("busy-previews"), "").ok();
            this.update(cx, |s, cx| s.reader.update(cx, |r, cx| { r.ask_chapter_translation(cx); r.ask_format(cx); })).ok();
            wait(4000).await;
            step!("m04-preview-busy-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note_tr!("m04");
            // Nothing resends meanwhile.
            wait(6000).await;
            step!("m04-preview-busy-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            note_tr!("m04-later");
            std::fs::remove_file(dir.join("busy-previews")).ok();
            step!("m05-preview-retried-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.retry_busy_chapter_translation(cx); r.ask_format(cx); });
            });
            wait(4000).await;
            step!("m05-preview-retried-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note_tr!("m05");
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 格式整理 / 批量格式整理 against the backend's synthetic format fixture over
        // its socket (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM, MEMEX_WALK_CASE=
        // success|failed|unknown|batch; batch: MEMEX_WALK_GROUP, MEMEX_WALK_TRASH;
        // success: MEMEX_WALK_BOOK). The harness answers `<dir>/need-*` files
        // (restart the daemon, trash / untrash B) with `<dir>/done-*`:
        // `MEMEX_DESKTOP_WALK=formatcleanup`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("formatcleanup") {
            use memex_reader::model::Mode;
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "formatcleanup requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let var = |k: &str| std::env::var(k).unwrap_or_default();
            let (item, case, book, group, trash) = (var("MEMEX_WALK_ITEM"), var("MEMEX_WALK_CASE"), var("MEMEX_WALK_BOOK"), var("MEMEX_WALK_GROUP"), var("MEMEX_WALK_TRASH"));
            let summary = |s: &Shell, cx: &App| {
                let r = s.reader.read(cx);
                let rev = r.model.book().map(|b| b.doc.revision_id.clone()).or_else(|| r.model.detail().map(|d| d.canonical.revision_id.clone()));
                format!("rev={rev:?}\tmode={:?}\tcomposer={:?}\tcompare={}\tfmt[{}]", r.model.mode(), s.chat.read(cx).composer().read(cx).value().to_string(),
                    r.dev_compare().map(|c| c.read(cx).summary()).unwrap_or_default(), r.dev_format())
            };
            macro_rules! note { ($tag:expr) => {{ let line = this.read_with(cx, |s, cx| summary(s, cx)).unwrap_or_default(); let _ = writeln!(log, "{}\t{}", $tag, line); }}; }
            // Waits for the harness to act on `need-<what>`.
            macro_rules! harness {
                ($what:expr) => {{ harness!($what, String::new()) }};
                ($what:expr, $payload:expr) => {{
                    let done = dir.join(format!("done-{}", $what));
                    std::fs::write(dir.join(format!("need-{}", $what)), $payload).ok();
                    let mut ok = false;
                    for _ in 0..240 { if done.exists() { ok = true; break; } wait(250).await; }
                    let _ = writeln!(log, "harness-{}\tdone={ok}", $what);
                }};
            }
            // Waits (bounded) until the flow is no longer previewing / following an unsettled batch.
            macro_rules! settle_flow {
                ($group:expr) => {{
                    for _ in 0..60 {
                        let busy = this.read_with(cx, |s, cx| { let d = s.reader.read(cx).dev_format(); let part = if $group { d.split(" batch[").nth(1).unwrap_or("").to_string() } else { d.split(" single[").nth(1).unwrap_or("").split(" batch[").next().unwrap_or("").to_string() };
                            part.contains("state=Previewing") || (part.contains("state=Following") && part.contains("rows=None")) }).unwrap_or(false);
                        if !busy { break; }
                        wait(500).await;
                    }
                }};
            }
            let client = this.read_with(cx, |s, _| s.client.clone()).unwrap();
            let status = |c: memex_client::Client, batch: Option<String>| async move { batch.map(|b| c.format_status(&b).map(|b| (b.state, b.usage.requests_dispatched))) };
            let pending = |s: &Shell, cx: &App, group: bool| s.reader.read(cx).dev_format_pending(group);
            step!("f01-open-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：格式整理期间应保留", window, cx));
                this.select(item.clone(), false, cx);
            });
            wait(2000).await;
            note!("f01");
            if case == "batch" {
                let name = this.read_with(cx, |s, cx| s.library.read(cx).model.group(&group).map(|g| g.name.clone()).unwrap_or_default()).unwrap_or_default();
                step!("b01-folder-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.library.update(cx, |l, cx| l.set_filter(Filter::Group(group.clone()), cx));
                    let tree = this.sidebar.read(cx).tree();
                    tree.update(cx, |t, cx| { t.reveal(&group, cx); t.open_menu(&group, point(px(150.), px(330.)), cx); });
                });
                step!("b02-confirm-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    let tree = this.sidebar.read(cx).tree();
                    tree.update(cx, |t, cx| t.close_menu(cx));
                    let event = LibraryEvent::FormatBatch { group_id: group.clone(), name: name.clone() };
                    this.library.update(cx, |_, cx| cx.emit(event));
                });
                settle_flow!(true);
                step!("b02-confirm-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("b02");
                step!("b02-confirm-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                // 含子文件夹 → a new preview of the subtree selection.
                step!("b03-descendants-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.set_format_descendants(true, cx)); });
                settle_flow!(true);
                step!("b03-descendants-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_format_consent(true, true, cx)));
                note!("b03");
                step!("b04-running-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.confirm_format(true, cx)));
                note!("b04");
                for _ in 0..40 { if this.read_with(cx, |s, cx| s.reader.read(cx).dev_format().contains(":done:")).unwrap_or(false) { break; } wait(500).await; }
                wait(2500).await;
                step!("b05-results-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("b05");
                step!("b05-results-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                let handles = this.read_with(cx, |s, cx| s.reader.read(cx).dev_format_handles(true)).unwrap_or_default();
                let _ = writeln!(log, "b05-handles\t{:?}", handles.iter().map(|h| (h.item_id.clone(), h.changed)).collect::<Vec<_>>());
                let a = handles.iter().find(|h| h.item_id == item).cloned();
                let b = handles.iter().find(|h| h.item_id == trash).cloned();
                if let Some(a) = a.clone() {
                    step!("b06-review-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.review_format(a.clone(), cx)); });
                    wait(3000).await;
                    step!("b06-review-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    note!("b06");
                    step!("b06-review-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                }
                // The same folder again: already formatted, no budget.
                step!("b07-again-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| { r.close_revision_compare(cx); r.restart_format_batch(cx); }); });
                settle_flow!(true);
                step!("b07-again-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("b07");
                let batch_id = this.read_with(cx, |s, cx| pending(s, cx, true)).ok().flatten().and_then(|p| p.2);
                let before = status(client.clone(), batch_id.clone()).await;
                let _ = writeln!(log, "b07-status\t{before:?}");
                // A/B: B globally trashed (not removed from the folder).
                harness!("trash", b.as_ref().map(|h| h.result_revision_id.clone()).unwrap_or_default());
                step!("b08-denied-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    // As the sidebar does (titles from the library listing).
                    this.reader.update(cx, |r, cx| r.close_format_batch(cx));
                    let event = LibraryEvent::FormatBatch { group_id: group.clone(), name: name.clone() };
                    this.library.update(cx, |_, cx| cx.emit(event));
                });
                wait(3000).await;
                step!("b08-denied-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("b08");
                step!("b08-denied-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                // Direct reads: whole-batch reads refused (no partial data); A's own proof works, B's not.
                let c = client.clone();
                let (bid, ha, hb) = (batch_id.clone().unwrap_or_default(), a.clone(), b.clone());
                let direct = executor.spawn(async move {
                    (c.format_status(&bid).map(|b| b.counts).map_err(|e| format!("{e:?}")),
                     c.format_results(&bid, 0, 20).map(|p| p.total).map_err(|e| format!("{e:?}")),
                     c.format_resume(&bid).map(|b| b.usage.requests_dispatched).map_err(|e| format!("{e:?}")),
                     ha.map(|h| c.format_changes(&h, 0).map(|p| p.total).map_err(|e| format!("{e:?}"))),
                     hb.map(|h| c.format_changes(&h, 0).map(|p| p.total).map_err(|e| format!("{e:?}"))))
                }).await;
                let _ = writeln!(log, "b08-direct\t(status, results, resume, A changes, B changes)={direct:?}");
                if let Some(a) = a.clone() {
                    step!("b09-restore-a-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.restore_format(a.clone(), cx)); });
                    wait(3000).await;
                    step!("b09-restore-a-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    note!("b09");
                    let last = this.read_with(cx, |s, cx| s.reader.read(cx).dev_format_last_restore()).ok().flatten();
                    if let Some((h, id)) = last {
                        let c = client.clone();
                        let replay = executor.spawn(async move { c.format_restore(&h, &id) }).await;
                        let _ = writeln!(log, "b09-replay\t{replay:?}");
                    }
                }
                // Untrash B (explicit owner action), then the batch's usage: unchanged by the refused resume.
                harness!("untrash");
                let after = status(client.clone(), batch_id.clone()).await;
                let _ = writeln!(log, "b10-status-after-untrash\tbefore={before:?}\tafter={after:?}");
                note!("b10");
            } else {
                step!("f02-confirm-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.ask_format(cx)));
                settle_flow!(false);
                step!("f02-confirm-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("f02");
                step!("f02-confirm-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                step!("f03-sent-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.confirm_format(false, cx)); });
                note!("f03-sent");
                settle_flow!(false);
                wait(3000).await;
                step!(format!("f03-{case}-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("f03");
                step!(format!("f03-{case}-dark"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                let p0 = this.read_with(cx, |s, cx| pending(s, cx, false)).ok().flatten();
                let d0 = status(client.clone(), p0.as_ref().and_then(|p| p.2.clone())).await;
                let _ = writeln!(log, "f03-check\tpending={p0:?}\tstatus={d0:?}");
                match case.as_str() {
                    "failed" => {
                        step!("f04-retry-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.ask_format(cx)); });
                        settle_flow!(false);
                        step!("f04-retry-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                        note!("f04");
                        let d1 = status(client.clone(), p0.as_ref().and_then(|p| p.2.clone())).await;
                        let _ = writeln!(log, "f04-check\tstatus_after_retry_preview={d1:?}");
                    }
                    "unknown" => {
                        step!("f04-query-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.query_format(false, cx)); });
                        settle_flow!(false);
                        wait(2000).await;
                        step!("f04-query-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                        note!("f04");
                        let p1 = this.read_with(cx, |s, cx| pending(s, cx, false)).ok().flatten();
                        let d1 = status(client.clone(), p1.as_ref().and_then(|p| p.2.clone())).await;
                        let _ = writeln!(log, "f04-check\tsame_request={}\tstatus={d1:?}", p0.as_ref().map(|p| &p.0) == p1.as_ref().map(|p| &p.0));
                    }
                    _ => {
                        let h = this.read_with(cx, |s, cx| s.reader.read(cx).dev_format_handles(false)).unwrap_or_default().into_iter().next();
                        if let Some(h) = h {
                            step!("f04-compare-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.review_format(h.clone(), cx)); });
                            wait(3000).await;
                            step!("f04-compare-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                            note!("f04");
                            step!("f04-compare-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                            step!("f05-restored-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| { r.close_revision_compare(cx); r.restore_format(h.clone(), cx); }); });
                            wait(4000).await;
                            step!("f05-restored-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                            note!("f05");
                            let last = this.read_with(cx, |s, cx| s.reader.read(cx).dev_format_last_restore()).ok().flatten();
                            if let Some((h, id)) = last {
                                let c = client.clone();
                                let replay = executor.spawn(async move { (c.format_restore(&h, &id), c.format_restore(&h, &id)) }).await;
                                let _ = writeln!(log, "f05-replay\t{replay:?}");
                            }
                        }
                        // Daemon restart, then 查询原任务 replays the same request id: no new dispatch.
                        harness!("restart");
                        step!("f06-replayed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.query_format(false, cx)));
                        settle_flow!(false);
                        wait(2000).await;
                        step!("f06-replayed-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                        note!("f06");
                        let p1 = this.read_with(cx, |s, cx| pending(s, cx, false)).ok().flatten();
                        let d1 = status(client.clone(), p1.as_ref().and_then(|p| p.2.clone())).await;
                        let _ = writeln!(log, "f06-check\tsame_request={}\tbefore={d0:?}\tafter={d1:?}", p0.as_ref().map(|p| &p.0) == p1.as_ref().map(|p| &p.0));
                        if !book.is_empty() {
                            // A book: only one side's chapter; in 对照 the button is off with the reason.
                            step!("f07-book-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(book.clone(), false, cx));
                            wait(3000).await;
                            step!("f07-book-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.ask_format(cx); }));
                            note!("f07");
                            step!("f08-book-chapter-confirm-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.ask_format(cx); }));
                            settle_flow!(false);
                            step!("f08-book-chapter-confirm-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                            note!("f08");
                            step!("f08-book-chapter-confirm-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                        }
                    }
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // AI chapter translation found on open (`translate_metadata`) and its recorded
        // block correspondence (`translate_alignment`) against the backend's synthetic
        // fixture (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM, MEMEX_WALK_CASE=
        // legacy|mapped|plain, isolated only): `MEMEX_DESKTOP_WALK=aitranslation`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("aitranslation") {
            use memex_reader::model::Mode;
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "aitranslation requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let case = std::env::var("MEMEX_WALK_CASE").unwrap_or_default();
            let summary = |s: &Shell, cx: &App| {
                let r = s.reader.read(cx);
                let b = r.model.book();
                format!("at={:?}\tmode={:?}\tparallel={}\ttops={:?}\trows={:?}\tcomposer={:?}\ttr[{}]", b.map(|b| b.at), r.model.mode(), r.model.parallel(), r.column_tops(),
                    r.dev_ai_rows(), s.chat.read(cx).composer().read(cx).value().to_string(), r.dev_chapter_translation())
            };
            macro_rules! note { ($tag:expr) => {{ let line = this.read_with(cx, |s, cx| summary(s, cx)).unwrap_or_default(); let _ = writeln!(log, "{}\t{}", $tag, line); }}; }
            step!("a01-open-original-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：读取 AI 译文期间应保留", window, cx));
                this.select(item.clone(), false, cx);
            });
            wait(2000).await;
            step!("a01-open-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.go_chapter(1, cx); }));
            wait(3000).await;
            step!("a01-open-original-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("a01");
            if case == "plain" {
                // Nothing published yet: 翻译本章; after it, a reopen finds the result by discovery.
                this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.ask_chapter_translation(cx))).ok();
                wait(2500).await;
                this.update(cx, |s, cx| s.reader.update(cx, |r, cx| r.confirm_chapter_translation(cx))).ok();
                wait(8000).await;
                note!("a02-translated");
                step!("a03-reopened-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(0, cx)));
                wait(2000).await;
                step!("a03-reopened-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.go_chapter(1, cx); r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); }));
                wait(4000).await;
                step!("a03-reopened-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("a03");
                step!("a03-reopened-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            } else {
                step!("a02-pairs-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); }));
                wait(1500).await;
                step!("a02-pairs-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("a02");
                step!("a02-pairs-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                step!("a03-parallel-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| r.set_book_parallel(true, cx)); });
                note!("a03");
                // Scroll the original to the last matched row's start: the AI column follows to that row only.
                let target = this.read_with(cx, |s, cx| s.reader.read(cx).dev_ai_rows().last().map(|r| (r.0.start, r.1.start))).ok().flatten();
                if let Some((o, t)) = target {
                    step!("a04-parallel-synced-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_column(false, o, cx)));
                    step!("a04-parallel-synced-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    let tops = this.read_with(cx, |s, cx| s.reader.read(cx).column_tops()).unwrap_or_default();
                    let _ = writeln!(log, "a04-sync\texpected=({o},{t})\ttops={tops:?}");
                    step!("a04-parallel-synced-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                } else {
                    step!("a04-parallel-unsynced-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
                }
                // One row per page: the same correspondence (paging works).
                let before = this.read_with(cx, |s, cx| s.reader.read(cx).dev_ai_rows()).unwrap_or_default();
                step!("a05-paged-pairs-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); this.reader.update(cx, |r, cx| { r.set_book_parallel(false, cx); r.dev_set_ai_alignment_page(1, cx); }); });
                wait(3000).await;
                step!("a05-paged-pairs-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                note!("a05");
                let after = this.read_with(cx, |s, cx| s.reader.read(cx).dev_ai_rows()).unwrap_or_default();
                let _ = writeln!(log, "a05-check\tsame_rows_with_limit_1={}", before == after);
                step!("a06-translation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx)));
                note!("a06");
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Multi-selection (fixture only): Shift and Option clicks in a folder's
        // reading list and classic table, the menu for the selection, the folder
        // lists of 移动到文件夹… and 映射到其他库…, and a move whose second step fails:
        // `MEMEX_DESKTOP_WALK=multiselect`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("multiselect") {
            assert!(!agent, "multiselect refuses a real agent");
            let folder = "grp_c_sleep".to_string();
            let shift = Modifiers { shift: true, ..Default::default() };
            let option = Modifiers { alt: true, ..Default::default() };
            let state = |this: &Shell, cx: &App| {
                let l = this.library.read(cx);
                format!("open={:?}\tselection={:?}\tpicker={:?}\tnotice={:?}\tfilter={:?}", l.model.selected(), l.model.selection(), l.picker.as_ref().map(|p| p.choosing), l.member_notice, l.model.filter())
            };
            step!("m00-folder-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group(folder.clone()), cx));
                this.sidebar.update(cx, |s, cx| s.reveal(&folder, cx));
            });
            let pick = |this: &mut Shell, cx: &mut Context<Shell>| {
                let id = |this: &Shell, ix, cx: &App| row_id(this, ix, cx);
                let (first, last, extra) = (id(this, 1, cx), id(this, 4, cx), id(this, 7, cx));
                this.select(first, false, cx);
                this.library.update(cx, |l, cx| {
                    l.click_row(4, last, shift, cx);
                    l.click_row(7, extra, option, cx);
                });
            };
            step!("m01-list-multiselect-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| pick(this, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m01\t{}", state(this, cx)); }).ok();
            step!("m02-list-multiselect-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("m03-table-multiselect-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            step!("m04-table-multiselect-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m04\t{}", state(this, cx)); }).ok();
            step!("m05-table-menu-for-selection-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                let id = row_id(this, 2, cx);
                this.library.update(cx, |l, cx| l.open_picker(id, point(px(520.), px(190.)), cx));
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m05\t{}", state(this, cx)); }).ok();
            step!("m06-list-menu-for-selection-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.set_view(ViewMode::Reading, cx);
                let id = row_id(this, 3, cx);
                this.library.update(cx, |l, cx| { l.close_picker(cx); l.open_picker(id, point(px(330.), px(210.)), cx); });
            });
            step!("m07-move-folder-list-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.choose_picker_mode(memex_library::view::PickMode::MoveToFolder, cx));
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m07\t{}", state(this, cx)); }).ok();
            step!("m08-map-library-list-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.library.update(cx, |l, cx| l.choose_picker_mode(memex_library::view::PickMode::MapToLibrary, cx));
            });
            step!("m09-move-remove-failed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { fail_next_remove_members: Some("internal_error".into()), ..f.controls() }); }
                this.library.update(cx, |l, cx| { l.close_picker(cx); l.move_selection_to("grp_c_attention".into(), cx) });
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m09\t{}", state(this, cx)); }).ok();
            step!("m10-move-retried-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.library.update(cx, |l, cx| { l.close_picker(cx); l.move_selection_to("grp_c_attention".into(), cx) });
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m10\t{}", state(this, cx)); }).ok();
            step!("m11-inbox-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Inbox, cx));
            });
            step!("m12-inbox-menu-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = row_id(this, 2, cx);
                this.library.update(cx, |l, cx| l.open_picker(id, point(px(330.), px(170.)), cx));
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m12\t{}", state(this, cx)); }).ok();
            this.update_in(cx, |this, _, cx| this.set_appearance(Appearance::Light, cx)).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // The sidebar (fixture only): Inbox at the top and selected at start, the
        // folder ⋯ menu, rename by double click, and an agent without
        // Files dragged in from Finder and the import tray (synthetic fixture and
        // small synthetic files under /private/tmp only): the drop hint over the
        // reader and over a sidebar folder, a tray of mixed states, and an agent
        // without the import capability: `MEMEX_DESKTOP_WALK=import`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("import") {
            if agent {
                std::fs::write(dir.join("states.tsv"), "import walk runs on the synthetic fixture only; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            use memex_client::fixture::ImportControls;
            use memex_library::import::DropSpot;
            let files = PathBuf::from(format!("/private/tmp/memex-import-walk-files-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&files);
            let put = |name: &str, bytes: &[u8]| {
                let p = files.join(name);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, bytes).unwrap();
                p
            };
            let mut first = vec![
                put("认知负荷综述.pdf", b"%PDF-1.7\nsynthetic"),
                put("会议笔记.md", b"# synthetic"),
                put("网页存档.html", b"<p>synthetic</p>"),
                put("小说样本.epub", b"PK\x03\x04synthetic"),
                put("扫描件-broken.pdf", b"%PDF-1.4\nsynthetic"),
                put("讲义-lost.pdf", b"%PDF-1.4\nsynthetic"),
                put("表格.xlsx", b"PK\x03\x04synthetic"),
            ];
            let huge = put("超大扫描.pdf", b"%PDF-1.4\n");
            std::fs::OpenOptions::new().write(true).open(&huge).unwrap().set_len(120 * 1024 * 1024).unwrap();
            first.push(huge);
            put("资料夹/摘录.txt", b"synthetic");
            put("资料夹/子目录/附录.md", b"# synthetic");
            first.push(files.join("资料夹"));
            let second = vec![put("第二批-甲.pdf", b"%PDF-1.7\nsynthetic"), put("第二批-乙.md", b"# synthetic"), put("第二批-丙.txt", b"synthetic")];
            let imports = this.read_with(cx, |s, _| s.import_fixture.clone()).unwrap().expect("synthetic import backend");
            let library = this.read_with(cx, |s, _| s.library.clone()).unwrap();
            imports.set_controls(ImportControls { instant: true, fail_names: vec!["broken".into()], lose_receipt_names: vec!["lost".into()], ..Default::default() });
            let reader = point(px(760.), px(430.));
            // Shell listeners run on these events: dispatch outside any shell update.
            macro_rules! file_drop {
                ($e:expr) => {{ cx.update(|window, cx| { window.dispatch_event(PlatformInput::FileDrop($e), cx); }).ok(); }};
            }
            let paths = |v: &[PathBuf]| ExternalPaths(v.iter().cloned().collect());
            this.update_in(cx, |this, _, cx| {
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_rag".into()), cx));
            }).ok();
            wait(settle).await;
            file_drop!(FileDropEvent::Entered { position: reader, paths: paths(&first) });
            file_drop!(FileDropEvent::Pending { position: reader });
            step!("m01-drop-hint-reader-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            step!("m02-drop-hint-reader-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // Find the 待读 row by moving down the sidebar (release builds keep no element bounds).
            let target = DropSpot::Group("grp_c_toread".into());
            let mut folder_at = None;
            for y in (60..880).step_by(6) {
                let at = point(px(96.), px(y as f32));
                file_drop!(FileDropEvent::Pending { position: at });
                if cx.update(|_, cx| library.read(cx).drop_hover.clone()).ok().flatten() == Some(target.clone()) {
                    folder_at = Some(at);
                    break;
                }
            }
            let folder_at = folder_at.expect("the 待读 row takes file drops");
            step!("m03-drop-hint-folder-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            step!("m04-drop-hint-folder-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // The first batch lands in 待读 at once; the second (slow) on the reader, into RAG.
            file_drop!(FileDropEvent::Submit { position: folder_at });
            file_drop!(FileDropEvent::Exited);
            wait(2500).await;
            imports.set_controls(ImportControls { slow: true, ..Default::default() });
            file_drop!(FileDropEvent::Entered { position: reader, paths: paths(&second) });
            file_drop!(FileDropEvent::Pending { position: reader });
            file_drop!(FileDropEvent::Submit { position: reader });
            file_drop!(FileDropEvent::Exited);
            wait(4300).await;
            let tray = |this: &Shell, cx: &App| this.importer.read(cx).tray.rows().iter().map(|r| format!("{}={}@{}", r.name, r.state.label(), r.destination.label())).collect::<Vec<_>>().join(" | ");
            step!("m05-tray-mixed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m05\t{}", tray(this, cx)); }).ok();
            step!("m06-tray-mixed-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "m06\t{}", tray(this, cx)); }).ok();
            // An agent without the import capability: the hint says so, a drop sends nothing.
            imports.set_controls(ImportControls { capability_missing: true, instant: true, ..Default::default() });
            this.update_in(cx, |this, _, cx| this.importer.update(cx, |i, cx| { i.tray = Default::default(); i.probe(cx); })).ok();
            wait(settle).await;
            let sent = imports.submits().len();
            file_drop!(FileDropEvent::Entered { position: reader, paths: paths(&second) });
            file_drop!(FileDropEvent::Pending { position: reader });
            step!("m07-closed-hint-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            step!("m08-closed-hint-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            file_drop!(FileDropEvent::Submit { position: reader });
            file_drop!(FileDropEvent::Exited);
            step!("m09-closed-notice-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            step!("m10-closed-notice-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update_in(cx, |this, _, cx| { let n = this.importer.read(cx).tray.notice.clone(); let _ = writeln!(log, "m10\tnotice={n:?}\tsubmits_after_closed={}", imports.submits().len() - sent); }).ok();
            let _ = std::fs::remove_dir_all(&files);
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 发布日期 and 时长 (fixture only): the reading list, the reader's line under
        // the title, the info panel and the classic table's optional columns, on
        // the synthetic podcast (2026-10-30, 3626 s): `MEMEX_DESKTOP_WALK=dates`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("dates") {
            use memex_client::fixture::PODCAST_ITEM;
            this.update(cx, |this, cx| {
                this.library.update(cx, |l, cx| { l.set_filter(Filter::ItemType("网页文章".into()), cx); l.set_sort(memex_client::SortKey::Title, false, cx); });
            }).ok();
            wait(1000).await;
            this.update(cx, |this, cx| this.library.update(cx, |l, cx| l.ensure_rows(0..200, cx))).ok();
            wait(1000).await;
            let dates = |this: &Shell, cx: &App| {
                let row = this.library.read(cx).row_for(PODCAST_ITEM);
                let line = this.reader.read(cx).dates_line(PODCAST_ITEM);
                let keys = this.table.read(cx).table.read(cx).delegate().shown_keys();
                format!("row={:?}\tline={line:?}\tcolumns={keys:?}", row.map(|r| (r.list_meta(), r.published_at, r.duration_seconds)))
            };
            step!("d01-reading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(PODCAST_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "d01\t{}", dates(this, cx)); }).ok();
            step!("d02-reading-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("d03-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            // The optional columns sit at the right end: bring them into view.
            step!("d03-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let table = this.table.read(cx).table.clone();
                table.update(cx, |t, cx| t.scroll_to_col(8, cx));
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "d03\t{}", dates(this, cx)); }).ok();
            step!("d04-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update(cx, |this, cx| { this.set_appearance(Appearance::Light, cx); this.set_view(ViewMode::Reading, cx); }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // `query_unfiled`: `MEMEX_DESKTOP_WALK=sidebar`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("sidebar") {
            let folder = "grp_c_sleep".to_string();
            let inbox = |this: &Shell, cx: &App| {
                let m = &this.library.read(cx).model;
                format!("filter={:?}\tunfiled={}\tcount={:?}\tsupported={:?}\trows={}\tnotice={}", m.filter(), m.spec.unfiled, m.inbox_count, m.unfiled_supported, m.len(), m.inbox_notice().is_some())
            };
            step!("s01-inbox-default-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "s01\t{}", inbox(this, cx)); }).ok();
            step!("s02-inbox-default-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("s03-folder-more-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::Group(folder.clone()), cx));
                let tree = this.sidebar.read(cx).tree();
                // Where the selected row's ⋯ sits (the menu opens at the pointer).
                tree.update(cx, |t, cx| { t.reveal(&folder, cx); t.open_menu(&folder, point(px(186.), px(186.)), cx); });
            });
            step!("s04-rename-by-double-click-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| { t.close_menu(cx); t.start_edit(memex_library::edit::Draft::Rename { id: folder.clone() }, window, cx); });
            });
            this.update_in(cx, |this, _, cx| { let r = this.sidebar.read(cx).tree().read(cx).renaming().map(str::to_string); let _ = writeln!(log, "s04\trenaming={r:?}"); }).ok();
            // 删除…: the server preview confirmed at the row (a parent with subfolders).
            let parent = "grp_c_cog".to_string();
            step!("s04b-delete-confirm-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| { let _ = window; t.close_menu(cx); });
                this.sidebar.update(cx, |s, cx| s.reveal(&parent, cx));
                this.library.update(cx, |l, cx| l.begin_delete(parent.clone(), None, cx));
            });
            this.update_in(cx, |this, _, cx| { let l = this.library.read(cx); let _ = writeln!(log, "s04b\tflow={:?}", l.delete); }).ok();
            step!("s04c-deleted-libraries-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.confirm_delete(cx));
                this.sidebar.update(cx, |s, cx| { s.deletions_open = true; cx.notify(); });
            });
            this.update_in(cx, |this, _, cx| { let l = this.library.read(cx); let _ = writeln!(log, "s04c\tdeletions={:?}\tfilter={:?}\tcog={}\tinbox={:?}", l.deletions.iter().map(|d| &d.name).collect::<Vec<_>>(), l.model.filter(), l.model.group("grp_c_cog").is_some(), l.model.inbox_count); }).ok();
            step!("s04d-restored-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                let id = this.library.read(cx).deletions.first().map(|d| d.deletion_id.clone());
                if let Some(id) = id { this.library.update(cx, |l, cx| l.restore_deletion(id, cx)); }
            });
            this.update_in(cx, |this, _, cx| { let l = this.library.read(cx); let _ = writeln!(log, "s04d\tdeletions={}\tcog={}\tinbox={:?}", l.deletions.len(), l.model.group("grp_c_cog").is_some(), l.model.inbox_count); }).ok();
            step!("s05-inbox-without-unfiled-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.sidebar.update(cx, |s, cx| { s.deletions_open = false; cx.notify(); });
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| t.close_menu(cx));
                if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { no_unfiled_query: true, ..f.controls() }); }
                this.library.update(cx, |l, cx| { l.client().forget_capabilities(); l.set_filter(Filter::Inbox, cx); });
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "s05\t{}", inbox(this, cx)); }).ok();
            step!("s06-inbox-without-unfiled-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update_in(cx, |this, _, cx| this.set_appearance(Appearance::Light, cx)).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // The 翻译 button fixed in the 对照 / 译文 toolbar (synthetic fixture only):
        // untranslated, failed, a failed status read read again first, running,
        // translated, a Chinese source, no model, and a book chapter with a human
        // translation: `MEMEX_DESKTOP_WALK=translatebutton`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("translatebutton") {
            use memex_reader::model::Mode;
            let mode = |this: &mut Shell, m: Mode, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(m, cx));
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| { let r = s.reader.read(cx); (r.model.active_item().map(str::to_string), r.model.mode(), format!("{:?}", r.translate.button()), r.model.book().map(|_| r.chapter_tr_button().0)) }).ok();
                    let _ = writeln!(log, "{}\t{s:?}", $name);
                }};
            }
            let offline = |this: &mut Shell, on: bool| if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { offline: on, ..f.controls() }); };
            step!("t01-untranslated-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select("item_fixture_000002".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            note!("t01");
            step!("t02-untranslated-translation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                mode(this, Mode::Translation, cx);
            });
            note!("t02");
            step!("t03-failed-retry-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select("item_fixture_000014".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            note!("t03");
            // The status read fails: 重试 reads again before it may request.
            this.update(cx, |this, cx| { this.select("item_fixture_000002".into(), false, cx); mode(this, Mode::Bilingual, cx); }).ok();
            wait(1000).await;
            step!("t04-read-failed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                offline(this, true);
                this.reader.update(cx, |r, cx| r.recheck_translation(cx));
            });
            note!("t04");
            step!("t05-reread-then-running-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                offline(this, false);
                this.reader.update(cx, |r, cx| r.recheck_translation(cx));
            });
            wait(800).await;
            step!("t05-reread-then-running-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("t05");
            step!("t06-running-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            wait(6000).await;
            step!("t07-translated-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            note!("t07");
            // Pressing a disabled 已翻译 cannot send; reading again stays put.
            step!("t08-showcase-translated-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.select(memex_client::fixture::SHOWCASE_ITEM.into(), false, cx);
                mode(this, Mode::Translation, cx);
            });
            note!("t08");
            step!("t09-chinese-source-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select("item_fixture_000008".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            note!("t09");
            step!("t10-no-model-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture { f.set_controls(FixtureControls { no_translation_provider: true, ..f.controls() }); }
                this.client.forget_capabilities();
                this.reader.update(cx, |r, cx| r.probe_capability(cx));
                this.select("item_fixture_000014".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            note!("t10");
            step!("t11-book-human-chapter-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture { f.set_controls(FixtureControls { no_translation_provider: false, ..f.controls() }); }
                this.client.forget_capabilities();
                this.reader.update(cx, |r, cx| r.probe_capability(cx));
                this.select(memex_client::fixture::BOOK_ITEM.into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            wait(800).await;
            step!("t11-book-human-chapter-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(1, cx)));
            wait(800).await;
            step!("t11-book-human-chapter-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("t11");
            step!("t12-book-human-chapter-translation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                mode(this, Mode::Translation, cx);
            });
            note!("t12");
            this.update(cx, |this, cx| { this.set_appearance(Appearance::Light, cx); mode(this, Mode::Bilingual, cx); }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Translation progress and diagnostics of a newer agent (synthetic fixture
        // only): running 1/2 段 · 3 分钟 with its moving bar, a finished translation
        // with blocks kept in the original, and a failure naming its part, each light
        // and dark: `MEMEX_DESKTOP_WALK=translationprogress`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("translationprogress") {
            use memex_client::fixture::TranslationDemo;
            use memex_reader::model::Mode;
            let demo = |this: &mut Shell, item: &str, d: TranslationDemo| if let Some(f) = this.fixture.as_ref() { f.set_translation_demo(item, d); };
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| { let r = s.reader.read(cx); (r.model.active_item().map(str::to_string), format!("{:?}", r.translate.button()), r.translate.status()) }).ok();
                    let _ = writeln!(log, "{}\t{s:?}", $name);
                }};
            }
            // An occluded window has no display-link frames. Pump and capture
            // one full animation cycle, rather than only its initial position.
            macro_rules! motion {
                ($name:expr) => {{
                    let started = std::time::Instant::now();
                    for frame in 0..20 {
                        wait(80).await;
                        let name = format!("{}-motion-{frame:02}", $name);
                        let path = dir.join(format!("{name}.png"));
                        let saved = cx.update(|window, cx| {
                            window.draw(cx).clear(cx);
                            window.render_to_image().map_err(|e| e.to_string()).and_then(|img| img.save(&path).map_err(|e| e.to_string()))
                        }).map_err(|e| e.to_string()).and_then(|r| r);
                        let _ = writeln!(log, "{name}\telapsed_ms={}\tsaved={:?}", started.elapsed().as_millis(), saved.err());
                    }
                }};
            }
            step!("p01-running-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                demo(this, "item_fixture_000002", TranslationDemo::Running { done: 1, total: 2, minutes: 3 });
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select("item_fixture_000002".into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            note!("p01");
            motion!("p01-running-light");
            step!("p02-running-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            note!("p02");
            motion!("p02-running-dark");
            step!("p03-partial-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                demo(this, memex_client::fixture::SHOWCASE_ITEM, TranslationDemo::Partial { untranslated: 3 });
                this.set_appearance(Appearance::Light, cx);
                this.select(memex_client::fixture::SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            note!("p03");
            step!("p04-partial-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            note!("p04");
            step!("p05-failed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                demo(this, memex_client::fixture::PARTIAL_TRANSLATION_ITEM, TranslationDemo::Failed { done: 2, total: 5, detail: "batch_3_of_5:count_mismatch".into() });
                this.set_appearance(Appearance::Light, cx);
                this.select(memex_client::fixture::PARTIAL_TRANSLATION_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            note!("p05");
            step!("p06-failed-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            note!("p06");
            this.update(cx, |this, cx| this.set_appearance(Appearance::Light, cx)).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // AI-translation marks and the translation-side speaker bubble (fixture only):
        // five marks on an AI chapter translation (one in its second 512-byte chunk,
        // one on the second of two identical sentences), the same marks in 对照, a lost
        // reply with 重试同一请求, a retranslation leaving them stale (delete-only),
        // an agent without the capability, and 译文 of a podcast whose translation
        // renamed the speakers: `MEMEX_DESKTOP_WALK=aimarks`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("aimarks") {
            use memex_client::fixture::{FixtureControls, AI_BOOK_ITEM, PODCAST_ITEM};
            use memex_client::reader_state::MarkStyle;
            use memex_reader::model::Mode;
            if agent {
                std::fs::write(dir.join("states.tsv"), "aimarks runs on the synthetic fixture only; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| { let r = s.reader.read(cx); format!("mode={:?}\tai[{}]\ttr[{}]\tlr[{}]", r.model.mode(), r.dev_ai_marks(), r.dev_chapter_translation(), r.dev_longread()) }).unwrap_or_default();
                    let _ = writeln!(log, "{}\t{s}", $name);
                }};
            }
            let controls = |this: &mut Shell, cx: &mut Context<Shell>, f: &dyn Fn(FixtureControls) -> FixtureControls| {
                if let Some(fx) = this.fixture.as_ref() { fx.set_controls(f(fx.controls())); }
                this.client.forget_capabilities();
                this.reader.update(cx, |r, cx| r.probe_capability(cx));
            };
            // Selects the n-th (0-based) occurrence of `term` in the AI text on screen
            // (standing in for a drag), then applies `style`.
            let mark = |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>, term: &str, nth: usize, style: MarkStyle| this.reader.update(cx, |r, cx| {
                let text = r.dev_ai_text();
                let Some((at, _)) = text.match_indices(term).nth(nth) else { return };
                r.dev_select_ai(at..at + term.len(), window, cx);
                r.apply_mark(style, window, cx);
            });
            this.update(cx, |this, cx| controls(this, cx, &|c| FixtureControls { chapter_translations: true, ..c })).ok();
            wait(600).await;
            step!("m00-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(AI_BOOK_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx));
            });
            wait(1500).await;
            this.update(cx, |this, cx| this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx))).ok();
            wait(800).await;
            note!("m00");
            for (term, nth, style) in [("潮水来了", 2, MarkStyle::Yellow), ("铜齿被盐雾咬出了缺口", 0, MarkStyle::Red), ("钟不会骗人", 0, MarkStyle::Bold),
                ("防波堤", 0, MarkStyle::Underline), ("风大的夜里", 0, MarkStyle::Blue)] {
                this.update_in(cx, |this, window, cx| mark(this, window, cx, term, nth, style)).ok();
                wait(700).await;
            }
            step!("m01-ai-marks-translation-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m01");
            step!("m01-ai-marks-translation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("m02-ai-marks-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            note!("m02");
            step!("m02-ai-marks-bilingual-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // The agent goes away while a mark is sent: unknown, kept as sent, resent
            // only by 重试同一请求 (here after it is back).
            step!("m03-unknown-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx));
                if let Some(fx) = this.fixture.as_ref() { fx.set_controls(FixtureControls { offline: true, ..fx.controls() }); }
                mark(this, window, cx, "三个晚上", 0, MarkStyle::Yellow);
            });
            wait(800).await;
            step!("m03-unknown-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m03");
            step!("m03-retried-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(fx) = this.fixture.as_ref() { fx.set_controls(FixtureControls { offline: false, ..fx.controls() }); }
                this.reader.update(cx, |r, cx| r.dev_retry_ai_mark(cx));
            });
            wait(800).await;
            step!("m03-retried-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m03b");
            // A retranslation publishes: every mark of chapter 1 is stale, drawn nowhere.
            step!("m04-stale-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(fx) = this.fixture.as_ref() { fx.retranslate_ai_chapter("chapter-1"); }
                this.reader.update(cx, |r, cx| r.reload(cx));
            });
            wait(2000).await;
            step!("m04-stale-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m04");
            step!("m04-stale-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // An agent without the capability: the bar says why, nothing is sent.
            step!("m05-unsupported-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                controls(this, cx, &|c| FixtureControls { no_ai_annotations: true, ..c });
            });
            wait(800).await;
            step!("m05-unsupported-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                let text = r.dev_ai_text();
                if let Some(at) = text.find("铜齿") { r.dev_select_ai(at..at + "铜齿".len(), window, cx); }
            }));
            note!("m05");
            // 译文 of a podcast: the bubble names the original speaker.
            step!("m06-speaker-translation-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                gpui_kit::base::TextSelection::clear(window, cx);
                controls(this, cx, &|c| FixtureControls { no_ai_annotations: false, ..c });
                this.select(PODCAST_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx));
            });
            wait(1200).await;
            step!("m06-speaker-translation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.dev_scroll_to(1 + 9, cx)));
            wait(400).await;
            step!("m06-speaker-translation-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m06");
            step!("m06-speaker-translation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("m07-speaker-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.dev_scroll_to(1 + 9, cx); });
            });
            wait(400).await;
            step!("m07-speaker-original-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m07");
            // 说话人名字 on the AI book's transcript chapter: its recorded one-to-one block
            // correspondence pairs 「托尔斯滕」 with Thorsten, so each choice applies.
            step!("m08-open-transcript-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select(AI_BOOK_ITEM.into(), false, cx);
            });
            wait(1500).await;
            this.update(cx, |this, cx| this.reader.update(cx, |r, cx| { r.set_mode(Mode::Translation, cx); r.go_chapter(2, cx); })).ok();
            wait(2500).await;
            for (names, tag) in [(memex_client::SpeakerNames::Original, "original"), (memex_client::SpeakerNames::Translated, "translated"), (memex_client::SpeakerNames::Both, "both")] {
                let name = format!("m08-speaker-names-{tag}-light");
                step!(name.clone(), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_speaker_names(names, cx);
                    this.reader.update(cx, |r, cx| { r.set_mode(Mode::Translation, cx); r.scroll_column(true, 2, cx); });
                });
                wait(400).await;
                step!(name, |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.dev_scroll_column_by(true, 2, 30., cx)));
                note!(format!("m08-{tag}"));
            }
            step!("m08-speaker-names-both-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("m09-original-translated-names-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_speaker_names(memex_client::SpeakerNames::Translated, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.dev_scroll_to(1 + 3, cx); });
            });
            wait(400).await;
            step!("m09-original-translated-names-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("m09");
            for dark in [false, true] {
                this.update_in(cx, |s, window, cx| {
                    s.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    s.set_speaker_names(memex_client::SpeakerNames::Original, cx);
                    s.open_settings(window, cx);
                    if let Some((_, settings)) = &s.settings { settings.update(cx, |st, cx| { st.page = memex_settings::view::Page::Appearance; st.speaker_names = memex_client::SpeakerNames::Original; cx.notify(); }); }
                }).ok();
                wait(settle).await;
                let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
                if let Some(h) = handle {
                    let file = dir.join(format!("m10-settings-speaker-names-{}.png", if dark { "dark" } else { "light" }));
                    let saved = cx.update_window(h, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(&file).map_err(|e| e.to_string()))
                    });
                    let _ = writeln!(log, "m10\tdark={dark}\tsaved={:?}", saved.map_err(|e| e.to_string()).and_then(|r| r).err());
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 长文阅读 v2 rev10 on the synthetic transcript (fixture only): toolbar order,
        // rail and knobs, 重点 menu, 目录 floating / 钉住 / 常驻, the dial, the knob
        // list with bookmarks, the selection bar and a mark, the end zone, a revision
        // change and an item without AI highlights: `MEMEX_DESKTOP_WALK=longread`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("longread") {
            use memex_client::fixture::LONGREAD_ITEM;
            use memex_reader::model::Mode;
            use memex_reader::nav::Filter;
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| s.reader.read(cx).dev_longread()).unwrap_or_default();
                    let _ = writeln!(log, "{}\t{s}", $name);
                }};
            }
            let r = |this: &mut Shell, cx: &mut Context<Shell>, f: &dyn Fn(&mut memex_reader::view::Reader, &mut Context<memex_reader::view::Reader>)| this.reader.update(cx, |r, cx| f(r, cx));
            step!("l00-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(LONGREAD_ITEM.into(), false, cx);
                r(this, cx, &|r, cx| r.set_mode(Mode::Original, cx));
            });
            wait(800).await;
            step!("l01-reading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| r.dev_scroll_to(14, cx)));
            wait(400).await;
            step!("l01-reading-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("l01");
            step!("l02-reading-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("l03-highlight-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, cx| r.dev_open_menu(cx)); });
            step!("l04-toc-floating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| { r.lr.menu_open = false; r.dev_toc(true, false, false, cx); }));
            step!("l05-toc-pinned-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| { r.dev_toc(true, true, false, cx); r.lr.toc_top = 120.; }));
            step!("l06-toc-docked-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Dark, cx); r(this, cx, &|r, cx| r.dev_toc(true, true, true, cx)); });
            note!("l06");
            step!("l07-dial-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, cx| { r.dev_toc(false, false, false, cx); r.dev_open_dial(cx); }); });
            note!("l07");
            step!("l08-dial-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Dark, cx); r(this, cx, &|r, cx| r.lr_rotate(2., cx)); });
            // A bookmark at the middle paragraph, and 上次读到 from a ▼ step.
            step!("l09-knob-list-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, cx| { r.lr.dial = None; r.dev_add_bookmark(cx); }); });
            wait(600).await;
            step!("l09-knob-list-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| { r.lr_step(false, cx); r.dev_open_list(cx); }));
            wait(500).await;
            step!("l09-knob-list-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("l09");
            step!("l10-knob-list-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("l11-end-zone-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, cx| { r.lr.list_open = false; r.lr_pick(memex_reader::view::longread::KnobKey::Stop(memex_reader::nav::Stop::Chat), cx); }); });
            wait(500).await;
            step!("l11-end-zone-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            // Back to 上次读到, then a selection with its bar.
            step!("l12-selection-bar-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                r.lr_go_last(cx);
                let md = r.model.detail().map(|d| d.canonical.markdown.clone()).unwrap_or_default();
                let at = md.find("账单反而更高").unwrap_or(0);
                r.dev_select(at..at + "账单反而更高".len(), false, window, cx);
            }));
            note!("l12");
            step!("l13-marked-yellow-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                r.apply_mark(memex_client::reader_state::MarkStyle::Yellow, window, cx);
            }));
            // One edit in flight at a time: the second mark waits for the first receipt.
            wait(500).await;
            step!("l13-marked-yellow-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                let md = r.model.detail().map(|d| d.canonical.markdown.clone()).unwrap_or_default();
                let at = md.find("真正做事").unwrap_or(0);
                r.dev_select(at..at + "真正做事".len(), false, window, cx);
                r.apply_mark(memex_client::reader_state::MarkStyle::Red, window, cx);
            }));
            wait(500).await;
            // Jump to the first 手工 mark (as the dial would): both marks in view.
            step!("l13-marked-yellow-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| {
                if let Some(k) = r.lr.terms.iter().position(|t| t.manual) { r.lr_jump_term(k, cx); }
            }));
            wait(300).await;
            step!("l13-marked-yellow-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("l13");
            step!("l14-dial-with-manual-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Dark, cx); r(this, cx, &|r, cx| r.dev_open_dial(cx)); });
            step!("l15-mine-only-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, cx| { r.lr.dial = None; r.set_filter(Filter::Mine, cx); r.dev_open_menu(cx); }); });
            note!("l15");
            step!("l16-translation-selection-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                r.lr.menu_open = false;
                r.set_filter(Filter::All, cx);
                let md = r.model.detail().map(|d| d.canonical.markdown.clone()).unwrap_or_default();
                let at = md.find("重新安排一天").unwrap_or(0);
                r.dev_select(at..at + "重新安排一天".len(), true, window, cx);
            }));
            // Another revision: the old position does not bind.
            step!("l17-revision-changed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = this.fixture.as_ref() { f.simulate_external_edit(LONGREAD_ITEM); }
                r(this, cx, &|r, cx| r.reload(cx));
            });
            wait(800).await;
            step!("l17-revision-changed-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("l17");
            // An item without AI highlights: the menu offers generation, nothing sent.
            step!("l18-no-ai-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select("item_fixture_000008".into(), false, cx);
            });
            wait(800).await;
            step!("l18-no-ai-menu-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| r.dev_open_menu(cx)));
            note!("l18");
            step!("l19-no-ai-menu-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update(cx, |this, cx| { this.set_appearance(Appearance::Light, cx); r(this, cx, &|r, _| { r.lr.menu_open = false; }); }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 阅读记录 (reader_passes_v1) on synthetic items (fixture only): the end card with
        // seeded history (a 补记, a finished pass, an unfinished one), the 补记 form, ⏱ on
        // with time waiting to sync, the 阅读统计 panel, 标记读完, an item without a
        // translation (rates 「—」) and an agent without the capability:
        // `MEMEX_DESKTOP_WALK=passes`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("passes") {
            use memex_client::fixture::{LONGREAD_ITEM, SHOWCASE_ITEM};
            use memex_reader::model::Mode;
            use memex_reader::nav::Stop;
            use memex_reader::view::longread::KnobKey;
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| s.reader.read(cx).dev_passes()).unwrap_or_default();
                    let _ = writeln!(log, "{}\t{s}", $name);
                }};
            }
            let r = |this: &mut Shell, cx: &mut Context<Shell>, f: &dyn Fn(&mut memex_reader::view::Reader, &mut Context<memex_reader::view::Reader>)| this.reader.update(cx, |r, cx| f(r, cx));
            let end = |r: &mut memex_reader::view::Reader, cx: &mut Context<memex_reader::view::Reader>| r.lr_pick(KnobKey::Stop(Stop::Chat), cx);
            let offline = |this: &mut Shell, on: bool| if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { offline: on, ..f.controls() }); };
            this.update(cx, |this, _| if let Some(f) = this.fixture.as_ref() { f.seed_reading_passes(SHOWCASE_ITEM); f.seed_reading_passes(LONGREAD_ITEM); }).ok();
            step!("p00-end-card-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                r(this, cx, &|r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            wait(800).await;
            step!("p00-end-card-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &end));
            wait(500).await;
            step!("p00-end-card-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("p00");
            step!("p01-end-card-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("p02-backfill-form-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.dev_backfill("1.5", window, cx); end(r, cx); });
            });
            wait(400).await;
            step!("p02-backfill-form-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &end));
            step!("p03-backfill-form-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // A sub-minute hour value is refused, not rounded.
            step!("p04-backfill-refused-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.dev_backfill("0.01", window, cx); r.pass_save_backfill(cx); end(r, cx); });
            });
            note!("p04");
            step!("p05-history-with-backfill-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| { r.dev_backfill("1.5", window, cx); r.pass_save_backfill(cx); });
            });
            wait(600).await;
            step!("p05-history-with-backfill-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &end));
            note!("p05");
            step!("p06-history-with-backfill-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // ⏱ on by itself (the unfinished pass): a real activity starts it, then the agent
            // goes away and a mode switch closes a segment: it waits to sync, not counted.
            step!("p07-timing-on-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                r(this, cx, &|r, cx| r.dev_pass_activity(cx));
            });
            wait(4000).await;
            step!("p07-timing-on-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                offline(this, true);
                r(this, cx, &|r, cx| { r.dev_pass_activity(cx); r.set_mode(Mode::Original, cx); });
            });
            note!("p07");
            step!("p08-timing-on-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("p09-stats-panel-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                offline(this, false);
                this.set_appearance(Appearance::Light, cx);
                r(this, cx, &|r, cx| { r.set_mode(Mode::Bilingual, cx); r.pass_open_stats(None, cx); });
            });
            wait(800).await;
            step!("p09-stats-panel-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("p09");
            step!("p10-stats-panel-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("p11-marked-done-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                r(this, cx, &|r, cx| { r.passes.stats = None; r.pass_mark_done(cx); });
            });
            wait(800).await;
            step!("p11-marked-done-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &end));
            note!("p11");
            step!("p12-marked-done-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // No translation: 按译文 / 按对照 are 「—」.
            step!("p13-no-translation-rates-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select(LONGREAD_ITEM.into(), false, cx);
            });
            wait(800).await;
            step!("p13-no-translation-rates-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &|r, cx| r.pass_open_stats(None, cx)));
            wait(500).await;
            step!("p13-no-translation-rates-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("p13");
            step!("p14-no-translation-rates-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // A 补记's statistics: approximate, no days.
            step!("p15-backfill-stats-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                r(this, cx, &|r, cx| r.pass_open_stats(Some("pass_fixture_seed_1".into()), cx));
            });
            note!("p15");
            // An agent without reader_passes_v1: nothing sent, the card says so.
            step!("p16-capability-missing-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { no_reader_passes: true, ..f.controls() }); }
                r(this, cx, &|r, cx| { r.passes.stats = None; r.dev_forget_capabilities(); r.reload(cx); });
            });
            wait(800).await;
            step!("p16-capability-missing-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| r(this, cx, &end));
            wait(400).await;
            step!("p16-capability-missing-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("p16");
            step!("p17-capability-missing-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update(cx, |this, cx| {
                this.set_appearance(Appearance::Light, cx);
                if let Some(f) = this.fixture.as_ref() { f.set_controls(FixtureControls { no_reader_passes: false, ..f.controls() }); }
            }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 古籍 display on the synthetic 道德经 sample (fixture only): 简繁, 竖排 and
        // 寒蝉拙楷 (its note is in the system Kaiti SC, for comparison):
        // `MEMEX_DESKTOP_WALK=guji`.
        // Item covers (`item_cover_v1`) above the text, synthetic fixture only:
        // shown light and dark, an item without a cover, an agent without the
        // capability, a version change mid-read, a hash mismatch: `MEMEX_DESKTOP_WALK=cover`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("cover") {
            use memex_client::fixture::{LONGREAD_ITEM, PODCAST_ITEM, SHOWCASE_ITEM};
            if agent {
                std::fs::write(dir.join("states.tsv"), "cover walk runs on the synthetic fixture only; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let controls = |this: &mut Shell, f: &dyn Fn(FixtureControls) -> FixtureControls| {
                let fixture = this.fixture.as_ref().unwrap();
                fixture.set_controls(f(fixture.controls()));
                this.client.forget_capabilities();
            };
            let cover = |this: &Shell, cx: &App| format!("{:?}", this.reader.read(cx).dev_cover());
            step!("c01-cover-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(LONGREAD_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c01\tcover={}", cover(this, cx)); }).ok();
            step!("c02-cover-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c02\tcover={}", cover(this, cx)); }).ok();
            step!("c03-podcast-cover-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(PODCAST_ITEM.into(), false, cx));
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c03\tcover={}", cover(this, cx)); }).ok();
            step!("c04-no-cover-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select("item_fixture_000008".into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c04\tcover={}", cover(this, cx)); }).ok();
            step!("c05-no-capability-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                controls(this, &|c| FixtureControls { no_item_cover: true, ..c });
                this.select(LONGREAD_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c05\tcover={}\tfeatures_has_cover={}", cover(this, cx), this.client.capabilities().map(|c| c.item_cover).unwrap_or(false)); }).ok();
            step!("c06-stale-then-new-version-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                controls(this, &|c| FixtureControls { no_item_cover: false, cover_stale_once: true, ..c });
                this.select(PODCAST_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c06\tcover={}\tstale_left={}", cover(this, cx), this.fixture.as_ref().unwrap().controls().cover_stale_once); }).ok();
            step!("c07-hash-mismatch-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                controls(this, &|c| FixtureControls { cover_corrupt: true, ..c });
                this.select(SHOWCASE_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| { let _ = writeln!(log, "c07\tcover={}", cover(this, cx)); }).ok();
            step!("c08-batch-null-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                controls(this, &|c| FixtureControls { cover_corrupt: false, cover_null: true, ..c });
                this.set_appearance(Appearance::Dark, cx);
                this.select(LONGREAD_ITEM.into(), false, cx);
            });
            this.update_in(cx, |this, _, cx| {
                let _ = writeln!(log, "c08\tcover={}", cover(this, cx));
                controls(this, &|c| FixtureControls { cover_null: false, ..c });
                this.set_appearance(Appearance::Light, cx);
            }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("guji") {
            use memex_client::{fixture::GUJI_ITEM, opencc::Script, ItemStyle, ReadingFont};
            use memex_reader::model::Mode;
            let _ = writeln!(log, "font\tkai_family={:?}", ReadingFont::Kai.family());
            let style = |script, vertical, font| ItemStyle { script, vertical, font };
            // The style belongs to the item on screen: open it first.
            step!("g00-horizontal-original-app-font-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(GUJI_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            step!("g01-horizontal-original-kai-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_item_style(style(Script::Original, false, Some(ReadingFont::Kai)), cx)));
            step!("g02-horizontal-traditional-kai-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_script(Script::Traditional, cx)));
            step!("g03-vertical-simplified-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_item_style(style(Script::Simplified, true, None), cx)));
            step!("g04-vertical-traditional-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.set_script(Script::Traditional, cx));
            });
            step!("g05-vertical-traditional-read-on-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_vertical(600., cx)));
            step!("g06-vertical-traditional-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            this.update_in(cx, |this, _, cx| {
                let r = this.reader.read(cx);
                let _ = writeln!(log, "end\tstyle={:?}\tfont={:?}\tsaved={:?}", r.item_style(), r.item_font(), this.prefs.item_styles.get(GUJI_ITEM));
                // Leaves the development state as it was found.
                this.set_view(ViewMode::Reading, cx);
                this.reader.update(cx, |r, cx| r.set_item_style(ItemStyle::default(), cx));
            }).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // 翻译本章 against the backend's synthetic translation fixture over its socket
        // (MEMEX_AGENT_SOCKET + MEMEX_WALK_ITEM, MEMEX_WALK_OUTCOME=success|failed|unknown):
        // `MEMEX_DESKTOP_WALK=chaptertranslate`. Its appendix is English with no counterpart.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("chaptertranslate") {
            use memex_reader::model::Mode;
            let socket = std::env::var("MEMEX_AGENT_SOCKET").unwrap_or_default();
            if !agent || !socket.starts_with("/private/tmp/memex-mobile-") || !socket.ends_with("/agent.sock") {
                std::fs::write(dir.join("states.tsv"), "chaptertranslate requires isolated mobile_fixture; refused\n").ok();
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            let item = std::env::var("MEMEX_WALK_ITEM").unwrap_or_default();
            let outcome = std::env::var("MEMEX_WALK_OUTCOME").unwrap_or_default();
            macro_rules! note {
                ($name:expr) => {{
                    let s = this.read_with(cx, |s, cx| {
                        let r = s.reader.read(cx);
                        (r.model.book().map(|b| (b.doc.revision_id.clone(), b.at, b.translation.is_some())), r.dev_chapter_translation(), s.chat.read(cx).composer().read(cx).value().to_string())
                    }).ok();
                    let _ = writeln!(log, "{}\t{s:?}", $name);
                }};
            }
            let draft = "草稿：翻译期间应保留";
            // Chapter 1 has a human translation: no 翻译本章, nothing previewed.
            step!(format!("x01-{outcome}-ineligible-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                this.select(item.clone(), false, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(true, cx); });
            });
            wait(1500).await;
            step!(format!("x01-{outcome}-ineligible-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("x01");
            step!(format!("x02-{outcome}-ineligible-dark"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!(format!("x03-{outcome}-offer-light"), |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value(draft, window, cx));
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.go_chapter(1, cx); });
            });
            note!("x03");
            let previews = |this: &Shell, cx: &App| this.reader.read(cx).dev_chapter_translation_previews();
            step!(format!("x03b-{outcome}-toolbar-offer-translation-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx)));
            this.read_with(cx, |s, cx| { let _ = writeln!(log, "x03b\ttoolbar={:?}", s.reader.read(cx).chapter_tr_button()); }).ok();
            let opened = this.read_with(cx, |s, cx| previews(s, cx)).unwrap_or(usize::MAX);
            let _ = writeln!(log, "check\tpreviews_after_open={opened}");
            assert_eq!(opened, 0, "opening chapters never previews");
            // Pressed on the toolbar's 翻译本章 (in 译文): the same confirmation card.
            step!(format!("x04-{outcome}-confirm-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { if let (_, Some(a)) = r.chapter_tr_button() { r.press_chapter_tr(a, cx); } }));
            note!("x04");
            let clicked = this.read_with(cx, |s, cx| previews(s, cx)).unwrap_or(usize::MAX);
            let _ = writeln!(log, "check\tpreviews_after_click={clicked}");
            assert_eq!(clicked, 1, "exactly one preview per click");
            step!(format!("x05-{outcome}-confirm-dark"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update(cx, |s, cx| { s.set_appearance(Appearance::Light, cx); s.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.confirm_chapter_translation(cx); }); }).ok();
            wait(6000).await;
            step!(format!("x06-{outcome}-terminal-original-light"), |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            note!("x06");
            step!(format!("x07-{outcome}-terminal-parallel-light"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx)));
            step!(format!("x08-{outcome}-terminal-translation-dark"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx));
            });
            note!("x08");
            let last = this.read_with(cx, |s, cx| s.reader.read(cx).dev_chapter_translation_last()).ok().flatten();
            let doc = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| b.doc.clone())).ok().flatten();
            let client = this.read_with(cx, |s, _| s.client.clone()).unwrap();
            if let (Some((rid, qid, jid)), Some(doc)) = (last.clone(), doc.clone()) {
                // Replay of the original request: the original job, no new dispatch;
                // then a status read and a full text read dispatch nothing either.
                let c = client.clone();
                let check = cx.background_executor().spawn(async move {
                    let replay = c.reader_translate_request(&doc, "appendix", &rid, &qid);
                    let before = replay.as_ref().ok().and_then(|j| j.usage.clone()).map(|u| u.requests_dispatched);
                    let text = replay.as_ref().ok().and_then(|j| j.translation.clone()).map(|m| c.reader_translation_text(&m).map(|t| t.len()));
                    let after = c.reader_translate_status(&doc.item_id, &jid).ok().and_then(|j| j.usage).map(|u| (u.requests_dispatched, u.input_tokens, u.output_tokens, u.reported_total_tokens, u.unknown_usage_requests));
                    (replay.map(|j| (j.job_id == jid, j.state, j.error_code, j.completed_blocks, j.total_blocks)), before, text, after)
                }).await;
                let _ = writeln!(log, "replay\t{check:?}");
            }
            match outcome.as_str() {
                "unknown" => {
                    // 查询原任务: replays the same id; the original job comes back.
                    step!("x09-unknown-after-query-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        this.set_appearance(Appearance::Light, cx);
                        this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.query_chapter_translation(cx); });
                    });
                    wait(3000).await;
                    step!("x09-unknown-after-query-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                    note!("x09");
                }
                "failed" => {
                    // 再试一次 goes through preview and confirmation again.
                    step!("x09-failed-retry-confirm-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        this.set_appearance(Appearance::Light, cx);
                        this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.ask_chapter_translation(cx); });
                    });
                    note!("x09");
                }
                _ => {
                    // Back to chapter 1 and again to the appendix: the result is read without cost.
                    step!("x09-success-reopen-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                        this.set_appearance(Appearance::Light, cx);
                        this.reader.update(cx, |r, cx| { r.set_mode(Mode::Original, cx); r.go_chapter(0, cx); });
                    });
                    step!("x09-success-reopen-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(1, cx)));
                    note!("x09");
                    let reopened = this.read_with(cx, |s, cx| previews(s, cx)).unwrap_or(usize::MAX);
                    let _ = writeln!(log, "check\tpreviews_after_reopen={reopened}");
                    assert_eq!(reopened, 1, "a known result is read again without a preview");
                    step!("x10-success-reopen-parallel-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx)));
                    if let Some((_, _, jid)) = last {
                        let item = item.clone();
                        let n = cx.background_executor().spawn(async move { client.reader_translate_status(&item, &jid).ok().and_then(|j| j.usage).map(|u| u.requests_dispatched) }).await;
                        let _ = writeln!(log, "after-reopen\trequests_dispatched={n:?}");
                    }
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Preview/pinned tabs, notes (smaller, ruled, note typeface), 下一章 at a
        // chapter's end and 全书完 (synthetic fixture): `MEMEX_DESKTOP_WALK=readerux`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("readerux") {
            use memex_client::fixture::BOOK_ITEM;
            use memex_reader::model::Mode;
            let tabs = |s: &Shell, cx: &App| s.reader.read(cx).model.tabs().iter().map(|t| format!("{}{}", t.item_id, if t.pinned { "*" } else { "" })).collect::<Vec<_>>().join(",");
            step!("u01-tab-preview-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                let (a, b) = (row_id(this, 1, cx), row_id(this, 2, cx));
                this.select(a, true, cx);
                this.select(b, false, cx);
            });
            let t1 = this.read_with(cx, |s, cx| tabs(s, cx)).unwrap_or_default();
            step!("u02-tab-preview-replaced-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { let c = row_id(this, 3, cx); this.select(c, false, cx); });
            let t2 = this.read_with(cx, |s, cx| tabs(s, cx)).unwrap_or_default();
            step!("u03-tab-pinned-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| if let Some(i) = r.model.active() { r.pin_tab(i, cx) });
                let d = row_id(this, 4, cx);
                this.select(d, false, cx);
            });
            let t3 = this.read_with(cx, |s, cx| tabs(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "tabs\tafter_preview={t1}\tafter_replace={t2}\tafter_pin={t3}");
            step!("u04-book-translation-note-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select(BOOK_ITEM.into(), true, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Translation, cx); });
            });
            step!("u04-book-translation-note-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(1, cx)));
            step!("u04-book-translation-note-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            let end = this.read_with(cx, |s, cx| s.reader.read(cx).chapter_after()).ok().flatten();
            let _ = writeln!(log, "u04\tchapter_after={end:?}");
            step!("u05-book-translation-note-end-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("u06-book-parallel-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(true, cx); r.go_chapter(1, cx); });
            });
            step!("u06-book-parallel-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { let n = r.model.original().len(); r.scroll_column(false, n.saturating_sub(1), cx); let m = r.model.translation().len(); r.scroll_column(true, m.saturating_sub(1), cx); }));
            step!("u07-book-paragraphs-note-song-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_note_font(memex_client::NoteFont::Song, cx);
                this.reader.update(cx, |r, cx| { r.set_book_parallel(false, cx); r.go_chapter(1, cx); });
            });
            step!("u07-book-paragraphs-note-song-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            step!("u08-book-last-chapter-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_note_font(memex_client::NoteFont::Kai, cx);
                this.reader.update(cx, |r, cx| { let n = r.model.book().map(|b| b.entries.len()).unwrap_or(1); r.set_mode(Mode::Original, cx); r.go_chapter(n - 1, cx); });
            });
            step!("u08-book-last-chapter-end-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            let end = this.read_with(cx, |s, cx| s.reader.read(cx).chapter_after()).ok().flatten();
            let _ = writeln!(log, "u08\tchapter_after={end:?}");
            this.update_in(cx, |s, window, cx| {
                s.open_settings(window, cx);
                if let Some((_, settings)) = &s.settings {
                    settings.update(cx, |settings, cx| { settings.page = memex_settings::view::Page::Appearance; settings.choose_note_font(memex_client::NoteFont::Kai, cx); });
                }
            }).ok();
            wait(settle).await;
            let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
            if let Some(h) = handle {
                let saved = cx.update_window(h, |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.draw(cx).clear(cx);
                    window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(dir.join("u09-settings-note-font.png")).map_err(|e| e.to_string()))
                });
                let font = this.read_with(cx, |s, _| s.prefs.note_font).ok();
                let _ = writeln!(log, "u09\tnote_font={font:?}\tsaved={:?}", saved.map_err(|e| e.to_string()).and_then(|r| r).err());
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if agent {
            step!("a01-agent-reading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                let id = row_id(this, 2, cx);
                if !id.is_empty() {
                    this.select(id, false, cx);
                }
            });
            step!("a02-agent-citation-jump", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                // A citation to the last paragraph, read back through context_read.
                if let Some(d) = this.reader.read(cx).model.detail().cloned() {
                    let md = &d.canonical.markdown;
                    let start = md.trim_end().rfind("\n").map(|i| i + 1).unwrap_or(0);
                    let end = md[start..].char_indices().nth(10).map(|(i, _)| start + i).unwrap_or(md.len());
                    let c = memex_client::Citation { label: "末段".into(), item_id: d.canonical.item_id.clone(), revision_id: d.canonical.revision_id.clone(), start_byte: start, end_byte: end };
                    this.reader.update(cx, |r, cx| r.jump(c, cx));
                }
            });
            step!("a03-agent-classic-last-row", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Classic, cx);
                let last = this.library.read(cx).model.len().saturating_sub(1);
                let table = this.table.read(cx).table.clone();
                table.update(cx, |t, cx| t.scroll_to_row(last, cx));
            });
            step!("a04-agent-saved", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let title = this.reader.read(cx).model.detail().map(|d| format!("{}（GPUI 保存）", d.canonical.title)).unwrap_or_default();
                this.metadata.update(cx, |m, cx| {
                    m.edit_title(&title, cx);
                    m.save(cx);
                });
            });
            step!("a05-agent-old-writer-conflict", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.metadata.update(cx, |m, cx| m.edit_title("GPUI 本地草稿（未保存）", cx));
                let id = this.metadata.read(cx).item_id().unwrap_or_default().to_string();
                let rev = this.metadata.read(cx).session.revision_id().unwrap_or_default().to_string();
                if let (Some(helper), Some(root)) = (std::env::var_os("MEMEX_DEV_OLD_WRITER"), std::env::var_os("MEMEX_DEV_OLD_WRITER_ROOT")) {
                    let out = std::process::Command::new(helper).arg("update").arg(root).args([&id, &rev, "合成外部修改标题 🧭"]).output();
                    this.notice = Some(txf("开发模式：旧 writer 已改写当前条目（{0}）", &[&out.map(|o| o.status.success()).unwrap_or(false)]));
                }
                this.metadata.update(cx, |m, cx| m.save(cx));
            });
            step!("a06-agent-navigation-guard", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = row_id(this, 5, cx);
                this.select(id, false, cx);
            });
            step!("a07-agent-stale-view-refreshed", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                // Cancel reloads the item and the reader; evidence for refresh only
                // (last-row reach is shown by a03, not here).
                this.metadata.update(cx, |m, cx| m.cancel(cx));
                this.reader.update(cx, |r, cx| r.reload(cx));
            });
            step!("a08-agent-reading-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.set_view(ViewMode::Reading, cx);
            });
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Reader modes, 翻译此文 and group editing (synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=bilingual` runs these states instead of the full walk.
        // Clears the fixture's folders, so the full walk below does not run after it.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("bilingual") {
            let lang_start = this.read_with(cx, |s, _| s.prefs.language).unwrap_or_default();
            use memex_library::edit::Draft;
            use memex_reader::model::Mode;
            let mode = |this: &mut Shell, m: Mode, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(m, cx));
            step!("b01-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select("item_fixture_000005".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            step!("b02-bilingual-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("b03-translation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                mode(this, Mode::Translation, cx);
            });
            step!("b03b-mismatch-falls-back-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                mode(this, Mode::Bilingual, cx);
                this.select("item_fixture_000006".into(), false, cx);
            });
            let provider = |this: &mut Shell, available: bool, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_translation_provider: !available, ..f.controls() });
                }
                this.client.forget_capabilities();
                this.reader.update(cx, |r, cx| r.probe_capability(cx));
            };
            step!("b03c-no-model-configured-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                provider(this, false, cx);
                mode(this, Mode::Original, cx);
                this.select("item_fixture_000002".into(), false, cx);
            });
            step!("b04-original-translate-button-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| provider(this, true, cx));
            step!("b05-translating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.request_translation(cx)));
            wait(6000).await;
            step!("b06-translated-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| mode(this, Mode::Bilingual, cx));
            step!("b07a-chinese-before-request-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select("item_fixture_000008".into(), false, cx));
            step!("b07-chinese-source-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.request_translation(cx)));
            step!("b07b-failed-with-reason-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                mode(this, Mode::Original, cx);
                this.select("item_fixture_000014".into(), false, cx);
            });
            step!("b08-groups-empty-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.clear_groups();
                }
                this.library.update(cx, |l, cx| l.reload(cx));
            });
            step!("b09-groups-new-field-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| t.start_edit(Draft::NewRoot, window, cx));
            });
            step!("b10-groups-root-created-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| t.submit_name(if en { "Reading club" } else { "读书会" }, window, cx));
            });
            step!("b11-groups-subfolder-field-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let root = this.library.read(cx).model.groups().first().map(|g| g.id.clone()).unwrap_or_default();
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| t.start_edit(Draft::NewChild { parent_id: root }, window, cx));
            });
            step!("b12-groups-with-subfolder-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let tree = this.sidebar.read(cx).tree();
                tree.update(cx, |t, cx| t.submit_name(if en { "Issue 1" } else { "第一期" }, window, cx));
            });
            step!("b13-add-to-group-picker-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = this.metadata.read(cx).item_id().unwrap_or_default().to_string();
                this.library.update(cx, |l, cx| {
                    l.open_picker(id, point(px(1180.), px(120.)), cx);
                    l.choose_picker(cx);
                });
            });
            step!("b14-added-to-group-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let id = this.metadata.read(cx).item_id().unwrap_or_default().to_string();
                let sub = this.library.read(cx).model.groups().iter().find(|g| g.parent_id.is_some()).map(|g| g.id.clone()).unwrap_or_default();
                this.library.update(cx, |l, cx| {
                    l.close_picker(cx);
                    l.edit_groups(memex_client::GroupEditOp::AddMembers { group_id: sub, item_ids: vec![id] }, cx);
                });
            });
            step!("b15-bilingual-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_language(LanguagePref::En, cx);
                this.select("item_fixture_000005".into(), false, cx);
                mode(this, Mode::Bilingual, cx);
            });
            this.update(cx, |this, cx| this.set_language(lang_start, cx)).ok();
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Translation progress and the recoverable trash (synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=progress` runs these states instead of the full walk.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("progress") {
            use memex_client::fixture::PARTIAL_TRANSLATION_ITEM;
            use memex_reader::model::Mode;
            macro_rules! act {
                ($body:expr) => {{
                    this.update_in(cx, |this, window, cx| {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(this, window, cx);
                        window.refresh();
                    })
                    .ok();
                }};
            }
            let lang_start = this.read_with(cx, |s, _| s.prefs.language).unwrap_or_default();
            this.update(cx, |this, cx| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_item_chat: true, ..f.controls() });
                }
                this.refresh_capabilities(cx);
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(PARTIAL_TRANSLATION_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            })
            .ok();
            wait(settle).await;
            step!("p01-failed-with-progress-retry-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!("p02-failed-progress-side-panel-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
            step!("p03-translating-progress-chip-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.reader.update(cx, |r, cx| r.request_translation(cx));
            });
            wait(2500).await;
            step!("p04-translating-progress-later-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            wait(9000).await;
            step!("p05-translation-resumed-done-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            step!("p06-sidebar-trash-count-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
            step!("p07-trash-list-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.library.update(cx, |l, cx| l.set_show_trash(true, cx)));
            step!("p08-trash-list-reading-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.set_view(ViewMode::Reading, cx);
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
                let first = this.library.read(cx).trash.entries.first().map(|e| e.item_id.clone());
                if let Some(id) = first {
                    this.library.update(cx, |l, cx| l.restore_from_trash(&id, cx));
                }
            });
            wait(settle).await;
            step!("p09-after-restore-classic-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!("p10-trashed-item-note-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                let trashed = this.library.read(cx).trash.entries.first().map(|e| (e.item_id.clone(), e.title.clone()));
                this.library.update(cx, |l, cx| l.set_show_trash(false, cx));
                this.set_view(ViewMode::Reading, cx);
                if let Some((id, title)) = trashed {
                    // Development only: the UI never offers a trashed item; open it directly.
                    this.reader.update(cx, |r, cx| r.open(&id, &title, true, cx));
                }
            });
            step!("p11-trash-list-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_language(LanguagePref::En, cx);
                this.set_view(ViewMode::Classic, cx);
                this.library.update(cx, |l, cx| l.set_show_trash(true, cx));
            });
            this.update(cx, |this, cx| this.set_language(lang_start, cx)).ok();
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Larger 纪要 reading and the reading typeface (synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=reading` runs these states instead of the full walk.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("reading") {
            use memex_client::fixture::SHOWCASE_ITEM;
            use memex_reader::model::Mode;
            let font_start = memex_client::reading_font();
            step!("r01-minutes-card-preview-sans-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_reading_font(memex_client::ReadingFont::Sans, cx);
                    r.set_mode(Mode::Bilingual, cx);
                    r.set_minutes_collapsed(false, cx);
                    r.set_minutes_full(false, cx);
                });
            });
            step!("r02-minutes-card-show-all-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_minutes_full(true, cx)));
            step!("r03-minutes-full-view-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.open_minutes(cx)));
            step!("r04-minutes-full-view-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("r05-minutes-full-view-serif-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.set_reading_font(memex_client::ReadingFont::Serif, cx));
            });
            step!("r06-text-serif-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| {
                    r.set_insight_view(memex_reader::insights::View::Text, cx);
                    r.set_minutes_full(false, cx);
                });
            });
            step!("r07-text-sans-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_reading_font(memex_client::ReadingFont::Sans, cx)));
            step!("r08-side-panel-read-enlarged-classic", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| { this.set_view(ViewMode::Classic, cx); this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)); });
            step!("r09-full-view-after-classic", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.reader.update(cx, |r, cx| r.open_minutes(cx));
            });
            this.update_in(cx, |this, _, cx| this.reader.update(cx, |r, cx| r.set_reading_font(font_start, cx))).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Real input paths on the live window (synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=input`. Mouse events and keystrokes go through the
        // window's own dispatch (hit-testing, drag, keymap); IME composition through
        // a PlatformInputHandler on the composer, as macOS's NSTextInputClient calls.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("input") {
            use memex_client::fixture::{MINUTES_FAIL_ITEM, SHOWCASE_ITEM};
            use memex_reader::model::Mode;
            macro_rules! act {
                ($body:expr) => {{
                    this.update_in(cx, |this, window, cx| {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(this, window, cx);
                        window.refresh();
                    })
                    .ok();
                }};
            }
            let mut notes = String::new();
            let layout_file = std::env::var_os("MEMEX_DESKTOP_STATE").map(PathBuf::from);
            let saved = |key: &str| -> String {
                layout_file
                    .as_ref()
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .map(|v| v[key].to_string())
                    .unwrap_or_else(|| "(no file)".into())
            };
            let input = |cx: &mut AsyncWindowContext, event: PlatformInput| {
                cx.update(|window, cx| {
                    window.dispatch_event(event, cx);
                })
                .ok();
            };
            let mods = Modifiers::default();
            let shares = |cx: &mut AsyncWindowContext| {
                this.read_with(cx, |s, cx| {
                    let st = s.center_split.read(cx);
                    let sizes = st.sizes().clone();
                    format!("reader={:?} chat={:?} container={:?}", sizes.first(), sizes.get(1), st.container_size())
                })
                .unwrap_or_default()
            };
            step!("u01-conversation-before-drag-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Bilingual, cx);
                    r.set_minutes_full(false, cx);
                });
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, cx| {
                c.send(if en { "What is the main argument?" } else { "这篇文章的核心观点是什么？" }, cx);
            }));
            wait(4000).await;
            step!("u01b-conversation-before-drag-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "before\t{}\tcenter_split={}", shares(cx), saved("center_split"));

            // 1. Divider drag: press on the grip, move with the button held, release.
            let x = px(560.);
            let top = this.read_with(cx, |s, _| s.conversation_top.get()).unwrap_or_default();
            input(cx, PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: point(x, top), modifiers: mods, click_count: 1, first_mouse: false }));
            for dy in [10., 40., 80., 120., 150.] {
                input(cx, PlatformInput::MouseMove(MouseMoveEvent { position: point(x, top - px(dy)), pressed_button: Some(MouseButton::Left), modifiers: mods }));
            }
            step!("u02-divider-dragging-button-held-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "held\t{}\tcenter_split={}", shares(cx), saved("center_split"));
            input(cx, PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: point(x, top - px(150.)), modifiers: mods, click_count: 1 }));
            step!("u03-divider-released-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "released\t{}\tcenter_split={}", shares(cx), saved("center_split"));
            for (name, to) in [("u04-divider-past-top-reader-min-light", 0.), ("u05-divider-past-bottom-chat-min-light", 2000.)] {
                let top = this.read_with(cx, |s, _| s.conversation_top.get()).unwrap_or_default();
                input(cx, PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: point(x, top), modifiers: mods, click_count: 1, first_mouse: false }));
                for i in 1..=6 {
                    let y = top + (px(to) - top) * (i as f32 / 6.);
                    input(cx, PlatformInput::MouseMove(MouseMoveEvent { position: point(x, y), pressed_button: Some(MouseButton::Left), modifiers: mods }));
                }
                input(cx, PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: point(x, px(to)), modifiers: mods, click_count: 1 }));
                input(cx, PlatformInput::MouseMove(MouseMoveEvent { position: point(px(20.), px(20.)), pressed_button: None, modifiers: mods }));
                step!(name, |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                let _ = writeln!(notes, "{name}\t{}\tcenter_split={}", shares(cx), saved("center_split"));
            }

            // 2. ⌘J through the window's keymap.
            for name in ["u06-cmd-j-folded-light", "u07-cmd-j-unfolded-light"] {
                let handled = cx.update(|window, cx| window.dispatch_keystroke(Keystroke::parse("cmd-j").unwrap(), cx)).unwrap_or(false);
                step!(name, |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                let collapsed = this.read_with(cx, |s, _| s.prefs.conversation_collapsed).unwrap_or_default();
                let _ = writeln!(notes, "{name}\thandled={handled}\tcollapsed={collapsed}\tsaved={}", saved("conversation_collapsed"));
            }

            // 4. IME: pinyin marked in the composer, Enter declined by the IME, commit, send.
            let composer = this.read_with(cx, |s, cx| s.chat.read(cx).composer().clone()).unwrap();
            cx.update(|window, cx| composer.update(cx, |c, cx| c.focus(window, cx))).ok();
            wait(300).await;
            cx.update(|window, cx| {
                window.dispatch_keystroke(Keystroke::parse("Q").unwrap(), cx);
            })
            .ok();
            let mut ime = cx.update(|window, cx| PlatformInputHandler::new(window.to_async(cx), Box::new(ElementInputHandler::new(Bounds::default(), composer.clone())))).unwrap();
            ime.replace_and_mark_text_in_range(None, "ni'hao", Some(6..6));
            step!("u08-ime-pinyin-marked-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let value = |cx: &mut AsyncWindowContext| cx.update(|_, cx| composer.read(cx).value().to_string()).unwrap_or_default();
            let questions = |cx: &mut AsyncWindowContext| this.read_with(cx, |s, cx| s.chat.read(cx).model.question_count()).unwrap_or_default();
            let _ = writeln!(notes, "marked\tvalue={:?}\tmarked={:?}\tquestions={}", value(cx), ime.marked_text_range(), questions(cx));
            input(cx, PlatformInput::KeyDown(KeyDownEvent { keystroke: Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false }));
            step!("u09-ime-enter-while-composing-not-sent-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "enter-composing\tvalue={:?}\tmarked={:?}\tquestions={}", value(cx), ime.marked_text_range(), questions(cx));
            ime.replace_text_in_range(None, "你好");
            step!("u10-ime-committed-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "committed\tvalue={:?}\tmarked={:?}\tquestions={}", value(cx), ime.marked_text_range(), questions(cx));
            input(cx, PlatformInput::KeyDown(KeyDownEvent { keystroke: Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false }));
            wait(3500).await;
            step!("u11-ime-enter-sent-committed-text-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            let last = this.read_with(cx, |s, cx| s.chat.read(cx).model.transcript().iter().rev().find(|m| m.speaker == memex_chat::model::Speaker::User).map(|m| m.text.clone())).unwrap_or_default();
            let _ = writeln!(notes, "sent\tvalue={:?}\tquestions={}\tlast_question={last:?}", value(cx), questions(cx));

            // 3. 纪要 button, clicked with dispatched mouse events.
            let click_minutes = |cx: &mut AsyncWindowContext| -> Option<Point<Pixels>> {
                let at = this.read_with(cx, |s, cx| s.reader.read(cx).minutes_button_bounds()).ok().flatten()?.center();
                for down in [true, false] {
                    let event = if down {
                        PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: 1, first_mouse: false })
                    } else {
                        PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: 1 })
                    };
                    cx.update(|window, cx| {
                        window.dispatch_event(event, cx);
                    })
                    .ok();
                }
                Some(at)
            };
            let button = |cx: &mut AsyncWindowContext| this.read_with(cx, |s, cx| s.reader.read(cx).translate.minutes_button().map(|b| b.label())).ok().flatten();
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.chat.update(cx, |c, cx| c.confirm_clear(cx));
                this.select("item_fixture_000008".into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            wait(settle).await;
            step!("u12-minutes-not-generated-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let _ = writeln!(notes, "u12\tbutton={:?}", button(cx));
            // Waits (up to ~15 s) until the button leaves 生成中; the fixture's
            // 5 s generation plus the poll backoff makes fixed sleeps unreliable.
            let terminal = |s: Option<memex_reader::insights::ButtonState>| !matches!(s, Some(memex_reader::insights::ButtonState::Generating));
            let state = |cx: &mut AsyncWindowContext| this.read_with(cx, |s, cx| s.reader.read(cx).translate.minutes_button()).ok().flatten();
            for (name, ms) in [("u13-minutes-clicked-generating-light", 900), ("u14-minutes-done-light", 0), ("u15-minutes-clicked-hidden-light", 0), ("u16-minutes-clicked-shown-light", 0)] {
                let at = if name.contains("done") { None } else { click_minutes(cx) };
                wait(ms).await;
                if name.contains("done") {
                    for _ in 0..60 {
                        if terminal(state(cx)) { break; }
                        wait(250).await;
                    }
                }
                step!(name, |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                let hidden = this.read_with(cx, |s, cx| s.reader.read(cx).minutes_card_hidden()).unwrap_or_default();
                let _ = writeln!(notes, "{name}\tclicked_at={at:?}\tbutton={:?}\tcard_hidden={hidden}", button(cx));
            }
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(MINUTES_FAIL_ITEM.into(), false, cx));
            wait(settle).await;
            for (name, ms) in [("u17-minutes-clicked-then-failed-light", 900), ("u18-minutes-retry-clicked-generating-light", 900), ("u19-minutes-retry-done-light", 0)] {
                let at = if name.contains("done") { None } else { click_minutes(cx) };
                wait(ms).await;
                if !name.contains("generating") {
                    for _ in 0..60 {
                        if terminal(state(cx)) { break; }
                        wait(250).await;
                    }
                }
                step!(name, |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
                let _ = writeln!(notes, "{name}\tclicked_at={at:?}\tbutton={:?}", button(cx));
            }
            std::fs::write(dir.join("input.tsv"), &notes).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Manual AI 纪要 (`minutes_request_v1`, synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=manual`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("manual") {
            use memex_client::fixture::MINUTES_FAIL_ITEM;
            use memex_reader::model::Mode;
            macro_rules! act {
                ($body:expr) => {{
                    this.update_in(cx, |this, window, cx| {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(this, window, cx);
                        window.refresh();
                    })
                    .ok();
                }};
            }
            const CHINESE: &str = "item_fixture_000008";
            step!("m01-chinese-minutes-not-generated-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(CHINESE.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Original, cx);
                    r.set_insight_view(memex_reader::insights::View::Text, cx);
                    r.set_minutes_collapsed(false, cx);
                    r.set_minutes_full(false, cx);
                });
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_minutes(cx)));
            wait(900).await;
            step!("m02-minutes-generating-card-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            wait(5500).await;
            step!("m03-minutes-done-card-shown-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!("m04-minutes-card-hidden-by-button-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_minutes(cx)));
            step!("m05-minutes-card-shown-again-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.press_minutes(cx));
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select(MINUTES_FAIL_ITEM.into(), false, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            wait(settle).await;
            step!("m06-side-panel-not-generated-with-generate-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
            // What the panel's 生成 does (MetadataEvent::GenerateMinutes).
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.request_minutes(cx)));
            wait(1500).await;
            step!("m07-side-panel-generating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
            wait(5000).await;
            step!("m08-minutes-failed-retry-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Reading, cx));
            step!("m09-minutes-retry-generating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_minutes(cx)));
            wait(5500).await;
            step!("m10-minutes-retry-done-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_minutes_provider: true, ..f.controls() });
                }
                this.select("item_fixture_000002".into(), false, cx);
            });
            wait(settle).await;
            step!("m11-minutes-no-provider-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_minutes(cx)));
            step!("m12-minutes-no-provider-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_minutes_provider: false, no_minutes_request: true, ..f.controls() });
                }
                this.refresh_capabilities(cx);
                this.select("item_fixture_000014".into(), false, cx);
            });
            wait(settle).await;
            step!("m13-old-agent-no-minutes-button-chinese-offer-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                // Another Chinese item whose minutes were never asked for.
                let model = &this.library.read(cx).model;
                let other = (0..model.len())
                    .filter_map(|i| model.row(i).cloned())
                    .find(|r| r.item_id != CHINESE && r.item_id != "item_fixture_000020" && memex_reader::translate::looks_chinese(&r.title, ""))
                    .map(|r| r.item_id);
                if let Some(id) = other {
                    this.select(id, false, cx);
                }
            });
            act!(|this: &mut Shell, _: &mut Window, _cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_minutes_request: false, ..f.controls() });
                }
            });
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Reader / conversation split: clipping, scrolling and the conversation pane
        // (synthetic fixture only): `MEMEX_DESKTOP_WALK=layout`.
        // The chat's own surface against the article, both themes:
        // `MEMEX_DESKTOP_WALK=surface`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("surface") {
            use memex_client::fixture::SHOWCASE_ITEM;
            use memex_reader::model::Mode;
            for (name, dark) in [("s01-idle-input-light", false), ("s02-idle-input-dark", true)] {
                step!(name, |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    this.set_view(ViewMode::Reading, cx);
                    this.set_conversation_collapsed(false, cx);
                    this.select(SHOWCASE_ITEM.into(), false, cx);
                    this.reader.update(cx, |r, cx| {
                        r.set_mode(Mode::Bilingual, cx);
                        r.set_insight_view(memex_reader::insights::View::Text, cx);
                        r.set_minutes_full(true, cx);
                    });
                });
            }
            this.update_in(cx, |this, window, cx| {
                this.set_appearance(Appearance::Light, cx);
                this.chat.update(cx, |c, cx| c.send(if en { "What is the article's core argument?" } else { "这篇文章的核心论证是什么？" }, cx));
                window.refresh();
            })
            .ok();
            wait(4000).await;
            step!("s03-split-reply-minutes-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            step!("s04-split-reply-minutes-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("s05-folded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_conversation_collapsed(true, cx);
            });
            step!("s06-folded-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Grouped 来源, foldable sections and the list 排序 menu (synthetic fixture
        // only): `MEMEX_DESKTOP_WALK=sources`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("sources") {
            let click = |window: &mut Window, cx: &mut App, at: Point<Pixels>| {
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }), cx);
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: 1 }), cx);
            };
            step!("g01-sources-grouped-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                if let Some(f) = &this.fixture {
                    f.vary_sources();
                }
                this.library.update(cx, |l, cx| l.reload(cx));
            });
            step!("g02-site-group-open-others-open-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.sidebar.update(cx, |s, cx| {
                    s.toggle_group("36kr.com", cx);
                    s.toggle_section("其他来源", cx);
                });
            });
            step!("g03-sub-site-selected-types-folded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.library.update(cx, |l, cx| l.set_filter(Filter::Source("https://m.36kr.com".into()), cx));
                this.sidebar.update(cx, |s, cx| {
                    s.toggle_section("其他来源", cx);
                    s.toggle_section("类型", cx);
                });
            });
            step!("g04-sources-grouped-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.library.update(cx, |l, cx| l.set_filter(Filter::All, cx));
            });
            step!("g05-sources-section-folded-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.sidebar.update(cx, |s, cx| s.toggle_section("来源", cx)));
            // The 排序 menu, opened with a real click on its button (right end of the list's search row).
            this.update_in(cx, |this, window, cx| {
                this.set_appearance(Appearance::Light, cx);
                this.sidebar.update(cx, |s, cx| s.toggle_section("来源", cx));
                window.refresh();
            })
            .ok();
            wait(settle).await;
            let at = cx.update(|window, _| point(window.viewport_size().width - px(40.), px(84.))).unwrap_or_default();
            cx.update(|window, cx| click(window, cx, at)).ok();
            step!("g06-sort-menu-open-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            cx.update(|window, cx| window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx)).ok();
            step!("g07-sorted-by-title-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.library.update(cx, |l, cx| l.set_sort(memex_client::SortKey::Title, false, cx)));
            cx.update(|window, cx| click(window, cx, at)).ok();
            step!("g08-sort-menu-open-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            let sort = this.read_with(cx, |s, cx| format!("{:?}", s.library.read(cx).model.sort())).unwrap_or_default();
            let _ = writeln!(log, "final_sort\t{sort}\tclicked_at={at:?}");
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Markdown in the reader (原文 / 对照), the AI 纪要 card and a chat reply
        // (synthetic fixture only): `MEMEX_DESKTOP_WALK=markdown`.
        // A paired book through reader_v1 (synthetic fixture): 原文 = English edition,
        // 译文 = Chinese edition, 对照 = the two chapters side by side, the chapter
        // list and a chapter that only the translation has: `MEMEX_DESKTOP_WALK=book`.
        // The AI chat folds away (default) to the reader's stats line and a centered
        // AI button; unfolding keeps the draft: `MEMEX_DESKTOP_WALK=aifold`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("aifold") {
            use memex_client::fixture::{BOOK_ITEM, CHAT_HISTORY_ITEM};
            let draft = |this: &Shell, cx: &App| this.chat.read(cx).composer().read(cx).value().to_string();
            step!("f01-default-folded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(BOOK_ITEM.into(), false, cx);
            });
            let folded = this.read_with(cx, |s, _| s.prefs.conversation_collapsed).unwrap_or_default();
            let _ = writeln!(log, "f01\tdefault_folded={folded}");
            step!("f02-unfolded-with-draft-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：折叠后应保留", window, cx));
            });
            step!("f03-folded-again-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_conversation_collapsed(true, cx));
            let kept = this.read_with(cx, |s, cx| draft(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "f03\tdraft_while_folded={kept}");
            step!("f04-unfolded-draft-kept-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_conversation_collapsed(false, cx));
            let kept = this.read_with(cx, |s, cx| draft(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "f04\tdraft_after_unfold={kept}");
            step!("f05-history-folded-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(CHAT_HISTORY_ITEM.into(), false, cx);
            });
            step!("f06-history-unfolded-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_conversation_collapsed(false, cx));
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Book 对照 by paragraph index (synthetic fixture, frozen 2026-11-04 seam):
        // 段落 pairs with n:1, 1:n and a note, a partial chapter, 并排 following at
        // matched rows only, and 整理本章段落 through a new revision and a failure:
        // `MEMEX_DESKTOP_WALK=bookalign`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("bookalign") {
            use memex_client::fixture::BOOK_ITEM;
            use memex_reader::model::Mode;
            let tops = |this: &Shell, cx: &App| this.reader.read(cx).column_tops();
            step!("a01-ch1-pairs-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(BOOK_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| { r.set_mode(Mode::Bilingual, cx); r.set_book_parallel(false, cx); });
            });
            // An unpaired chapter keeps the 段落 / 并排 switch: 段落 disabled, 并排 shown.
            step!("a00-unpaired-switch-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(0, cx)));
            step!("a01-ch1-pairs-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(1, cx)));
            step!("a02-ch1-pairs-note-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            step!("a03-ch2-partial-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(2, cx)));
            step!("a03b-ch2-partial-unresolved-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            step!("a04-ch1-parallel-top-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_book_parallel(true, cx); r.go_chapter(1, cx); }));
            // Original block 4 (n:1 → the row o4 ↔ t3+t4): the translation follows to block 3.
            step!("a05-ch1-parallel-synced-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_column(false, 4, cx)));
            let t = this.read_with(cx, |s, cx| tops(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "a05\tcolumn_tops={t:?}\texpect=(4,3)");
            step!("a06-ch2-parallel-unresolved-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(2, cx)));
            step!("a06-ch2-parallel-unresolved-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_column(false, 5, cx)));
            let t = this.read_with(cx, |s, cx| tops(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "a06a\tcolumn_tops={t:?}\texpect=(5,5)");
            // Into the unresolved original rows: the translation stays at the last match.
            step!("a06-ch2-parallel-unresolved-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_column(false, 8, cx)));
            let t = this.read_with(cx, |s, cx| tops(s, cx)).unwrap_or_default();
            let _ = writeln!(log, "a06b\tcolumn_tops={t:?}\texpect=(8,5)");
            step!("a07-ch3-missing-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| { r.set_book_parallel(false, cx); r.go_chapter(3, cx); }));
            step!("a08-ch3-confirm-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.ask_repair(cx)));
            let rev0 = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| b.doc.revision_id.clone())).ok().flatten();
            step!("a09-ch3-running-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.start_repair(cx)));
            wait(4500).await;
            step!("a10-ch3-done-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let rev1 = this.read_with(cx, |s, cx| s.reader.read(cx).model.book().map(|b| (b.doc.revision_id.clone(), b.at))).ok().flatten();
            let meta10 = this.read_with(cx, |s, cx| s.metadata.read(cx).session.revision_id().map(str::to_string)).ok().flatten();
            let _ = writeln!(log, "a10\trevision_before={rev0:?}\tafter={rev1:?}\tdetails_rev={meta10:?}");
            // Drafts across a repair that publishes a new revision: the AI composer
            // draft and an unsaved item-details title must survive the reload.
            step!("a13-ch3-drafts-before-repair-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_conversation_collapsed(false, cx);
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("草稿：整理期间应保留", window, cx));
                this.metadata.update(cx, |m, cx| m.edit_title("本地标题草稿（未保存）", cx));
                this.reader.update(cx, |r, cx| { r.go_chapter(3, cx); r.ask_repair(cx); r.start_repair(cx); });
            });
            wait(4500).await;
            step!("a14-ch3-drafts-after-repair-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            let after = this.read_with(cx, |s, cx| {
                let m = s.metadata.read(cx);
                (s.reader.read(cx).model.book().map(|b| b.doc.revision_id.clone()), s.chat.read(cx).composer().read(cx).value().to_string(),
                 m.session.title.clone(), m.session.is_dirty(), m.session.revision_id().map(str::to_string))
            }).ok();
            let _ = writeln!(log, "a14\treader_rev,composer,meta_title,meta_dirty,meta_rev={after:?}");
            // Saving the stale title draft must meet a conflict and keep the draft.
            step!("a15-ch3-stale-title-save-conflict-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.save(cx)));
            wait(1500).await;
            step!("a15-ch3-stale-title-save-conflict-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
            let saved = this.read_with(cx, |s, cx| { let m = s.metadata.read(cx); (m.session.status(), m.session.title.clone(), m.session.message().map(str::to_string)) }).ok();
            let _ = writeln!(log, "a15\tstatus,title,message={saved:?}");
            this.update_in(cx, |this, window, cx| {
                this.metadata.update(cx, |m, cx| m.cancel(cx));
                let composer = this.chat.read(cx).composer().clone();
                composer.update(cx, |c, cx| c.set_value("", window, cx));
                this.set_conversation_collapsed(true, cx);
            }).ok();
            step!("a11-ch1-pairs-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.go_chapter(1, cx));
            });
            step!("a12-ch2-failed-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture { f.fail_next_repair(); }
                this.reader.update(cx, |r, cx| r.go_chapter(2, cx));
            });
            step!("a12-ch2-failed-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.start_repair(cx)));
            wait(3500).await;
            step!("a12-ch2-failed-dark", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("book") {
            use memex_client::fixture::BOOK_ITEM;
            use memex_reader::model::Mode;
            for dark in [false, true] {
                let suffix = if dark { "dark" } else { "light" };
                step!(format!("b01-preface-translation-only-original-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    this.set_view(ViewMode::Reading, cx);
                    this.set_conversation_collapsed(true, cx);
                    this.select(BOOK_ITEM.into(), false, cx);
                    this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
                });
                step!(format!("b02-chapter1-original-english-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(1, cx)));
                step!(format!("b03-chapter1-translation-chinese-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Translation, cx)));
                step!(format!("b04-chapter1-bilingual-columns-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx)));
                step!(format!("b05-chapter-list-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.toggle_toc(cx)));
                step!(format!("b06-long-chapter3-bilingual-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.go_chapter(3, cx)));
                step!(format!("b07-long-chapter3-end-{suffix}"), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_to_end(cx)));
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("markdown") {
            use memex_client::fixture::MARKDOWN_ITEM;
            use memex_reader::model::Mode;
            for (top, scrolled, dark) in [("m01-original-top-light", "m02-original-lists-table-code-light", false), ("m04-original-top-dark", "m05-original-lists-table-code-dark", true)] {
                step!(top, |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                    this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    this.set_view(ViewMode::Reading, cx);
                    this.set_conversation_collapsed(false, cx);
                    this.select(MARKDOWN_ITEM.into(), false, cx);
                    this.reader.update(cx, |r, cx| {
                        r.set_mode(Mode::Original, cx);
                        r.set_minutes_full(true, cx);
                        r.scroll_rows(0, cx);
                    });
                });
                step!(scrolled, |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.scroll_rows(1 + 5, cx)));
                if !dark {
                    step!("m03-bilingual-lists-table-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| {
                        r.set_mode(Mode::Bilingual, cx);
                        r.scroll_rows(1 + 2, cx);
                    }));
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Original, cx);
                    r.scroll_rows(0, cx);
                });
                this.chat.update(cx, |c, cx| c.send(if en { "Show the structure with Markdown, please." } else { "请用 Markdown 举例说明这篇的结构" }, cx));
                window.refresh();
            })
            .ok();
            wait(4000).await;
            step!("m06-chat-markdown-reply-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            step!("m07-chat-markdown-reply-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Translated titles, highlights v2 on the side being read with the tick rail
        // and 重点导航 list, and the 最近加入资料库 order (2026-11-03 candidates;
        // synthetic fixture only): `MEMEX_DESKTOP_WALK=titles`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("titles") {
            use memex_client::fixture::{ORIGINAL_ONLY_HIGHLIGHTS_ITEM, SHOWCASE_ITEM, STALE_HIGHLIGHTS_ITEM};
            use memex_reader::model::Mode;
            step!("t01-list-translated-titles-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(true, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Translation, cx);
                    r.set_emphasis(true, cx);
                    r.set_minutes_collapsed(true, cx);
                });
            });
            step!("t02-classic-table-translated-titles-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
            step!("t03-translation-side-marks-ticks-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_view(ViewMode::Reading, cx);
                this.reader.update(cx, |r, cx| r.scroll_rows(1, cx));
            });
            step!("t04-bilingual-both-sides-marked-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Bilingual, cx);
                    r.scroll_rows(1, cx);
                });
            });
            step!("t05-highlight-list-open-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| {
                    r.nav_pinned = true;
                    cx.notify();
                });
            });
            step!("t06-original-only-in-translation-note-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| {
                    r.nav_pinned = false;
                    cx.notify();
                });
                this.select(ORIGINAL_ONLY_HIGHLIGHTS_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Translation, cx);
                    r.set_emphasis(true, cx);
                });
            });
            step!("t07-original-only-bilingual-note-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            step!("t08-translation-changed-expired-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.select(STALE_HIGHLIGHTS_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Bilingual, cx);
                    r.set_emphasis(true, cx);
                });
            });
            step!("t09-sorted-by-added-at-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.library.update(cx, |l, cx| l.set_sort(memex_client::SortKey::AddedAt, true, cx));
            });
            step!("t10-added-at-refused-back-to-capture-time-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_added_at_sort: true, ..f.controls() });
                }
                this.library.update(cx, |l, cx| l.set_sort(memex_client::SortKey::AddedAt, true, cx));
            });
            let sort = this.read_with(cx, |s, cx| format!("{:?} rejected={}", s.library.read(cx).model.sort(), s.library.read(cx).model.added_at_rejected)).unwrap_or_default();
            let _ = writeln!(log, "final_sort\t{sort}");
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Settings 「版本」 page (synthetic fixture only): `MEMEX_DESKTOP_WALK=version`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("version") {
            // The agent's version as reported, while it still hashes (null), and from an older agent.
            for (name, dark, hashing, missing) in [("v01-version-tab-light", false, false, false), ("v02-version-tab-dark", true, false, false), ("v03-agent-hash-calculating-light", false, true, false), ("v04-agent-version-not-reported-dark", true, false, true)] {
                this.update_in(cx, |this, window, cx| {
                    if let Some(f) = &this.fixture {
                        f.set_controls(FixtureControls { agent_hashing: hashing, no_agent_version: missing, ..f.controls() });
                    }
                    this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    this.open_settings(window, cx);
                    if let Some((_, e)) = &this.settings {
                        e.update(cx, |s, cx| {
                            s.page = memex_settings::view::Page::Version;
                            s.reload_version(cx);
                            cx.notify();
                        });
                    }
                })
                .ok();
                // Hashing the debug binary takes a moment.
                for _ in 0..60 {
                    let loaded = this.read_with(cx, |s, cx| s.settings.as_ref().is_some_and(|(_, e)| e.read(cx).version_loaded())).unwrap_or(false);
                    if loaded {
                        break;
                    }
                    wait(250).await;
                }
                wait(settle).await;
                let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
                if let Some(h) = handle {
                    let path = dir.join(format!("{name}.png"));
                    let saved = cx.update_window(h, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(&path).map_err(|e| e.to_string()))
                    });
                    let _ = writeln!(log, "{name}\tsaved={:?}", saved.map_err(|e| e.to_string()).and_then(|r| r).err());
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Settings 「更新日志」 page (synthetic fixture only): `MEMEX_DESKTOP_WALK=changelog`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("changelog") {
            for (name, dark) in [("l01-changelog-light", false), ("l02-changelog-dark", true)] {
                this.update_in(cx, |this, window, cx| {
                    this.set_appearance(if dark { Appearance::Dark } else { Appearance::Light }, cx);
                    this.open_settings(window, cx);
                    if let Some((_, e)) = &this.settings {
                        e.update(cx, |s, cx| {
                            s.page = memex_settings::view::Page::Changelog;
                            cx.notify();
                        });
                    }
                })
                .ok();
                wait(settle).await;
                let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
                if let Some(h) = handle {
                    let path = dir.join(format!("{name}.png"));
                    let saved = cx.update_window(h, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(&path).map_err(|e| e.to_string()))
                    });
                    let _ = writeln!(log, "{name}\tsaved={:?}", saved.map_err(|e| e.to_string()).and_then(|r| r).err());
                }
            }
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // Stored conversations (`item_chat_history_v1`, provisional; synthetic
        // fixture only): `MEMEX_DESKTOP_WALK=history`.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("history") {
            use memex_client::fixture::{CHAT_HISTORY_FAIL_ITEM, CHAT_HISTORY_ITEM, SHOWCASE_ITEM};
            let set_slow = |this: &mut Shell, slow: bool| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { slow_history: slow, ..f.controls() });
                }
            };
            step!("h01-history-loading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.set_conversation_collapsed(false, cx);
                set_slow(this, true);
                this.select(CHAT_HISTORY_ITEM.into(), false, cx);
            });
            this.update(cx, |this, _| set_slow(this, false)).ok();
            wait(6000).await;
            step!("h02-history-loaded-split-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_first()));
            step!("h03-history-loaded-split-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            this.update(cx, |this, cx| {
                this.set_appearance(Appearance::Light, cx);
                this.chat.update(cx, |c, cx| c.send("继续：还有哪些局限？", cx));
            })
            .ok();
            wait(4000).await;
            step!("h04-follow-up-with-history-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            step!("h05-history-failed-retry-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(CHAT_HISTORY_FAIL_ITEM.into(), false, cx));
            step!("h06-history-empty-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(SHOWCASE_ITEM.into(), false, cx));
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("layout") {
            use memex_client::fixture::SHOWCASE_ITEM;
            use memex_reader::model::Mode;
            macro_rules! act {
                ($body:expr) => {{
                    this.update_in(cx, |this, window, cx| {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(this, window, cx);
                        window.refresh();
                    })
                    .ok();
                }};
            }
            let question = if en {
                "Could you walk me through the article's main argument step by step, explain which evidence supports each step, and point out where the author's reasoning seems weakest or relies on assumptions the article never states explicitly?"
            } else {
                "能不能一步一步讲清楚这篇文章的核心论证？每一步分别有哪些证据支撑，作者的推理在哪里最薄弱，又有哪些前提是文章里从来没有明确说出来、却默认读者接受的？"
            };
            step!("l01-no-conversation-minutes-full-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Bilingual, cx);
                    r.set_insight_view(memex_reader::insights::View::Text, cx);
                    r.set_minutes_full(true, cx);
                });
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, cx| {
                c.send(question, cx);
            }));
            wait(4000).await;
            step!("l02-split-minutes-full-long-exchange-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            step!("l02b-split-long-question-top-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_first()));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            step!("l03-split-minutes-full-long-exchange-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("l04-split-reader-scrolled-card-away-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.scroll_rows(3, cx));
            });
            step!("l05-split-tiny-reader-25-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.scroll_rows(0, cx));
                this.center_split.update(cx, |s, cx| {
                    let c = s.container_size();
                    s.resize_panel(0, c * 0.25, window, cx);
                });
            });
            step!("l06-split-tiny-reader-25-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            // A real double click on the divider (synthetic mouse events through the
            // window's dispatch, so the handle's own listener resets the share).
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Light, cx));
            wait(400).await;
            // Outside the shell's update: the handle's listener updates the shell itself.
            let at = point(px(560.), this.read_with(cx, |s, _| s.conversation_top.get()).unwrap_or_default());
            cx.update(|window, cx| {
                for count in [1, 2] {
                    window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: count, first_mouse: false }), cx);
                    window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: count }), cx);
                }
                hover(window, cx, point(px(20.), px(20.)));
            })
            .ok();
            step!("l07-divider-double-click-reset-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!("l08-conversation-collapsed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.toggle_conversation(cx));
            step!("l09-conversation-collapsed-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.toggle_conversation(cx);
            });
            wait(settle).await;
            step!("l10-divider-hover-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                let y = this.conversation_top.get();
                hover(window, cx, point(px(560.), y));
            });
            step!("l11-divider-hover-dark", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                let y = this.conversation_top.get();
                hover(window, cx, point(px(560.), y));
            });
            step!("l12-clear-confirm-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                hover(window, cx, point(px(20.), px(20.)));
                this.set_appearance(Appearance::Light, cx);
                this.chat.update(cx, |c, cx| c.ask_clear(cx));
            });
            step!("l13-cleared-no-conversation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, cx| c.confirm_clear(cx)));
            // A question asked while folded unfolds the pane to show its answer.
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_conversation_collapsed(true, cx));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, cx| {
                c.send(if en { "And the limitations?" } else { "局限在哪里？" }, cx);
            }));
            step!("l14-folded-then-asked-unfolds-pending-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            std::fs::write(dir.join("prefs-after.json"), this.read_with(cx, |s, _| serde_json::to_string_pretty(&s.prefs).unwrap_or_default()).unwrap_or_default()).ok();
            std::fs::write(dir.join("states.tsv"), &log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // AI 纪要, item chat and the new side-panel fields (synthetic fixture only):
        // `MEMEX_DESKTOP_WALK=minutes` runs these states instead of the full walk.
        if std::env::var("MEMEX_DESKTOP_WALK").as_deref() == Ok("minutes") {
            use memex_client::fixture::{CHAT_NO_PROVIDER_ITEM, CHAT_UNKNOWN_ITEM, SHOWCASE_ITEM};
            use memex_reader::model::Mode;
            let lang_start = this.read_with(cx, |s, _| s.prefs.language).unwrap_or_default();
            macro_rules! act {
                ($body:expr) => {{
                    this.update_in(cx, |this, window, cx| {
                        #[allow(clippy::redundant_closure_call)]
                        ($body)(this, window, cx);
                        window.refresh();
                    })
                    .ok();
                }};
            }
            let ask = |this: &mut Shell, text: &str, cx: &mut Context<Shell>| {
                this.chat.update(cx, |c, cx| {
                    c.send(text, cx);
                });
            };
            step!("n01-reader-minutes-expanded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Reading, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Bilingual, cx);
                    r.set_minutes_collapsed(false, cx);
                });
            });
            step!("n02-reader-minutes-collapsed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.set_minutes_collapsed(true, cx)));
            step!("n03-reader-minutes-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.reader.update(cx, |r, cx| r.set_minutes_collapsed(false, cx));
            });
            step!("n04-side-panel-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            step!("n04b-side-panel-classic-scrolled-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
            step!("n04c-side-panel-reading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Reading, cx));
            step!("n05-chinese-generate-minutes-button-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select("item_fixture_000008".into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            step!("n06-minutes-generating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.request_translation(cx)));
            wait(6000).await;
            step!("n07-chinese-minutes-done-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            // 重点标注 / 金句 / 事实核查 (synthetic insights; the showcase item has them all).
            use memex_client::insights::InsightKind;
            use memex_reader::insights::View;
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(SHOWCASE_ITEM.into(), false, cx));
            wait(settle).await;
            step!("i01-emphasis-original-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Original, cx);
                    r.set_minutes_collapsed(true, cx);
                    r.set_insight_view(View::Text, cx);
                    r.set_emphasis(true, cx);
                });
            });
            step!("i02-emphasis-original-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("i03-emphasis-bilingual-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            step!("i04-quotes-english-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_insight(InsightKind::Quotes, cx)));
            step!("i05-factcheck-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_insight(InsightKind::FactCheck, cx)));
            step!("i06-factcheck-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_appearance(Appearance::Dark, cx));
            step!("i07-locate-flash-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.reader.update(cx, |r, cx| {
                    if let Some(block) = r.insights.facts().and_then(|f| f.get(2)).map(|f| f.block_index) {
                        r.locate(block, cx);
                    }
                });
            });
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| r.set_emphasis(false, cx));
                this.select("item_fixture_000008".into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Original, cx));
            });
            wait(settle).await;
            step!("i08-chinese-quotes-generating-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_insight(InsightKind::Quotes, cx)));
            wait(6500).await;
            step!("i09-chinese-quotes-done-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_insight_provider: true, ..f.controls() });
                }
                this.select("item_fixture_000009".into(), false, cx);
            });
            wait(settle).await;
            step!("i10-factcheck-no-provider-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.press_insight(InsightKind::FactCheck, cx)));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_insight_provider: false, no_insights: true, ..f.controls() });
                }
                this.refresh_capabilities(cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
            });
            wait(settle).await;
            step!("i11-old-agent-no-insight-buttons-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.reader.update(cx, |r, cx| r.reload(cx)));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if let Some(f) = &this.fixture {
                    f.set_controls(FixtureControls { no_insights: false, ..f.controls() });
                }
                this.refresh_capabilities(cx);
                this.reader.update(cx, |r, cx| r.reload(cx));
            });
            wait(settle).await;
            // The chat follows the reader's loaded item: select, let it load, then ask.
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.select(SHOWCASE_ITEM.into(), false, cx);
                this.reader.update(cx, |r, cx| r.set_mode(Mode::Bilingual, cx));
            });
            wait(settle).await;
            step!("n08-chat-pending-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                ask(this, if en { "What is the main argument of this article?" } else { "这篇文章的核心观点是什么？" }, cx);
            });
            wait(2500).await;
            step!("n09-chat-exchange-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, _| c.scroll_to_latest()));
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(CHAT_NO_PROVIDER_ITEM.into(), false, cx));
            wait(settle).await;
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| ask(this, if en { "Summarize the method section." } else { "帮我概括方法部分。" }, cx));
            wait(3500).await;
            step!("n10-chat-no-provider-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select(CHAT_UNKNOWN_ITEM.into(), false, cx));
            wait(settle).await;
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| ask(this, if en { "Which datasets are used?" } else { "用了哪些数据集？" }, cx));
            wait(3500).await;
            step!("n11-chat-unknown-outcome-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            act!(|this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.chat.update(cx, |c, cx| c.retry(cx)));
            wait(3500).await;
            step!("n12-chat-unknown-retried-light", |_: &mut Shell, _: &mut Window, _: &mut Context<Shell>| {});
            step!("n13-chat-exchange-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Dark, cx);
                this.select(SHOWCASE_ITEM.into(), false, cx);
            });
            step!("n14-side-panel-classic-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.set_appearance(Appearance::Light, cx);
                this.set_language(LanguagePref::En, cx);
                this.set_view(ViewMode::Classic, cx);
            });
            step!("n15-reader-minutes-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Reading, cx));
            step!("i12-emphasis-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                this.reader.update(cx, |r, cx| {
                    r.set_mode(Mode::Original, cx);
                    r.set_emphasis(true, cx);
                });
            });
            this.update(cx, |this, cx| this.set_language(lang_start, cx)).ok();
            std::fs::write(dir.join("states.tsv"), log).ok();
            cx.update(|_, cx| cx.quit()).ok();
            return;
        }

        // The full walk shows the labelled sample transcripts: no item_chat here.
        this.update(cx, |this, cx| {
            if let Some(f) = &this.fixture {
                f.set_controls(FixtureControls { no_item_chat: true, ..f.controls() });
            }
            this.refresh_capabilities(cx);
        })
        .ok();
        wait(settle).await;

        // g2 folder tree and md1 bibliographic inspector (synthetic fixture only).
        let lang_before = this.read_with(cx, |s, _| s.prefs.language).unwrap_or_default();
        step!("g01-tree-direct-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.set_view(ViewMode::Classic, cx);
            this.set_include_subfolders(false, cx);
            this.library.update(cx, |l, cx| l.set_filter(Filter::Group("grp_c_bias".into()), cx));
            this.sidebar.update(cx, |s, cx| s.reveal("grp_c_anchor", cx));
        });
        step!("g02-tree-subfolders-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_include_subfolders(true, cx));
        step!("g03-journal-inspector-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select("item_fixture_000001".into(), false, cx));
        step!("g04-book-inspector-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            let l = this.library.read(cx);
            let book = (0..l.model.len()).filter_map(|i| l.model.row(i)).find(|r| r.item_type == "书籍").map(|r| r.item_id.clone());
            if let Some(id) = book {
                this.select(id, false, cx);
            }
        });
        step!("g05-reading-folder-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Reading, cx));
        step!("g06-classic-unparsed-date-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Dark, cx);
            this.set_view(ViewMode::Classic, cx);
            this.select("item_fixture_000013".into(), false, cx);
        });
        step!("g07-classic-unknown-added-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.set_language(LanguagePref::En, cx);
            this.select("item_fixture_000009".into(), false, cx);
        });
        step!("g08-classic-all-items-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_language(lang_before, cx);
            this.set_include_subfolders(false, cx);
            this.library.update(cx, |l, cx| l.set_filter(Filter::All, cx));
        });
        // Bibliographic editing and 补全预览 (synthetic fixture; no network).
        let biblio = |this: &Shell, cx: &App| this.metadata.read(cx).biblio.clone();
        step!("m01-journal-inspector-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.set_view(ViewMode::Classic, cx);
            this.select("item_fixture_000001".into(), false, cx);
        });
        step!("m02-inspector-editing-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            biblio(this, cx).update(cx, |b, cx| {
                b.dev_set_input("volume", "186", cx);
                let n = b.original().map(|o| o.creators.len()).unwrap_or(0);
                b.add_creator(cx);
                b.dev_set_input(&format!("creator:{n}:family"), "林", cx);
                b.dev_set_input(&format!("creator:{n}:given"), "晓", cx);
                b.set_precision(memex_client::DatePrecision::Month, cx);
            });
        });
        step!("m03-field-error-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.metadata.update(cx, |m, cx| m.cancel(cx));
            // A wrong ISSN check digit: the writer rejects it, shown next to the field.
            biblio(this, cx).update(cx, |b, cx| b.dev_set_input("issn", "0036-8076", cx));
            this.metadata.update(cx, |m, cx| m.save(cx));
        });
        step!("m04-lookup-sheet-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.metadata.update(cx, |m, cx| m.cancel(cx));
            biblio(this, cx).update(cx, |b, cx| b.open_lookup("doi", cx));
        });
        step!("m05-lookup-requires-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            // Creators linked to institutions cannot be applied without them.
            biblio(this, cx).update(cx, |b, cx| {
                let ids = b.dev_candidates(&["institutions", "creators"]);
                for id in ids {
                    b.toggle_candidate(&id, cx);
                }
            });
        });
        step!("m06-lookup-sheet-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Dark, cx);
            biblio(this, cx).update(cx, |b, cx| {
                let ids = b.dev_candidates(&["institutions"]);
                for id in ids {
                    b.toggle_candidate(&id, cx);
                }
            });
        });
        step!("m07-lookup-sheet-en", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.set_language(LanguagePref::En, cx);
        });
        step!("m08-apply-result-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_language(lang_before, cx);
            biblio(this, cx).update(cx, |b, cx| b.apply_lookup(cx));
        });
        step!("m09-stale-conflict-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            biblio(this, cx).update(cx, |b, cx| b.dev_set_input("issue", "4158", cx));
            this.simulate_external_edit(cx);
            this.metadata.update(cx, |m, cx| m.save(cx));
        });
        step!("m09b-stale-conflict-message-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
        step!("m10a-conflict-reloaded-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.notice = None;
            this.metadata.update(cx, |m, cx| m.cancel(cx));
        });
        step!("m10-inspector-scrolled-bottom-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.metadata.update(cx, |m, cx| m.scroll_to_end(cx)));
        step!("m11a-book-inspector-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            let l = this.library.read(cx);
            let book = (0..l.model.len()).filter_map(|i| l.model.row(i)).find(|r| r.item_type == "书籍").map(|r| r.item_id.clone());
            if let Some(id) = book {
                this.select(id, false, cx);
            }
        });
        step!("m11-book-isbn-lookup-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| biblio(this, cx).update(cx, |b, cx| b.open_lookup("isbn", cx)));
        step!("m12-lookup-rate-limited-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(f) = &this.fixture {
                f.set_controls(FixtureControls { lookup_error: Some("lookup_rate_limited".into()), ..f.controls() });
            }
            biblio(this, cx).update(cx, |b, cx| b.open_lookup("isbn", cx));
        });
        step!("m13a-sheet-closed-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(f) = &this.fixture {
                f.set_controls(FixtureControls { lookup_error: None, ..f.controls() });
            }
            biblio(this, cx).update(cx, |b, cx| b.close_lookup(cx));
        });
        // Separate step: the hit test uses the last drawn frame, which must no longer have the sheet.
        step!("m13-row-tooltip-light", |_: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| hover(window, cx, point(px(300.), px(122.))));
        step!("m14a-long-title-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.select("item_fixture_000001".into(), false, cx));
        step!("m14-title-tooltip-light", |_: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| hover(window, cx, point(px(1290.), px(200.))));
        this.update(cx, |this, cx| this.set_view(ViewMode::Reading, cx)).ok();

        step!("01-reading-conversation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            let id = row_id(this, 0, cx);
            this.select(id, false, cx);
        });
        step!("02-reading-no-conversation-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            let id = row_id(this, 1, cx);
            this.select(id, false, cx);
        });
        step!("03-classic-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
        step!("04-returned-to-reading-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Reading, cx));
        step!("04b-scrolled-to-ends-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            // Clipped half-lines must be reachable: last block, last message, last row, action row.
            let id = row_id(this, 0, cx);
            this.select(id, false, cx);
        });
        step!("04c-scrolled-to-ends-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| scroll_all(this, cx));
        this.update(cx, |this, cx| {
            let _ = writeln!(log, "  reach\treader_end={:?}\tchat_latest={}\tlist_end={:?}", this.reader.read(cx).at_end(), this.chat.read(cx).at_latest(), this.list.read(cx).at_end());
        })
        .ok();
        step!("05-reading-conversation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Dark, cx);
            let id = row_id(this, 3, cx);
            this.select(id, false, cx);
        });
        step!("05b-citation-from-translation-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            // Translation offsets are separate; with no mapping the reader jumps to the original.
            this.reader.update(cx, |r, cx| r.set_mode(memex_reader::model::Mode::Translation, cx));
            let cite = this.chat.read(cx).model.transcript().iter().flat_map(|m| m.citations.clone()).next();
            if let Some(c) = cite {
                this.reader.update(cx, |r, cx| r.jump(c, cx));
            }
        });
        step!("06-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
        step!("07-conflict-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.metadata.update(cx, |m, cx| m.edit_title(if en { "Synthetic sample: a title draft edited locally (unsaved)" } else { "合成样例：在本地改过的标题草稿（未保存）" }, cx));
            this.simulate_external_edit(cx);
            this.metadata.update(cx, |m, cx| m.save(cx));
        });
        step!("08-navigation-guard", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            let id = row_id(this, 5, cx);
            this.select(id, false, cx);
        });
        step!("09-offline", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.metadata.update(cx, |m, cx| m.cancel(cx));
            this.set_view(ViewMode::Reading, cx);
            this.set_fixture(|c| c.offline = true, cx);
        });
        step!("10-narrow-reading", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            this.set_fixture(|c| c.offline = false, cx);
            window.resize(size(px(960.), px(640.)));
        });
        step!("11-narrow-classic-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Dark, cx);
            this.set_view(ViewMode::Classic, cx);
        });
        step!("12-narrow-reading-scrolled-ends", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.set_view(ViewMode::Reading, cx);
            scroll_all(this, cx);
        });
        step!("13-narrow-classic-last-row", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_view(ViewMode::Classic, cx);
            let last = this.library.read(cx).model.len().saturating_sub(1);
            let table = this.table.read(cx).table.clone();
            table.update(cx, |t, cx| t.scroll_to_row(last, cx));
            this.metadata.update(cx, |m, cx| m.scroll_to_end(cx));
        });
        // Settings window (s2), synthetic configuration only; no key or network.
        macro_rules! settings_step {
            ($name:expr, $body:expr) => {{
                this.update_in(cx, |this, window, cx| {
                    #[allow(clippy::redundant_closure_call)]
                    ($body)(this, window, cx);
                })
                .ok();
                wait(settle).await;
                let handle = this.read_with(cx, |s, _| s.settings.as_ref().map(|(h, _)| *h)).ok().flatten();
                if let Some(h) = handle {
                    let path = dir.join(format!("{}.png", $name));
                    let saved = cx.update_window(h, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                        window.render_to_image().map_err(|e| e.to_string()).and_then(|i| i.save(&path).map_err(|e| e.to_string()))
                    });
                    let state = this.read_with(cx, |s, cx| s.settings.as_ref().map(|(_, e)| {
                        let m = &e.read(cx).model;
                        format!("rev={:?}\tstatus={:?}\tdirty={}\tmessage={:?}", m.revision(), m.status(), m.is_dirty(), m.message())
                    })).ok().flatten().unwrap_or_default();
                    let _ = writeln!(log, "{}\t{}\tsaved={:?}", $name, state, saved.map_err(|e| e.to_string()).and_then(|r| r).err());
                }
            }};
        }
        let settings = |this: &Shell| this.settings.as_ref().map(|(_, e)| e.clone());
        settings_step!("s01-settings-empty-light", |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            this.open_settings(window, cx);
        });
        settings_step!("s02-settings-draft", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| {
                    s.dev_add_provider(if en { "Local agent (synthetic)" } else { "本机代理（合成）" }, "http://127.0.0.1:18317/v1/", &[("Alpha", "fixture-model-alpha"), ("Beta", "fixture-model-beta"), ("Terra 6.1", "gpt-6.1-terra")], cx);
                    let cards: Vec<String> = s.model.draft().models.iter().map(|m| m.id.clone()).collect();
                    s.model.select(memex_client::Purpose::Chat, cards.first().cloned());
                    s.model.select(memex_client::Purpose::Suggestions, cards.get(1).cloned());
                    s.model.select(memex_client::Purpose::Translation, cards.first().cloned());
                    s.model.set_reasoning_effort(memex_client::Purpose::Translation, Some("low"));
                    s.model.select(memex_client::Purpose::Classification, cards.get(2).cloned());
                });
            }
        });
        settings_step!("s03-settings-saved", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| s.save(cx));
            }
        });
        settings_step!("s04-settings-key-and-discovery", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            // A synthetic key goes straight to the fixture (the UI path is the masked input).
            let client = this.client.clone();
            if let Some(e) = settings(this) {
                let rev = e.read(cx).model.revision().unwrap_or(0);
                let pid = e.read(cx).model.draft().providers.first().map(|p| p.id.clone()).unwrap_or_default();
                let _ = client.replace_provider_credential(rev, &pid, 0, memex_client::Secret::new("synthetic-key-abc".into()));
                e.update(cx, |s, cx| s.reload(cx));
            }
        });
        settings_step!("s05-settings-discovered", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                let pid = e.read(cx).model.draft().providers.first().map(|p| p.id.clone()).unwrap_or_default();
                e.update(cx, |s, cx| s.discover(pid, cx));
            }
        });
        settings_step!("s06-settings-conflict", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            let client = this.client.clone();
            if let Some(e) = settings(this) {
                // Elsewhere: another save advances the revision behind this window.
                let current = client.model_settings().unwrap();
                let mut other = current.configuration.clone();
                other.selections.retain(|s| s.purpose != memex_client::Purpose::Suggestions);
                let _ = client.replace_model_settings(current.revision, other);
                e.update(cx, |s, cx| {
                    let card = s.model.draft().models.last().map(|m| m.id.clone());
                    s.model.select(memex_client::Purpose::Summary, card);
                    s.save(cx);
                });
            }
        });
        settings_step!("s07-settings-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| s.reload(cx));
            }
            this.set_appearance(Appearance::Dark, cx);
        });
        settings_step!("s08-settings-bottom-dark", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| s.scroll_to_end(cx));
            }
        });
        settings_step!("s09-settings-bottom-light", |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            this.set_appearance(Appearance::Light, cx);
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| s.scroll_to_end(cx));
            }
        });
        let reach = this.read_with(cx, |s, cx| s.settings.as_ref().map(|(_, e)| e.read(cx).at_end())).ok().flatten();
        let _ = writeln!(log, "  reach\tsettings_at_end={reach:?}");
        // Interface language: the General page control, then a live switch to the
        // other language through that control (no restart), in both windows.
        let other = if en { LanguagePref::Zh } else { LanguagePref::En };
        let tag = |en: bool| if en { "en" } else { "zh" };
        settings_step!(format!("s10-settings-language-{}", tag(en)), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| {
                    s.page = memex_settings::view::Page::General;
                    cx.notify();
                });
            }
        });
        settings_step!(format!("s11-settings-language-switched-to-{}", tag(!en)), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| s.choose_language(other, cx));
            }
        });
        settings_step!(format!("s12-settings-models-switched-to-{}", tag(!en)), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
            if let Some(e) = settings(this) {
                e.update(cx, |s, cx| {
                    s.page = memex_settings::view::Page::Models;
                    cx.notify();
                });
            }
        });
        step!(format!("14-main-switched-to-{}", tag(!en)), |this: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
            window.resize(size(px(1440.), px(900.)));
            this.set_view(ViewMode::Reading, cx);
        });
        step!(format!("15-classic-switched-to-{}", tag(!en)), |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| this.set_view(ViewMode::Classic, cx));
        let _ = writeln!(log, "  language\tpref={:?}\tresolved={:?}", this.read_with(cx, |s, _| s.prefs.language).ok(), i18n::language());
        std::fs::write(dir.join("states.tsv"), log).ok();
        cx.update(|_, cx| cx.quit()).ok();
    })
    .detach();
}

fn scroll_all(this: &mut Shell, cx: &mut Context<Shell>) {
    this.reader.update(cx, |r, cx| r.scroll_to_end(cx));
    this.chat.update(cx, |c, _| c.scroll_to_latest());
    this.list.update(cx, |l, cx| l.scroll_to_end(cx));
    this.metadata.update(cx, |m, cx| m.scroll_to_end(cx));
}

/// A synthetic pointer move, so hover tooltips can be captured (no OS input).
fn hover(window: &mut Window, cx: &mut App, position: Point<Pixels>) {
    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
}
