//! MCP elicitations: what a server asks for, and the only answers Bootty sends back. Every accept
//! is checked against the server's own schema, and accepting a tool approval grants only the scope
//! the server offered.
use serde_json::{Map, Value, json};

use crate::native_protocol::field;

/// Deliberate limit: a form shows at most this many fields and options per field, so a server
/// can't grow the request panel without bound. Raise it if real servers hit it.
const MAX_FIELDS: usize = 32;
const MAX_OPTIONS: usize = 64;
/// Text answers longer than this many characters are refused whatever the schema allows.
const MAX_TEXT_CHARS: usize = 4096;

/// What an MCP server asks of the user.
#[derive(Clone, Debug, PartialEq)]
pub enum Elicitation {
    /// A yes/no confirmation, with the reusable scopes the server offers.
    Approval { session: bool, always: bool },
    /// Fields to fill in, in the server's order.
    Form(Vec<ElicitationField>),
    /// A page the user opens in their browser, such as a sign-in.
    Url { url: String, host: String },
    /// A request Bootty can't fill in; it can only be declined or cancelled.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElicitationField {
    pub key: String,
    /// The schema's title, or the key without one.
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text {
        format: Option<TextFormat>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        default: Option<String>,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
        default: Option<f64>,
    },
    Boolean {
        default: bool,
    },
    Choice {
        options: Vec<FieldOption>,
        default: Option<String>,
    },
    Choices {
        options: Vec<FieldOption>,
        min_items: Option<usize>,
        max_items: Option<usize>,
        default: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

/// Read an `mcpServer/elicitation/request`'s parameters.
#[must_use]
pub fn parse(parameters: &Value) -> Elicitation {
    match field(parameters, "mode").as_str() {
        Some("url") => url(field(parameters, "url").as_str().unwrap_or_default()),
        Some("form") => {
            form(field(parameters, "requestedSchema")).map_or(Elicitation::Unsupported, |fields| {
                if fields.is_empty() {
                    let (session, always) = persistence(field(parameters, "_meta"));
                    Elicitation::Approval { session, always }
                } else {
                    Elicitation::Form(fields)
                }
            })
        }
        _ => Elicitation::Unsupported,
    }
}

/// Only web pages open from a request; the user sees the host before anything opens.
fn url(url: &str) -> Elicitation {
    let Ok(parsed) = url::Url::parse(url) else {
        return Elicitation::Unsupported;
    };
    match (parsed.scheme(), parsed.host_str()) {
        ("https" | "http", Some(host)) if url.len() <= 8192 => Elicitation::Url {
            url: url.to_owned(),
            host: host.to_owned(),
        },
        _ => Elicitation::Unsupported,
    }
}

/// The reusable scopes in Codex's approval metadata: `persist` names `session` and `always`.
fn persistence(meta: &Value) -> (bool, bool) {
    let offered = |scope: &str| match field(meta, "persist") {
        Value::String(value) => value == scope,
        Value::Array(values) => values.iter().any(|value| value == scope),
        _ => false,
    };
    (
        offered("session"),
        offered("always") || field(meta, "allowPersistentApproval") == true,
    )
}

fn form(schema: &Value) -> Option<Vec<ElicitationField>> {
    if field(schema, "type") != "object" {
        return None;
    }
    let properties = field(schema, "properties").as_object()?;
    let required = match field(schema, "required") {
        Value::Null => Vec::new(),
        Value::Array(keys) => keys
            .iter()
            .map(|key| key.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if properties.len() > MAX_FIELDS || required.iter().any(|key| !properties.contains_key(key)) {
        return None;
    }
    properties
        .iter()
        .map(|(key, property)| {
            Some(ElicitationField {
                key: key.clone(),
                title: text(property, "title").unwrap_or_else(|| key.clone()),
                description: text(property, "description"),
                required: required.contains(key),
                kind: kind(property)?,
            })
        })
        .collect()
}

fn text(value: &Value, key: &str) -> Option<String> {
    field(value, key)
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(512).collect())
}

fn count(value: &Value, key: &str) -> Option<usize> {
    field(value, key)
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
}

fn kind(property: &Value) -> Option<FieldKind> {
    match field(property, "type").as_str()? {
        "string" => {
            if let Some(options) = options(property) {
                return Some(FieldKind::Choice {
                    default: field(property, "default")
                        .as_str()
                        .filter(|value| options.iter().any(|option| option.value == *value))
                        .map(str::to_owned),
                    options,
                });
            }
            Some(FieldKind::Text {
                format: match field(property, "format").as_str() {
                    None => None,
                    Some("email") => Some(TextFormat::Email),
                    Some("uri") => Some(TextFormat::Uri),
                    Some("date") => Some(TextFormat::Date),
                    Some("date-time") => Some(TextFormat::DateTime),
                    Some(_) => return None,
                },
                min_length: count(property, "minLength"),
                max_length: count(property, "maxLength"),
                default: field(property, "default").as_str().map(str::to_owned),
            })
        }
        kind @ ("number" | "integer") => Some(FieldKind::Number {
            integer: kind == "integer",
            minimum: field(property, "minimum").as_f64(),
            maximum: field(property, "maximum").as_f64(),
            default: field(property, "default").as_f64(),
        }),
        "boolean" => Some(FieldKind::Boolean {
            default: field(property, "default") == true,
        }),
        "array" => {
            let options = options(field(property, "items"))?;
            let default = field(property, "default")
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|value| options.iter().any(|option| option.value == *value))
                .map(str::to_owned)
                .collect();
            Some(FieldKind::Choices {
                options,
                min_items: count(property, "minItems"),
                max_items: count(property, "maxItems"),
                default,
            })
        }
        _ => None,
    }
}

/// Choices from `enum` (labelled by `enumNames`), `oneOf` or `anyOf` constants.
fn options(schema: &Value) -> Option<Vec<FieldOption>> {
    let options = if let Some(values) = field(schema, "enum").as_array() {
        let names = field(schema, "enumNames").as_array();
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.as_str()?.to_owned();
                let label = names
                    .and_then(|names| names.get(index))
                    .and_then(Value::as_str)
                    .map_or_else(|| value.clone(), str::to_owned);
                Some(FieldOption { value, label })
            })
            .collect::<Option<Vec<_>>>()?
    } else {
        let constants = field(schema, "oneOf")
            .as_array()
            .or_else(|| field(schema, "anyOf").as_array())?;
        constants
            .iter()
            .map(|option| {
                let value = field(option, "const").as_str()?.to_owned();
                Some(FieldOption {
                    label: text(option, "title").unwrap_or_else(|| value.clone()),
                    value,
                })
            })
            .collect::<Option<Vec<_>>>()?
    };
    (!options.is_empty() && options.len() <= MAX_OPTIONS).then_some(options)
}

impl ElicitationField {
    /// Why `value` can't be this field's answer, in words for the person filling it in. `None`
    /// is an empty answer.
    ///
    /// # Errors
    /// Returns the first rule the answer breaks.
    pub fn check(&self, value: Option<&Value>) -> Result<(), String> {
        let Some(value) = value else {
            return if self.required {
                Err(format!("{} is required", self.title))
            } else {
                Ok(())
            };
        };
        match &self.kind {
            FieldKind::Text {
                format,
                min_length,
                max_length,
                ..
            } => {
                let text = value.as_str().ok_or("Enter text")?;
                let length = text.chars().count();
                if length > MAX_TEXT_CHARS || max_length.is_some_and(|max| length > max) {
                    return Err(format!(
                        "Use at most {} characters",
                        max_length.unwrap_or(MAX_TEXT_CHARS).min(MAX_TEXT_CHARS)
                    ));
                }
                if let Some(min) = min_length.filter(|min| length < *min) {
                    return Err(format!("Use at least {min} characters"));
                }
                format.map_or(Ok(()), |format| check_format(format, text))
            }
            FieldKind::Number {
                integer,
                minimum,
                maximum,
                ..
            } => {
                let number = value.as_f64().ok_or("Enter a number")?;
                if *integer && (number.fract() != 0.0 || !number.is_finite()) {
                    return Err("Enter a whole number".into());
                }
                if minimum.is_some_and(|min| number < min)
                    || maximum.is_some_and(|max| number > max)
                {
                    return Err(match (minimum, maximum) {
                        (Some(min), Some(max)) => format!("Enter a number from {min} to {max}"),
                        (Some(min), None) => format!("Enter {min} or more"),
                        (None, Some(max)) => format!("Enter {max} or less"),
                        (None, None) => "Enter a number".into(),
                    });
                }
                Ok(())
            }
            FieldKind::Boolean { .. } => value
                .as_bool()
                .map(|_| ())
                .ok_or_else(|| "Choose yes or no".into()),
            FieldKind::Choice { options, .. } => value
                .as_str()
                .filter(|value| options.iter().any(|option| option.value == *value))
                .map(|_| ())
                .ok_or_else(|| "Choose one of the options".into()),
            FieldKind::Choices {
                options,
                min_items,
                max_items,
                ..
            } => {
                let chosen = value.as_array().ok_or("Choose from the options")?;
                let mut seen = Vec::new();
                for value in chosen {
                    let value = value
                        .as_str()
                        .filter(|value| options.iter().any(|option| option.value == *value))
                        .ok_or("Choose from the options")?;
                    if seen.contains(&value) {
                        return Err("Choose each option once".into());
                    }
                    seen.push(value);
                }
                if let Some(min) = min_items.filter(|min| chosen.len() < *min) {
                    return Err(format!("Choose at least {min}"));
                }
                if let Some(max) = max_items.filter(|max| chosen.len() > *max) {
                    return Err(format!("Choose at most {max}"));
                }
                Ok(())
            }
        }
    }
}

fn check_format(format: TextFormat, text: &str) -> Result<(), String> {
    let valid = match format {
        TextFormat::Email => text.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !text.chars().any(char::is_whitespace)
        }),
        TextFormat::Uri => url::Url::parse(text).is_ok(),
        TextFormat::Date => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
        TextFormat::DateTime => chrono::DateTime::parse_from_rfc3339(text).is_ok(),
    };
    if valid {
        Ok(())
    } else {
        Err(match format {
            TextFormat::Email => "Enter an email address",
            TextFormat::Uri => "Enter a full address, such as https://example.com",
            TextFormat::Date => "Enter a date as YYYY-MM-DD",
            TextFormat::DateTime => "Enter a date and time, such as 2026-01-31T09:30:00Z",
        }
        .into())
    }
}

/// The accept answer for a form, or the first field the answers don't satisfy. `answers` holds
/// the non-empty answers by field key.
///
/// # Errors
/// Returns the first field's problem, prefixed with nothing so it can show beside that field.
pub fn form_response(
    fields: &[ElicitationField],
    answers: &Map<String, Value>,
) -> Result<Value, (String, String)> {
    for field in fields {
        field
            .check(answers.get(&field.key))
            .map_err(|problem| (field.key.clone(), problem))?;
    }
    if let Some(key) = answers
        .keys()
        .find(|key| !fields.iter().any(|field| field.key == **key))
    {
        return Err((key.clone(), "This field isn't in the form".into()));
    }
    Ok(json!({"action":"accept","content":answers}))
}

/// The accept answer for a confirmation, granting `scope` when the server offered it.
#[must_use]
pub fn approval_response(parameters: &Value, scope: Option<&str>) -> Option<Value> {
    let Elicitation::Approval { session, always } = parse(parameters) else {
        return None;
    };
    match scope {
        None => Some(json!({"action":"accept","content":{}})),
        Some("session") if session => {
            Some(json!({"action":"accept","content":{},"_meta":{"persist":"session"}}))
        }
        Some("always") if always => {
            Some(json!({"action":"accept","content":{},"_meta":{"persist":"always"}}))
        }
        Some(_) => None,
    }
}

/// Whether this is a confirmation with nothing to fill in.
pub fn is_approval(parameters: &Value) -> bool {
    matches!(parse(parameters), Elicitation::Approval { .. })
}

/// Check an answer before it reaches the server.
///
/// # Errors
/// Rejects answers the request didn't ask for, content the schema doesn't allow, and scopes the
/// server didn't offer.
pub fn validate_response(parameters: &Value, response: &Value) -> Result<(), String> {
    if response.as_object().is_none_or(|fields| {
        fields
            .keys()
            .any(|key| !matches!(key.as_str(), "action" | "content" | "_meta"))
    }) {
        return Err("MCP decisions require an explicit action and optional content".into());
    }
    let meta = field(response, "_meta");
    match field(response, "action").as_str() {
        Some("decline" | "cancel") if field(response, "content").is_null() && meta.is_null() => {
            Ok(())
        }
        Some("accept") => match parse(parameters) {
            Elicitation::Approval { .. } => {
                let scope = match meta {
                    Value::Null => None,
                    meta => Some(
                        meta.as_object()
                            .filter(|meta| meta.len() == 1)
                            .and_then(|_| field(meta, "persist").as_str())
                            .ok_or("MCP approval metadata may only name a scope")?,
                    ),
                };
                if approval_response(parameters, scope).as_ref() == Some(response) {
                    Ok(())
                } else {
                    Err("This MCP approval doesn't offer that scope".into())
                }
            }
            Elicitation::Form(fields) if meta.is_null() => {
                let answers = field(response, "content")
                    .as_object()
                    .ok_or("MCP form answers must be an object")?;
                form_response(&fields, answers)
                    .map(|_| ())
                    .map_err(|(_, problem)| problem)
            }
            Elicitation::Url { .. } if meta.is_null() && field(response, "content").is_null() => {
                Ok(())
            }
            _ => Err("This MCP request can only be declined or cancelled".into()),
        },
        _ => Err("MCP requests require accept, decline or cancel without invented content".into()),
    }
}
