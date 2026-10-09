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
        attachments: Vec::new(),
    }
}

#[gpui_kit::test]
fn restored_provider_history_is_visible_without_scrolling(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = saved_record("idle");
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
    assert!(preview.size.width > px(0.) && preview.size.height > px(0.));
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
        visual.debug_bounds("native-tool-heading-tool").is_some(),
        "live work is expanded"
    );
    let fold = visual
        .debug_bounds("native-work-group-tool")
        .expect("live work log");
    visual.simulate_click(fold.center(), Modifiers::none());
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("native-tool-heading-tool").is_none());
    assert!(
        visual.debug_bounds("native-working").is_some(),
        "collapsing work keeps activity visible"
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
