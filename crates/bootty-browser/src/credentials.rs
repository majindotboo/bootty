use crate::{
    CredentialAccount, CredentialDecision, CredentialError, CredentialTarget, SecretPassword,
    WebOrigin,
};

/// An injected native store. Account listing must load metadata only, scoped to this origin.
pub trait CredentialStore {
    /// # Errors
    /// Returns a sanitized store failure without reading password data.
    fn accounts(&self, origin: &WebOrigin) -> Result<Vec<CredentialAccount>, CredentialError>;
    /// # Errors
    /// Returns a sanitized failure if the explicit entry cannot be saved.
    fn save(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
        password: &SecretPassword,
    ) -> Result<(), CredentialError>;
    /// # Errors
    /// Returns a sanitized failure if the exact entry cannot be read.
    fn read(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
    ) -> Result<SecretPassword, CredentialError>;
    /// # Errors
    /// Returns a sanitized failure if the exact entry cannot be deleted.
    fn delete(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
    ) -> Result<(), CredentialError>;
}

pub struct Credentials<S> {
    store: S,
}

impl<S: CredentialStore> Credentials<S> {
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    /// List accounts after an explicit native credential action, without reading passwords.
    ///
    /// # Errors
    /// Refuses changed pages/origins and returns store errors without secret data.
    pub fn accounts(
        &self,
        target: &CredentialTarget,
        current: &CredentialTarget,
    ) -> Result<Vec<CredentialAccount>, CredentialError> {
        target.validate_current(current)?;
        let mut accounts = self.store.accounts(target.origin())?;
        accounts.sort();
        accounts.dedup();
        Ok(accounts)
    }

    /// Save only the account and password entered into the native confirmation.
    ///
    /// # Errors
    /// Cancellation and changed targets touch no store; native failures remain explicit.
    pub fn save(
        &self,
        target: &CredentialTarget,
        current: &CredentialTarget,
        account: &CredentialAccount,
        password: &SecretPassword,
        decision: CredentialDecision,
    ) -> Result<(), CredentialError> {
        confirm(target, current, decision)?;
        self.store.save(target.origin(), account, password)
    }

    /// Read only the explicitly selected account. No account is chosen automatically.
    /// The host must recheck `target` against the current native page before filling the result.
    ///
    /// # Errors
    /// Missing selection, cancellation and changed targets touch no password entry.
    pub fn fill(
        &self,
        target: &CredentialTarget,
        current: &CredentialTarget,
        account: Option<&CredentialAccount>,
        decision: CredentialDecision,
    ) -> Result<SecretPassword, CredentialError> {
        confirm(target, current, decision)?;
        self.store.read(
            target.origin(),
            account.ok_or(CredentialError::AccountRequired)?,
        )
    }

    /// Delete only the origin and account shown in the native confirmation.
    ///
    /// # Errors
    /// Cancellation and changed targets touch no store; missing entries remain explicit.
    pub fn delete(
        &self,
        target: &CredentialTarget,
        current: &CredentialTarget,
        account: &CredentialAccount,
        decision: CredentialDecision,
    ) -> Result<(), CredentialError> {
        confirm(target, current, decision)?;
        self.store.delete(target.origin(), account)
    }
}

fn confirm(
    target: &CredentialTarget,
    current: &CredentialTarget,
    decision: CredentialDecision,
) -> Result<(), CredentialError> {
    if decision == CredentialDecision::Cancel {
        return Err(CredentialError::Cancelled);
    }
    target.validate_current(current)
}
