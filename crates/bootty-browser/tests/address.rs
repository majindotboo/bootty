use bootty_browser::{AddressError, normalize_address, resolve_address};
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

#[rstest]
#[case("terminal ui", "https://duckduckgo.com/?q=terminal+ui")]
#[case("localhost:3000", "http://localhost:3000/")]
#[case("example.com/docs", "https://example.com/docs")]
#[case("rust", "https://duckduckgo.com/?q=rust")]
fn address_bar_resolves_searches_and_websites(#[case] input: &str, #[case] expected: &str) {
    assert_eq!(
        resolve_address(input, "https://duckduckgo.com/"),
        Ok(expected.to_owned())
    );
}

#[rstest]
#[case("javascript:alert(1)")]
#[case("data:text/html,hi")]
#[case("file:///tmp/page.html")]
#[case("https://user:password@example.com")]
fn searching_does_not_hide_unsafe_addresses(#[case] input: &str) {
    assert_eq!(
        resolve_address(input, "https://duckduckgo.com/"),
        normalize_address(input)
    );
}

proptest! {
    #[test]
    fn search_terms_round_trip_without_changing_the_destination(words in "[a-zA-Z][a-zA-Z &#+=?]{0,99}") {
        let resolved = resolve_address(&words, "https://www.google.com/search")?;
        let url = url::Url::parse(&resolved)?;
        prop_assert_eq!(url.host_str(), Some("www.google.com"));
        prop_assert_eq!(url.query_pairs().find(|(key, _)| key == "q").map(|(_, value)| value.into_owned()), Some(words.trim().to_owned()));
    }
}
