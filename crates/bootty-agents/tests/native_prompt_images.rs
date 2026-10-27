#![cfg(unix)]

#[path = "support/image_peer.rs"]
pub mod image_peer;
#[path = "support/native_peer.rs"]
pub mod native_peer;
pub mod support;

use std::{error::Error, fs, path::Path, sync::Arc, time::Instant};

use assert_fs::TempDir;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_agents::{
    AgentKind, MAX_NATIVE_PROMPT_IMAGE_BYTES, MAX_NATIVE_PROMPT_IMAGES,
    MAX_NATIVE_PROMPT_TEXT_BYTES, NativeAgentService, NativeAttachmentKind,
    NativeAttachmentReference, NativeImageReference, NativePrompt, NativePromptImage,
    NativeSessionConfig, NativeSessionStatus, NativeTurnOutcome, ToolBridge, ToolBridgeContext,
    ToolCapture, ToolCapturedCommand, ToolPolicy, ToolScope,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

const SESSION: &str = "01234567-89ab-4cde-8123-456789abcdef";

fn png(metadata_bytes: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if metadata_bytes > 0 {
            encoder.add_text_chunk("fixture".to_owned(), "x".repeat(metadata_bytes))?;
        }
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[1, 2, 3, 255])?;
        writer.finish()?;
    }
    Ok(bytes)
}

fn reference(id: &str) -> NativeImageReference {
    NativeImageReference {
        id: id.to_owned(),
        pixel_width: 1,
        pixel_height: 1,
    }
}

fn fixture(root: &Path) -> String {
    root.join("image-provider.py")
        .to_string_lossy()
        .into_owned()
}

#[rstest]
#[case::codex(AgentKind::Codex)]
#[case::pi(AgentKind::Pi)]
#[case::claude(AgentKind::Claude)]
fn large_fork_context_echoes_preserve_turn_ownership_and_image_delivery(
    #[case] provider: AgentKind,
) {
    let root = TempDir::new().unwrap();
    // The provider supplies long responses; building the history is not the behavior under test.
    fs::write(root.path().join("history-mode"), "界".repeat(100_000)).unwrap();
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = fixture(root.path());
    config.fresh_session_id = (provider == AgentKind::Claude).then(|| SESSION.to_owned());
    let service = open(root.path().join("native.json")).unwrap();
    let parent = service
        .create_for_task("binding", "task", "Parent", config)
        .unwrap();
    let session = service.resolve(&parent.target()).unwrap();
    let barrier = match provider {
        AgentKind::Codex => "thread/read",
        AgentKind::Pi => "get_state",
        AgentKind::Claude => "initialize",
    };
    for _ in 0..2 {
        service
            .prompt(&parent.target(), "Build copied history")
            .unwrap();
        session
            .rpc(
                barrier,
                if provider == AgentKind::Codex {
                    json!({"threadId":session.snapshot().session_id})
                } else {
                    json!({})
                },
            )
            .unwrap();
    }
    assert!(
        session
            .snapshot()
            .transcript
            .iter()
            .map(|item| item.text.len())
            .sum::<usize>()
            >= 600_000
    );
    service.checkpoint().unwrap();
    let parent_identity = session.snapshot().session_id;
    let bytes = png(1024 * 1024).unwrap();
    for image in [false, true] {
        let child = service
            .fork_side_chat(&parent.target(), None, None)
            .unwrap();
        let images = if image {
            vec![NativePromptImage::from_host_png(reference("fork-image"), bytes.clone()).unwrap()]
        } else {
            Vec::new()
        };
        let prompt = NativePrompt::new("Continue copied history".into(), images).unwrap();
        service.prompt_input(&child.target(), &prompt).unwrap();
        let session = service.resolve(&child.target()).unwrap();
        session
            .rpc(
                barrier,
                if provider == AgentKind::Codex {
                    json!({"threadId":session.snapshot().session_id})
                } else {
                    json!({})
                },
            )
            .unwrap();
        assert_eq!(session.snapshot().status, NativeSessionStatus::Idle);
        assert_eq!(session.snapshot().error, None);
        assert!(session.snapshot().session_id.is_some());
        assert_ne!(session.snapshot().session_id, parent_identity);
        assert_eq!(
            session.snapshot().first_turn.unwrap().outcome,
            NativeTurnOutcome::Succeeded
        );
        // Escaped Unicode makes even the text-only provider echo exceed its ordinary frame limit.
        assert!(fs::metadata(root.path().join("prompt.json")).unwrap().len() > 1024 * 1024);
        if image {
            assert_eq!(fs::read(root.path().join("received.png")).unwrap(), bytes);
        }
        service.stop(&child.target()).unwrap();
    }
    service.shutdown().unwrap();
}

#[rstest]
#[case::codex_text(AgentKind::Codex, "Read this annotation", 0, false, 1)]
#[case::pi_text(AgentKind::Pi, "Read this annotation", 0, false, 1)]
#[case::claude_text(AgentKind::Claude, "Read this annotation", 0, false, 1)]
#[case::claude_reencoded_echo(AgentKind::Claude, "Read this annotation", 0, true, 1)]
#[case::codex_large_image_only(AgentKind::Codex, "", 1024 * 1024, false, 1)]
#[case::pi_large_image_only(AgentKind::Pi, "", 1024 * 1024, false, 1)]
#[case::claude_large_image_only(AgentKind::Claude, "", 1024 * 1024, false, 1)]
#[case::claude_large_reencoded_echo(AgentKind::Claude, "", 1024 * 1024, true, 1)]
#[case::codex_five(AgentKind::Codex, "Five screenshots", 0, false, 5)]
#[case::pi_five(AgentKind::Pi, "Five screenshots", 0, false, 5)]
#[case::claude_five(AgentKind::Claude, "Five screenshots", 0, false, 5)]
fn provider_images_are_exact_transport_bytes_and_durable_safe_references(
    #[case] provider: AgentKind,
    #[case] text: &str,
    #[case] metadata_bytes: usize,
    #[case] jpeg_echo: bool,
    #[case] image_count: usize,
) {
    let root = TempDir::new().unwrap();
    if jpeg_echo {
        fs::write(root.path().join("jpeg-echo"), "").unwrap();
    }
    let path = root.path().join("native.json");
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = fixture(root.path());
    config.fresh_session_id = (provider == AgentKind::Claude).then(|| SESSION.to_owned());
    config.account_directory = Some(root.path().join("account").to_string_lossy().into_owned());
    let bytes = png(metadata_bytes).unwrap();
    let references = (0..image_count)
        .map(|ix| reference(&format!("annotation_{ix}")))
        .collect::<Vec<_>>();
    let images = references
        .iter()
        .map(|reference| {
            NativePromptImage::from_host_png(reference.clone(), bytes.clone()).unwrap()
        })
        .collect();
    let prompt = NativePrompt::new(text.to_owned(), images).unwrap();
    let service = open(&path).unwrap();
    let record = service
        .create_for_task("binding", "task", "Image annotation", config)
        .unwrap();
    service.prompt_input(&record.target(), &prompt).unwrap();
    let session = service.resolve(&record.target()).unwrap();
    let barrier = match provider {
        AgentKind::Codex => "thread/read",
        AgentKind::Pi => "get_state",
        AgentKind::Claude => "initialize",
    };
    session
        .rpc(
            barrier,
            if provider == AgentKind::Codex {
                json!({"threadId":"image-session"})
            } else {
                json!({})
            },
        )
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(
        snapshot.first_turn.as_ref().unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    let users = snapshot
        .transcript
        .iter()
        .filter(|item| item.role == "user")
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    let user = users.first().unwrap();
    assert_eq!(user.text, text);
    assert_eq!(user.images, references);
    assert_eq!(fs::read(root.path().join("received.png")).unwrap(), bytes);
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert_eq!(prompt.image_count(), image_count);
    assert!(wire.to_string().contains(&STANDARD.encode(&bytes)));
    let refreshed = session.refresh_history().unwrap();
    let refreshed_user = refreshed
        .transcript
        .iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(refreshed_user.images, user.images);
    assert_eq!(refreshed_user.text, text);
    service.checkpoint().unwrap();
    service.stop(&record.target()).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    assert!(!saved.contains(&STANDARD.encode(&bytes)));
    assert!(!saved.contains("data:image/png"));
    service.shutdown().unwrap();
    let restored = open(&path).unwrap();
    let records = restored.sessions();
    let record = records.first().unwrap();
    assert_eq!(record.binding_id, "binding");
    assert_eq!(record.task_identity.as_deref(), Some("task"));
    let user = record
        .snapshot
        .transcript
        .iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(user.images, references);
    assert_eq!(user.text, text);
    restored.shutdown().unwrap();
}

#[rstest]
#[case::path("/private/image.png")]
#[case::url("https://image.test/a.png")]
#[case::empty("")]
fn image_identity_never_admits_paths_or_urls(#[case] id: &str) {
    assert!(NativePromptImage::from_host_png(reference(id), png(0).unwrap()).is_err());
}

#[rstest]
fn image_admission_checks_pixels_dimensions_and_submission_budgets() {
    let bytes = png(0).unwrap();
    assert!(NativePromptImage::from_host_png(reference("valid"), b"not a PNG".to_vec()).is_err());
    let mut truncated = bytes.clone();
    truncated.truncate(truncated.len().saturating_sub(10));
    assert!(NativePromptImage::from_host_png(reference("valid"), truncated).is_err());
    let mut wrong = reference("valid");
    wrong.pixel_width = 2;
    assert!(NativePromptImage::from_host_png(wrong, bytes.clone()).is_err());
    assert!(
        NativePromptImage::from_host_png(
            reference("valid"),
            vec![0; MAX_NATIVE_PROMPT_IMAGE_BYTES + 1]
        )
        .is_err()
    );
    let image = NativePromptImage::from_host_png(reference("valid"), bytes).unwrap();
    assert!(
        NativePrompt::new(String::new(), vec![image.clone(); MAX_NATIVE_PROMPT_IMAGES]).is_ok()
    );
    assert!(
        NativePrompt::new(
            String::new(),
            vec![image.clone(); MAX_NATIVE_PROMPT_IMAGES + 1]
        )
        .is_err()
    );
    assert!(NativePrompt::text(&"x".repeat(MAX_NATIVE_PROMPT_TEXT_BYTES + 1)).is_err());
    assert!(NativePrompt::text("").is_err());
    assert!(NativePrompt::new(String::new(), vec![image]).is_ok());
    let big = NativePromptImage::from_host_png(
        reference("large"),
        png(MAX_NATIVE_PROMPT_IMAGE_BYTES / 2).unwrap(),
    )
    .unwrap();
    assert!(NativePrompt::new(String::new(), vec![big; 2]).is_err());
}

proptest! {
    #[test]
    fn text_wrappers_keep_literal_content(text in "[a-zA-Z0-9 '\"\\n\\t]{1,256}") {
        let prompt = NativePrompt::text(&text).unwrap();
        prop_assert_eq!(prompt.message(), text);
        prop_assert_eq!(prompt.image_count(), 0);
        prop_assert_eq!(prompt.image_references(), Vec::<NativeImageReference>::new());
    }
}

fn capture_provider(root: &Path) -> String {
    root.join("capture-provider.py")
        .to_string_lossy()
        .into_owned()
}

fn target(kind: ResourceKind) -> CommandTarget {
    CommandTarget {
        kind,
        handle: format!("captured-{kind:?}"),
        generation: 17,
    }
}

#[rstest]
#[case::cancelled("checkpoint_cancelled")]
#[case::identity_changed("checkpoint_identity")]
#[case::missing_registration("checkpoint_missing")]
fn fresh_pi_conversations_require_an_exact_confirmed_checkpoint(#[case] fault: &str) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("capture-fault"), fault).unwrap();
    fs::write(root.path().join("capture.png"), png(0).unwrap()).unwrap();
    let mut config = NativeSessionConfig::new(AgentKind::Pi, root.path());
    config.program = capture_provider(root.path());
    let service = open(root.path().join("native.json")).unwrap();
    assert!(
        service
            .create_for_task_with_tools(
                "binding",
                "task",
                "Capture",
                config,
                capture_tools(AgentKind::Pi, false).unwrap(),
            )
            .is_err()
    );
    assert!(
        service
            .sessions()
            .iter()
            .all(|record| record.snapshot.status == NativeSessionStatus::Error)
    );
    service.shutdown().unwrap();
}

fn capture_tools(provider: AgentKind, computer: bool) -> Result<Arc<ToolBridge>, String> {
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), Caller::Internal);
    invocation.target = Some(target(ResourceKind::ApplicationWindow));
    let tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider,
                binding: target(ResourceKind::Binding),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                computer_capture: computer,
                ..ToolPolicy::own_terminal()
            },
            captures: if computer {
                vec![ToolCapturedCommand {
                    capture: ToolCapture::Computer,
                    invocation,
                }]
            } else {
                Vec::new()
            },
            spawn: None,
        },
        &std::env::current_exe().map_err(|error| error.to_string())?,
        Arc::new(|_: CommandInvocation, _: Instant, _: CommandCancellation| {
            CommandOutcome::Success {
                value: json!({}),
                warnings: Vec::new(),
            }
        }),
    )?;
    tools.lease().bind(
        &target(ResourceKind::Binding),
        target(ResourceKind::Terminal),
    )?;
    Ok(Arc::new(tools))
}

#[rstest]
#[case::codex(AgentKind::Codex, false, false)]
#[case::pi(AgentKind::Pi, false, false)]
#[case::pi_nested(AgentKind::Pi, true, false)]
#[case::pi_legacy(AgentKind::Pi, false, true)]
fn exact_attached_computer_results_cross_the_control_budget_without_persisting_pixels(
    #[case] provider: AgentKind,
    #[case] nested: bool,
    #[case] legacy_ack: bool,
) {
    let root = TempDir::new().unwrap();
    let bytes = png(1024 * 1024).unwrap();
    fs::write(root.path().join("capture.png"), &bytes).unwrap();
    if nested {
        fs::write(root.path().join("capture-fault"), "nested").unwrap();
    }
    if legacy_ack {
        fs::write(root.path().join("capture-fault"), "legacy_ack").unwrap();
    }
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = capture_provider(root.path());
    let service = open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Capture",
            config,
            capture_tools(provider, true).unwrap(),
        )
        .unwrap();
    service
        .prompt(&record.target(), "Capture the exact attached window")
        .unwrap();
    let session = service.resolve(&record.target()).unwrap();
    session
        .rpc(
            if provider == AgentKind::Pi {
                "get_state"
            } else {
                "__barrier"
            },
            json!({}),
        )
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.status, NativeSessionStatus::Idle);
    if provider == AgentKind::Pi {
        assert_eq!(
            snapshot
                .transcript
                .iter()
                .find_map(|item| item.tool.as_ref())
                .unwrap()
                .name,
            if nested {
                "codemode"
            } else {
                "computer_snapshot"
            }
        );
    }
    assert_eq!(
        snapshot.first_turn.as_ref().unwrap().outcome,
        NativeTurnOutcome::Succeeded
    );
    service.checkpoint().unwrap();
    let journal = fs::read_to_string(root.path().join("native.json")).unwrap();
    assert!(!journal.contains(&STANDARD.encode(bytes)));
    // The tool's row shows the capture from a file beside the conversation's attachments.
    let capture = tool_image(&service, &record.target()).expect("the tool row shows its image");
    assert_eq!(
        (capture.pixel_width, capture.pixel_height),
        (Some(1), Some(1))
    );
    assert_eq!(
        session.refresh_history().unwrap().status,
        NativeSessionStatus::Idle
    );
    service.checkpoint().unwrap();
    service.stop(&record.target()).unwrap();
    service.shutdown().unwrap();
    let restored = open(root.path().join("native.json")).unwrap();
    let records = restored.sessions();
    let previous = records.first().unwrap();
    let resumed = restored
        .resume_with_tools(&previous.target(), capture_tools(provider, true).unwrap())
        .unwrap();
    assert_eq!(
        resumed.config.session_id.as_deref(),
        Some("capture-session")
    );
    assert_eq!(resumed.binding_id, "binding");
    assert_eq!(resumed.task_identity.as_deref(), Some("task"));
    assert_eq!(resumed.snapshot.status, NativeSessionStatus::Idle);
    assert_eq!(resumed.snapshot.first_turn, None);
    assert_eq!(tool_image(&restored, &resumed.target()), Some(capture));
    if provider == AgentKind::Pi {
        assert_eq!(
            resumed
                .snapshot
                .transcript
                .iter()
                .find_map(|item| item.tool.as_ref())
                .unwrap()
                .name,
            if nested {
                "codemode"
            } else {
                "computer_snapshot"
            }
        );
    }
    restored.shutdown().unwrap();
}

/// The image on the conversation's tool row, when it opens as a preview.
fn tool_image(
    service: &NativeAgentService,
    target: &CommandTarget,
) -> Option<NativeAttachmentReference> {
    service
        .sessions()
        .into_iter()
        .find(|record| record.target() == *target)?
        .snapshot
        .transcript
        .into_iter()
        .filter(|item| item.tool.is_some())
        .flat_map(|item| item.attachments)
        .find(|reference| reference.kind == NativeAttachmentKind::Image)
        .filter(|image| service.preview_attachment(target, &image.id).is_ok())
}

#[rstest]
#[case::codex_foreign_id(AgentKind::Codex, "id")]
#[case::codex_unrelated_name(AgentKind::Codex, "name")]
#[case::codex_foreign_server(AgentKind::Codex, "server")]
#[case::pi_foreign_id(AgentKind::Pi, "id")]
#[case::pi_unrelated_name(AgentKind::Pi, "name")]
#[case::pi_foreign_server(AgentKind::Pi, "server")]
#[case::pi_unqualified_name(AgentKind::Pi, "plain")]
#[case::pi_no_computer_grant(AgentKind::Pi, "permission")]
#[case::codex_no_computer_grant(AgentKind::Codex, "permission")]
#[case::codex_invalid_png(AgentKind::Codex, "png")]
#[case::pi_invalid_png(AgentKind::Pi, "png")]
#[case::pi_foreign_nested_parent(AgentKind::Pi, "nested_parent")]
#[case::pi_changed_nested_pixels(AgentKind::Pi, "nested_changed_image")]
#[case::pi_changed_capture_parent(AgentKind::Pi, "nested_capture_parent")]
#[case::pi_changed_completion_pixels(AgentKind::Pi, "completion_image")]
#[case::pi_foreign_completion_id(AgentKind::Pi, "completion_id")]
#[case::pi_foreign_completion_parent(AgentKind::Pi, "nested_completion_parent")]
/// A large tool image Bootty didn't capture is cut down instead of ending the conversation: it
/// never becomes an image, and a notice says what was left out.
fn large_tool_records_bootty_did_not_capture_never_become_images(
    #[case] provider: AgentKind,
    #[case] fault: &str,
) {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("capture-fault"), fault).unwrap();
    let bytes = if fault == "png" {
        vec![0; 1024 * 1024]
    } else {
        png(1024 * 1024).unwrap()
    };
    fs::write(root.path().join("capture.png"), bytes).unwrap();
    if matches!(fault, "nested_changed_image" | "completion_image") {
        fs::write(
            root.path().join("changed.png"),
            png(1024 * 1024 + 1).unwrap(),
        )
        .unwrap();
    }
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = capture_provider(root.path());
    let service = open(root.path().join("native.json")).unwrap();
    let record = service
        .create_for_task_with_tools(
            "binding",
            "task",
            "Capture",
            config,
            capture_tools(provider, fault != "permission").unwrap(),
        )
        .unwrap();
    service
        .prompt(&record.target(), "Capture the exact attached window")
        .unwrap();
    let snapshot = service.resolve(&record.target()).unwrap().snapshot();
    assert_ne!(snapshot.status, NativeSessionStatus::Error);
    assert_eq!(snapshot.error, None);
    assert!(
        snapshot
            .transcript
            .iter()
            .all(|item| item.images.is_empty())
    );
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|item| item.role == "notice" && item.text.starts_with("Omitted ")),
        "{:#?}",
        snapshot.transcript
    );
    service.shutdown().unwrap();
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn review_comment_context_reaches_the_provider_and_survives_history(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("native.json");
    let mut config = NativeSessionConfig::new(provider, root.path());
    config.program = fixture(root.path());
    config.fresh_session_id = (provider == AgentKind::Claude).then(|| SESSION.to_owned());
    config.account_directory = Some(root.path().join("account").to_string_lossy().into_owned());
    let service = open(&path).unwrap();
    let record = service
        .create_for_task("binding", "task", "Review", config)
        .unwrap();
    let review = bootty_agents::NativeReviewComment {
        id: "review-source".into(),
        anchor: bootty_git::diff::DiffAnchor {
            path: "src/main.rs".into(),
            side: bootty_git::diff::DiffSide::Left,
            start_line: 2,
            line: 3,
        },
        quote: "before\nsecond source line".into(),
        body: "Preserve the original behavior".into(),
        prompt_range: 0..8,
    };
    let prompt = NativePrompt::new_with_review_comments(
        "[review]".into(),
        vec![],
        bootty_agents::NativePromptAttachments::default(),
        vec![],
        vec![review.clone()],
    )
    .unwrap();
    let queued = service.queue_input(&record.target(), &prompt).unwrap();
    assert_eq!(queued.review_comments, vec![review.clone()]);
    assert_eq!(
        queued.preview_text,
        "main.rs:2–3: Preserve the original behavior"
    );
    service.shutdown().unwrap();
    drop(service);
    let service = open(&path).unwrap();
    assert_eq!(
        service.sessions()[0].queued_messages[0].review_comments,
        vec![review.clone()]
    );
    service
        .cancel_followup(&record.target(), &queued.id)
        .unwrap();
    let record = service.resume(&record.target()).unwrap();
    service.prompt_input(&record.target(), &prompt).unwrap();
    let session = service.resolve(&record.target()).unwrap();
    let (barrier, params) = match provider {
        AgentKind::Codex => ("thread/read", json!({"threadId":"image-session"})),
        AgentKind::Pi => ("get_state", json!({})),
        AgentKind::Claude => ("initialize", json!({})),
    };
    session.rpc(barrier, params).unwrap();
    let wire: Value =
        serde_json::from_slice(&fs::read(root.path().join("prompt.json")).unwrap()).unwrap();
    assert!(wire.to_string().contains("Preserve the original behavior"));
    assert!(wire.to_string().contains("second source line"));
    let refreshed = session.refresh_history().unwrap();
    let user = refreshed
        .transcript
        .iter()
        .find(|item| item.role == "user")
        .unwrap();
    assert_eq!(user.text, "[review]");
    assert_eq!(user.review_comments, vec![review.clone()]);
    service.checkpoint().unwrap();
    service.shutdown().unwrap();
    drop(service);
    let service = open(&path).unwrap();
    let record = service.sessions().remove(0);
    assert_eq!(
        record
            .snapshot
            .transcript
            .iter()
            .find(|item| item.role == "user")
            .unwrap()
            .review_comments,
        vec![review]
    );
    service.shutdown().unwrap();
}

fn open(path: impl Into<std::path::PathBuf>) -> Result<NativeAgentService, String> {
    NativeAgentService::open_with(path, native_peer::clock(), image_peer::launcher())
}

proptest! {
    #[test]
    fn escaped_provider_frames_preserve_unicode_and_ascii_payloads(
        text in proptest::collection::vec(any::<char>(), 0..64),
    ) {
        let text = text.into_iter().collect::<String>();
        let value = json!({text.clone(): [text, "base64 payload".repeat(128)]});
        let encoded = native_peer::ascii_json(&value);
        assert!(encoded.is_ascii());
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    }
}
