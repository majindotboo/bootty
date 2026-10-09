use assert_fs::prelude::*;
use bootty_config::config::{BoottyConfig, BrowserSearchEngine, load_config_from_path};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::error::Error;

fn load(source: &str) -> Result<BoottyConfig, Box<dyn Error>> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.child("config.toml");
    path.write_str(source)?;
    load_config_from_path(path.path()).map_err(Into::into)
}

#[rstest]
#[case(
    "duckduckgo",
    BrowserSearchEngine::DuckDuckGo,
    "https://duckduckgo.com/"
)]
#[case("google", BrowserSearchEngine::Google, "https://www.google.com/search")]
#[case("bing", BrowserSearchEngine::Bing, "https://www.bing.com/search")]
#[case("brave", BrowserSearchEngine::Brave, "https://search.brave.com/search")]
fn search_engine_tokens_load_their_search_base(
    #[case] token: &str,
    #[case] expected: BrowserSearchEngine,
    #[case] address: &str,
) -> Result<(), Box<dyn Error>> {
    let config = load(&format!("[browser]\nsearch-engine = \"{token}\"\n"))?;

    assert_eq!(config.browser.search_engine, expected);
    assert_eq!(config.browser.search_engine.address(), address);
    Ok(())
}

#[rstest]
#[case("", true)]
#[case("[browser]\n", true)]
#[case("[browser]\npersist-site-data = false\n", false)]
fn browser_defaults_apply_to_missing_fields_and_can_be_overridden(
    #[case] source: &str,
    #[case] persistent: bool,
) {
    let config = load(source).expect("valid browser configuration");
    assert_eq!(
        config.browser.search_engine,
        BrowserSearchEngine::DuckDuckGo
    );
    assert_eq!(config.browser.persist_site_data, persistent);
}

#[test]
fn unknown_search_engine_is_rejected() {
    let error = load("[browser]\nsearch-engine = \"example\"\n")
        .expect_err("search-engine accepts only its declared choices");

    assert!(error.to_string().contains("unknown variant"));
}
