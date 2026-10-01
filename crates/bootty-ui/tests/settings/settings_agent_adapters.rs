use std::sync::Arc;

use assert_fs::TempDir;
use bootty_config::{
    config::{BoottyConfig, load_or_create_config_document},
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsSession};
use pretty_assertions::assert_eq;
use proptest::prelude::*;

proptest! {
    #[test]
    fn legacy_adapter_requests_report_unsupported_without_work_or_config_changes(
        identity in "[a-z.]{1,40}",
        module in "[a-z./]{1,40}",
        id in "[a-z-]{1,40}",
        uninstall in any::<bool>(),
    ) {
        let root = TempDir::new().expect("isolated settings");
        let path = root.path().join("config.toml");
        std::fs::write(&path, "# keep existing settings\n[font]\nsize = 17\n")
            .expect("existing settings");
        let document = load_or_create_config_document(&path).expect("settings document");
        let before = std::fs::read(&path).expect("settings bytes");
        let mut session = SettingsSession::new(
            AcceptedSettings {
                config: Arc::new(BoottyConfig::default()),
                revision: 1,
                document,
                schema: Arc::new(SettingsSchema::new(SettingsSchema::builtin().specs().to_vec())),
            },
            Catalogs::default(),
        );
        if uninstall {
            session.uninstall_integration(identity.clone(), module, id);
        } else {
            session.install_integration(identity.clone(), module, id);
        }
        prop_assert!(session.take_effects().is_empty());
        prop_assert!(!session.has_unsaved_changes());
        prop_assert_eq!(session.draft_document().i64_at(&["font", "size"]), Some(17));
        prop_assert!(session.integration_error(&identity).is_some_and(|error|
            error.contains("unsupported") && error.contains("no hook installation is required")));
        assert_eq!(std::fs::read(&path).expect("preserved settings bytes"), before);
    }
}
