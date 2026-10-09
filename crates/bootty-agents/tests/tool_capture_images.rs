use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_agents::{
    AgentCommandExecutor, AgentKind, MAX_TOOL_IMAGE_RESPONSE_BYTES, MAX_TOOL_MESSAGE_BYTES,
    ToolCapture, ToolCapturedCommand, ToolLease, ToolPolicy, ToolProtocol, ToolScope,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::{Value, json};

fn target(kind: ResourceKind) -> CommandTarget {
    CommandTarget {
        kind,
        handle: format!("exact-{kind:?}"),
        generation: 17,
    }
}

#[fixture]
#[allow(
    clippy::unwrap_used,
    reason = "Fixture issues and binds a known valid exact host authority"
)]
fn lease() -> ToolLease {
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), Caller::Internal);
    invocation.target = Some(target(ResourceKind::ApplicationWindow));
    let lease = ToolLease::issue(
        ToolScope {
            provider: AgentKind::Codex,
            binding: target(ResourceKind::Binding),
        },
        Caller::Socket,
        ToolPolicy {
            computer_capture: true,
            ..ToolPolicy::own_terminal()
        },
        vec![ToolCapturedCommand {
            capture: ToolCapture::Computer,
            invocation,
        }],
    )
    .unwrap();
    lease
        .bind(
            &target(ResourceKind::Binding),
            target(ResourceKind::Terminal),
        )
        .unwrap();
    lease
}

#[expect(
    clippy::unwrap_used,
    reason = "Fixture encodes fixed bounded PNG dimensions and pixels"
)]
fn image(width: u32, height: u32) -> Value {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_compression(png::Compression::NoCompression);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let size = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|size| size.checked_mul(4))
            .and_then(|size| usize::try_from(size).ok())
            .unwrap();
        writer.write_image_data(&vec![128; size]).unwrap();
        writer.finish().unwrap();
    }
    json!({"result":"snapshot","png_base64":STANDARD.encode(bytes),"pixel_width":width,"pixel_height":height,
        "target":{"window_id":42,"process_id":123,"bundle_id":"dev.bootty.test", "launch_time":1000.0,
            "bounds":{"x":-100.0,"y":20.0,"width":width,"height":height},"title":null}})
}

const fn receipt(value: Value) -> CommandOutcome {
    CommandOutcome::Success {
        value,
        warnings: Vec::new(),
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "Fixture serializes a valid request and requires its response"
)]
fn call(lease: &ToolLease, name: &str, executor: &dyn AgentCommandExecutor) -> Value {
    let request = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","id":"\u{0001}".repeat(128),"method":"tools/call",
        "params":{"name":name,"arguments":{}}}),
    )
    .unwrap();
    ToolProtocol::new(lease.clone())
        .handle(&request, Instant::now(), executor)
        .unwrap()
}

#[rstest]
fn granted_capture_returns_an_image_and_preserves_exact_host_identity(lease: ToolLease) {
    let value = image(2, 3);
    let expected_png = value["png_base64"].clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = calls.clone();
    let executor = move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        observed.lock().unwrap().push(invocation);
        receipt(value.clone())
    };
    let response = call(&lease, "computer_snapshot", &executor);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["content"][0],
        json!({"type":"image","mimeType":"image/png","data":expected_png})
    );
    let caption = response["result"]["content"][1]["text"].as_str().unwrap();
    assert!(caption.contains("2x3") && caption.contains("x=-100") && caption.len() < 4096);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "computer.capture");
    assert_eq!(calls[0].caller, Caller::Socket);
    assert_eq!(
        calls[0].target,
        Some(target(ResourceKind::ApplicationWindow))
    );
    assert_eq!(calls[0].arguments, Vec::<String>::new());
    drop(calls);
}

#[rstest]
#[case::posted(json!({"result":"posted"}))]
#[case::path_only(json!({"path":"/never/read/this.png","pixel_width":1,"pixel_height":1}))]
#[case::unknown_field({ let mut v = image(1,1); v["path"] = "/never/read/this.png".into(); v })]
#[case::bad_base64({ let mut v = image(1,1); v["png_base64"] = "!invalid".into(); v })]
#[case::not_png({ let mut v = image(1,1); v["png_base64"] = STANDARD.encode(b"not a PNG").into(); v })]
#[case::dimensions({ let mut v = image(1,1); v["pixel_width"] = 2.into(); v })]
#[case::zero_dimensions({ let mut v = image(1,1); v["pixel_height"] = 0.into(); v })]
#[case::missing_geometry({ let mut v = image(1,1); v["requested_region"] = json!({"x":-100.0,"y":20.0,"width":1.0,"height":1.0}); v })]
#[case::outside_geometry({ let mut v = image(1,1); v["requested_region"] = json!({"x":-100.0,"y":20.0,"width":1.0,"height":1.0}); v["region"] = json!({"x":0.0,"y":20.0,"width":1.0,"height":1.0}); v })]
fn malformed_capture_has_no_image_or_encoded_text_fallback(lease: ToolLease, #[case] value: Value) {
    let executor =
        move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(value.clone());
    let response = call(&lease, "computer_snapshot", &executor);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["content"],
        json!([{"type":"text","text":"Computer capture returned an invalid image"}])
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn revocation_during_capture_discards_pixels(lease: ToolLease, #[case] disable: bool) {
    let during = lease.clone();
    let executor = move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        assert!(cancellation.try_start());
        if disable {
            during.restrict(ToolPolicy::own_terminal());
        } else {
            during.revoke();
        }
        receipt(image(1, 1))
    };
    let response = call(&lease, "computer_snapshot", &executor);
    assert_eq!(response["result"]["isError"], true);
    assert_ne!(response["result"]["content"][0]["type"], "image");
}

#[rstest]
fn ordinary_tools_cannot_select_the_image_response_budget(lease: ToolLease) {
    let value = image(600, 600);
    let executor =
        move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(value.clone());
    assert_eq!(
        call(&lease, "terminal_read", &executor)["error"]["code"],
        -32603
    );
    let response = call(&lease, "computer_snapshot", &executor);
    assert_eq!(response["result"]["isError"], false);
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(bytes.len() > MAX_TOOL_MESSAGE_BYTES && bytes.len() < MAX_TOOL_IMAGE_RESPONSE_BYTES);
}

proptest! {
    #[test]
    fn damaged_pngs_never_become_images(index in 0_usize..64) {
        let lease = lease();
        let mut value = image(1, 1);
        let mut png = STANDARD.decode(value["png_base64"].as_str().unwrap()).unwrap();
        png.truncate(index.min(png.len()));
        value["png_base64"] = STANDARD.encode(png).into();
        let executor = move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(value.clone());
        prop_assert_eq!(call(&lease, "computer_snapshot", &executor)["result"]["isError"].clone(), json!(true));
    }
}

#[cfg(unix)]
#[rstest]
fn private_stdio_delivers_large_images_without_raising_request_limits() {
    use bootty_agents::{ToolBridge, ToolBridgeContext, tool_stdio};
    use std::{io::Cursor, path::Path};
    let value = image(600, 600);
    let expected = value["png_base64"].clone();
    let commands =
        move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(value.clone());
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), Caller::Internal);
    invocation.target = Some(target(ResourceKind::ApplicationWindow));
    let bridge = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: target(ResourceKind::Binding),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                computer_capture: true,
                ..ToolPolicy::own_terminal()
            },
            captures: vec![ToolCapturedCommand {
                capture: ToolCapture::Computer,
                invocation,
            }],
            spawn: None,
        },
        Path::new("/host/bootty-dev"),
        Arc::new(commands),
    )
    .unwrap();
    bridge
        .lease()
        .bind(
            &target(ResourceKind::Binding),
            target(ResourceKind::Terminal),
        )
        .unwrap();
    let arguments = bridge.arguments();
    let argument = arguments
        .iter()
        .find(|argument| argument.starts_with("mcp_servers.") && argument.contains(".args="))
        .unwrap();
    let args: Vec<String> = serde_json::from_str(argument.split_once('=').unwrap().1).unwrap();
    let connection = Path::new(args.last().unwrap());
    let mut output = Vec::new();
    tool_stdio(connection, &mut Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"computer_snapshot\"}}\n"), &mut output).unwrap();
    assert!(output.len() > MAX_TOOL_MESSAGE_BYTES && output.len() <= MAX_TOOL_IMAGE_RESPONSE_BYTES);
    assert_eq!(
        serde_json::from_slice::<Value>(&output).unwrap()["result"]["content"][0]["data"],
        expected
    );
    assert!(
        tool_stdio(
            connection,
            &mut Cursor::new(vec![b' '; MAX_TOOL_MESSAGE_BYTES + 1]),
            &mut Vec::new()
        )
        .is_err()
    );
}

#[rstest]
fn maximum_supported_dimensions_and_raw_budget_are_enforced(lease: ToolLease) {
    let accepted = image(1600, 1300);
    let executor =
        move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(accepted.clone());
    let response = call(&lease, "computer_snapshot", &executor);
    assert_eq!(response["result"]["isError"], false);
    assert!(serde_json::to_vec(&response).unwrap().len() < MAX_TOOL_IMAGE_RESPONSE_BYTES);
    let oversized = image(1600, 1600);
    let executor =
        move |_: CommandInvocation, _: Instant, _: CommandCancellation| receipt(oversized.clone());
    assert_eq!(
        call(&lease, "computer_snapshot", &executor)["result"]["isError"],
        true
    );
}

#[rstest]
fn disabled_capture_executes_nothing(lease: ToolLease) {
    lease.restrict(ToolPolicy::own_terminal());
    let executor = |_: CommandInvocation, _: Instant, _: CommandCancellation| -> CommandOutcome {
        panic!("Disabled capture must not execute")
    };
    assert_eq!(
        call(&lease, "computer_snapshot", &executor)["result"]["isError"],
        true
    );
}

#[rstest]
#[case(None, Vec::new())]
#[case(Some(ResourceKind::Binding), Vec::new())]
#[case(Some(ResourceKind::ApplicationWindow), vec!["/caller/path.png".into()])]
fn computer_attachment_has_only_an_exact_window_and_empty_arguments(
    #[case] kind: Option<ResourceKind>,
    #[case] arguments: Vec<String>,
) {
    let mut invocation = CommandInvocation::new("computer.capture", arguments, Caller::Internal);
    invocation.target = kind.map(target);
    assert!(
        ToolLease::issue(
            ToolScope {
                provider: AgentKind::Codex,
                binding: target(ResourceKind::Binding)
            },
            Caller::Socket,
            ToolPolicy {
                computer_capture: true,
                ..ToolPolicy::own_terminal()
            },
            vec![ToolCapturedCommand {
                capture: ToolCapture::Computer,
                invocation
            }]
        )
        .is_err()
    );
}
