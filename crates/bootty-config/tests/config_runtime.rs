#![allow(clippy::float_cmp)] // Acceptance preserves exact assigned font sizes.

use assert_fs::{TempDir, prelude::*};
use bootty_config::{ConfigRuntime, config::load_config_from_path};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(false)]
#[case(true)]
fn rejected_candidate_retains_live_state_until_accepted(#[case] reload: bool) {
    let directory = TempDir::new().expect("temporary config directory");
    let file = directory.child("config.toml");
    file.write_str("[window]\ntitle = \"accepted\"\n[font]\nsize = 14\n")
        .expect("write initial config");
    let mut runtime = ConfigRuntime::new(load_config_from_path(file.path()).expect("load config"))
        .expect("create runtime");
    runtime.set_font_size(20.0);
    let revision = runtime.revision();
    let mut candidate = runtime.document().clone();
    candidate
        .set_str(&["window", "title"], "candidate")
        .expect("edit title");
    candidate
        .set_f32(&["font", "size"], 16.0)
        .expect("edit font");

    if reload {
        file.write_str("[window]\ntitle = \"candidate\"\n[font]\nsize = 16\n")
            .expect("external config edit");
        assert!(
            runtime
                .reload(|_| Err::<(), _>("rejected".to_owned()))
                .is_err()
        );
    } else {
        assert!(
            runtime
                .commit_document(candidate.clone(), |_| Err::<(), _>("rejected".to_owned()))
                .is_err()
        );
    }
    assert_eq!(runtime.current().window.title, "accepted");
    assert_eq!(runtime.current().font.size, 20.0);
    assert_eq!(runtime.configured_font_size(), 14.0);
    assert_eq!(runtime.revision(), revision);

    let change = if reload {
        runtime.reload(|_| Ok(())).expect("accept reload").0
    } else {
        runtime
            .commit_document(candidate, |_| Ok(()))
            .expect("accept document")
            .0
    };
    assert_eq!(change.previous().window.title, "accepted");
    assert_eq!(change.previous().font.size, 20.0);
    assert_eq!(change.current().window.title, "candidate");
    assert_eq!(runtime.current().font.size, 16.0);
    assert_eq!(runtime.configured_font_size(), 16.0);
    assert_eq!(runtime.revision(), revision.wrapping_add(1));
}
