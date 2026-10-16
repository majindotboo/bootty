#![cfg(test)]
//! What an MCP server asks for, and which answers Bootty will send back.

use bootty_agents::{
    Elicitation, ElicitationField, FieldKind, FieldOption, NativeAgentRequest,
    NativeApprovalDecision as Decision, TextFormat, form_response,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Map, Value, json};

fn request(parameters: &Value) -> NativeAgentRequest {
    serde_json::from_value(json!({
        "id": "request", "method": "mcpServer/elicitation/request", "parameters": parameters
    }))
    .unwrap()
}

fn form(schema: &Value) -> Vec<ElicitationField> {
    match request(
        &json!({"mode":"form","message":"Fill in","serverName":"crm","requestedSchema":schema}),
    )
    .elicitation()
    {
        Some(Elicitation::Form(fields)) => fields,
        other => panic!("expected a form, got {other:?}"),
    }
}

fn answers(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

#[rstest]
fn a_schema_becomes_typed_fields_in_the_servers_order() {
    let fields = form(
        &json!({"type":"object","required":["email","plan"],"properties":{
            "email":{"type":"string","format":"email","title":"Work email"},
            "seats":{"type":"integer","minimum":1,"maximum":50,"default":5},
            "plan":{"type":"string","oneOf":[{"const":"team","title":"Team"},{"const":"pro","title":"Pro"}]},
            "regions":{"type":"array","items":{"enum":["eu","us"]},"maxItems":2,"default":["eu","mars"]},
            "notify":{"type":"boolean","description":"Send a summary"}
        }}),
    );
    let summary = fields
        .iter()
        .map(|field| (field.key.as_str(), field.title.as_str(), field.required))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            ("email", "Work email", true),
            ("seats", "seats", false),
            ("plan", "plan", true),
            ("regions", "regions", false),
            ("notify", "notify", false),
        ]
    );
    let kinds = fields
        .iter()
        .map(|field| field.kind.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            FieldKind::Text {
                format: Some(TextFormat::Email),
                min_length: None,
                max_length: None,
                default: None
            },
            FieldKind::Number {
                integer: true,
                minimum: Some(1.0),
                maximum: Some(50.0),
                default: Some(5.0)
            },
            FieldKind::Choice {
                options: vec![
                    FieldOption {
                        value: "team".into(),
                        label: "Team".into()
                    },
                    FieldOption {
                        value: "pro".into(),
                        label: "Pro".into()
                    },
                ],
                default: None
            },
            FieldKind::Choices {
                options: vec![
                    FieldOption {
                        value: "eu".into(),
                        label: "eu".into()
                    },
                    FieldOption {
                        value: "us".into(),
                        label: "us".into()
                    },
                ],
                min_items: None,
                max_items: Some(2),
                // A default outside the options is dropped.
                default: vec!["eu".into()]
            },
            FieldKind::Boolean { default: false },
        ]
    );
}

/// Each answer is checked against the server's own rules before it can be sent.
#[rstest]
#[case::email_ok(json!({"type":"string","format":"email"}), json!("ada@example.com"), None)]
#[case::email_bad(json!({"type":"string","format":"email"}), json!("ada at example"), Some("Enter an email address"))]
#[case::date_ok(json!({"type":"string","format":"date"}), json!("2026-01-31"), None)]
#[case::date_bad(json!({"type":"string","format":"date"}), json!("31/01/2026"), Some("Enter a date as YYYY-MM-DD"))]
#[case::too_short(json!({"type":"string","minLength":3}), json!("ab"), Some("Use at least 3 characters"))]
#[case::too_long(json!({"type":"string","maxLength":2}), json!("abc"), Some("Use at most 2 characters"))]
#[case::whole(json!({"type":"integer"}), json!(2.5), Some("Enter a whole number"))]
#[case::range(json!({"type":"number","minimum":1,"maximum":10}), json!(11), Some("Enter a number from 1 to 10"))]
#[case::text_for_number(json!({"type":"number"}), json!("7"), Some("Enter a number"))]
#[case::unknown_choice(json!({"type":"string","enum":["a","b"]}), json!("c"), Some("Choose one of the options"))]
#[case::repeated(json!({"type":"array","items":{"enum":["a","b"]}}), json!(["a","a"]), Some("Choose each option once"))]
#[case::too_many(json!({"type":"array","items":{"enum":["a","b"]},"maxItems":1}), json!(["a","b"]), Some("Choose at most 1"))]
#[case::boolean(json!({"type":"boolean"}), json!(true), None)]
fn answers_follow_the_fields_rules(
    #[case] property: Value,
    #[case] value: Value,
    #[case] problem: Option<&str>,
) {
    let fields = form(&json!({"type":"object","properties":{"field":property}}));
    assert_eq!(
        form_response(&fields, &answers(&json!({"field":value}))).err(),
        problem.map(|problem| ("field".to_owned(), problem.to_owned()))
    );
}

#[rstest]
fn a_form_needs_its_required_fields_and_nothing_else() {
    let fields = form(&json!({"type":"object","required":["name"],"properties":{
        "name":{"type":"string","title":"Name"},"note":{"type":"string"}
    }}));
    assert_eq!(
        form_response(&fields, &Map::new()).err(),
        Some(("name".to_owned(), "Name is required".to_owned()))
    );
    assert_eq!(
        form_response(&fields, &answers(&json!({"name":"Ada","invented":1}))).err(),
        Some((
            "invented".to_owned(),
            "This field isn't in the form".to_owned()
        ))
    );
    assert_eq!(
        form_response(&fields, &answers(&json!({"name":"Ada"}))),
        Ok(json!({"action":"accept","content":{"name":"Ada"}}))
    );
}

/// Only web pages open from a request, and only after the user sees their host.
#[rstest]
#[case::https("https://auth.example.com/device?code=1", Some("auth.example.com"))]
#[case::http("http://localhost:8080/callback", Some("localhost"))]
#[case::script("javascript:alert(1)", None)]
#[case::file("file:///etc/passwd", None)]
#[case::relative("/authorize", None)]
fn sign_in_pages_must_be_web_pages(#[case] url: &str, #[case] host: Option<&str>) {
    let elicitation =
        request(&json!({"mode":"url","message":"Sign in","url":url,"elicitationId":"e"}))
            .elicitation()
            .unwrap();
    assert_eq!(
        elicitation,
        host.map_or(Elicitation::Unsupported, |host| Elicitation::Url {
            url: url.to_owned(),
            host: host.to_owned()
        })
    );
}

/// A tool approval offers only the reusable scopes its server named.
#[rstest]
#[case::none(json!(null), [false, false])]
#[case::session(json!({"persist":"session"}), [true, false])]
#[case::both(json!({"codex_approval_kind":"mcp_tool_call","persist":["session","always"]}), [true, true])]
#[case::legacy(json!({"allowPersistentApproval":true}), [false, true])]
fn tool_approvals_offer_only_the_named_scopes(#[case] meta: Value, #[case] offered: [bool; 2]) {
    let approval = request(
        &json!({"mode":"form","message":"Allow the crm MCP server to run tool \"lookup\"?",
        "serverName":"crm","_meta":meta,"requestedSchema":{"type":"object","properties":{}}}),
    );
    assert_eq!(
        approval.approval_response(Decision::AllowOnce),
        Some(json!({"action":"accept","content":{}}))
    );
    assert_eq!(
        approval.approval_response(Decision::Deny),
        Some(json!({"action":"decline","content":null}))
    );
    let [session, always] = offered;
    assert_eq!(
        approval.approval_response(Decision::AllowSession),
        session.then(|| json!({"action":"accept","content":{},"_meta":{"persist":"session"}}))
    );
    assert_eq!(
        approval.approval_response(Decision::AlwaysAllow),
        always.then(|| json!({"action":"accept","content":{},"_meta":{"persist":"always"}}))
    );
}
