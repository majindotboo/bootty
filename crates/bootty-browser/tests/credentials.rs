use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use bootty_browser::{
    CredentialAccount, CredentialDecision, CredentialError, CredentialStore, CredentialTarget,
    Credentials, SecretPassword, WebOrigin,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[derive(Clone, Default)]
struct FakeStore {
    calls: Rc<Cell<usize>>,
    entries: Rc<RefCell<BTreeMap<(WebOrigin, CredentialAccount), String>>>,
    failure: Option<CredentialError>,
}

impl FakeStore {
    fn access(&self) -> Result<(), CredentialError> {
        self.calls.set(
            self.calls
                .get()
                .checked_add(1)
                .ok_or(CredentialError::StoreFailed)?,
        );
        self.failure.map_or(Ok(()), Err)
    }
}

impl CredentialStore for FakeStore {
    fn accounts(&self, origin: &WebOrigin) -> Result<Vec<CredentialAccount>, CredentialError> {
        self.access()?;
        Ok(self
            .entries
            .borrow()
            .keys()
            .filter(|(saved_origin, _)| saved_origin == origin)
            .map(|(_, account)| account.clone())
            .collect())
    }

    fn save(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
        password: &SecretPassword,
    ) -> Result<(), CredentialError> {
        self.access()?;
        self.entries.borrow_mut().insert(
            (origin.clone(), account.clone()),
            password.expose().to_owned(),
        );
        Ok(())
    }

    fn read(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
    ) -> Result<SecretPassword, CredentialError> {
        self.access()?;
        let password = self
            .entries
            .borrow()
            .get(&(origin.clone(), account.clone()))
            .cloned()
            .ok_or(CredentialError::NotFound)?;
        SecretPassword::new(password)
    }

    fn delete(
        &self,
        origin: &WebOrigin,
        account: &CredentialAccount,
    ) -> Result<(), CredentialError> {
        self.access()?;
        self.entries
            .borrow_mut()
            .remove(&(origin.clone(), account.clone()))
            .ok_or(CredentialError::NotFound)?;
        Ok(())
    }
}

#[fixture]
fn store() -> FakeStore {
    FakeStore::default()
}

fn target(address: &str) -> Result<CredentialTarget, CredentialError> {
    CredentialTarget::new(1, 3, address)
}

proptest! {
    #[test]
    fn canonical_origins_ignore_document_parts(host in "[a-z]{1,16}", path in "[a-z]{0,32}") {
        let plain = WebOrigin::parse(&format!("https://{host}.example"))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let document = WebOrigin::parse(&format!("https://{host}.example:443/{path}?secret=dummy#fragment"))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(plain.as_str(), format!("https://{host}.example"));
        prop_assert_eq!(document, plain);
    }

    #[test]
    fn save_fill_delete_only_the_explicit_key(account_name in "[a-zA-Z0-9 _.-]{1,40}", password_text in "[a-zA-Z0-9!# _-]{1,64}") {
        let store = FakeStore::default();
        let credentials = Credentials::new(store.clone());
        let page = target("https://accounts.example/login").map_err(|error| TestCaseError::fail(error.to_string()))?;
        let other = target("https://different.example").map_err(|error| TestCaseError::fail(error.to_string()))?;
        let account = CredentialAccount::new(account_name).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let password = SecretPassword::new(password_text.clone()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(format!("{password:?}"), "SecretPassword([redacted])");
        credentials.save(&page, &page, &account, &password, CredentialDecision::Confirm)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(credentials.fill(&other, &other, Some(&account), CredentialDecision::Confirm).err(), Some(CredentialError::NotFound));
        let loaded = credentials.fill(&page, &page, Some(&account), CredentialDecision::Confirm)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(loaded.expose(), password_text);
        credentials.delete(&page, &page, &account, CredentialDecision::Confirm)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(credentials.fill(&page, &page, Some(&account), CredentialDecision::Confirm).err(), Some(CredentialError::NotFound));
        prop_assert_eq!(store.entries.borrow().len(), 0);
    }
}

#[rstest]
#[case("https://login.example", 2, 3, CredentialError::StalePage)]
#[case("https://login.example", 1, 4, CredentialError::StalePage)]
#[case("http://login.example", 1, 3, CredentialError::WrongOrigin)]
#[case("https://login.example:444", 1, 3, CredentialError::WrongOrigin)]
#[case("https://child.login.example", 1, 3, CredentialError::WrongOrigin)]
fn changed_target_touches_no_store(
    store: FakeStore,
    #[case] address: &str,
    #[case] id: u64,
    #[case] revision: u64,
    #[case] expected: CredentialError,
) -> Result<(), CredentialError> {
    let credentials = Credentials::new(store.clone());
    let captured = target("https://login.example")?;
    let current = CredentialTarget::new(id, revision, address)?;
    let account = CredentialAccount::new("dummy".to_owned())?;
    let password = SecretPassword::new("dummy-password".to_owned())?;
    assert_eq!(credentials.accounts(&captured, &current), Err(expected));
    assert_eq!(
        credentials.save(
            &captured,
            &current,
            &account,
            &password,
            CredentialDecision::Confirm
        ),
        Err(expected)
    );
    assert_eq!(
        credentials
            .fill(
                &captured,
                &current,
                Some(&account),
                CredentialDecision::Confirm
            )
            .err(),
        Some(expected)
    );
    assert_eq!(
        credentials.delete(&captured, &current, &account, CredentialDecision::Confirm),
        Err(expected)
    );
    assert_eq!(store.calls.get(), 0);
    Ok(())
}

#[rstest]
fn cancellation_never_accesses_the_store(store: FakeStore) -> Result<(), CredentialError> {
    let credentials = Credentials::new(store.clone());
    let page = target("https://login.example")?;
    let account = CredentialAccount::new("dummy".to_owned())?;
    let password = SecretPassword::new("dummy-password".to_owned())?;
    assert_eq!(
        credentials.save(
            &page,
            &page,
            &account,
            &password,
            CredentialDecision::Cancel
        ),
        Err(CredentialError::Cancelled)
    );
    assert_eq!(
        credentials
            .fill(&page, &page, Some(&account), CredentialDecision::Cancel)
            .err(),
        Some(CredentialError::Cancelled)
    );
    assert_eq!(
        credentials.delete(&page, &page, &account, CredentialDecision::Cancel),
        Err(CredentialError::Cancelled)
    );
    assert_eq!(store.calls.get(), 0);
    Ok(())
}

#[rstest]
fn multiple_accounts_require_an_explicit_selection(
    store: FakeStore,
) -> Result<(), CredentialError> {
    let credentials = Credentials::new(store.clone());
    let page = target("https://login.example")?;
    let alpha = CredentialAccount::new("alpha".to_owned())?;
    let zulu = CredentialAccount::new("zulu".to_owned())?;
    store.entries.borrow_mut().insert(
        (page.origin().clone(), zulu.clone()),
        "dummy-zulu".to_owned(),
    );
    store.entries.borrow_mut().insert(
        (page.origin().clone(), alpha.clone()),
        "dummy-alpha".to_owned(),
    );
    assert_eq!(
        credentials.accounts(&page, &page)?,
        vec![alpha, zulu.clone()]
    );
    let before = store.calls.get();
    assert_eq!(
        credentials
            .fill(&page, &page, None, CredentialDecision::Confirm)
            .err(),
        Some(CredentialError::AccountRequired)
    );
    assert_eq!(store.calls.get(), before);
    assert_eq!(
        credentials
            .fill(&page, &page, Some(&zulu), CredentialDecision::Confirm)?
            .expose(),
        "dummy-zulu"
    );
    Ok(())
}

#[rstest]
#[case(CredentialError::Locked)]
#[case(CredentialError::Unavailable)]
#[case(CredentialError::Cancelled)]
#[case(CredentialError::Unsupported)]
#[case(CredentialError::StoreFailed)]
fn native_failures_remain_explicit(
    store: FakeStore,
    #[case] error: CredentialError,
) -> Result<(), CredentialError> {
    let failing = FakeStore {
        failure: Some(error),
        ..store
    };
    let credentials = Credentials::new(failing);
    let page = target("https://login.example")?;
    let account = CredentialAccount::new("dummy".to_owned())?;
    let password = SecretPassword::new("dummy-password".to_owned())?;
    assert_eq!(credentials.accounts(&page, &page), Err(error));
    assert_eq!(
        credentials.save(
            &page,
            &page,
            &account,
            &password,
            CredentialDecision::Confirm
        ),
        Err(error)
    );
    assert_eq!(
        credentials
            .fill(&page, &page, Some(&account), CredentialDecision::Confirm)
            .err(),
        Some(error)
    );
    assert_eq!(
        credentials.delete(&page, &page, &account, CredentialDecision::Confirm),
        Err(error)
    );
    Ok(())
}

#[rstest]
#[case("file:///tmp/passwords")]
#[case("data:text/html,dummy")]
#[case("https://dummy:password@login.example")]
#[case("https://dummy@login.example")]
#[case("not-an-address")]
fn invalid_origins_are_refused(#[case] address: &str) {
    assert_eq!(
        WebOrigin::parse(address),
        Err(CredentialError::InvalidOrigin)
    );
}

#[rstest]
fn navigation_after_read_invalidates_the_fill(store: FakeStore) -> Result<(), CredentialError> {
    let credentials = Credentials::new(store);
    let captured = target("https://login.example")?;
    let account = CredentialAccount::new("dummy".to_owned())?;
    let password = SecretPassword::new("dummy-password".to_owned())?;
    credentials.save(
        &captured,
        &captured,
        &account,
        &password,
        CredentialDecision::Confirm,
    )?;
    let loaded = credentials.fill(
        &captured,
        &captured,
        Some(&account),
        CredentialDecision::Confirm,
    )?;
    let current = CredentialTarget::new(1, 4, "https://login.example")?;
    assert_eq!(
        captured.validate_current(&current),
        Err(CredentialError::StalePage)
    );
    assert_eq!(format!("{loaded:?}"), "SecretPassword([redacted])");
    Ok(())
}

#[rstest]
#[case("", CredentialError::InvalidAccount)]
#[case("dummy\naccount", CredentialError::InvalidAccount)]
fn invalid_accounts_are_refused(#[case] account: &str, #[case] expected: CredentialError) {
    assert_eq!(CredentialAccount::new(account.to_owned()), Err(expected));
}

#[rstest]
#[case(0, false)]
#[case(1, true)]
#[case(4096, true)]
#[case(4097, false)]
fn password_length_is_bounded(#[case] bytes: usize, #[case] accepted: bool) {
    assert_eq!(SecretPassword::new("x".repeat(bytes)).is_ok(), accepted);
}
