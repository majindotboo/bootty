use async_channel::Receiver;

use super::{BrowserView, NativeBrowserError};
use crate::{BrowserDocumentSnapshot, valid_document_token};

impl BrowserView {
    /// Read the exact top document without navigating, focusing, or editing the page.
    /// # Errors
    /// Refuses invalid tokens, changed addresses, and native dispatch failures.
    pub fn document_snapshot(
        &self,
        document: &str,
        address: &str,
    ) -> Result<Receiver<Result<BrowserDocumentSnapshot, NativeBrowserError>>, NativeBrowserError>
    {
        if !valid_document_token(document) || self.current_address()? != address {
            return Err(NativeBrowserError::Platform(
                "The browser document changed before it could be read.".into(),
            ));
        }
        let payload = serde_json::json!([document, address]);
        let script = format!("({})({payload})", include_str!("../snapshot.js"));
        let document = document.to_owned();
        let address = address.to_owned();
        let (sender, receiver) = async_channel::bounded(1);
        self.view
            .evaluate_script_with_callback(&script, move |result| {
                _ = sender.try_send(BrowserDocumentSnapshot::parse(&result, &document, &address));
            })
            .map_err(|_| {
                NativeBrowserError::Platform("Browser document reading is unavailable.".into())
            })?;
        Ok(receiver)
    }
}
