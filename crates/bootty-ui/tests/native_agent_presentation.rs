#![cfg(test)]

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::Caller;
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{UiPalette, init_theme},
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
    let mut record = saved_record("stopped");
    let reference: bootty_agents::NativeAttachmentReference =
        serde_json::from_value(serde_json::json!({
            "id":"restored-image", "kind":"image", "name":"image.png", "mime_type":"image/png",
            "size_bytes":png.get_ref().len(),"pixel_width":500,"pixel_height":500
        }))
        .unwrap();
    record.attachments.push(reference.clone());
    let imported = serde_json::to_value(&reference).unwrap();
    record
        .snapshot
        .transcript
        .iter_mut()
        .find(|item| item.id == "user")
        .unwrap()
        .attachments
        .push(reference);
    let target = record.target();
    let mailbox = std::sync::Arc::new(std::sync::Mutex::new(
        None::<bootty_control::AppCommandReceiver>,
    ));
    let confirmed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let inbox = mailbox.clone();
    let accepted = confirmed.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        8,
        std::sync::Arc::new(move || {
            let inbox = inbox.lock().unwrap();
            let receiver = inbox.as_ref().unwrap();
            while let Ok(request) = receiver.try_recv() {
                let value = if request.invocation.command == "agents.native.attachment-preview" {
                    assert_eq!(request.invocation.target, Some(target.clone()));
                    assert_eq!(
                        request.invocation.arguments,
                        ["saved-pi", "1", "restored-image"]
                    );
                    accepted.store(true, std::sync::atomic::Ordering::Relaxed);
                    serde_json::json!({"png":encoded})
                } else if request.invocation.command == "agents.native.import" {
                    imported.clone()
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
    let position = composer.center();
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
    assert!(
        text.contains(path.to_str().unwrap()),
        "dropped folder becomes prompt context: {text}"
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

/// While a turn streams, every assistant message stays visible in order; only the work between
/// messages folds.
#[gpui_kit::test]
fn live_turn_keeps_every_message_visible(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let mut record = saved_record("working");
    record.snapshot.transcript = serde_json::from_value(serde_json::json!([
        {"id": "prompt", "role": "user", "text": "Audit this", "complete": true},
        {"id": "first-tool", "role": "tool", "text": "", "complete": true,
         "tool": {"name": "Bash", "input": "{\"command\":\"ls\"}", "status": "completed"}},
        {"id": "status", "role": "assistant", "text": "Found the cause.", "complete": true},
        {"id": "second-tool", "role": "tool", "text": "", "complete": false,
         "tool": {"name": "Bash", "input": "{\"command\":\"cargo test\"}", "status": "running"}},
        {"id": "latest", "role": "assistant", "text": "Running the tests.", "complete": false}
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
    let status = visual
        .debug_bounds("native-message-status")
        .expect("an earlier message stays visible while the turn streams");
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
        ("/new", true),
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

#[gpui_kit::test]
fn child_tabs_load_and_follow_provider_updates_until_hidden_or_closed(cx: &mut TestAppContext) {
    use bootty_ui::SubagentPanel;
    use gpui_kit::AppContext as _;
    use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, PanelId, panel_handle};
    use std::sync::{Arc, Mutex};

    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let (record, reply) = subagent_record_and_reply();
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
        panels = Some((parent, child, other, area.clone()));
        gpui_kit::component::Root::new(area, window, cx)
    });
    let (_parent, child, other, area) = panels.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        !reads.lock().unwrap().is_empty(),
        "opening the child loads it without Refresh"
    );
    assert!(
        visual
            .debug_bounds("subagent-message:audit-answer")
            .is_some()
    );
    assert!(
        visual.debug_bounds("subagent-status:Completed").is_some(),
        "the read reply replaces stale Working status"
    );
    reply.lock().unwrap()["agent"]["status"] = serde_json::json!("running");
    let before = reads.lock().unwrap().len();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        reads.lock().unwrap().len() > before,
        "an idle child remains followed for later turns"
    );
    assert!(visual.debug_bounds("subagent-status:Running").is_some());

    visual.update(|window, cx| {
        area.update(cx, |area, cx| {
            area.select_panel(PanelId::from(other.entity_id()), window, cx);
        });
    });
    visual.run_until_parked();
    let before = reads.lock().unwrap().len();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(4));
    visual.run_until_parked();
    assert_eq!(
        reads.lock().unwrap().len(),
        before,
        "hidden children stop polling"
    );
    visual.update(|window, cx| area.update(cx, |area, cx| area.remove_panel(child, window, cx)));
    visual.run_until_parked();
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(4));
    visual.run_until_parked();
    assert_eq!(
        reads.lock().unwrap().len(),
        before,
        "closed children stop polling"
    );
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
