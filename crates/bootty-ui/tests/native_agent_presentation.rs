#![cfg(test)]

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::Caller;
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{CommandAction, UiPalette, init_theme},
};
use gpui_kit::{
    AppContext as _, Entity, Focusable as _, Modifiers, MouseButton, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, point, px, size,
};
use std::ops::Add as _;

#[gpui_kit::test]
fn saved_conversation_skips_empty_rows_and_keeps_a_readable_responsive_lane(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    verify_responsive_fold(visual);
    assert!(
        visual.debug_bounds("copy-message:reply").is_some(),
        "a settled turn's reply keeps its actions"
    );
}

fn saved_record(status: &str) -> NativeSessionRecord {
    NativeSessionRecord {
        id: "saved-pi".to_owned(),
        binding_id: "captured-binding".to_owned(),
        task_identity: Some("saved-task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config: NativeSessionConfig::new(AgentKind::Pi, "/tmp/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "pi", "session_id": "provider-session", "turn_id": null,
            "status": status, "requests": [], "usage": null, "error": null,
            "revision": 1,
            "transcript": [
                {"id": "empty", "role": "assistant", "text": "", "complete": true},
                {"id": "cleared-status", "role": "notice", "text": "\u{001b}[39m", "complete": true},
                {"id": "user", "role": "user", "text": "Review this change.", "complete": true},
                {"id": "tool", "role": "tool", "text": "Command output", "complete": true,
                 "tool": {"name": "exec_command", "input": "cargo check", "status": "completed"}},
                {"id": "reply", "role": "assistant", "text": "A readable **Markdown** reply.", "complete": true}
            ]
        })).expect("saved public conversation"),
        side_chat: None,
        spawn_parent: None,
        queued_messages: Vec::new(),
        attachments: Vec::new(),
        draft: None,
        seen_turns: 0,
    }
}

/// Codex exposes stored history only after a fresh thread's first turn settles.
#[gpui_kit::test]
fn fresh_codex_turn_uses_live_content_until_history_is_available(cx: &mut TestAppContext) {
    use bootty_agents::{NativeTurnOutcome, NativeTurnReceipt};
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for (outcome, reads_history) in [
        (Some(NativeTurnOutcome::Running), false),
        (Some(NativeTurnOutcome::Succeeded), true),
        (Some(NativeTurnOutcome::Interrupted), true),
        (None, true), // An existing thread resumed into a new provider process.
    ] {
        let mut record = saved_record("working");
        record.config.provider = AgentKind::Codex;
        record.config.session_id = Some("provider-session".into());
        record.snapshot.provider = AgentKind::Codex;
        record.snapshot.first_turn = outcome.map(|outcome| NativeTurnReceipt {
            id: "first-turn".into(),
            outcome,
        });
        let (sender, calls) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("native-message-reply").is_some(),
            "live content stays visible"
        );
        pretty_assertions::assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.command == "agents.native.history"),
            reads_history,
            "first turn: {outcome:?}",
        );
    }
}

#[gpui_kit::test]
fn restored_provider_history_is_visible_without_scrolling(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("working");
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let inbox = mailbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = inbox.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                let value = if request.invocation.command == "agents.native.history" {
                    serde_json::json!({
                        "transcript": [
                            {"id":"recent-user","role":"user","text":"From the copied conversation, repeat the earlier assistant response exactly, preceded by OLDER_FORK_CONTEXT_OK. Do not use tools, run commands, read or modify files.","complete":true},
                            {"id":"recent-reply","role":"assistant","text":"OLDER_FORK_CONTEXT_OK: A readable response to review.","complete":true}
                        ],
                        "has_older":false,"has_newer":false,"at_latest":true
                    })
                } else {
                    serde_json::json!([])
                };
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let mut source = saved_record("stopped");
        source.id = "source-pi".into();
        for item in &mut source.snapshot.transcript {
            item.id = format!("source-{}", item.id);
        }
        let first = cx.new(|cx| {
            NativeAgentSessionView::new(
                source,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        let second = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        let panes = cx.new(|_| RestoredNativePanes { first, second });
        gpui_kit::component::Root::new(panes, window, cx)
    });
    visual.simulate_resize(size(px(960.), px(790.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let reply = visual
        .debug_bounds("native-message-body-recent-reply")
        .expect("restored latest response");
    assert!(reply.size.height > px(0.));
    assert!(reply.left() >= px(480.), "restored child stays in its pane");
    assert!(
        reply.top() >= px(0.) && reply.bottom() < px(790.),
        "latest response is in the viewport: {reply:?}"
    );
}

struct RestoredNativePanes {
    first: Entity<NativeAgentSessionView>,
    second: Entity<NativeAgentSessionView>,
}

#[gpui_kit::test]
fn side_chat_keeps_copied_parent_messages_when_provider_history_loads(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record
        .snapshot
        .transcript
        .retain(|item| matches!(item.role.as_str(), "user" | "assistant") && !item.text.is_empty());
    for item in &mut record.snapshot.transcript {
        item.id = format!("fork:parent:{}", item.id);
    }
    record.side_chat = Some(bootty_agents::NativeSideChat {
        source_id: "parent".into(),
        boundary: "fork:parent:reply".into(),
        seeded_identity: Some("provider-session".into()),
        transcript: record.snapshot.transcript.clone(),
    });
    // New provider activity can evict every copied item from the recent transcript.
    record.snapshot.transcript.clear();
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let inbox = mailbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = inbox.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                let value = if request.invocation.command == "agents.native.history" {
                    serde_json::json!({"transcript":[
                    {"id":"child-user","role":"user","text":"Follow up","complete":true},
                    {"id":"child-reply","role":"assistant","text":"The child response","complete":true}
                ],"has_older":false,"has_newer":false,"at_latest":true})
                } else {
                    serde_json::json!([])
                };
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let conversation = cx.new(|cx| {
            NativeAgentSessionView::new(
                record.clone(),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(conversation, window, cx)
    });
    visual.simulate_resize(size(px(960.), px(790.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "native-message-body-fork:parent:reply",
        "native-message-body-child-reply",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
}

impl gpui_kit::Render for RestoredNativePanes {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::{ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .flex()
            .child(
                gpui_kit::div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.first.clone()),
            )
            .child(
                gpui_kit::div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.second.clone()),
            )
    }
}

/// A stopped conversation that sent an image and a file, with a tool row that returned a
/// screenshot, and the image's reference as an import reply.
fn restored_attachments(png_bytes: usize) -> (NativeSessionRecord, serde_json::Value) {
    let mut record = saved_record("stopped");
    let reference: bootty_agents::NativeAttachmentReference =
        serde_json::from_value(serde_json::json!({
            "id":"restored-image", "kind":"image", "name":"image.png", "mime_type":"image/png",
            "size_bytes":png_bytes,"pixel_width":500,"pixel_height":500
        }))
        .unwrap();
    record.attachments.push(reference.clone());
    let imported = serde_json::to_value(&reference).unwrap();
    // A screenshot the tool returned belongs to its row.
    let capture = bootty_agents::NativeAttachmentReference {
        id: "tool-image-capture".into(),
        name: "exec_command image 1.png".into(),
        ..reference.clone()
    };
    let notes: bootty_agents::NativeAttachmentReference =
        serde_json::from_value(serde_json::json!({
            "id":"restored-notes", "kind":"file", "name":"notes.md", "mime_type":"text/markdown",
            "size_bytes":9
        }))
        .unwrap();
    record.attachments.push(notes.clone());
    for (item, image) in [("user", reference), ("user", notes), ("tool", capture)] {
        record
            .snapshot
            .transcript
            .iter_mut()
            .find(|row| row.id == item)
            .unwrap()
            .attachments
            .push(image);
    }
    (record, imported)
}

#[gpui_kit::test]
fn restored_attachment_accepts_a_large_valid_encoded_preview(cx: &mut TestAppContext) {
    use base64::Engine as _;
    use bootty_control::CommandOutcome;
    use pretty_assertions::assert_eq;
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut seed = 17_u32;
    let pixels = image::RgbaImage::from_fn(500, 500, |_, _| {
        let mut pixel = [0; 4];
        for channel in &mut pixel {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *channel = seed.to_le_bytes()[3];
        }
        image::Rgba(pixel)
    });
    let mut png = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut png, image::ImageFormat::Png).unwrap();
    assert!(png.get_ref().len() <= 1024 * 1024);
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
    assert!(
        encoded.len() > 1024 * 1024,
        "a valid PNG grows during transport encoding"
    );
    let (record, imported) = restored_attachments(png.get_ref().len());
    let target = record.target();
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let confirmed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let inbox = mailbox.clone();
    let accepted = confirmed.clone();
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let opens = opened.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = inbox.lock().unwrap();
            let receiver = inbox.as_ref().unwrap();
            while let Ok(request) = receiver.try_recv() {
                let value = if request.invocation.command == "agents.native.attachment-preview" {
                    assert_eq!(request.invocation.target, Some(target.clone()));
                    assert_eq!(request.invocation.arguments[..2], ["saved-pi", "1"]);
                    accepted.store(true, std::sync::atomic::Ordering::Relaxed);
                    serde_json::json!({"png":encoded})
                } else if request.invocation.command == "agents.native.import" {
                    imported.clone()
                } else if request.invocation.command == "agents.native.attachment-open" {
                    opens
                        .lock()
                        .unwrap()
                        .push(request.invocation.arguments.clone());
                    serde_json::Value::Null
                } else {
                    serde_json::json!([])
                };
                request
                    .response
                    .send(CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
            drop(inbox);
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    assert!(confirmed.load(std::sync::atomic::Ordering::Relaxed));
    visual.simulate_resize(size(px(1000.), px(800.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let preview = visual
        .debug_bounds("attachment-preview:restored-image")
        .expect("restored preview renders");
    verify_image_viewer(visual, preview);
    // A sent file opens from its chip.
    let chip = visual
        .debug_bounds("sent-attachment:restored-notes")
        .unwrap();
    visual.simulate_click(chip.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        *opened.lock().unwrap(),
        [["saved-pi", "1", "restored-notes"]]
    );
    verify_draft_image_removal(visual, png.get_ref());
}

fn verify_draft_image_removal(visual: &mut gpui_kit::VisualTestContext, png: &[u8]) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("image.png");
    std::fs::write(&path, png).unwrap();
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.update(|_, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::ExternalPaths(
                gpui_kit::ExternalPaths(std::iter::once(path).collect()),
            )],
        });
    });
    visual.simulate_keystrokes("cmd-v");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let remove = visual
        .debug_bounds("remove-preview:restored-image")
        .unwrap();
    visual.simulate_click(remove.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    pretty_assertions::assert_eq!(visual.debug_bounds("remove-preview:restored-image"), None);
    pretty_assertions::assert_eq!(visual.debug_bounds("attachment-image-viewer"), None);
}

fn verify_image_viewer(
    visual: &mut gpui_kit::VisualTestContext,
    preview: gpui_kit::Bounds<gpui_kit::Pixels>,
) {
    visual.simulate_click(preview.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let viewer = visual
        .debug_bounds("attachment-image-viewer")
        .expect("image opens in a viewer");
    assert!(viewer.size.width > preview.size.width);
    assert!(viewer.size.height > preview.size.height);
    let close = visual.debug_bounds("close-image-preview").unwrap();
    assert!(close.bottom() <= viewer.top());
    let image = |visual: &mut gpui_kit::VisualTestContext| {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.debug_bounds("image-viewer-image").unwrap()
    };
    pretty_assertions::assert_eq!(image(visual), viewer);
    // Zoom grows the image inside the same frame, and 0 fits it again.
    visual.simulate_keystrokes("=");
    let zoomed = image(visual);
    assert!(zoomed.size.width > viewer.size.width);
    pretty_assertions::assert_eq!(visual.debug_bounds("attachment-image-viewer"), Some(viewer));
    visual.simulate_keystrokes("0");
    pretty_assertions::assert_eq!(image(visual), viewer);
    // The arrows step through every image the conversation shows, the tool's included.
    assert!(visual.debug_bounds("image-viewer-showing:0").is_some());
    visual.simulate_keystrokes("right");
    image(visual);
    assert!(visual.debug_bounds("image-viewer-showing:1").is_some());
    visual.simulate_keystrokes("right");
    image(visual);
    assert!(visual.debug_bounds("image-viewer-showing:0").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("attachment-image-viewer").is_none());
}

fn verify_responsive_fold(visual: &mut gpui_kit::VisualTestContext) {
    for (width, height) in [
        (600.0, 800.0),
        (600.0, 350.0),
        (600.0, 800.0),
        (1200.0, 800.0),
    ] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("native-message-empty").is_none());
        assert!(
            visual
                .debug_bounds("native-message-cleared-status")
                .is_none()
        );
        let user = visual
            .debug_bounds("native-message-body-user")
            .expect("user prompt");
        let reply = visual
            .debug_bounds("native-message-reply")
            .expect("assistant message");
        assert!(user.size.height > px(0.0));
        assert!(
            user.size.height < px(100.0),
            "short prompts do not wrap into a narrow column"
        );
        assert!(reply.size.height > px(0.0));
        assert!(
            visual.debug_bounds("native-tool-heading-tool").is_none(),
            "completed work is folded"
        );
        let fold = visual
            .debug_bounds("native-work-group-tool")
            .expect("completed work disclosure");
        visual.simulate_click(fold.center(), Modifiers::none());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let tool = visual
            .debug_bounds("native-tool-heading-tool")
            .expect("expanded tool disclosure");
        pretty_assertions::assert_eq!(
            tool.left(),
            reply.left(),
            "tool headings share the text lane"
        );
        assert!(
            user.left() > reply.left(),
            "user message aligns to the right"
        );
        assert!(
            user.right() <= reply.right(),
            "bubble stays in the conversation lane"
        );
        assert!(
            reply.size.width <= px(48.0 * 16.0),
            "text width stays readable on wide panes"
        );
        assert!(reply.left() >= px(0.0) && reply.right() <= px(width));
        assert!(reply.top() >= px(0.0) && reply.bottom() <= px(height));
        let fold = visual
            .debug_bounds("native-work-group-tool")
            .expect("expanded work disclosure");
        visual.simulate_click(fold.center(), Modifiers::none());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("native-tool-heading-tool").is_none());
        assert!(visual.debug_bounds("native-message-reply").is_some());
    }
}

#[gpui_kit::test]
fn long_prompt_expands_in_place_and_copy_keeps_the_full_message(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    let text = (1..=12)
        .map(|line| format!("Requirement {line}: keep the conversation readable."))
        .collect::<Vec<_>>()
        .join("\n\n");
    record.snapshot.transcript.retain(|item| item.id == "user");
    record.snapshot.transcript[0].text.clone_from(&text);
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let collapsed = visual
        .debug_bounds("native-message-body-user")
        .expect("prompt");
    assert!(collapsed.size.height < px(250.), "long prompt is clipped");
    let copy = visual
        .debug_bounds("copy-message:user")
        .expect("copy control");
    visual.simulate_click(copy.center(), Modifiers::none());
    visual.update(|_, cx| {
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text)
        );
    });
    let expand = visual
        .debug_bounds("expand-message:user")
        .expect("expand prompt");
    visual.simulate_click(expand.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let expanded = visual
        .debug_bounds("native-message-body-user")
        .expect("full prompt");
    assert!(
        expanded.size.height > collapsed.size.height,
        "full prompt is visible"
    );
    pretty_assertions::assert_eq!(expanded.right(), collapsed.right());
    let collapse = visual
        .debug_bounds("expand-message:user")
        .expect("collapse prompt");
    visual.simulate_click(collapse.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    pretty_assertions::assert_eq!(
        visual
            .debug_bounds("native-message-body-user")
            .expect("compact prompt")
            .size,
        collapsed.size
    );
}

#[gpui_kit::test]
fn working_turn_keeps_work_visible_and_shows_activity(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    let tool = record
        .snapshot
        .transcript
        .iter_mut()
        .find_map(|item| item.tool.as_mut())
        .expect("running command");
    tool.status = bootty_agents::NativeToolStatus::Running;
    record.snapshot.transcript.push(
        serde_json::from_value(serde_json::json!({
            "id": "steer", "role": "user", "text": "Also explain the result.", "complete": true
        }))
        .expect("follow-up during active work"),
    );
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-working").is_some());
    assert!(
        visual.debug_bounds("native-tool-heading-tool").is_none(),
        "live work starts collapsed"
    );
    let fold = visual
        .debug_bounds("native-work-group-tool")
        .expect("live work log");
    visual.simulate_click(fold.center(), Modifiers::none());
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-tool-heading-tool").is_some());
    assert!(
        visual.debug_bounds("native-working").is_some(),
        "opening work keeps activity visible"
    );
}

/// The quote's comment editor moves with its quote as the transcript scrolls.
#[gpui_kit::test]
fn quote_comment_editors_follow_their_quote(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("stopped");
    record.snapshot.transcript.retain(|item| item.id == "reply");
    record.snapshot.transcript[0].text = format!(
        "A readable response to review.{}",
        "\n\nMore of it.".repeat(60)
    );
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    // Start from the top of the reply.
    visual.simulate_event(ScrollWheelEvent {
        position: point(px(400.), px(300.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(100_000.))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    select_response(visual);
    assert!(visual.debug_bounds("response-selection-toolbar").is_some());
    visual.simulate_keystrokes("c");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let before = visual
        .debug_bounds("response-quote-editor")
        .expect("comment editor");
    visual.simulate_event(ScrollWheelEvent {
        position: point(px(400.), px(300.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let after = visual
        .debug_bounds("response-quote-editor")
        .expect("the editor stays open");
    assert!(
        after.top() < before.top(),
        "the editor moved up with the transcript: {before:?} → {after:?}"
    );
}

#[gpui_kit::test]
fn selected_response_has_compact_comments_and_a_reaction_submenu(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("stopped");
    record.snapshot.transcript.retain(|item| item.id == "reply");
    record.snapshot.transcript[0].text = "A readable response to review.".into();
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let mut conversation = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        conversation = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    verify_selection_focus(visual, conversation.as_ref().expect("conversation"));
    select_response(visual);
    visual.simulate_keystrokes("y");
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.draw(cx).clear(cx);
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard()
                .expect("copied selection")
                .text()
                .expect("selected text"),
            "A readable response to review.\n"
        );
    });
    assert!(
        visual.debug_bounds("response-selection-toolbar").is_none(),
        "Y copies and clears selection"
    );
    verify_selection_copy_button(visual);
    select_response(visual);
    assert!(
        visual.debug_bounds("response-selection-comment").is_some(),
        "single Comment action"
    );
    for (action, key) in [
        ("response-selection-comment", "kbd:c"),
        ("response-selection-react", "kbd:r"),
        ("response-selection-copy", "kbd:y"),
    ] {
        let button = visual.debug_bounds(action).expect("annotation action");
        let hint = visual
            .debug_bounds(key)
            .expect("inline annotation key hint");
        assert!(
            hint.left() > button.center().x,
            "key hint trails the action label"
        );
        assert!(
            button.contains(&hint.center()),
            "key hint belongs to its action"
        );
    }
    visual.simulate_keystrokes("c");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual
        .debug_bounds("response-quote-editor")
        .expect("comment editor");
    assert!(editor.size.width <= px(288.));
    assert!(editor.size.height < px(160.));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("response-quote-editor").is_none());
    verify_reaction_submenu(visual);
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-quote-token").is_none(),
        "reference deletes with draft content"
    );
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-quote-token").is_some(),
        "undo restores the reference"
    );
}

fn verify_selection_copy_button(visual: &mut gpui_kit::VisualTestContext) {
    select_response(visual);
    let copy = visual
        .debug_bounds("response-selection-copy")
        .expect("copy selected text");
    visual.simulate_click(copy.center(), Modifiers::none());
    visual.update(|_, cx| {
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("A readable response to review.\n".into()),
            "a toolbar click keeps the captured text after pointer down clears selection"
        );
    });
}

fn verify_selection_focus(
    visual: &mut gpui_kit::VisualTestContext,
    conversation: &Entity<NativeAgentSessionView>,
) {
    select_response(visual);
    let composer = visual
        .debug_bounds("native-composer")
        .expect("prompt editor");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.update(|window, cx| {
        assert!(
            conversation.read(cx).focus_handle(cx).is_focused(window),
            "clicking the prompt editor acquires its keyboard focus"
        );
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-selection-toolbar").is_none(),
        "changing focus to another surface dismisses the response toolbar"
    );
}

fn verify_reaction_submenu(visual: &mut gpui_kit::VisualTestContext) {
    select_response(visual);
    assert!(
        visual.debug_bounds("response-reaction-menu").is_none(),
        "reactions are hidden until requested"
    );
    visual.simulate_keystrokes("r");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-reaction-menu").is_some(),
        "R opens Pi reactions"
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("response-reaction-menu").is_none());
    assert!(
        visual.debug_bounds("response-selection-toolbar").is_some(),
        "Escape closes only the submenu"
    );
    let react = visual
        .debug_bounds("response-selection-react")
        .expect("React action");
    visual.simulate_click(react.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-reaction-menu").is_some(),
        "pointer opens the same reaction menu"
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-selection-toolbar").is_none(),
        "second Escape clears selection"
    );
    select_response(visual);
    visual.simulate_keystrokes("r 0 8");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-reaction-menu").is_some(),
        "unassigned numbers keep the menu open"
    );
    visual.simulate_keystrokes("7");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("response-reaction-menu").is_none(),
        "7 selects a reaction and closes the menu"
    );
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    select_response(visual);
    visual.simulate_keystrokes("r down down down enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("response-selection-toolbar").is_none());
    assert!(
        visual.debug_bounds("response-quote-token").is_some(),
        "reaction becomes an editable inline reference"
    );
}

fn select_response(visual: &mut gpui_kit::VisualTestContext) {
    drag_response(visual);
    paint_selection(visual);
}

fn drag_response(visual: &mut gpui_kit::VisualTestContext) {
    drag_message(visual, "native-message-reply");
}

fn drag_message(visual: &mut gpui_kit::VisualTestContext, selector: &'static str) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let reply = visual.debug_bounds(selector).expect("selectable response");
    let start = point(reply.left().add(px(1.)), reply.top().add(px(16.)));
    let end = point(reply.left().add(px(400.)), reply.top().add(px(16.)));
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    visual.simulate_event(MouseUpEvent {
        position: end,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
}

fn paint_selection(visual: &mut gpui_kit::VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    visual.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui_kit::test]
fn response_shortcuts_accept_a_released_selection_before_its_next_frame(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("stopped");
    record.snapshot.transcript.retain(|item| item.id == "reply");
    record.snapshot.transcript[0].text = "A readable response to review.".into();
    let mut later = record.snapshot.transcript[0].clone();
    later.id = "later".into();
    later.text = "The newer response.".into();
    record.snapshot.transcript.push(later);
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    for (key, selector) in [
        ("c", "response-quote-editor"),
        ("r", "response-reaction-menu"),
    ] {
        drag_response(visual);
        visual.simulate_keystrokes(key);
        paint_selection(visual);
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{key} survives frame admission"
        );
        visual.simulate_keystrokes("escape");
        paint_selection(visual);
    }
    drag_message(visual, "native-message-later");
    visual.simulate_keystrokes("y");
    paint_selection(visual);
    visual.update(|_, cx| {
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("The newer response.\n".into()),
            "a new focused response replaces the previously captured quote"
        );
    });
}

#[gpui_kit::test]
fn history_gutter_jumps_to_saved_turns_without_mounting_the_whole_transcript(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript[4].text = "A long answer.\n\n".repeat(50);
    record.snapshot.transcript.extend(
        serde_json::from_value::<Vec<bootty_agents::NativeTranscriptItem>>(serde_json::json!([
            {"id":"user-two", "role":"user", "text":"Review the follow-up.", "complete":true},
            {"id":"reply-two", "role":"assistant", "text":"Follow-up reviewed.", "complete":true}
        ]))
        .expect("second saved turn"),
    );
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(500.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-history-rail").is_some());
    assert!(visual.debug_bounds("native-message-user").is_none());
    let first = visual
        .debug_bounds("history-turn:user")
        .expect("first history tick");
    visual.simulate_click(first.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-user").is_some());
    let latest = visual
        .debug_bounds("native-jump-latest")
        .expect("return to latest after reading history");
    visual.simulate_click(latest.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-jump-latest").is_none());
    let copy = visual
        .debug_bounds("copy-message:reply-two")
        .expect("completed reply copy action");
    visual.simulate_click(copy.center(), Modifiers::none());
    let copied = visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    pretty_assertions::assert_eq!(copied.as_deref(), Some("Follow-up reviewed."));
    let second = visual
        .debug_bounds("history-turn:user-two")
        .expect("second history tick");
    visual.simulate_click(second.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-user-two").is_some());
}

/// The rail answers the keyboard: once focused it previews the turn in view, arrows pick
/// another, and Enter jumps to it. A second turn doesn't move the messages.
#[gpui_kit::test]
fn history_rail_previews_and_jumps_from_the_keyboard(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript[4].text = "A long answer.\n\n".repeat(50);
    let single = open_conversation(record.clone(), cx)
        .debug_bounds("native-composer")
        .expect("composer");
    record.snapshot.transcript.extend(
        serde_json::from_value::<Vec<bootty_agents::NativeTranscriptItem>>(serde_json::json!([
            {"id":"user-two", "role":"user", "text":"Review the follow-up.", "complete":true},
            {"id":"reply-two", "role":"assistant", "text":"Follow-up reviewed.", "complete":true}
        ]))
        .expect("second turn"),
    );
    let visual = open_conversation(record, cx);
    assert!(visual.debug_bounds("native-history-rail").is_some());
    assert_eq!(
        visual
            .debug_bounds("native-composer")
            .map(|bounds| bounds.left()),
        Some(single.left()),
        "a second turn leaves the messages where they were"
    );
    let tick = visual.debug_bounds("history-turn:user").unwrap();
    let rem = visual.update(|window, _| window.rem_size());
    assert!(tick.size.height >= rem, "a turn is at least 1rem tall");
    let first = visual.debug_bounds("history-turn:user").unwrap();
    visual.simulate_click(first.center(), Modifiers::none());
    visual.run_until_parked();
    // Off the rail, the focused rail still previews the turn in view.
    visual.simulate_mouse_move(point(px(900.), px(250.)), None, Modifiers::none());
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("history-preview:0").is_some());
    visual.simulate_keystrokes("down");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("history-preview:1").is_some());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-user-two").is_some());
}

/// A 1000×500 conversation window showing `record`, drawn once.
fn open_conversation(
    record: NativeSessionRecord,
    cx: &mut TestAppContext,
) -> &mut gpui_kit::VisualTestContext {
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(500.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual
}

#[gpui_kit::test]
fn large_approval_scope_keeps_its_decision_buttons_visible_and_operable(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("waiting");
    record.config.provider = AgentKind::Codex;
    record.snapshot.provider = AgentKind::Codex;
    record.snapshot.requests = serde_json::from_value(serde_json::json!([{
        "id":"large-scope", "method":"item/commandExecution/requestApproval",
        "parameters":{"command":"printf approved > owned-qa-file", "cwd":"/tmp/project",
            "reason":"This command changes the requested file. ".repeat(80),
            "threadId":"provider-thread", "turnId":"provider-turn", "itemId":"provider-item",
            "networkApprovalContext":{"host":"example.com","protocol":"https"}}
    }]))
    .expect("public provider approval");
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let deny = visual
        .debug_bounds("deny:large-scope")
        .expect("visible decline control");
    let allow = visual
        .debug_bounds("allow:large-scope")
        .expect("visible once-only approval control");
    for bounds in [deny, allow] {
        assert!(
            bounds.top() >= px(0.) && bounds.bottom() <= px(600.),
            "decision stays inside the window: {bounds:?}"
        );
    }
    let scope = visual
        .debug_bounds("approval-scope:large-scope")
        .expect("approval scope");
    let before = visual
        .debug_bounds("request-details-text")
        .expect("full scope text");
    visual.simulate_event(ScrollWheelEvent {
        position: scope.center(),
        delta: ScrollDelta::Lines(point(0., -18.)),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let after = visual
        .debug_bounds("request-details-text")
        .expect("scrolled scope text");
    assert!(
        after.top() < before.top(),
        "full scope scrolls: {before:?} -> {after:?}"
    );
    pretty_assertions::assert_eq!(visual.debug_bounds("deny:large-scope"), Some(deny));
    visual.simulate_click(deny.center(), Modifiers::none());
    let invocation = calls
        .lock()
        .unwrap()
        .iter()
        .find(|invocation| invocation.command == "agents.native.approve")
        .cloned()
        .expect("the visible decision submits through the command owner");
    pretty_assertions::assert_eq!(invocation.target, Some(target));
    pretty_assertions::assert_eq!(
        invocation.arguments.last().map(String::as_str),
        Some("deny")
    );
}

type CapturedCommands = std::sync::Arc<std::sync::Mutex<Vec<bootty_control::CommandInvocation>>>;

fn native_command_sender(
    record: &NativeSessionRecord,
) -> (bootty_control::AppCommandSender, CapturedCommands) {
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let inbox = mailbox.clone();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = calls.clone();
    let history = record.snapshot.transcript.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        std::sync::Arc::new(move || {
            loop {
                let next = {
                    let inbox = inbox.lock().unwrap();
                    inbox.as_ref().unwrap().try_recv()
                };
                let Ok(request) = next else {
                    break;
                };
                let value = if request.invocation.command == "agents.native.history" {
                    serde_json::json!({"transcript":history,"has_older":false,"has_newer":false,"at_latest":true})
                } else if request.invocation.command == "agents.native.models-info" {
                    serde_json::json!({"models":[],"permissions":null})
                } else if request.invocation.command == "git.overview" {
                    serde_json::json!({"root":"/repo","history":[],"stashes":[],"branches":[
                        {"name":"feature","commit":"1111","current":true,"upstream":null},
                        {"name":"main","commit":"2222","current":false,"upstream":"origin/main",
                            "ahead":0,"behind":2}]})
                } else if request.invocation.command == "git.status" {
                    serde_json::json!({"root":"/repo","branch":"feature","files":[]})
                } else if request.invocation.command == "agents.native.turn-diff" {
                    serde_json::json!({"diff":"diff --git a/turn.txt b/turn.txt\n+first\n"})
                } else if request.invocation.command == "agents.native.completions" {
                    serde_json::json!({"options":[{"kind":"command","name":"compact",
                        "display_name":"compact","description":"Summarize the context",
                        "argument_hint":null,"path":null,"scope":null}]})
                } else {
                    serde_json::json!([])
                };
                captured.lock().unwrap().push(request.invocation);
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    (sender, calls)
}

#[gpui_kit::test]
fn narrow_conversation_keeps_interrupt_and_send_inside_the_pane(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    record.config.model = Some("gpt-6.1-sol".into());
    record.snapshot.usage =
        Some(serde_json::json!({"totalTokens":25_000,"modelContextWindow":272_000}));
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    for width in [800., 400.] {
        visual.simulate_resize(size(px(width), px(600.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        for selector in ["native-attach", "native-interrupt", "native-send"] {
            let bounds = visual.debug_bounds(selector).expect("composer action");
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= px(width),
                "{selector} stays inside the pane: {bounds:?}"
            );
        }
    }
    let interrupt = visual.debug_bounds("native-interrupt").unwrap();
    visual.simulate_click(interrupt.center(), Modifiers::none());
    let invocation = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.command == "agents.native.interrupt")
        .cloned()
        .expect("visible interruption reaches its command owner");
    pretty_assertions::assert_eq!(invocation.target, Some(target));
}

/// A control that leaves under a resting pointer takes its tooltip with it.
#[gpui_kit::test]
fn interrupt_tooltip_leaves_with_the_finished_turn(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("working");
    let mut idle = serde_json::to_value(&record.snapshot).unwrap();
    idle["status"] = "idle".into();
    idle["revision"] = 2.into();
    let inbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                // The queued follow-up's reply reports that the turn already ended.
                let value = if request.invocation.command == "agents.native.queue" {
                    idle.clone()
                } else {
                    serde_json::Value::Null
                };
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input("Wrap up");
    let interrupt = visual
        .debug_bounds("native-interrupt")
        .expect("a working turn offers Interrupt");
    visual.simulate_mouse_move(interrupt.center(), None, Modifiers::none());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(1));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("tooltip:Interrupt").is_some());

    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-interrupt").is_none());
    assert!(
        visual.debug_bounds("tooltip:Interrupt").is_none(),
        "the tooltip leaves with its button"
    );
}

#[gpui_kit::test]
fn disconnected_conversation_offers_an_exact_resume_without_submitting_its_draft(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("error");
    record.snapshot.error = Some("Provider process ended".into());
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(400.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_keystrokes("d r a f t");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let resume = visual
        .debug_bounds("native-resume")
        .expect("visible recovery action");
    assert!(resume.left() >= px(0.) && resume.right() <= px(400.));
    visual.simulate_click(resume.center(), Modifiers::none());
    let calls = calls.lock().unwrap().clone();
    let invocation = calls
        .iter()
        .find(|call| call.command == "agents.native.resume")
        .expect("resume reaches the command owner");
    pretty_assertions::assert_eq!(invocation.target, Some(target));
    assert!(
        calls
            .iter()
            .all(|call| call.command != "agents.native.prompt")
    );
}

#[gpui_kit::test]
fn current_subagents_share_a_group_beside_working_and_can_open_individually(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    for (id, status) in [("review", "running"), ("checks", "completed")] {
        record.snapshot.transcript.push(serde_json::from_value(serde_json::json!({
            "id": id, "role": "tool", "text": "Inspect the changes", "complete": status == "completed",
            "subagent": {"id": id, "title": id, "prompt": "Review this change carefully.",
                "model": "model", "thread_id": null, "owner_session": "provider-session",
                "status": status, "started_at": null, "completed_at": null}
        })).unwrap());
    }
    record.snapshot.transcript.push(serde_json::from_value(serde_json::json!({
        "id": "copied-child", "role": "tool", "text": "A report copied from another conversation", "complete": true,
        "subagent": {"id":"copied-child", "title":"Copied child", "prompt":"Copied history",
            "status":"running", "owner_session":"another-provider-session"}
    })).unwrap());
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.update(|window, _| window.resize(size(px(1200.), px(900.))));
    visual.run_until_parked();
    let group = visual
        .debug_bounds("subagent-group")
        .expect("group near working indicator");
    assert!(group.size.width < px(300.), "the track stays content-sized");
    let progress = visual
        .debug_bounds("native-progress-status")
        .expect("parent progress");
    pretty_assertions::assert_eq!(
        group.center().y,
        progress.center().y,
        "subagents and progress share one row"
    );
    let reply_before = visual
        .debug_bounds("native-message-reply")
        .expect("parent reply");
    visual.simulate_click(group.center(), Modifiers::none());
    visual.run_until_parked();
    assert!(visual.debug_bounds("subagent-open:review").is_some());
    assert!(visual.debug_bounds("subagent-open:checks").is_some());
    assert!(visual.debug_bounds("subagent-open:copied-child").is_none());
    pretty_assertions::assert_eq!(
        visual.debug_bounds("native-message-reply"),
        Some(reply_before),
        "opening the popover does not move the conversation"
    );
}

#[gpui_kit::test]
fn another_writer_replaces_the_editor_with_exact_recovery_actions(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("stopped");
    record.snapshot.provider_writer = bootty_agents::NativeProviderWriterState::Blocked;
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-writer-callout").is_some());
    assert!(visual.debug_bounds("native-composer").is_none());
    for (selector, command) in [
        ("native-writer-retry", "agents.native.resume"),
        ("native-writer-fork", "agents.native.fork"),
    ] {
        let bounds = visual.debug_bounds(selector).expect("actionable recovery");
        visual.simulate_click(bounds.center(), Modifiers::none());
        visual.run_until_parked();
        let invocation = calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.command == command)
            .cloned()
            .expect("action reaches command owner");
        pretty_assertions::assert_eq!(invocation.target, Some(target.clone()));
    }
}

#[gpui_kit::test]
fn composer_accepts_folders_from_paste_and_platform_drop(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().to_owned();
    let record = saved_record("idle");
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(
        point(composer.left().add(px(30.)), composer.top().add(px(20.))),
        Modifiers::none(),
    );
    visual.update(|_, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::ExternalPaths(
                gpui_kit::ExternalPaths(std::iter::once(path.clone()).collect()),
            )],
        });
    });
    visual.simulate_keystrokes("cmd-v");
    visual.run_until_parked();
    visual.simulate_keystrokes("cmd-a cmd-c");
    let text = visual
        .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap();
    assert!(
        text.contains(path.to_str().unwrap()),
        "pasted folder becomes prompt context: {text}"
    );
    visual.simulate_keystrokes("backspace");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for position in [
        composer.center(),
        visual
            .debug_bounds("native-message-reply")
            .unwrap()
            .center(),
    ] {
        visual.simulate_event(gpui_kit::FileDropEvent::Entered {
            position,
            paths: gpui_kit::ExternalPaths(std::iter::once(path.clone()).collect()),
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.simulate_event(gpui_kit::FileDropEvent::Submit { position });
        visual.run_until_parked();
        visual.simulate_keystrokes("cmd-a cmd-c");
        let text = visual
            .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .unwrap();
        pretty_assertions::assert_eq!(
            text.matches(path.to_str().unwrap()).count(),
            1,
            "a drop adds the folder once, including over the transcript"
        );
        visual.simulate_keystrokes("backspace");
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui_kit::test]
fn a_file_drop_on_the_collapsed_composer_restores_the_draft(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().to_owned();
    let mut record = saved_record("idle");
    let user = record.snapshot.transcript[2].clone();
    let reply = record.snapshot.transcript[4].clone();
    record.snapshot.transcript.clear();
    for ix in 0..40 {
        let mut user = user.clone();
        user.id = format!("user-{ix}");
        let mut reply = reply.clone();
        reply.id = format!("reply-{ix}");
        record.snapshot.transcript.extend([user, reply]);
    }
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(600.)));
    visual.run_until_parked();
    visual.simulate_event(ScrollWheelEvent {
        position: point(px(500.), px(250.)),
        delta: ScrollDelta::Lines(point(0., 60.)),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let position = visual
        .debug_bounds("native-collapsed-composer")
        .expect("scrolled-up composer collapses")
        .center();
    visual.simulate_event(gpui_kit::FileDropEvent::Entered {
        position,
        paths: gpui_kit::ExternalPaths(vec![path.clone()].into()),
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_event(gpui_kit::FileDropEvent::Submit { position });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-composer").is_some());
    visual.simulate_keystrokes("cmd-a cmd-c");
    let text = visual
        .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap();
    assert!(text.contains(path.to_str().unwrap()));
}

#[gpui_kit::test]
fn cancelling_an_attachment_import_keeps_its_late_result_out_of_the_draft(cx: &mut TestAppContext) {
    use gpui_kit::InputEvent as _;
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, "Attachment body").unwrap();
    let record = saved_record("idle");
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let position = visual
        .debug_bounds("native-message-reply")
        .unwrap()
        .center();
    // Dispatch without advancing the executor: the filesystem result is still in flight.
    visual.update(|window, cx| {
        window.dispatch_event(
            gpui_kit::FileDropEvent::Entered {
                position,
                paths: gpui_kit::ExternalPaths(vec![path.clone()].into()),
            }
            .to_platform_input(),
            cx,
        );
        window.draw(cx).clear(cx);
        window.dispatch_event(
            gpui_kit::FileDropEvent::Submit { position }.to_platform_input(),
            cx,
        );
        window.draw(cx).clear(cx);
    });
    let cancel = visual
        .debug_bounds("cancel-attachment-import:0")
        .expect("pending import has a cancel action");
    visual.update(|window, cx| {
        window.dispatch_event(
            gpui_kit::MouseDownEvent {
                position: cancel.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position: cancel.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("cancel-attachment-import:0").is_none());
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.command == "agents.native.import")
    );
}

#[gpui_kit::test]
fn turn_rail_keeps_every_marker_in_the_available_height(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    let user = record.snapshot.transcript[2].clone();
    let reply = record.snapshot.transcript[4].clone();
    record.snapshot.transcript.clear();
    for ix in 0..128 {
        let mut user = user.clone();
        user.id = format!("turn-{ix}");
        record.snapshot.transcript.push(user);
        let mut reply = reply.clone();
        reply.id = format!("reply-{ix}");
        record.snapshot.transcript.push(reply);
    }
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    for height in [600., 300.] {
        visual.simulate_resize(size(px(1000.), px(height)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let rail = visual.debug_bounds("native-history-rail").unwrap();
        for selector in [
            "history-turn:turn-0",
            "history-turn:turn-64",
            "history-turn:turn-127",
        ] {
            let tick = visual.debug_bounds(selector).unwrap();
            assert!(
                tick.top() >= rail.top() && tick.bottom() <= rail.bottom().add(px(0.1)),
                "{selector} fits the rail: {tick:?} in {rail:?}"
            );
        }
    }
    let first = visual.debug_bounds("history-turn:turn-0").unwrap();
    visual.simulate_click(first.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-turn-0").is_some());
}

#[gpui_kit::test]
fn first_live_user_message_keeps_a_readable_width(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    record
        .snapshot
        .transcript
        .retain(|item| item.role == "user");
    let item = &mut record.snapshot.transcript[0];
    item.complete = false;
    item.text = "- Cmd+ and Cmd- should adjust all fonts together, including terminal text and UI text.\n- Leave the separate terminal and UI font actions unbound by default.".into();
    let (sender, _receiver) = bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    for (width, height) in [(600.0, 800.0), (1800.0, 1100.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let message = visual
            .debug_bounds("native-message-body-user")
            .expect("initial user message");
        assert!(
            message.size.width > px(200.0),
            "initial message has a readable width: {message:?}"
        );
        assert!(
            message.size.height < px(180.0),
            "initial message does not become a vertical strip: {message:?}"
        );
    }
}

/// A conversation with no messages says how to start instead of showing a blank pane.
#[gpui_kit::test]
fn empty_conversation_explains_how_to_start(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for (transcript_empty, shown) in [(true, true), (false, false)] {
        let mut record = saved_record("idle");
        if transcript_empty {
            record.snapshot.transcript.clear();
        }
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            visual.debug_bounds("native-empty-conversation").is_some(),
            shown
        );
    }
}

/// Copying a turn's reply copies every reply in the turn; copying a prompt brings the passage it
/// quoted and the files it attached.
#[gpui_kit::test]
fn copies_keep_the_whole_reply_and_the_prompts_context(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "ask", "role": "user", "text": "Why [quote]?", "complete": true,
            "citations": [{"message_id": "earlier", "source_range": {"start": 0, "end": 8},
                "quote": "it fails", "comment": "this part",
                "prompt_range": {"start": 4, "end": 11}}],
            "attachments": [{"id": "log", "kind": "file", "name": "build.log",
                "mime_type": "text/plain", "size_bytes": 3}]},
        {"id": "found", "role": "assistant", "text": "Found it.", "complete": true},
        {"id": "tool", "role": "tool", "text": "", "complete": true,
            "tool": {"name": "Bash", "input": "{\"command\":\"ls\"}", "status": "completed"}},
        {"id": "fixed", "role": "assistant",
            "text": "Fixed.\n\n| Name | **Size** |\n| --- | ---: |\n| a\\|b | `1` |", "complete": true}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let copied = |visual: &mut gpui_kit::VisualTestContext, selector: &'static str| {
        let copy = visual.debug_bounds(selector).expect(selector);
        visual.simulate_click(copy.center(), Modifiers::none());
        visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
    };
    assert_eq!(
        copied(visual, "copy-message:fixed").as_deref(),
        Some("Found it.\n\nFixed.\n\n| Name | **Size** |\n| --- | ---: |\n| a\\|b | `1` |")
    );
    // A reply's tables copy as cells a spreadsheet pastes, without their formatting.
    assert_eq!(
        copied(visual, "copy-table:fixed").as_deref(),
        Some("Name\tSize\na|b\t1")
    );
    assert!(visual.debug_bounds("copy-table:ask").is_none());
    let prompt = copied(visual, "copy-message:ask").unwrap_or_default();
    assert!(prompt.contains("> it fails\nthis part"), "{prompt}");
    assert!(prompt.ends_with("Attached: build.log"), "{prompt}");
}

/// A settled turn lists every file it changed under its reply, with +/− summed across its
/// edits, and each opens the file.
#[gpui_kit::test]
fn settled_turns_list_the_files_they_changed(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let edit = |id: &str, path: &str, diff: &str| {
        serde_json::json!({"id": id, "role": "tool", "text": "", "complete": true,
            "tool": {"name": "fileChange", "status": "completed", "input":
                serde_json::json!([{"path": format!("/tmp/project/{path}"), "diff": diff}]).to_string()}})
    };
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Fix it", "complete": true},
        edit("first", "src/main.rs", "@@ -1 +1,2 @@\n-old\n+new\n+more\n"),
        edit("second", "src/lib.rs", "@@ -1 +1 @@\n-a\n+b\n"),
        edit("third", "src/main.rs", "@@ -3 +3 @@\n-x\n+y\n"),
        {"id": "done", "role": "assistant", "text": "Fixed.", "complete": true}
    ]))
    .unwrap();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-turn-changes:+4:-3").is_some());
    let main = visual
        .debug_bounds("native-turn-change:src/main.rs")
        .expect("main.rs once");
    visual.simulate_click(main.center(), Modifiers::none());
    visual.run_until_parked();
    let opened = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.open-link")
        .map(|invocation| invocation.arguments[2].clone())
        .collect::<Vec<_>>();
    assert_eq!(opened, ["src/main.rs"]);
}

/// Progress messages and changed files remain visible after a turn settles or another begins.
#[gpui_kit::test]
fn turns_keep_progress_and_changed_files_visible(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for status in ["working", "idle", "stopped", "error"] {
        let mut record = saved_record(status);
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Audit this", "complete": true},
        {"id": "first-tool", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"ls\"}", "status": "completed"}},
        {"id": "status", "role": "assistant", "text": "Found the cause.", "complete": true},
        {"id": "second-tool", "role": "tool", "text": "", "complete": false,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo test\"}", "status": "running"}},
        {"id": "edit", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "fileChange", "input": "[{\"path\":\"/tmp/project/src/main.rs\",\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\\n\"}]", "status": "completed"}},
        {"id": "latest", "role": "assistant", "text": "Running the tests.", "complete": false}
    ]))
    .unwrap();
        if status == "working" {
            for item in &mut record.snapshot.transcript {
                item.complete = true;
                if let Some(tool) = &mut item.tool {
                    tool.status = bootty_agents::NativeToolStatus::Completed;
                }
            }
            record.snapshot.transcript.push(
                serde_json::from_value(serde_json::json!({
                    "id":"follow-up", "role":"user", "text":"Is it fixed?", "complete":true
                }))
                .unwrap(),
            );
        }
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let status = visual
            .debug_bounds("native-message-status")
            .expect("an earlier progress message stays visible");
        let latest = visual
            .debug_bounds("native-message-latest")
            .expect("latest message");
        let between = visual
            .debug_bounds("native-work-group-second-tool")
            .expect("work between messages folds");
        assert!(status.top() < between.top() && between.top() < latest.top());
        assert!(
            visual
                .debug_bounds("native-work-group-first-tool")
                .is_some()
        );
        // Commentary between tools carries no timestamp or actions; the prompt keeps them.
        assert!(visual.debug_bounds("copy-message:status").is_none());
        assert!(visual.debug_bounds("copy-message:prompt").is_some());
        assert!(visual.debug_bounds("native-tool-heading-edit").is_some());
        assert!(
            visual
                .debug_bounds("native-file-change:src/main.rs:+1:-1")
                .is_some()
        );
        let disclosure = visual
            .debug_bounds("tool-disclosure:edit")
            .expect("file disclosure");
        visual.simulate_click(disclosure.center(), Modifiers::none());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let diff = visual
            .debug_bounds("native-inline-diff:src/main.rs")
            .expect("expanded diff");
        assert!(
            diff.size.height > px(30.),
            "a text diff has room for its source lines"
        );
    }
}

/// Long tool output scrolls inside its row instead of stretching the transcript.
#[gpui_kit::test]
fn expanded_tool_output_scrolls_within_a_capped_box(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let output = (1..=200)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Show the log", "complete": true},
        {"id": "log", "role": "tool", "text": output, "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"cat build.log\"}", "status": "completed"}},
        {"id": "reply", "role": "assistant", "text": "That's the log.", "complete": true}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(1600.)));
    for selector in ["native-work-summary:Ran 1 command", "tool-disclosure:log"] {
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let control = visual.debug_bounds(selector).expect(selector);
        visual.simulate_click(control.center(), Modifiers::none());
    }
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let output = visual
        .debug_bounds("tool-output-scroll:log")
        .expect("expanded output");
    assert!(
        output.size.height < px(400.),
        "200 lines stay in a capped box: {:?}",
        output.size
    );
}

/// A message with nothing to draw takes no row, even when it carries image references.
#[gpui_kit::test]
fn image_references_alone_draw_no_empty_row(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Look at this", "complete": true},
        {"id": "pictures", "role": "user", "text": "", "complete": true,
         "images": [{"id": "shot", "pixel_width": 800, "pixel_height": 600}]}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-prompt").is_some());
    assert!(visual.debug_bounds("native-message-pictures").is_none());
}

/// A settled turn's last fold says how long the turn took.
#[gpui_kit::test]
fn settled_folds_say_how_long_the_turn_took(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Build it", "complete": true,
         "created_at": 1_000_000},
        {"id": "build", "role": "tool", "text": "", "complete": true,
         "created_at": 1_002_000, "updated_at": 1_030_000,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo build\"}", "status": "completed"}},
        {"id": "progress", "role": "assistant", "text": "Built; testing.", "complete": true,
         "created_at": 1_031_000},
        {"id": "test", "role": "tool", "text": "", "complete": true,
         "created_at": 1_032_000, "updated_at": 1_060_000,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo test\"}", "status": "completed"}},
        {"id": "reply", "role": "assistant", "text": "Built.", "complete": true,
         "created_at": 1_065_000, "updated_at": 1_072_000}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    // The turn folds two runs; only the last says how long the turn took.
    assert!(
        visual
            .debug_bounds("native-work-summary:Ran 1 command")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("native-work-summary:Ran 1 command · 1m 12s")
            .is_some()
    );
}

/// Folds say what their tools did, mark failures, and name a running command's program.
#[gpui_kit::test]
fn work_folds_summarize_what_their_tools_did(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let settled = serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Audit this", "complete": true},
        {"id": "read", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Read", "input": "{\"file_path\":\"/tmp/a.rs\"}", "status": "completed"}},
        {"id": "check", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo check\"}", "status": "completed"}},
        {"id": "test", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo test\"}", "status": "failed"}},
        {"id": "reply", "role": "assistant", "text": "One test fails.", "complete": true}
    ]);
    let live = serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Run the tests", "complete": true},
        {"id": "test", "role": "tool", "text": "", "complete": false,
         "tool": {"name": "Bash", "input": "{\"command\":\"cd /tmp && RUST_LOG=1 cargo test\"}",
                  "status": "running"}}
    ]);
    for (status, transcript, selector) in [
        (
            "idle",
            settled,
            "native-work-summary:Ran 2 commands, read 1 file, 1 failed",
        ),
        ("working", live, "native-work-summary:Running cargo"),
    ] {
        let mut record = saved_record(status);
        record.snapshot.transcript = serde_json::from_value(transcript).unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{status} fold reads {selector}"
        );
    }
}

/// Each tool row names what it did in plain words and keeps the imperative for a failed step.
#[gpui_kit::test]
fn tool_rows_name_what_each_tool_did(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Check the docs", "complete": true},
        {"id": "fetch", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "WebFetch", "input": "{\"url\":\"https://example.com/guide\",\"prompt\":\"Summarize\"}",
                  "status": "completed"}},
        {"id": "agent", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Task", "input": "{\"description\":\"Audit the parser\",\"prompt\":\"Read src/parse.rs\"}",
                  "status": "completed"}},
        {"id": "todos", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "TodoWrite", "status": "completed", "input": serde_json::json!({"todos": [
             {"content": "Read docs", "status": "completed", "activeForm": "Reading docs"},
             {"content": "Write tests", "status": "in_progress", "activeForm": "Writing tests"}
         ]}).to_string()}},
        {"id": "plan", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "ExitPlanMode", "input": "{\"plan\":\"# Ship the guide\\n\\n1. Write it\"}",
                  "status": "completed"}},
        {"id": "test", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo test\"}", "status": "failed"}},
        {"id": "reply", "role": "assistant", "text": "Done.", "complete": true}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(1600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let fold = visual
        .debug_bounds("native-work-summary:Ran 1 command, used 4 tools, 1 failed")
        .expect("work fold");
    visual.simulate_click(fold.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for title in [
        "native-tool-title:fetch:Fetched https://example.com/guide",
        "native-tool-title:agent:Ran agent Audit the parser",
        "native-tool-title:todos:Updated tasks Writing tests",
        "native-tool-title:plan:Proposed plan Ship the guide",
        "native-tool-title:test:Run cargo test",
    ] {
        assert!(visual.debug_bounds(title).is_some(), "row reads {title}");
    }
}

/// Opening a Work row lists each thought as one line; its text shows only once opened.
#[gpui_kit::test]
fn thoughts_in_work_rows_open_one_at_a_time(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Fix it", "complete": true},
        {"id": "think", "role": "reasoning", "complete": true,
         "text": "**Planning the fix**\n\nRead the parser first, then the tests."},
        {"id": "check", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo check\"}", "status": "completed"}},
        {"id": "reply", "role": "assistant", "text": "Fixed.", "complete": true}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(1600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let fold = visual
        .debug_bounds("native-work-summary:Ran 1 command")
        .expect("work fold");
    visual.simulate_click(fold.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("native-thinking-title:think:Thought Planning the fix")
            .is_some()
    );
    assert!(visual.debug_bounds("thinking-scroll:think").is_none());
    let toggle = visual
        .debug_bounds("thinking-disclosure:think")
        .expect("thought toggle");
    visual.simulate_click(toggle.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("thinking-scroll:think").is_some());
}

/// The context card's Sources lists the files messages carry; images already show in their
/// messages, so a conversation with only images has no Sources section.
#[gpui_kit::test]
fn context_sources_list_files_but_not_images(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let attachment = |id: &str, kind: &str, name: &str| {
        serde_json::json!({"id": id, "kind": kind, "name": name, "mime_type": "image/png",
            "size_bytes": 10})
    };
    for (attachments, file_shown) in [
        (
            vec![
                attachment("shot", "image", "shot.png"),
                attachment("notes", "file", "notes.md"),
            ],
            true,
        ),
        (vec![attachment("shot", "image", "shot.png")], false),
    ] {
        let mut record = saved_record("idle");
        record.attachments = serde_json::from_value(serde_json::json!(attachments)).unwrap();
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id": "prompt", "role": "user", "text": "Look", "complete": true,
             "attachments": attachments}
        ]))
        .unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1600.), px(900.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            visual.debug_bounds("native-context-sources").is_some(),
            file_shown
        );
        assert_eq!(
            visual.debug_bounds("context-source:notes").is_some(),
            file_shown
        );
        assert!(visual.debug_bounds("context-source:shot").is_none());
    }
}

/// Subagents whose initials collide read apart in the working row, which shows three of them
/// and how many more there are.
#[gpui_kit::test]
fn subagent_avatars_read_apart_and_count_the_rest(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    let subagent = |id: &str, title: &str| {
        serde_json::json!({"id": format!("subagent:{id}"), "role": "subagent", "text": "",
            "complete": false, "subagent": {"id": id, "title": title, "prompt": "",
            "model": null, "thread_id": null, "owner_session": null, "status": "running",
            "started_at": null, "completed_at": null}})
    };
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        subagent("a", "Review code"),
        subagent("b", "Review changes"),
        subagent("c", "Verify release"),
        subagent("d", "Audit docs"),
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "subagent-avatar:a:R1",
        "subagent-avatar:b:R2",
        "subagent-avatar:c:VR",
        "subagent-avatars-more:1",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector} shows");
    }
}

/// Pasting more images than one prompt carries stops at the limit instead of failing at send.
#[gpui_kit::test]
fn pasted_images_stop_at_the_prompt_image_limit(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.update(|_, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: (0..=bootty_agents::MAX_NATIVE_PROMPT_IMAGES)
                .map(|_| {
                    gpui_kit::ClipboardEntry::Image(gpui_kit::Image::from_bytes(
                        gpui_kit::ImageFormat::Png,
                        png.get_ref().clone(),
                    ))
                })
                .collect(),
        });
    });
    visual.simulate_keystrokes("cmd-v");
    visual.run_until_parked();
    let imports = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.import")
        .count();
    assert_eq!(imports, bootty_agents::MAX_NATIVE_PROMPT_IMAGES);
}

/// Enter accepts a suggestion only when one exists; text whose last token merely starts
/// with a trigger character still sends.
#[gpui_kit::test]
fn enter_submits_trigger_text_only_when_nothing_matches(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for (text, submits) in [
        ("/nothing", true),
        ("costs $5", true),
        ("ping @alice", true),
        ("/comp", false),
    ] {
        let record = saved_record("idle");
        let (sender, commands) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                );
                view.set_completion_binding(
                    Some(bootty_control::CommandTarget {
                        kind: bootty_control::ResourceKind::Binding,
                        handle: "captured-binding".into(),
                        generation: 1,
                    }),
                    window,
                    cx,
                );
                view
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let editor = visual.debug_bounds("native-composer").expect("composer");
        visual.simulate_click(editor.center(), Modifiers::none());
        visual.simulate_input(text);
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let submitted = commands
            .lock()
            .unwrap()
            .iter()
            .any(|invocation| invocation.command == "agents.native.prompt");
        assert_eq!(submitted, submits, "Enter after {text:?}");
    }
}

/// Bootty's own slash commands run the command their button uses instead of reaching the model.
#[gpui_kit::test]
fn bootty_slash_commands_run_instead_of_sending(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    // Picking `/stop` from the menu inserts it; Enter then runs it. A typed argument runs directly.
    for (keys, command, arguments) in [
        (
            &["/stop", "confirm", "enter"][..],
            "agents.native.interrupt",
            vec!["saved-pi", "1"],
        ),
        (
            &["/rename Better title", "enter"],
            "agents.native.rename",
            vec!["saved-pi", "1", "Better title", "Conversation"],
        ),
    ] {
        let record = saved_record("working");
        let (sender, commands) = native_command_sender(&record);
        let mut session = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                );
                view.set_completion_binding(
                    Some(bootty_control::CommandTarget {
                        kind: bootty_control::ResourceKind::Binding,
                        handle: "captured-binding".into(),
                        generation: 1,
                    }),
                    window,
                    cx,
                );
                view
            });
            session = Some(view.clone());
            gpui_kit::component::Root::new(view, window, cx)
        });
        let session = session.unwrap();
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let editor = visual.debug_bounds("native-composer").expect("composer");
        visual.simulate_click(editor.center(), Modifiers::none());
        for key in keys {
            if *key == "enter" {
                visual.simulate_keystrokes("enter");
            } else if *key == "confirm" {
                visual.update(|window, cx| {
                    session.update(cx, |view, cx| {
                        view.perform_completion(CommandAction::Confirm, window, cx);
                    });
                });
            } else {
                visual.simulate_input(key);
            }
            visual.run_until_parked();
        }
        let sent = commands.lock().unwrap().clone();
        let ran = sent
            .iter()
            .find(|invocation| invocation.command == command)
            .unwrap_or_else(|| panic!("{keys:?} runs {command}: {sent:?}"));
        pretty_assertions::assert_eq!(ran.arguments, arguments);
        assert!(
            !sent.iter().any(|invocation| matches!(
                invocation.command.as_str(),
                "agents.native.prompt" | "agents.native.queue"
            )),
            "{keys:?} never reaches the model"
        );
    }
}

/// Bootty's own messages read as written; a provider's raw diagnostic gets the catalog's text.
#[gpui_kit::test]
fn conversation_errors_keep_bootty_wording_and_summarize_provider_text(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.error = Some("Could not read /Users/private/token.json: invalid".into());
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("native-turn-failure:The turn failed.")
            .is_some(),
        "a failed turn says so where it ended"
    );
    let details = visual
        .debug_bounds("copy-turn-failure-details")
        .expect("provider diagnostics remain available");
    visual.simulate_click(details.center(), Modifiers::none());
    visual.update(|_, cx| {
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Could not read /Users/private/token.json: invalid"
        );
    });
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input("/rename ");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("native-conversation-error:Add the new title after /rename.")
            .is_some()
    );
}

/// Retry on a failed turn sends that turn's message again, and the failure goes away.
#[gpui_kit::test]
fn failed_turns_retry_by_sending_their_message_again(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.error = Some("stream disconnected before completion".into());
    let prompt = record
        .snapshot
        .transcript
        .iter()
        .rfind(|item| item.role == "user")
        .map(|item| item.text.clone())
        .expect("a sent message");
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let retry = visual.debug_bounds("native-retry-turn").expect("retry");
    visual.simulate_click(retry.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let sent = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.prompt")
        .map(|invocation| invocation.arguments[2].clone())
        .collect::<Vec<_>>();
    assert_eq!(sent, [prompt]);
    assert!(visual.debug_bounds("native-retry-turn").is_none());
}

/// A message that steered a running turn stays inside that turn: it says so, and the turn rail
/// counts turns, not steers.
#[gpui_kit::test]
fn steering_messages_stay_inside_their_turn(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id":"first", "role":"user", "text":"Fix the build.", "complete":true},
        {"id":"first-reply", "role":"assistant", "text":"Looking.", "complete":true},
        {"id":"steer", "role":"user", "text":"Skip the docs.", "complete":true, "steered":true},
        {"id":"steer-reply", "role":"assistant", "text":"Skipping them.\n\n".repeat(40), "complete":true},
        {"id":"second", "role":"user", "text":"Now the tests.", "complete":true},
        {"id":"second-reply", "role":"assistant", "text":"Done.", "complete":true}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(500.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("history-turn:first").is_some());
    assert!(visual.debug_bounds("history-turn:second").is_some());
    assert!(
        visual.debug_bounds("history-turn:steer").is_none(),
        "a steer isn't a turn of its own"
    );
    let steer = visual.debug_bounds("history-turn:first").unwrap();
    visual.simulate_click(steer.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-steered-steer").is_some());
}

/// A sent message comes back to the composer to change and send again.
#[gpui_kit::test]
fn sent_messages_come_back_to_edit_and_resend(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let prompt = record
        .snapshot
        .transcript
        .iter()
        .find(|item| item.id == "user")
        .map(|item| item.text.clone())
        .expect("a sent message");
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(700.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let edit = visual
        .debug_bounds("edit-resend:user")
        .expect("edit and resend");
    visual.simulate_click(edit.center(), Modifiers::none());
    visual.simulate_input(" again");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let sent = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.prompt")
        .map(|invocation| invocation.arguments[2].clone())
        .collect::<Vec<_>>();
    assert_eq!(sent, [format!("{prompt} again")]);
}

/// A message the provider never confirmed says so, and its text can be sent again.
#[gpui_kit::test]
fn undelivered_messages_offer_to_resend_their_text(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    // The attachment the conversation still holds goes back with the text.
    let notes = serde_json::json!({"id": "notes", "kind": "file", "name": "notes.txt",
        "mime_type": "text/plain", "size_bytes": 12, "prompt_ranges": [[13, 24]]});
    record.attachments = vec![serde_json::from_value(notes.clone()).unwrap()];
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "lost", "role": "user", "text": "Lost message [notes.txt]", "complete": false,
            "attachments": [notes]}
    ]))
    .unwrap();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-undelivered-lost").is_some());
    let resend = visual.debug_bounds("native-resend-lost").expect("resend");
    visual.simulate_click(resend.center(), Modifiers::none());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let prompt = commands
        .lock()
        .unwrap()
        .iter()
        .find(|invocation| invocation.command == "agents.native.prompt")
        .map(|invocation| invocation.arguments.join("\n"))
        .expect("the restored message is sent again");
    assert!(prompt.contains("Lost message"), "{prompt}");
    assert!(
        prompt.contains("\"notes\""),
        "the held attachment goes too: {prompt}"
    );
}

/// Up in an empty composer brings back sent messages, newest first, and Down goes forward.
#[gpui_kit::test]
fn arrow_keys_recall_sent_messages(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "one", "role": "user", "text": "First idea", "complete": true},
        {"id": "reply", "role": "assistant", "text": "Done.", "complete": true},
        {"id": "two", "role": "user", "text": "Second idea", "complete": true}
    ]))
    .unwrap();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    // Up past the oldest message stays on it.
    visual.simulate_keystrokes("up up up down enter");
    visual.run_until_parked();
    let sent = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.prompt")
        .map(|invocation| invocation.arguments.join("\n"))
        .collect::<Vec<_>>();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("Second idea"), "{sent:?}");
}

/// Whether Escape in the composer of a conversation in `status` interrupts it.
fn escape_interrupts(status: &str, cx: &mut TestAppContext) -> bool {
    let record = saved_record(status);
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    commands
        .lock()
        .unwrap()
        .iter()
        .any(|invocation| invocation.command == "agents.native.interrupt")
}

/// Escape in the composer interrupts a running turn and leaves an idle conversation alone.
#[gpui_kit::test]
fn escape_interrupts_only_a_running_turn(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    assert!(escape_interrupts("working", cx));
    assert!(!escape_interrupts("idle", cx));
}

/// `/plan` turns plan mode on, waiting for the model catalog when it hasn't loaded yet instead
/// of doing nothing.
#[gpui_kit::test]
fn plan_command_waits_for_models_and_turns_plan_mode_on(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.config = NativeSessionConfig::new(AgentKind::Claude, "/tmp/project");
    record.snapshot.provider = AgentKind::Claude;
    let (sender, calls) = replying_sender(|command| {
        if command == "agents.native.models-info" {
            serde_json::json!({"models":[{"id":"opus","display_name":"Opus",
                "reasoning_efforts":[],"default_reasoning_effort":null,
                "is_default":true}],"permissions":null})
        } else {
            serde_json::json!([])
        }
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("/plan");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let configured = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.configure")
        .filter_map(|invocation| invocation.arguments.last())
        .filter_map(|selection| serde_json::from_str::<serde_json::Value>(selection).ok())
        .map(|selection| selection["plan_mode"].clone())
        .collect::<Vec<_>>();
    assert_eq!(configured, [serde_json::json!(true)]);
}

/// A narrow composer compacts its controls instead of scrolling them sideways: the permission
/// control keeps its icon, and every control stays inside the composer.
#[gpui_kit::test]
fn narrow_composers_compact_their_controls(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.config = NativeSessionConfig::new(AgentKind::Codex, "/tmp/project");
    record.snapshot.provider = AgentKind::Codex;
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    for (width, labelled) in [(1000., true), (380., false)] {
        visual.simulate_resize(size(px(width), px(700.)));
        visual.run_until_parked();
        // The composer measures itself in one frame and compacts in the next.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            visual.debug_bounds("native-permissions-label").is_some(),
            labelled,
            "permission label at {width}px"
        );
        let composer = visual.debug_bounds("native-composer").expect("composer");
        let send = visual.debug_bounds("native-send").expect("send");
        assert!(
            send.right() <= composer.right(),
            "send stays inside the composer at {width}px"
        );
    }
}

/// A wide conversation keeps its messages centered once the context card fits in the margin,
/// and moves them aside only when the card would cover them.
#[gpui_kit::test]
fn wide_conversations_center_messages_beside_the_context_card(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    let rem = visual.update(|window, _| f32::from(window.rem_size()));
    for (width, centered) in [(rem * 90., true), (rem * 80., false)] {
        visual.simulate_resize(size(px(width), px(800.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let composer = visual.debug_bounds("native-composer").expect("composer");
        let offset = (f32::from(composer.center().x) - width / 2.).abs();
        assert_eq!(offset < 1., centered, "{width}px: off center by {offset}px");
    }
}

/// Right-clicking a file link in a reply offers to open it, show it in the file manager, or
/// copy its path, without opening it.
#[gpui_kit::test]
fn file_links_offer_their_file_actions(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript[4].text = "[notes](notes.md:2)".to_owned();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let row = visual.debug_bounds("native-message-reply").expect("reply");
    let link = point(row.left().add(px(12.)), row.top().add(px(10.)));
    visual.simulate_mouse_down(link, MouseButton::Right, Modifiers::none());
    visual.simulate_mouse_up(link, MouseButton::Right, Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-link-menu").is_some());
    let opened = |commands: &CapturedCommands, name: &str| {
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|invocation| invocation.command == name)
            .map(|invocation| invocation.arguments[2..].to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        opened(&commands, "agents.native.open-link"),
        Vec::<Vec<String>>::new()
    );
    let copy = visual.debug_bounds("native-link-copy").expect("copy path");
    visual.simulate_click(copy.center(), Modifiers::none());
    let copied = visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("/tmp/project/notes.md"));
    visual.simulate_mouse_down(link, MouseButton::Right, Modifiers::none());
    visual.simulate_mouse_up(link, MouseButton::Right, Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let reveal = visual.debug_bounds("native-link-reveal").expect("reveal");
    visual.simulate_click(reveal.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        opened(&commands, "agents.native.reveal-link"),
        [vec!["notes.md:2".to_owned()]]
    );
}

/// The completion menu says what it is loading and why nothing matches.
#[gpui_kit::test]
fn completion_menus_say_what_they_load_and_why_nothing_matches(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _) = replying_sender(|command| match command {
        "agents.native.completions" => serde_json::json!({"options":[]}),
        _ => serde_json::json!([]),
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(
                Some(bootty_control::CommandTarget {
                    kind: bootty_control::ResourceKind::Binding,
                    handle: "captured-binding".into(),
                    generation: 1,
                }),
                window,
                cx,
            );
            view
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("$zzz");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual
            .debug_bounds("composer-completion-status:No skills match “zzz”")
            .is_some()
    );
}

/// `@` with words offers other conversations by title or short name; choosing one leaves its
/// name in the message and sends the conversation along to be quoted. A folder can be chosen
/// as itself rather than only drilled into.
#[gpui_kit::test]
fn composer_mentions_other_conversations_and_folders(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, calls) = replying_sender(|command| match command {
        "agents.native.completions" => serde_json::json!({"options":[]}),
        "files.complete" => serde_json::json!({"files":["src/"],"omitted":0}),
        _ => serde_json::json!([]),
    });
    let other = bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Session,
        handle: "native:codex:other".into(),
        generation: 3,
    };
    let mut session = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(
                Some(bootty_control::CommandTarget {
                    kind: bootty_control::ResourceKind::Binding,
                    handle: "captured-binding".into(),
                    generation: 1,
                }),
                window,
                cx,
            );
            view.set_mentionable_conversations(
                vec![bootty_ui::MentionableConversation {
                    target: other.clone(),
                    name: "bootty-7c".into(),
                    title: "Flaky test hunt".into(),
                }],
                cx,
            );
            view
        });
        session = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let session = session.unwrap();
    let confirm = |visual: &mut gpui_kit::VisualTestContext| {
        visual.update(|window, cx| {
            session.update(cx, |view, cx| {
                view.perform_completion(CommandAction::Confirm, window, cx);
            });
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    };
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("ask @flaky");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    confirm(visual);
    let send = visual.debug_bounds("native-send").expect("send");
    visual.simulate_click(send.center(), Modifiers::none());
    visual.run_until_parked();
    let prompt = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.command == "agents.native.prompt")
        .cloned()
        .expect("sent prompt");
    assert_eq!(prompt.arguments[2], "ask @bootty-7c");
    assert_eq!(
        prompt.arguments[10],
        serde_json::to_string(&[other]).unwrap()
    );
    // `@src/` offers the folder itself, which closes the menu with the folder named.
    calls.lock().unwrap().clear();
    visual.simulate_input("see @src/");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    confirm(visual);
    visual.simulate_click(send.center(), Modifiers::none());
    visual.run_until_parked();
    let prompt = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.command == "agents.native.prompt")
        .cloned()
        .expect("sent prompt");
    assert_eq!(prompt.arguments[2], "see @src/ ");
}

/// Lines added from a document name the project file and its lines, the way an `@` mention
/// does; a file outside the project keeps its whole path.
#[gpui_kit::test]
fn document_lines_join_the_message_as_a_file_mention(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let (sender, calls) = replying_sender(|_| serde_json::json!([]));
    let mut session = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                saved_record("idle"),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        session = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let session = session.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| {
        session.update(cx, |view, cx| {
            view.mention_lines("/tmp/project/src/main.rs", (2, 3), window, cx);
            view.mention_lines("/etc/hosts", (7, 7), window, cx);
        });
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let send = visual.debug_bounds("native-send").expect("send");
    visual.simulate_click(send.center(), Modifiers::none());
    visual.run_until_parked();
    let prompt = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.command == "agents.native.prompt")
        .cloned()
        .expect("sent prompt");
    assert_eq!(prompt.arguments[2], "@src/main.rs:2-3 @/etc/hosts:7 ");
}

/// Cmd+S puts the composer's words aside, kept across restarts; on an empty composer it brings
/// the newest stashed prompt back.
#[gpui_kit::test]
fn stashed_prompts_come_back(cx: &mut TestAppContext) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config_path = directory.path().join("config.toml");
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
        cx.set_global(bootty_ui::prompt_stash::PromptStash::open(&config_path));
    });
    let (sender, calls) = replying_sender(|_| serde_json::json!([]));
    let mut session = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                saved_record("idle"),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        session = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let session = session.unwrap();
    let stash = |visual: &mut gpui_kit::VisualTestContext| {
        visual.update(|window, cx| {
            session.update(cx, |view, cx| {
                view.perform_completion(CommandAction::Stash, window, cx);
            });
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    };
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("Rename the flag later");
    stash(visual);
    assert!(visual.debug_bounds("native-stash").is_some());
    let saved = bootty_ui::prompt_stash::PromptStash::open(&config_path);
    assert_eq!(
        saved
            .entries()
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        ["Rename the flag later"]
    );
    stash(visual);
    assert!(visual.debug_bounds("native-stash").is_none());
    let send = visual.debug_bounds("native-send").expect("send");
    visual.simulate_click(send.center(), Modifiers::none());
    visual.run_until_parked();
    let restored = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.command == "agents.native.prompt")
        .map(|call| call.arguments[2].clone());
    assert_eq!(restored.as_deref(), Some("Rename the flag later"));
}

/// Their shortcuts toggle plan mode and fast mode when the provider offers them.
#[gpui_kit::test]
fn shortcuts_toggle_the_plan_and_fast_modes_a_provider_offers(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.config = NativeSessionConfig::new(AgentKind::Claude, "/tmp/project");
    record.snapshot.provider = AgentKind::Claude;
    let (sender, calls) = replying_sender(|command| match command {
        "agents.native.models-info" => serde_json::json!({"models":[{"id":"opus",
            "display_name":"Opus","reasoning_efforts":[],"default_reasoning_effort":null,
            "is_default":true,"supports_fast_mode":true}],"permissions":null}),
        "agents.native.completions" => serde_json::json!({"options":[],"plan_mode":true}),
        _ => serde_json::json!([]),
    });
    let mut view = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let session = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(
                Some(bootty_control::CommandTarget {
                    kind: bootty_control::ResourceKind::Binding,
                    handle: "captured-binding".into(),
                    generation: 1,
                }),
                window,
                cx,
            );
            view
        });
        view = Some(session.clone());
        gpui_kit::component::Root::new(session, window, cx)
    });
    let view = view.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    // Typing a command loads what the provider offers.
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("/");
    visual.run_until_parked();
    for action in [CommandAction::TogglePlan, CommandAction::ToggleFast] {
        let performed = visual.update(|window, cx| {
            view.update(cx, |view, cx| view.perform_completion(action, window, cx))
        });
        assert!(performed, "{action:?} is offered");
        visual.run_until_parked();
    }
    let configured = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.configure")
        .filter_map(|invocation| invocation.arguments.last())
        .filter_map(|selection| serde_json::from_str::<serde_json::Value>(selection).ok())
        .map(|selection| {
            (
                selection["plan_mode"].clone(),
                selection["fast_mode"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        configured,
        [
            (serde_json::json!(true), serde_json::Value::Null),
            (serde_json::Value::Null, serde_json::json!(true)),
        ]
    );
}

/// A command sender that answers each command with `reply(command)` and keeps what it was sent.
fn replying_sender(
    reply: impl Fn(&str) -> serde_json::Value + Send + Sync + 'static,
) -> (bootty_control::AppCommandSender, CapturedCommands) {
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let inbox = mailbox.clone();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = calls.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        std::sync::Arc::new(move || {
            loop {
                let next = {
                    let inbox = inbox.lock().unwrap();
                    inbox.as_ref().unwrap().try_recv()
                };
                let Ok(request) = next else {
                    break;
                };
                let value = reply(&request.invocation.command);
                captured.lock().unwrap().push(request.invocation.clone());
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    (sender, calls)
}

/// Whether a conversation that failed with `error` offers to sign in.
fn offers_sign_in(error: &str, cx: &mut TestAppContext) -> bool {
    let mut record = saved_record("error");
    record.snapshot.error = Some(error.to_owned());
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.debug_bounds("conversation-sign-in").is_some()
}

/// A provider that needs signing in offers its sign-in flow; another failure doesn't.
#[gpui_kit::test]
fn sign_in_failures_offer_to_sign_in(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    assert!(offers_sign_in("Not logged in · Please run /login", cx));
    assert!(!offers_sign_in("stream disconnected before completion", cx));
}

/// A proposed plan shows its steps above the composer, and Implement carries it out.
#[gpui_kit::test]
fn a_ready_plan_shows_its_steps_and_implements_in_one_click(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.config = NativeSessionConfig::new(AgentKind::Codex, "/tmp/project");
    record.snapshot.provider = AgentKind::Codex;
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "ask", "role": "user", "text": "Plan the change", "complete": true},
        {"id": "plan-1", "role": "plan", "text": "# Plan\n\n1. Make the change", "complete": true}
    ]))
    .unwrap();
    record.snapshot.todos = serde_json::from_value(serde_json::json!([
        {"text": "Read the code", "status": "completed"},
        {"text": "Make the change", "status": "in_progress"}
    ]))
    .unwrap();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-message-body-plan-1").is_some());
    assert!(visual.debug_bounds("native-todos:1 of 2 done").is_some());
    let implement = visual
        .debug_bounds("native-implement-plan")
        .expect("Implement");
    visual.simulate_click(implement.center(), Modifiers::none());
    visual.run_until_parked();
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|invocation| invocation.command == "agents.native.implement"),
        "Implement runs the shared command"
    );
}

/// A reply's code block names its language and copies exactly its code.
#[gpui_kit::test]
fn code_blocks_name_their_language_and_copy_their_code(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "ask", "role": "user", "text": "Show the loop", "complete": true},
        {"id": "reply", "role": "assistant", "complete": true,
         "text": "Here it is:\n\n```rust\nfor item in items {\n    println!(\"{item}\");\n}\n```\n"}
    ]))
    .unwrap();
    let (sender, _commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let copy = visual.debug_bounds("copy-code:rust").expect("copy code");
    visual.simulate_click(copy.center(), Modifiers::none());
    visual.update(|_, cx| {
        pretty_assertions::assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("for item in items {\n    println!(\"{item}\");\n}".to_owned())
        );
    });
}

/// A resume rebuilds the completion; app chips already in the draft still send.
#[gpui_kit::test]
fn app_chips_survive_a_rebuilt_completion(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = commands.clone();
    let inbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        std::sync::Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                let value = if request.invocation.command == "agents.native.completions" {
                    serde_json::json!({"options":[{"kind":"app","name":"github",
                        "display_name":"GitHub","description":"Repositories",
                        "argument_hint":null,"path":"app://github","scope":null}]})
                } else {
                    serde_json::Value::Null
                };
                observed.lock().unwrap().push(request.invocation.clone());
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let binding = |handle: &str| bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Binding,
        handle: handle.into(),
        generation: 1,
    };
    let mut session = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(Some(binding("first")), window, cx);
            view
        });
        session = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let session = session.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input("@Git");
    visual.run_until_parked();
    visual.update(|window, cx| {
        session.update(cx, |view, cx| {
            view.perform_completion(CommandAction::Confirm, window, cx);
        });
    });
    visual.run_until_parked();
    visual.update(|window, cx| {
        session.update(cx, |view, cx| {
            view.set_completion_binding(Some(binding("second")), window, cx);
        });
    });
    visual.run_until_parked();
    visual.simulate_input(" review this");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let sent = commands.lock().unwrap().clone();
    let prompt = sent
        .iter()
        .find(|invocation| invocation.command == "agents.native.prompt")
        .unwrap_or_else(|| panic!("the draft with its app chip is sent: {sent:?}"));
    assert!(
        prompt
            .arguments
            .get(9)
            .is_some_and(|mentions| mentions.contains("\"name\":\"github\"")),
        "{:?}",
        prompt.arguments
    );
}

/// A failed first message comes back with its attachment chips, so sending it again keeps them.
#[gpui_kit::test]
fn a_restored_failed_draft_keeps_its_attachments(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("stopped");
    record.pending_initial_message = Some("Review [notes.txt] please".into());
    record.attachments = serde_json::from_value(serde_json::json!([
        {"id":"placed","kind":"file","name":"notes.txt","mime_type":"text/plain",
         "size_bytes":12,"prompt_ranges":[{"start":7,"end":18}]},
        {"id":"unplaced","kind":"file","name":"summary.md","mime_type":"text/markdown",
         "size_bytes":40}
    ]))
    .unwrap();
    let mut resumed = serde_json::to_value(&record).unwrap();
    resumed["generation"] = 2.into();
    resumed["snapshot"]["status"] = "idle".into();
    resumed["pending_initial_message"] = serde_json::Value::Null;
    let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = commands.clone();
    let inbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        std::sync::Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                let value = if request.invocation.command == "agents.native.resume" {
                    resumed.clone()
                } else {
                    serde_json::Value::Null
                };
                observed.lock().unwrap().push(request.invocation.clone());
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let resume = visual.debug_bounds("native-resume").expect("resume");
    visual.simulate_click(resume.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let sent = commands.lock().unwrap().clone();
    let prompt = sent
        .iter()
        .find(|invocation| invocation.command == "agents.native.prompt")
        .unwrap_or_else(|| panic!("the restored draft is sent: {sent:?}"));
    let attachments: Vec<String> = serde_json::from_str(&prompt.arguments[4]).unwrap();
    pretty_assertions::assert_eq!(attachments.len(), 2, "{:?}", prompt.arguments);
    assert!(attachments.contains(&"placed".to_owned()));
    assert!(attachments.contains(&"unplaced".to_owned()));
}

#[gpui_kit::test]
fn working_composer_queues_by_default_and_command_enter_steers(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = commands.clone();
    let inbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                observed
                    .lock()
                    .unwrap()
                    .push(request.invocation.command.clone());
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value: serde_json::Value::Null,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                saved_record("working"),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input("Follow up after this turn");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "agents.native.queue")
    );
    visual.simulate_input("Steer this turn");
    visual.simulate_keystrokes("cmd-enter");
    visual.run_until_parked();
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "agents.native.prompt")
    );
}

#[gpui_kit::test]
fn queued_drawer_stays_attached_and_keeps_actions_reachable_at_zoom(cx: &mut TestAppContext) {
    use bootty_agents::{NativeQueuedMessage, NativeQueuedMessageStatus};
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    record.queued_messages = [
        ("next", NativeQueuedMessageStatus::Queued),
        ("failed", NativeQueuedMessageStatus::Failed),
        ("sending", NativeQueuedMessageStatus::Sending),
    ]
    .into_iter()
    .map(|(id, status)| NativeQueuedMessage {
        id: id.into(),
        text: "A long queued follow-up that should stay readable without pushing its actions out of the pane".into(),
        preview_text: "A long queued follow-up that should stay readable without pushing its actions out of the pane".into(),
        attachments: Vec::new(),
        status,
        steer: false,
    })
    .collect();
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    verify_queue_drawer_geometry(visual);
    let header = visual.debug_bounds("native-queued-header").unwrap();
    visual.simulate_click(header.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-queued-row:next").is_none());
    let header = visual.debug_bounds("native-queued-header").unwrap();
    visual.simulate_click(header.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for (operation, id, selector) in [
        ("queue-steer", "next", "native-queue-steer:next"),
        ("queue-retry", "failed", "native-queue-retry:failed"),
        ("queue-cancel", "next", "native-queue-cancel:next"),
    ] {
        let bounds = visual.debug_bounds(selector).unwrap();
        visual.simulate_click(bounds.center(), Modifiers::none());
        visual.run_until_parked();
        let invocation = calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.command == format!("agents.native.{operation}"))
            .cloned()
            .expect("queue action reaches the conversation owner");
        pretty_assertions::assert_eq!(invocation.target, Some(target.clone()));
        pretty_assertions::assert_eq!(invocation.arguments.last().map(String::as_str), Some(id));
    }
    let sending = visual.debug_bounds("native-queue-cancel:sending").unwrap();
    let before = calls.lock().unwrap().len();
    visual.simulate_click(sending.center(), Modifiers::none());
    visual.run_until_parked();
    pretty_assertions::assert_eq!(calls.lock().unwrap().len(), before);
}

fn verify_queue_drawer_geometry(visual: &mut gpui_kit::VisualTestContext) {
    for (width, font_size) in [(960., 16.), (400., 20.)] {
        visual.update(|window, cx| {
            gpui_kit::component::Theme::global_mut(cx).font_size = px(font_size);
            window.refresh();
        });
        visual.simulate_resize(size(px(width), px(720.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let drawer = visual.debug_bounds("native-queue").unwrap();
        let composer = visual.debug_bounds("native-composer").unwrap();
        pretty_assertions::assert_eq!(
            visual.debug_bounds("native-queue-label").unwrap().left(),
            visual
                .debug_bounds("native-queued-text:next")
                .unwrap()
                .left(),
        );
        assert!(drawer.left() > composer.left() && drawer.right() < composer.right());
        assert!(
            drawer.bottom() > composer.top(),
            "drawer shares the composer's seam"
        );
        for selector in [
            "native-queued-header",
            "native-queue-steer:next",
            "native-queue-retry:failed",
            "native-queue-cancel:next",
        ] {
            let bounds = visual.debug_bounds(selector).expect("queue action");
            assert!(bounds.left() >= drawer.left() && bounds.right() <= drawer.right());
            assert!(
                bounds.bottom() <= composer.top(),
                "{selector} remains above the seam"
            );
        }
    }
}

/// A subagent's tab draws its conversation with the conversation view's own rows, follows it
/// while it runs, rests while hidden, and stops once the subagent settles.
#[gpui_kit::test]
fn child_tabs_draw_the_conversation_and_follow_it_while_running(cx: &mut TestAppContext) {
    use bootty_ui::SubagentPanel;
    use gpui_kit::AppContext as _;
    use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, PanelId, panel_handle};
    use std::sync::{Arc, Mutex};

    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let (record, mut reply) = subagent_record_and_reply();
    reply["agent"]["status"] = serde_json::json!("running");
    let reply = Arc::new(Mutex::new(reply));
    let (sender, reads) = subagent_command_sender(reply.clone());
    let mut panels = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let parent = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        let child =
            cx.new(|cx| SubagentPanel::new(parent.clone(), "review".into(), "audit".into(), cx));
        let other =
            cx.new(|cx| SubagentPanel::new(parent.clone(), "missing".into(), "other".into(), cx));
        let area = cx.new(|cx| {
            DockArea::new("child-test", None, window, cx).with_renderer(DockSkin::new(cx))
        });
        area.update(cx, |area, cx| {
            area.set_center(
                DockLayout::tabs()
                    .panel_view(panel_handle(child.clone()), cx)
                    .panel_view(panel_handle(other.clone()), cx),
                window,
                cx,
            );
        });
        panels = Some((child, other, area.clone()));
        gpui_kit::component::Root::new(area, window, cx)
    });
    let (child, other, area) = panels.unwrap();
    visual.simulate_resize(size(px(900.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        !reads.lock().unwrap().is_empty(),
        "opening the tab reads it"
    );
    assert!(
        visual.debug_bounds("native-message-audit-answer").is_some(),
        "the reply is a conversation row"
    );
    assert!(visual.debug_bounds("native-working").is_some());
    assert!(
        visual.debug_bounds("subagent-stop:review").is_some(),
        "a running subagent can be stopped from its tab"
    );
    assert!(
        visual.debug_bounds("native-composer").is_none(),
        "a subagent's tab has no composer"
    );
    let before = reads.lock().unwrap().len();
    wait(visual, 2);
    assert!(
        reads.lock().unwrap().len() > before,
        "a running subagent is followed"
    );

    let select = |panel: PanelId, visual: &mut gpui_kit::VisualTestContext| {
        visual.update(|window, cx| {
            area.update(cx, |area, cx| area.select_panel(panel, window, cx));
        });
        visual.run_until_parked();
    };
    select(PanelId::from(other.entity_id()), visual);
    let hidden = reads.lock().unwrap().len();
    wait(visual, 4);
    assert_eq!(reads.lock().unwrap().len(), hidden, "a hidden tab rests");
    select(PanelId::from(child.entity_id()), visual);
    assert!(
        reads.lock().unwrap().len() > hidden,
        "showing it reads again"
    );

    // Once the subagent settles, the tab reads it once more and then rests.
    reply.lock().unwrap()["agent"]["status"] = serde_json::json!("completed");
    wait(visual, 2);
    wait(visual, 2);
    let settled = reads.lock().unwrap().len();
    wait(visual, 6);
    assert_eq!(
        reads.lock().unwrap().len(),
        settled,
        "a finished subagent rests"
    );
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-working").is_none());
    assert!(visual.debug_bounds("subagent-stop:review").is_none());
    assert!(visual.debug_bounds("native-message-audit-answer").is_some());
}

/// A running subagent stops from its parent's subagent list, through the parent conversation.
#[gpui_kit::test]
fn running_subagents_stop_from_the_parent(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let (record, _) = subagent_record_and_reply();
    let target = record.target();
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let group = visual
        .debug_bounds("subagent-group")
        .expect("subagent group");
    visual.simulate_click(group.center(), Modifiers::none());
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let stop = visual
        .debug_bounds("subagent-stop:review")
        .expect("stop control");
    visual.simulate_click(stop.center(), Modifiers::none());
    visual.run_until_parked();
    let stops = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.subagent-stop")
        .map(|invocation| (invocation.target.clone(), invocation.arguments.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        stops,
        [(
            Some(target),
            vec!["saved-pi".to_owned(), "1".to_owned(), "review".to_owned()]
        )]
    );
}

/// Lets `seconds` of the test clock pass and settles the window.
fn wait(visual: &gpui_kit::VisualTestContext, seconds: u64) {
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(seconds));
    visual.run_until_parked();
}

fn subagent_command_sender(
    response: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
) -> (bootty_control::AppCommandSender, CapturedCommands) {
    use std::sync::{Arc, Mutex};
    let mailbox = Arc::new(Mutex::new(None::<bootty_control::AppCommandReceiver>));
    let inbox = mailbox.clone();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let captured = reads.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        Arc::new(move || {
            loop {
                let next = inbox.lock().unwrap().as_ref().unwrap().try_recv();
                let Ok(request) = next else {
                    break;
                };
                let value = if request.invocation.command == "agents.native.subagent-read" {
                    captured.lock().unwrap().push(request.invocation.clone());
                    response.lock().unwrap().clone()
                } else {
                    serde_json::json!([])
                };
                request
                    .response
                    .send(bootty_control::CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    (sender, reads)
}

fn subagent_record_and_reply() -> (NativeSessionRecord, serde_json::Value) {
    let mut record = saved_record("idle");
    let agent = serde_json::json!({
        "id":"review", "title":"History audit", "prompt":"Review history", "thread_id":"child-thread",
        "owner_session":"provider-session", "status":"running"
    });
    record.snapshot.transcript.push(
        serde_json::from_value(serde_json::json!({
            "id":"subagent:review", "role":"subagent", "text":"", "complete":false, "subagent":agent
        }))
        .unwrap(),
    );
    let mut reply = serde_json::json!({
        "agent":agent,
        "transcript":[{"id":"audit-answer", "role":"assistant", "text":"The audit is finished", "complete":true}]
    });
    reply["agent"]["status"] = serde_json::json!("completed");
    (record, reply)
}

#[gpui_kit::test]
fn app_access_card_offers_conversation_and_permanent_grants(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("waiting");
    record.snapshot.requests = serde_json::from_value(serde_json::json!([{
        "id":"computer-access:dev.zed.Zed", "method":bootty_agents::APPLICATION_ACCESS_METHOD,
        "parameters":{"bundle_id":"dev.zed.Zed","name":"Zed"}
    }]))
    .expect("app access request");
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(600.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let card = "computer-access:dev.zed.Zed";
    for selector in [
        "deny:computer-access:dev.zed.Zed",
        "allow-session:computer-access:dev.zed.Zed",
        "always-allow:computer-access:dev.zed.Zed",
    ] {
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{selector} is offered"
        );
    }
    assert!(
        visual
            .debug_bounds("allow:computer-access:dev.zed.Zed")
            .is_none(),
        "a one-call grant is not offered"
    );
    let always = visual
        .debug_bounds("always-allow:computer-access:dev.zed.Zed")
        .expect("always allow control");
    visual.simulate_click(always.center(), Modifiers::none());
    let invocation = calls
        .lock()
        .unwrap()
        .iter()
        .find(|invocation| invocation.command == "agents.native.approve")
        .cloned()
        .expect("the decision submits through the command owner");
    pretty_assertions::assert_eq!(invocation.target, Some(target));
    pretty_assertions::assert_eq!(
        invocation.arguments[2..],
        [card.to_owned(), "always-allow".to_owned()]
    );
}

#[gpui_kit::test]
fn compaction_is_an_operation_instead_of_a_model_response(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for (status, operation, selector) in [
        (
            "working",
            "running",
            "native-compaction-status:Compacting context",
        ),
        (
            "idle",
            "completed",
            "native-compaction-status:Context compacted",
        ),
        (
            "idle",
            "interrupted",
            "native-compaction-status:Compaction interrupted",
        ),
        (
            "error",
            "interrupted",
            "native-compaction-status:Compaction failed",
        ),
    ] {
        let mut record = saved_record(status);
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id":"compact", "role":"tool", "text":"", "complete":operation != "running",
             "tool":{"name":"contextCompaction", "input":"", "status":operation}}
        ]))
        .unwrap();
        let (sender, _receiver) =
            bootty_control::app_command_channel(8, std::sync::Arc::new(|| {}));
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    false,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(960.), px(790.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
        assert!(visual.debug_bounds("native-message-body-compact").is_none());
        assert!(visual.debug_bounds("tool-disclosure:compact").is_none());
        assert!(visual.debug_bounds("copy-message:compact").is_none());
        if operation == "running" {
            assert!(
                visual.debug_bounds("native-progress-status").is_none(),
                "compaction owns its progress display"
            );
        }
    }
}

#[gpui_kit::test]
fn async_question_answers_steer_without_consuming_the_composer_draft(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for free_text in [false, true] {
        let mut record = saved_record("working");
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id":"user","role":"user","text":"Continue working","complete":true},
            {"id":"question","role":"assistant","text":"Which option?","complete":true,
             "questions":[{"title":"Which option?","options":["A","B"]}]},
            {"id":"more-work","role":"assistant","text":"Continuing the task","complete":false}
        ]))
        .unwrap();
        let target = record.target();
        let (sender, calls) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1000.), px(900.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let composer = visual.debug_bounds("native-composer").unwrap();
        visual.simulate_click(composer.center(), Modifiers::none());
        visual.simulate_input("Unrelated draft");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let value = if free_text {
            let input = visual.debug_bounds("async-input:question:0").unwrap();
            visual.simulate_click(input.center(), Modifiers::none());
            visual.simulate_input("A custom\nmultiline answer");
            "A custom\nmultiline answer"
        } else {
            let option = visual.debug_bounds("async-option:question:0:B").unwrap();
            visual.simulate_click(option.center(), Modifiers::none());
            "B"
        };
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let send = visual.debug_bounds("async-answer:question").unwrap();
        visual.simulate_click(send.center(), Modifiers::none());
        visual.run_until_parked();
        let answer = calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.command == "agents.native.prompt")
            .cloned()
            .expect("a question answer steers immediately");
        pretty_assertions::assert_eq!(answer.target, Some(target));
        pretty_assertions::assert_eq!(answer.arguments[2], format!("Which option?\n{value}"));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let composer = visual.debug_bounds("native-composer").unwrap();
        visual.simulate_click(composer.center(), Modifiers::none());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let draft = calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.command == "agents.native.queue")
            .cloned()
            .expect("the composer draft remains sendable");
        pretty_assertions::assert_eq!(draft.arguments[2], "Unrelated draft");
    }
}

/// A conversation paste goes through the exact session's import instead of the prompt limit.
#[gpui_kit::test]
fn large_conversation_paste_uses_attachment_import(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let target = record.target();
    let (sender, calls) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                false,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input("Existing draft");
    visual.update(|_, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("🥟".repeat(8192)));
    });
    visual.simulate_keystrokes("cmd-v");
    visual.run_until_parked();
    let calls = calls.lock().unwrap();
    let import = calls
        .iter()
        .find(|call| call.command == "agents.native.import")
        .unwrap();
    pretty_assertions::assert_eq!(import.target, Some(target));
    pretty_assertions::assert_eq!(
        std::path::Path::new(&import.arguments[2]).extension(),
        Some(std::ffi::OsStr::new("txt"))
    );
    assert!(
        !calls
            .iter()
            .any(|call| call.command == "agents.native.prompt")
    );
    drop(calls);
    // A rejected import keeps existing text, so the user can retry it.
    visual.simulate_keystrokes("cmd-a cmd-c");
    let draft = visual
        .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap();
    pretty_assertions::assert_eq!(draft, "Existing draft");
}

#[gpui_kit::test]
fn add_menu_inserts_provider_commands_as_editable_slash_text(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(
                Some(bootty_control::CommandTarget {
                    kind: bootty_control::ResourceKind::Binding,
                    handle: "captured-binding".into(),
                    generation: 1,
                }),
                window,
                cx,
            );
            view
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let add = visual.debug_bounds("native-attach").unwrap();
    visual.simulate_click(add.center(), Modifiers::none());
    visual.run_until_parked();
    visual.simulate_keystrokes("up");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let command = visual
        .debug_bounds("composer-add-Command-compact")
        .expect("provider command submenu");
    visual.simulate_click(command.center(), Modifiers::none());
    visual.run_until_parked();
    visual.simulate_keystrokes("cmd-a cmd-c");
    let text = visual
        .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap();
    pretty_assertions::assert_eq!(text, "/compact ");
}

#[gpui_kit::test]
fn parsed_file_changes_keep_the_providers_failure_output(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for status in ["failed", "completed"] {
        let mut record = saved_record("idle");
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id":"prompt","role":"user","text":"Edit the file","complete":true},
            {"id":"edit","role":"tool","text":"Patch could not be applied to src/main.rs.","complete":true,
             "tool":{"name":"fileChange","status":status,"input":"[{\"path\":\"/tmp/project/src/main.rs\",\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\\n\"}]"}}
        ])).unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual
                .debug_bounds("native-file-change:src/main.rs:+1:-1")
                .is_some()
        );
        let disclosure = visual.debug_bounds("tool-disclosure:edit").unwrap();
        visual.simulate_click(disclosure.center(), Modifiers::none());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        pretty_assertions::assert_eq!(
            visual.debug_bounds("failed-change-output:edit").is_some(),
            status == "failed"
        );
    }
}

#[gpui_kit::test]
fn settled_replies_without_work_folds_show_elapsed_time(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for (status, tool, duration) in [
        ("idle", None, true),
        ("idle", Some("fileChange"), true),
        ("idle", Some("Bash"), false),
        ("working", None, false),
    ] {
        let mut record = saved_record(status);
        let mut transcript = vec![
            serde_json::json!({"id":"prompt","role":"user","text":"Review","complete":true,"created_at":1_000_000}),
        ];
        if let Some(tool) = tool {
            transcript.push(serde_json::json!({"id":"tool","role":"tool","text":"","complete":true,"created_at":1_010_000,"tool":{"name":tool,"status":"completed","input":"[]"}}));
        }
        transcript.push(serde_json::json!({"id":"reply","role":"assistant","text":"Reviewed","complete":true,"created_at":1_050_000,"updated_at":1_072_000}));
        record.snapshot.transcript = serde_json::from_value(serde_json::json!(transcript)).unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        pretty_assertions::assert_eq!(
            visual
                .debug_bounds("native-turn-duration:reply:1m 12s")
                .is_some(),
            duration,
            "{status}, {tool:?}"
        );
    }
}

/// A conversation with a few messages to read through, open in its view.
fn reading_view(
    cx: &mut TestAppContext,
) -> (
    Entity<NativeAgentSessionView>,
    &mut gpui_kit::VisualTestContext,
) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "ask", "role": "user", "text": "Where is the needle?", "complete": true},
        {"id": "answer", "role": "assistant",
            "text": "One needle here.\n\nAnother **NEEDLE** there.", "complete": true},
        {"id": "thanks", "role": "user", "text": "Thanks", "complete": true},
        {"id": "done", "role": "assistant", "text": "You are welcome.", "complete": true,
            "usage": {"input": 48905, "output": 95, "cache_read": 48000, "cache_write": 900,
                "cost_micros": 42100}}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let mut conversation = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        conversation = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(700.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    (conversation.unwrap(), visual)
}

fn redraw(visual: &mut gpui_kit::VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

/// Find counts a query's matches across the conversation, ignoring case, steps through them
/// with Enter and Shift-Enter, and closes with Escape.
#[gpui_kit::test]
fn find_steps_through_matches_across_messages(cx: &mut TestAppContext) {
    let (view, visual) = reading_view(cx);
    view.update_in(visual, |view, window, cx| {
        view.find_action(
            &bootty_ui::app_actions::TerminalFindAction::Prompt,
            window,
            cx,
        );
    });
    visual.simulate_input("needle");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-find-count:1 of 3")
            .is_some()
    );
    visual.simulate_keystrokes("enter");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-find-count:2 of 3")
            .is_some()
    );
    visual.simulate_keystrokes("enter enter");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-find-count:1 of 3")
            .is_some()
    );
    visual.simulate_keystrokes("shift-enter");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-find-count:3 of 3")
            .is_some()
    );
    visual.simulate_input("x");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-find-count:No results")
            .is_some()
    );
    visual.simulate_keystrokes("escape");
    redraw(visual);
    assert!(visual.debug_bounds("conversation-find").is_none());
}

/// Copy mode moves through every message with vim's keys, copies a selection, and quotes one
/// from a reply into a comment.
#[gpui_kit::test]
fn copy_mode_selects_copies_and_quotes_with_the_keyboard(cx: &mut TestAppContext) {
    let (view, visual) = reading_view(cx);
    let enter = |visual: &mut gpui_kit::VisualTestContext| {
        view.update_in(visual, NativeAgentSessionView::enter_copy_mode);
        redraw(visual);
    };
    enter(visual);
    assert!(
        visual
            .debug_bounds("conversation-copy-mode:Copy mode")
            .is_some()
    );
    visual.simulate_keystrokes("g g 2 w v");
    redraw(visual);
    assert!(
        visual
            .debug_bounds("conversation-copy-mode:Visual")
            .is_some()
    );
    visual.simulate_keystrokes("e y");
    redraw(visual);
    let copied = visual.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("the"));
    assert!(
        visual
            .debug_bounds("conversation-copy-mode:Copied")
            .is_some()
    );
    // A reply's words quote into a comment; the provider reads their markdown.
    enter(visual);
    visual.simulate_keystrokes("g g j j w v e c");
    redraw(visual);
    assert!(visual.debug_bounds("response-quote-editor").is_some());
    assert!(
        visual
            .debug_bounds("conversation-copy-mode:Copy mode")
            .is_none()
    );
}

/// A prompt sent from here moves to the top of the transcript with room below for its reply,
/// and the transcript follows the reply's end again once the reply outgrows that room.
#[gpui_kit::test]
fn sent_prompts_hold_the_top_while_their_reply_comes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let long = "A long earlier reply that takes several lines to read. ".repeat(12);
    let mut record = saved_record("idle");
    record.snapshot.transcript = (0..6)
        .flat_map(|turn| {
            [
                serde_json::json!({"id": format!("ask-{turn}"), "role": "user",
                    "text": format!("Question {turn}"), "complete": true}),
                serde_json::json!({"id": format!("answer-{turn}"), "role": "assistant",
                    "text": long, "complete": true}),
            ]
        })
        .map(|item| serde_json::from_value(item).unwrap())
        .collect();
    let (sender, commands) = native_command_sender(&record);
    let mut conversation = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record.clone(),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        conversation = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let view = conversation.unwrap();
    visual.simulate_resize(size(px(900.), px(700.)));
    redraw(visual);
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("What next?");
    visual.simulate_keystrokes("enter");
    redraw(visual);
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.command == "agents.native.prompt")
    );
    // The provider shows the prompt and the start of its reply, then replaces the streaming
    // reply with its longer text.
    let mut show = |items: &[serde_json::Value], visual: &mut gpui_kit::VisualTestContext| {
        record.snapshot.revision = record.snapshot.revision.saturating_add(1);
        record.snapshot.transcript.retain(|item| item.id != "reply");
        record.snapshot.status = bootty_agents::NativeSessionStatus::Working;
        for item in items {
            record
                .snapshot
                .transcript
                .push(serde_json::from_value(item.clone()).unwrap());
        }
        view.update_in(visual, |view, window, cx| {
            view.update_record(record.clone(), window, cx);
        });
        for _ in 0..3 {
            redraw(visual);
        }
    };
    show(
        &[
            serde_json::json!({"id": "sent", "role": "user", "text": "What next?", "complete": true}),
            serde_json::json!({"id": "reply", "role": "assistant", "text": "Start.", "complete": false}),
        ],
        visual,
    );
    let prompt = visual.debug_bounds("native-message-sent").unwrap();
    assert!(prompt.top() < px(120.), "{prompt:?}");
    // A reply longer than the room lets the transcript follow its end.
    show(
        &[serde_json::json!({"id": "reply", "role": "assistant",
            "text": "A long reply line.\n\n".repeat(40), "complete": false})],
        visual,
    );
    let composer = visual.debug_bounds("native-composer").unwrap();
    let reply = visual.debug_bounds("native-message-reply").unwrap();
    assert!(
        f32::from(reply.bottom()) <= f32::from(composer.top()) + 64.,
        "{reply:?} {composer:?}"
    );
    assert!(
        visual
            .debug_bounds("native-message-sent")
            .is_none_or(|sent| sent.top() < px(0.))
    );
}

/// A turn's reply says what the turn used and cost.
#[gpui_kit::test]
fn turn_usage_shows_on_the_turns_reply(cx: &mut TestAppContext) {
    let (_, visual) = reading_view(cx);
    assert!(
        visual
            .debug_bounds("turn-usage:49k in · 95 out · $0.04")
            .is_some()
    );
}

/// The context meter's menu compacts the conversation the way `/compact` does, and keeps the
/// draft.
#[gpui_kit::test]
fn context_meter_compacts_the_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.usage =
        Some(serde_json::json!({"input": 150_000, "modelContextWindow": 200_000}));
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(700.)));
    redraw(visual);
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("A draft");
    let meter = visual.debug_bounds("native-context-trigger").unwrap();
    visual.simulate_click(meter.center(), Modifiers::none());
    redraw(visual);
    visual.simulate_keystrokes("down enter");
    redraw(visual);
    // The draft stays to send afterwards.
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_keystrokes("enter");
    redraw(visual);
    let prompts = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.command == "agents.native.prompt")
        .map(|call| call.arguments[2].clone())
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["/compact", "A draft"]);
}

/// A reply's image that names a file shows that file, read from the conversation's directory;
/// one whose file is missing says so where the image would be.
#[gpui_kit::test]
fn reply_images_show_files_from_the_conversation_directory(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let directory = assert_fs::TempDir::new().unwrap();
    image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 255]))
        .save(directory.path().join("chart.png"))
        .unwrap();
    let mut record = saved_record("idle");
    record.config.cwd = directory.path().to_path_buf();
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "reply", "role": "assistant", "complete": true,
            "text": "Here it is:\n\n![Chart](chart.png)\n\n![Gone](gone.png)"}
    ]))
    .unwrap();
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    redraw(visual);
    let root = directory.path().to_string_lossy().into_owned();
    // Selectors are static; these name this test's directory.
    let (chart, gone) = (
        format!("markdown-image:{root}/chart.png").leak(),
        format!("markdown-image:{root}/gone.png").leak(),
    );
    let chart = visual.debug_bounds(chart).expect("the chart shows");
    assert_eq!(
        (f32::from(chart.size.width), f32::from(chart.size.height)),
        (40., 20.)
    );
    assert!(
        visual.debug_bounds(gone).is_some(),
        "the missing image says so"
    );
}

/// A reply's HTML block offers to open the page in the browser; other code doesn't.
#[gpui_kit::test]
fn html_blocks_open_in_the_browser(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let offers = |text: &str, cx: &mut TestAppContext| {
        let mut record = saved_record("idle");
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id": "reply", "role": "assistant", "complete": true, "text": text}
        ]))
        .unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        redraw(visual);
        visual.debug_bounds("preview-html").is_some()
    };
    assert!(offers("```html\n<h1>Hi</h1>\n```\n", cx));
    assert!(!offers("```rust\nfn main() {}\n```\n", cx));
}

/// A reply's finished Mermaid fence shows the drawn diagram; one still streaming shows as code.
#[gpui_kit::test]
fn mermaid_fences_draw_once_finished(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let shows = |text: &str, cx: &mut TestAppContext| {
        let mut record = saved_record("idle");
        record.snapshot.transcript = serde_json::from_value(serde_json::json!([
            {"id": "reply", "role": "assistant", "complete": true, "text": text}
        ]))
        .unwrap();
        let (sender, _) = native_command_sender(&record);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                NativeAgentSessionView::new(
                    record,
                    sender.for_caller(Caller::Internal),
                    true,
                    window,
                    cx,
                )
            });
            gpui_kit::component::Root::new(view, window, cx)
        });
        redraw(visual);
        redraw(visual);
        visual.debug_bounds("markdown-diagram").is_some()
    };
    assert!(shows(
        "The flow:\n\n```mermaid\nflowchart LR\n  Plan --> Build\n```\n",
        cx
    ));
    assert!(!shows(
        "The flow:\n\n```mermaid\nflowchart LR\n  Plan --> Bu",
        cx
    ));
}

/// The composer shows the conversation's branch and switches its checkout to another local
/// branch through the conversation's binding.
#[gpui_kit::test]
fn the_composer_switches_the_checkout_branch(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let cwd = record.config.cwd.to_string_lossy().into_owned();
    let (sender, commands) = native_command_sender(&record);
    let binding = bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Binding,
        handle: "binding".to_owned(),
        generation: 1,
    };
    let context = bootty_ui::ConversationContext {
        project: None,
        branch: Some("feature".to_owned()),
        pull_request: None,
        binding: Some(binding.clone()),
        diff: None,
        target: None,
    };
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.set_conversation_context(Some(context), cx);
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    let picker = visual.debug_bounds("native-branch-picker").unwrap();
    visual.simulate_click(picker.center(), Modifiers::none());
    redraw(visual);
    visual.simulate_keystrokes("down enter");
    redraw(visual);
    let git = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.command.starts_with("git."))
        .map(|call| {
            (
                call.command.clone(),
                call.arguments.clone(),
                call.target.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        git,
        [
            (
                "git.overview".to_owned(),
                vec![cwd.clone(), "1".to_owned()],
                Some(binding.clone())
            ),
            (
                "git.status".to_owned(),
                vec![cwd.clone()],
                Some(binding.clone())
            ),
            (
                "git.branch-checkout".to_owned(),
                vec![cwd.clone(), "main".to_owned()],
                Some(binding.clone())
            ),
            // The branches are read again for the checkout's new state.
            (
                "git.overview".to_owned(),
                vec![cwd.clone(), "1".to_owned()],
                Some(binding.clone())
            ),
            ("git.status".to_owned(), vec![cwd], Some(binding)),
        ]
    );
}

/// The composer drawer shows the conversation's goal with what it has spent, and pauses or
/// removes it.
#[gpui_kit::test]
fn goals_show_what_they_spent_and_pause_or_go(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.goal = Some(bootty_agents::NativeGoal {
        objective: "Ship the release".into(),
        status: bootty_agents::NativeGoalStatus::Active,
        token_budget: Some(50_000),
        tokens_used: 1200,
        seconds_used: 90,
    });
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(700.)));
    redraw(visual);
    assert!(
        visual
            .debug_bounds("native-goal-state:Working on it · 1k of 50k tokens")
            .is_some()
    );
    for control in ["native-goal-toggle", "native-goal-remove"] {
        let button = visual.debug_bounds(control).unwrap();
        visual.simulate_click(button.center(), Modifiers::none());
        redraw(visual);
    }
    let sent = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.command.starts_with("agents.native.goal"))
        .map(|call| (call.command.clone(), call.arguments[2].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        sent,
        [
            ("agents.native.goal-status".to_owned(), "paused".to_owned()),
            ("agents.native.goal".to_owned(), String::new()),
        ]
    );
}

/// A turn a usage limit stopped says when the limit resets, and can wait to send the turn
/// again then.
#[gpui_kit::test]
fn limit_stopped_turns_can_retry_when_the_limit_resets(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.error = Some("You've hit your usage limit".into());
    record.snapshot.rate_limits = Some(bootty_agents::NativeRateLimits {
        windows: vec![bootty_agents::NativeRateLimit {
            label: "5h".into(),
            used_percent: 100,
            resets_at: Some(4_102_444_800),
            window_seconds: Some(18_000),
        }],
        reached: true,
    });
    let (sender, commands) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    assert!(visual.debug_bounds("native-limit-resets").is_some());
    for scheduled in [false, true] {
        let toggle = visual
            .debug_bounds(if scheduled {
                "native-retry-at-reset:true"
            } else {
                "native-retry-at-reset:false"
            })
            .unwrap();
        visual.simulate_click(toggle.center(), Modifiers::none());
        redraw(visual);
    }
    assert!(visual.debug_bounds("native-retry-at-reset:false").is_some());
    assert!(
        commands
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.command != "agents.native.prompt")
    );
}

/// A finished plan can be carried out in a new conversation instead of this one.
#[gpui_kit::test]
fn ready_plans_can_go_to_a_new_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    record.snapshot.transcript.push(
        serde_json::from_value(serde_json::json!({
            "id": "plan", "role": "plan", "text": "1. Fix the bug", "complete": true
        }))
        .unwrap(),
    );
    let (sender, _) = native_command_sender(&record);
    let plans = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let carried = plans.clone();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        cx.subscribe(
            &view,
            move |_, _, event: &bootty_ui::OpenNativeSession, _| {
                if let bootty_ui::OpenNativeSession::ImplementInNew { plan, .. } = event {
                    carried.lock().unwrap().push(plan.clone());
                }
            },
        )
        .detach();
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    let elsewhere = visual.debug_bounds("native-implement-plan-new").unwrap();
    visual.simulate_click(elsewhere.center(), Modifiers::none());
    redraw(visual);
    assert_eq!(*plans.lock().unwrap(), ["1. Fix the bug"]);
}

/// A comment on changed lines goes into the draft with the file, the lines and their text, and
/// is sent like anything else written there.
#[gpui_kit::test]
fn diff_comments_join_the_draft_with_their_lines(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, commands) = native_command_sender(&record);
    let mut conversation = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        conversation = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    conversation.unwrap().update_in(visual, |view, window, cx| {
        view.add_diff_comment(
            &bootty_ui::DiffComment {
                anchor: bootty_git::diff::DiffAnchor {
                    path: "src/main.rs".into(),
                    side: bootty_git::diff::DiffSide::Left,
                    start_line: 2,
                    line: 3,
                },
                quote: "-- old comment\nlet x = 1;".into(),
                body: "Why remove these?".into(),
            },
            window,
            cx,
        );
    });
    visual.simulate_keystrokes("enter");
    redraw(visual);
    let sent = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.command == "agents.native.prompt")
        .map(|call| call.arguments[2].trim_end().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        sent,
        [
            "@src/main.rs:2-3 (as it was before the change)\n```\n-- old comment\nlet x = 1;\n```\nWhy remove these?"
        ]
    );
}

/// A turn whose files Git kept shows what it changed, and its prompt rewinds the conversation
/// and the files, bringing the message back to the composer.
#[gpui_kit::test]
fn turns_show_their_changes_and_rewind(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("idle");
    for item in &mut record.snapshot.transcript {
        if matches!(item.id.as_str(), "user" | "reply") {
            item.checkpoint = Some(format!("{}-snapshot", item.id));
        }
    }
    let (sender, commands) = native_command_sender(&record);
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let changes = shown.clone();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        cx.subscribe(
            &view,
            move |_, _, event: &bootty_ui::OpenNativeSession, _| {
                if let bootty_ui::OpenNativeSession::ShowTurnChanges { diff } = event {
                    changes.lock().unwrap().push(diff.clone());
                }
            },
        )
        .detach();
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    let turn = visual.debug_bounds("turn-changes:reply").unwrap();
    visual.simulate_click(turn.center(), Modifiers::none());
    redraw(visual);
    assert_eq!(
        *shown.lock().unwrap(),
        ["diff --git a/turn.txt b/turn.txt\n+first\n"]
    );
    assert!(visual.debug_bounds("turn-changes:user").is_none());
    let rewind = visual.debug_bounds("rewind:user").unwrap();
    visual.simulate_click(rewind.center(), Modifiers::none());
    redraw(visual);
    let both = visual
        .debug_bounds("prompt-answer-Rewind conversation and files")
        .unwrap();
    visual.simulate_click(both.center(), Modifiers::none());
    redraw(visual);
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let asked = commands
        .lock()
        .unwrap()
        .iter()
        .filter(|call| {
            !matches!(
                call.command.as_str(),
                "agents.native.history" | "agents.native.models-info"
            )
        })
        // The command and its leading words, past the conversation's identity.
        .map(|call| {
            std::iter::once(call.command.as_str())
                .chain(
                    call.arguments
                        .iter()
                        .skip(2)
                        .take(2)
                        .map(String::as_str)
                        .filter(|argument| !argument.is_empty()),
                )
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        asked,
        [
            "agents.native.turn-diff reply",
            "agents.native.rewind user files",
            "agents.native.prompt Review this change."
        ]
    );
}

/// A wide conversation shows its context card beside the messages. A narrow one opens it from
/// the composer, over the messages, and a click outside closes it.
#[gpui_kit::test]
fn narrow_conversations_open_the_context_card_on_request(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _) = native_command_sender(&record);
    let context = bootty_ui::ConversationContext {
        project: Some("bootty".to_owned()),
        branch: Some("feature".to_owned()),
        pull_request: None,
        binding: None,
        diff: None,
        target: None,
    };
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.set_conversation_context(Some(context), cx);
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    let shown = |visual: &mut gpui_kit::VisualTestContext| {
        redraw(visual);
        redraw(visual);
        (
            visual.debug_bounds("native-context-card").is_some(),
            visual.debug_bounds("native-context-card-button").is_some(),
        )
    };
    visual.simulate_resize(size(px(1600.), px(900.)));
    assert_eq!(shown(visual), (true, false), "wide");
    visual.simulate_resize(size(px(900.), px(900.)));
    assert_eq!(shown(visual), (false, true), "narrow");
    let button = visual.debug_bounds("native-context-card-button").unwrap();
    visual.simulate_click(button.center(), Modifiers::none());
    assert_eq!(shown(visual), (true, true), "opened");
    visual.simulate_click(point(px(40.), px(40.)), Modifiers::none());
    assert_eq!(shown(visual), (false, true), "closed by a click outside");
}

/// Right-clicking a text field opens Bootty's own menu, whose items act on the field: Copy puts
/// the selection on the clipboard.
#[gpui_kit::test]
fn text_fields_open_bootty_menus(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
    let (sender, _) = native_command_sender(&record);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    redraw(visual);
    let composer = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_input("Wrap up");
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    // On the text, so the click keeps the selection.
    let text = point(composer.left().add(px(24.)), composer.top().add(px(22.)));
    visual.simulate_mouse_down(text, MouseButton::Right, Modifiers::none());
    visual.simulate_mouse_up(text, MouseButton::Right, Modifiers::none());
    redraw(visual);
    // Cut, then Copy.
    visual.simulate_keystrokes("down down enter");
    redraw(visual);
    assert_eq!(
        visual
            .read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("Wrap up")
    );
}
