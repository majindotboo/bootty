use std::{
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, TcpListener},
    sync::Arc,
    thread,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_mobile::{Connection, Invocation, LiveWorkspace, Target};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use rustls::{ServerConfig, ServerConnection, StreamOwned, pki_types::PrivatePkcs8KeyDer};
use serde_json::{Value, json};

fn host(
    replies: Vec<String>,
    wrong_certificate: bool,
) -> (Connection, thread::JoinHandle<Option<Value>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["bootty.local".into()]).unwrap();
    let trusted = if wrong_certificate {
        rcgen::generate_simple_self_signed(vec!["bootty.local".into()])
            .unwrap()
            .cert
            .der()
            .clone()
    } else {
        certificate.cert.der().clone()
    };
    let pairing = json!({"version":1, "host":"127.0.0.1", "port":listener.local_addr().unwrap().port(),
        "certificate":URL_SAFE_NO_PAD.encode(trusted), "token":URL_SAFE_NO_PAD.encode([1;32])});
    let connection = Connection::from_code(&format!(
        "bootty://pair/{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&pairing).unwrap())
    ))
    .unwrap();
    let tls =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
            )
            .unwrap();
    let worker = thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream = StreamOwned::new(
                ServerConnection::new(Arc::new(tls.clone())).unwrap(),
                socket,
            );
            let mut request = String::new();
            if BufReader::new(&mut stream).read_line(&mut request).is_err() {
                return None;
            }
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
            requests.push(serde_json::from_str(&request).ok()?);
        }
        requests.pop()
    });
    (connection, worker)
}

#[rstest]
fn only_the_paired_computer_receives_exact_command_values() {
    let (connection, worker) = host(
        vec![
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"success\",\"value\":null}}\n"
                .into(),
        ],
        false,
    );
    let target = Target {
        kind: "terminal".into(),
        handle: "opaque desktop target".into(),
        generation: "9007199254740993".into(),
    };
    connection
        .invoke(&Invocation::new(
            "terminal.paste",
            vec!["🥟 echo café".into()],
            Some(target.clone()),
        ))
        .unwrap();
    let request = worker.join().unwrap().unwrap();
    assert_eq!(
        request["request"]["params"]["invocation"]["target"],
        serde_json::to_value(target).unwrap()
    );
    assert_eq!(
        request["request"]["params"]["invocation"]["arguments"],
        json!(["🥟 echo café"])
    );
}

#[rstest]
fn a_changed_certificate_never_receives_the_credential_or_command() {
    let (connection, worker) = host(vec![String::new()], true);
    assert!(connection.rpc("system.ping", &Value::Null).is_err());
    assert!(worker.join().unwrap().is_none());
}

#[rstest]
#[case("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":null}\n", "incompatible")]
#[case(
    "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"message\":\"credential revoked\"}}\n",
    "credential revoked"
)]
#[case("{\"jsonrpc\":\"2.0\",\"id\":1}\n", "omitted")]
fn protocol_failures_are_visible(#[case] reply: &str, #[case] message: &str) {
    let (connection, worker) = host(vec![reply.into()], false);
    assert!(
        connection
            .rpc("system.ping", &Value::Null)
            .unwrap_err()
            .contains(message)
    );
    worker.join().unwrap();
}

#[rstest]
fn an_oversized_response_is_rejected() {
    let (connection, worker) = host(vec!["x".repeat(1_048_577)], false);
    assert!(
        connection
            .rpc("system.ping", &Value::Null)
            .unwrap_err()
            .contains("oversized")
    );
    worker.join().unwrap();
}

proptest! {
    #[test]
    fn malformed_pairing_text_never_becomes_a_connection(code in "[^\\p{C}]{0,200}") {
        prop_assert!(Connection::from_code(&code).is_err());
    }
}

#[rstest]
fn unavailable_capture_preserves_live_workspace_and_the_visible_error() {
    let target = Target {
        kind: "terminal".into(),
        handle: "issued terminal".into(),
        generation: "1".into(),
    };
    let listing = json!([{"scope":"1","name":"Default Space","backend":"native","host":"Local",
        "target":{"kind":"binding","handle":"binding","generation":"1"},
        "sessions":[{"name":"QA","target":{"kind":"session","handle":"session","generation":"1"},"terminal_target":target}]}]);
    let replies = vec![
        format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":1,"result":{"status":"success","value":listing}})
        ),
        format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":1,"result":{"status":"unavailable","message":"Terminal capture is temporarily unavailable"}})
        ),
    ];
    let (connection, worker) = host(replies, false);
    let live = LiveWorkspace::refresh(&connection, Some(&target)).unwrap();
    worker.join().unwrap();
    assert_eq!(live.spaces[0].sessions[0].name, "QA");
    assert_eq!(live.spaces[0].sessions[0].terminal_target, Some(target));
    assert!(live.terminal.is_none());
    assert_eq!(
        live.capture_error.as_deref(),
        Some("Terminal capture is temporarily unavailable")
    );
}
