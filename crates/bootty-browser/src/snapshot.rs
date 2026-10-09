use serde::{Deserialize, Serialize};

/// A bounded read of one top document, without DOM markup or form field values.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserDocumentSnapshot {
    pub document: String,
    pub address: String,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}

/// The document-start token distinguishes a reload from the same page at the same URL.
#[must_use]
pub fn valid_document_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl BrowserDocumentSnapshot {
    /// Validate the untrusted native callback against its host-captured document and address.
    /// # Errors
    /// Rejects changed documents, addresses, oversized fields, and malformed callback data.
    pub fn parse(
        response: &str,
        document: &str,
        address: &str,
    ) -> Result<Self, crate::NativeBrowserError> {
        // JSON escaping can expand each decoded byte to six bytes.
        let snapshot = (response.len() <= 6 * (64 * 1024 + 8192 + 1024) + 1024)
            .then(|| serde_json::from_str::<Self>(response).ok())
            .flatten()
            .filter(|snapshot| {
                valid_document_token(document)
                    && snapshot.document == document
                    && snapshot.address == address
                    && !snapshot.address.is_empty()
                    && snapshot.address.len() <= 8192
                    && snapshot.title.len() <= 1024
                    && snapshot.text.len() <= 64 * 1024
            });
        snapshot.ok_or_else(|| {
            crate::NativeBrowserError::Platform(
                "The browser document changed or its snapshot is unavailable.".into(),
            )
        })
    }
}
