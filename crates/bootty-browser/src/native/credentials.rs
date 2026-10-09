use async_channel::Receiver;

use super::{BrowserView, NativeBrowserError};
use crate::{CredentialAccount, CredentialError, CredentialTarget, SecretPassword, WebOrigin};

impl BrowserView {
    /// Capture the current top document before showing the native credential confirmation.
    ///
    /// # Errors
    /// Reports sanitized native dispatch failures; callback results contain only a bounded token.
    pub fn capture_credential_document(
        &self,
    ) -> Result<Receiver<Result<String, CredentialError>>, NativeBrowserError> {
        let (sender, receiver) = async_channel::bounded(1);
        self.view.evaluate_script_with_callback(
            "(() => { try { return window.__boottyCredentials?.document() ?? null; } catch { return null; } })()",
            move |result| {
                let token = if result.len() <= 64 {
                    serde_json::from_str::<String>(&result).ok().filter(|token| crate::valid_document_token(token))
                } else {
                    None
                };
                _ = sender.try_send(token.ok_or(CredentialError::Unavailable));
            },
        ).map_err(|_| NativeBrowserError::Platform("Credential document capture is unavailable.".into()))?;
        Ok(receiver)
    }

    /// Fill an explicitly chosen origin/account after the host rechecks page identity and revision.
    /// The document-start closure rechecks the exact document and origin at script execution.
    ///
    /// # Errors
    /// Refuses changed origins, invalid tokens, and sanitized native dispatch failures. The
    /// callback reports missing/ambiguous forms without returning field contents or submitting.
    pub fn fill_credential(
        &self,
        target: &CredentialTarget,
        document: &str,
        account: &CredentialAccount,
        password: &SecretPassword,
    ) -> Result<Receiver<Result<(), CredentialError>>, NativeBrowserError> {
        if !crate::valid_document_token(document)
            || WebOrigin::parse(&self.current_address()?).ok().as_ref() != Some(target.origin())
        {
            return Err(NativeBrowserError::Platform(
                "The browser page changed. Open the credential dialog again.".into(),
            ));
        }
        let payload = serde_json::json!({
            "origin": target.origin().as_str(),
            "document": document,
            "account": account.as_str(),
            "password": password.expose(),
        });
        let script = format!(
            "(() => {{ try {{ return window.__boottyCredentials?.fill({payload}) === true; }} catch {{ return false; }} }})()"
        );
        let (sender, receiver) = async_channel::bounded(1);
        self.view
            .evaluate_script_with_callback(&script, move |result| {
                let filled =
                    result.len() <= 8 && serde_json::from_str::<bool>(&result).ok() == Some(true);
                _ = sender.try_send(if filled {
                    Ok(())
                } else {
                    Err(CredentialError::FillUnavailable)
                });
            })
            .map_err(|_| {
                NativeBrowserError::Platform("Credential filling is unavailable.".into())
            })?;
        Ok(receiver)
    }
}
