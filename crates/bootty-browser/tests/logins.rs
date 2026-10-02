use bootty_browser::{BrowserProfile, login_origin};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use rstest::rstest;
use std::path::PathBuf;

#[rstest]
#[case("https://example.com/path?key=value#section", "https://example.com")]
#[case("https://example.com:8443/", "https://example.com:8443")]
#[case("http://localhost:3000/login", "http://localhost:3000")]
#[case("http://127.0.0.1:3000/login", "http://127.0.0.1:3000")]
#[case("http://[::1]:3000/login", "http://[::1]:3000")]
fn saved_logins_use_exact_origins(#[case] address: &str, #[case] expected: &str) {
    assert_eq!(login_origin(address), Ok(expected.into()));
}

#[rstest]
#[case("http://example.com/login")]
#[case("https://user:secret@example.com/")]
#[case("about:blank")]
#[case("javascript:alert(1)")]
fn credentials_never_target_unsafe_pages(#[case] address: &str) {
    assert!(login_origin(address).is_err());
}

proptest! {
    #[test]
    fn credentials_are_shared_within_an_origin_but_isolated_by_identity_and_port(
        path in "[a-z]{1,30}", port in 1024u16..=65535,
    ) {
        let production = BrowserProfile::new(PathBuf::from("/config/bootty/browser"));
        let development = BrowserProfile::new(PathBuf::from("/config/bootty-dev/browser"));
        let service = production.credential_service("https://example.com/")?;
        prop_assert_eq!(&service, &production.credential_service(&format!("https://example.com/{path}?q=test"))?);
        assert_ne!(&service, &development.credential_service("https://example.com/")?);
        assert_ne!(&service, &production.credential_service(&format!("https://example.com:{port}/"))?);
    }
}
