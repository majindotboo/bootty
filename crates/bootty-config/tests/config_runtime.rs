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

#[rstest]
fn a_stale_commit_does_not_publish_its_candidate() {
    let directory = TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str("[window]\ntitle = 'accepted'\n").unwrap();
    let mut runtime = ConfigRuntime::new(load_config_from_path(file.path()).unwrap()).unwrap();
    let mut candidate = runtime.document().clone();
    candidate.set_str(&["window", "title"], "draft").unwrap();
    let revision = runtime.revision();
    file.write_str("[window]\ntitle = 'external'\n").unwrap();

    assert!(runtime.commit_document(candidate, |_| Ok(())).is_err());
    assert_eq!(runtime.current().window.title, "accepted");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(
        runtime.document().str_at(&["window", "title"]),
        Some("accepted")
    );
    runtime.reload(|_| Ok(())).unwrap();
    assert_eq!(runtime.current().window.title, "external");
}

#[rstest]
fn reload_retains_the_root_document_without_flattening_includes() {
    let directory = TempDir::new().unwrap();
    let root = directory.child("config.toml");
    let included = directory.child("included.toml");
    root.write_str("[window]\ntitle = 'initial'\n").unwrap();
    let mut runtime = ConfigRuntime::new(load_config_from_path(root.path()).unwrap()).unwrap();
    included.write_str("[font]\nsize = 19\n").unwrap();
    root.write_str(
        "# preserve root comment\ninclude = ['included.toml']\n[window]\ntitle = 'reloaded'\n",
    )
    .unwrap();

    runtime.reload(|_| Ok(())).unwrap();

    assert_eq!(runtime.current().window.title, "reloaded");
    assert_eq!(runtime.current().font.size, 19.0);
    assert_eq!(
        runtime.document().str_at(&["window", "title"]),
        Some("reloaded")
    );
    assert!(!runtime.document().contains(&["font", "size"]));
    let mut draft = runtime.document().clone();
    draft.set_str(&["window", "title"], "saved").unwrap();
    runtime.commit_document(draft, |_| Ok(())).unwrap();
    let saved = std::fs::read_to_string(root.path()).unwrap();
    assert!(saved.starts_with("# preserve root comment"));
    assert!(saved.contains("included.toml"));
    assert!(!saved.contains("[font]"));
    assert_eq!(
        std::fs::read_to_string(included.path()).unwrap(),
        "[font]\nsize = 19\n"
    );
}

#[rstest]
fn changes_during_reload_validation_remain_pending_for_the_next_reload() {
    let directory = TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str("[window]\ntitle = 'initial'\n").unwrap();
    let mut runtime = ConfigRuntime::new(load_config_from_path(file.path()).unwrap()).unwrap();
    file.write_str("[window]\ntitle = 'candidate'\n").unwrap();

    runtime
        .reload(|config| {
            assert_eq!(config.window.title, "candidate");
            file.write_str("[window]\ntitle = 'next external edit'\n")
                .unwrap();
            Ok(())
        })
        .unwrap();

    assert_eq!(runtime.current().window.title, "candidate");
    assert_eq!(
        runtime.document().str_at(&["window", "title"]),
        Some("candidate")
    );
    assert!(
        runtime.reload_due(
            std::time::Instant::now()
                .checked_add(bootty_config::config_reload::CONFIG_HOT_RELOAD_INTERVAL)
                .unwrap()
        )
    );
    runtime.reload(|_| Ok(())).unwrap();
    assert_eq!(runtime.current().window.title, "next external edit");
}
