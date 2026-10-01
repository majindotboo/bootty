use bootty_browser::{AddressError, normalize_address};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("example.com", "https://example.com/")]
#[case("example.com:8443/app", "https://example.com:8443/app")]
#[case(
    " https://example.com/docs?x=1#part ",
    "https://example.com/docs?x=1#part"
)]
#[case("localhost:3000", "http://localhost:3000/")]
#[case("127.0.0.1:8080/app", "http://127.0.0.1:8080/app")]
#[case("[::1]:8080", "http://[::1]:8080/")]
#[case("about:blank", "about:blank")]
fn web_addresses(#[case] input: &str, #[case] expected: &str) {
    assert_eq!(normalize_address(input), Ok(expected.to_owned()));
}

#[rstest]
#[case("", AddressError::Empty)]
#[case("javascript:alert(1)", AddressError::UnsupportedScheme)]
#[case("file:///etc/passwd", AddressError::UnsupportedScheme)]
#[case("data:text/html,hello", AddressError::UnsupportedScheme)]
#[case("https://user:secret@example.com", AddressError::Credentials)]
#[case("example.com some text", AddressError::Invalid)]
fn invalid_addresses(#[case] input: &str, #[case] expected: AddressError) {
    assert_eq!(normalize_address(input), Err(expected));
}

proptest! {
    #[test]
    fn bare_domains_preserve_paths(host in "[a-z]{1,20}", path in "[a-z]{0,20}") {
        let normalized = normalize_address(&format!("{host}.example/{path}"))?;
        prop_assert_eq!(&normalized, &format!("https://{host}.example/{path}"));
        prop_assert_eq!(normalize_address(&normalized), Ok(normalized));
    }

    #[test]
    fn loopback_ports_keep_http(port in 1u16..=u16::MAX) {
        prop_assert_eq!(normalize_address(&format!("localhost:{port}")), Ok(format!("http://localhost:{port}/")));
    }
}
