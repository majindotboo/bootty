use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    GpuiSettings, SettingsCategory, SettingsContent, SettingsIntent, SettingsPage,
};
use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, Subscription, Window};

pub struct SettingsProbe {
    pub settings: Entity<GpuiSettings>,
    #[allow(
        dead_code,
        reason = "Layout tests share the interactive settings fixture."
    )]
    pub intents: Rc<RefCell<Vec<SettingsIntent>>>,
    _subscription: Subscription,
}

impl SettingsProbe {
    #[allow(
        dead_code,
        reason = "Some targets supply a configured draft instead of defaults."
    )]
    pub fn with_snapshot(snapshot: GpuiSettingsSnapshot, cx: &mut Context<Self>) -> Self {
        Self::with_draft(snapshot, draft(), cx)
    }

    pub fn with_draft(
        snapshot: GpuiSettingsSnapshot,
        draft: bootty_ui::settings_session::SettingsSession,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = cx.new(|cx| snapshot.build_with_draft(draft, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&intents);
        let subscription = cx.subscribe(&settings, move |_, _, intent, _| {
            received.borrow_mut().push(intent.clone());
        });
        Self {
            settings,
            intents,
            _subscription: subscription,
        }
    }
}

impl Render for SettingsProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.settings.clone()
    }
}

pub fn draft() -> bootty_ui::settings_session::SettingsSession {
    use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsSession};
    let directory = assert_fs::TempDir::new().expect("settings directory");
    let document =
        bootty_config::config::load_or_create_config_document(directory.path().join("config.toml"))
            .expect("empty settings document");
    SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 0,
            document,
            schema: std::sync::Arc::new(bootty_config::settings_schema::SettingsSchema::new(
                bootty_config::settings_schema::SettingsSchema::builtin()
                    .specs()
                    .to_vec(),
            )),
        },
        Catalogs::default(),
    )
}

#[derive(Clone)]
pub struct GpuiSettingsSnapshot {
    pub category: SettingsCategory,
    pub pages: Vec<SettingsPage>,
    pub search: String,
    pub write_error: Option<String>,
}

impl GpuiSettingsSnapshot {
    pub fn build_with_draft(
        self,
        draft: bootty_ui::settings_session::SettingsSession,
        cx: &mut Context<GpuiSettings>,
    ) -> GpuiSettings {
        let category = self.category;
        let search = self.search.clone();
        let mut settings = GpuiSettings::new(self.into_content(), draft, cx);
        settings.select_category(category, cx);
        settings.apply_search(&search, cx);
        settings
    }

    pub fn into_content(self) -> SettingsContent {
        SettingsContent {
            pages: self.pages,
            write_error: self.write_error,
        }
    }
}
