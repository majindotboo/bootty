//! Browser tabs own native child webviews; GPUI owns their chrome and visibility.
mod annotation;
mod login;
use bootty_browser::{
    BrowserBounds, BrowserElement, BrowserEvent, BrowserProfile, BrowserShortcut, BrowserView,
    NativeBrowserError, normalize_address, resolve_address,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    dock::{BasePanel, Panel, PanelEvent},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, canvas, div, prelude::*,
};

struct BrowserTab {
    id: u64,
    address: String,
    title: String,
    loading: bool,
    load_revision: u64,
    load_timeout: Option<gpui_kit::Task<()>>,
    error: Option<String>,
    view: Option<BrowserView>,
}

pub struct BrowserPaletteRequested;
pub struct BrowserClosed;
pub struct BrowserSettingsRequested;
pub struct BrowserFeedbackReady(pub String);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BrowserInteraction {
    #[default]
    Idle,
    Selecting,
    Editing,
    ManagingLogin,
    Palette,
    RestoringPageFocus,
}

#[derive(Clone)]
struct LoginTarget {
    tab: u64,
    revision: u64,
    origin: String,
}

pub struct BrowserPanel {
    tabs: Vec<BrowserTab>,
    profile: BrowserProfile,
    config: bootty_config::config::BrowserConfig,
    selected: u64,
    next_id: u64,
    address: Entity<InputState>,
    interaction: BrowserInteraction,
    visible: bool,
    active: bool,
    host_visible: bool,
    #[cfg(target_os = "linux")]
    platform_task: Option<gpui_kit::Task<()>>,
}

impl BrowserPanel {
    fn dispatch_chrome_command(
        &mut self,
        action: &crate::gpui_actions::InvokeCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let invocation = action.invocation();
        if invocation.target.is_some() || !invocation.arguments.is_empty() {
            cx.propagate();
            return;
        }
        match invocation.command.as_str() {
            "new_tab" => self.new_tab(None, window, cx),
            "close_surface" => self.close_tab(self.selected, window, cx),
            _ => {
                cx.propagate();
                return;
            }
        }
        cx.stop_propagation();
    }

    pub(crate) fn new(
        config: &bootty_config::config::BoottyConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter an address"));
        cx.subscribe_in(&address, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let address = this.address.read(cx).value().to_string();
                if let Err(error) = this.open_url(&address, window, cx) {
                    if let Some(tab) = this.selected_mut() {
                        tab.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            }
        })
        .detach();
        Self {
            tabs: vec![BrowserTab {
                id: 1,
                address: "about:blank".to_owned(),
                title: "New tab".to_owned(),
                loading: false,
                load_revision: 0,
                load_timeout: None,
                error: None,
                view: None,
            }],
            profile: BrowserProfile::new(config.config_path.with_file_name("browser")),
            config: config.browser,
            selected: 1,
            next_id: 2,
            address,
            interaction: BrowserInteraction::Idle,
            visible: false,
            active: false,
            host_visible: false,
            #[cfg(target_os = "linux")]
            platform_task: None,
        }
    }

    pub(crate) fn open_url(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        let address = resolve_address(address, self.config.search_engine.address())?;
        let navigation = self
            .selected_tab()
            .and_then(|tab| tab.view.as_ref())
            .map_or(Ok(()), |view| view.navigate(&address));
        if let Err(error) = navigation {
            if let Some(tab) = self.selected_mut() {
                tab.error = Some(error.to_string());
                tab.loading = false;
            }
            cx.notify();
            return Err(error);
        }
        if let Some(tab) = self.selected_mut() {
            tab.error = None;
            tab.address.clone_from(&address);
            tab.loading = address != "about:blank";
        }
        if address != "about:blank" {
            self.watch_load(self.selected, window, cx);
        } else if let Some(tab) = self.selected_mut()
            && let Some(view) = &mut tab.view
        {
            _ = view.set_visible(false);
        }
        self.address
            .update(cx, |input, cx| input.set_value(address, window, cx));
        cx.notify();
        Ok(())
    }

    /// The shell also hides native children for overlays, which render above the GPU scene.
    pub(crate) fn set_visible(&mut self, visible: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.host_visible = visible;
        self.sync_visibility(cx);
    }

    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        let visible = self.host_visible
            && self.active
            && !matches!(
                self.interaction,
                BrowserInteraction::Editing | BrowserInteraction::ManagingLogin
            );
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        for tab in &mut self.tabs {
            if let Some(view) = &mut tab.view
                && let Err(error) = view
                    .set_visible(visible && tab.id == self.selected && tab.address != "about:blank")
            {
                tab.error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    pub(crate) fn configure(
        &mut self,
        config: bootty_config::config::BrowserConfig,
        cx: &mut Context<Self>,
    ) {
        if self.config == config {
            return;
        }
        if self.config.persist_site_data != config.persist_site_data {
            for tab in &mut self.tabs {
                if let Some(view) = &mut tab.view {
                    _ = view.set_visible(false);
                }
                tab.view = None;
                tab.error = None;
            }
        }
        self.config = config;
        cx.notify();
    }

    fn selected_tab(&self) -> Option<&BrowserTab> {
        self.tabs.iter().find(|tab| tab.id == self.selected)
    }

    fn selected_mut(&mut self) -> Option<&mut BrowserTab> {
        self.tabs.iter_mut().find(|tab| tab.id == self.selected)
    }

    pub(crate) fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_annotation();
        self.selected = id;
        for tab in &mut self.tabs {
            if let Some(view) = &mut tab.view
                && let Err(error) =
                    view.set_visible(self.visible && tab.id == id && tab.address != "about:blank")
            {
                tab.error = Some(error.to_string());
            }
        }
        if let Some(address) = self.selected_tab().map(|tab| tab.address.clone()) {
            self.address.update(cx, |input, cx| {
                input.set_value(
                    if address == "about:blank" {
                        String::new()
                    } else {
                        address
                    },
                    window,
                    cx,
                );
            });
        }
        cx.notify();
    }

    pub(crate) fn new_tab(
        &mut self,
        address: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_annotation();
        let id = self.next_id;
        let Some(next_id) = id.checked_add(1) else {
            return;
        };
        self.next_id = next_id;
        self.tabs.push(BrowserTab {
            id,
            address: "about:blank".to_owned(),
            title: "New tab".to_owned(),
            loading: false,
            load_revision: 0,
            load_timeout: None,
            error: None,
            view: None,
        });
        self.select_tab(id, window, cx);
        if let Some(address) = address {
            _ = self.open_url(address, window, cx);
        } else {
            self.address.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    pub(crate) fn tabs(&self) -> impl Iterator<Item = (u64, &str)> {
        self.tabs.iter().map(|tab| (tab.id, tab.title.as_str()))
    }

    pub(crate) const fn selected(&self) -> u64 {
        self.selected
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_annotation();
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.selected == id {
            if let Some(tab) = self.tabs.get_mut(index)
                && let Some(view) = &mut tab.view
            {
                _ = view.set_visible(false);
            }
            crate::window::restore_keyboard_focus(window);
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.selected = 0;
            cx.emit(BrowserClosed);
        } else if self.selected == id
            && let Some(next) = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id)
        {
            self.select_tab(next, window, cx);
        }
        cx.notify();
    }

    fn navigate(
        &mut self,
        operation: impl FnOnce(&BrowserView) -> Result<(), NativeBrowserError>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.selected_mut()
            && let Some(view) = &tab.view
        {
            match operation(view) {
                Ok(()) => {
                    tab.error = None;
                    tab.loading = true;
                }
                Err(error) => {
                    tab.error = Some(error.to_string());
                    tab.loading = false;
                }
            }
        }
        self.watch_load(self.selected, window, cx);
        cx.notify();
    }

    fn watch_load(&mut self, id: u64, window: &Window, cx: &Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        tab.load_revision = tab.load_revision.saturating_add(1);
        let revision = tab.load_revision;
        // Wry exposes completion but no failed-navigation callback. Bound the busy state.
        let timeout = cx.spawn_in(window, async move |owner, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(30)).await;
            _ = owner.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.id == id)
                    && tab.load_revision == revision && tab.loading
                {
                    tab.loading = false;
                    tab.error = Some("This page has not finished loading. Reload or open it in your default browser.".to_owned());
                    cx.notify();
                }
            });
        });
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.load_timeout = Some(timeout);
        }
    }

    fn layout_webview(
        &mut self,
        bounds: BrowserBounds,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible
            || self
                .selected_tab()
                .is_none_or(|tab| tab.address == "about:blank")
            || bounds.width < 1.0
            || bounds.height < 1.0
        {
            if let Some(tab) = self.selected_mut()
                && let Some(view) = &mut tab.view
            {
                _ = view.set_visible(false);
            }
            return;
        }
        let selected = self.selected;
        let create = self.selected_tab().is_some_and(|tab| {
            tab.view.is_none() && tab.error.is_none() && tab.address != "about:blank"
        });
        if create {
            let (sender, receiver) = async_channel::bounded(128);
            let Some(address) = self.selected_tab().map(|tab| tab.address.clone()) else {
                return;
            };
            let view = BrowserView::new(
                window,
                &address,
                bounds,
                sender,
                &mut self.profile,
                self.config.persist_site_data,
            );
            let Some(tab) = self.selected_mut() else {
                return;
            };
            match view {
                Ok(view) => tab.view = Some(view),
                Err(error) => {
                    tab.error = Some(error.to_string());
                    tab.loading = false;
                    cx.notify();
                    return;
                }
            }
            #[cfg(target_os = "linux")]
            self.ensure_platform_pump(window, cx);
            cx.spawn_in(window, async move |owner, cx| {
                while let Ok(event) = receiver.recv().await {
                    let result = cx.update(|window, cx| {
                        owner.update(cx, |this, cx| {
                            this.receive_event(selected, event, window, cx);
                        })
                    });
                    if !matches!(result, Ok(Ok(()))) {
                        break;
                    }
                }
            })
            .detach();
            cx.notify();
        }
        if let Some(tab) = self.selected_mut()
            && let Some(view) = &mut tab.view
            && let Err(error) = view
                .set_bounds(bounds)
                .and_then(|()| view.set_visible(true))
        {
            tab.error = Some(error.to_string());
            cx.notify();
        }
        if matches!(
            self.interaction,
            BrowserInteraction::Palette | BrowserInteraction::RestoringPageFocus
        ) {
            let restore = self.interaction == BrowserInteraction::RestoringPageFocus;
            self.interaction = BrowserInteraction::Idle;
            if restore && let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref()) {
                // Root restores GPUI focus; the page's native responder must be restored separately.
                window.blur(cx);
                if let Err(error) = view.focus() {
                    if let Some(tab) = self.selected_mut() {
                        tab.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            }
        }
    }

    pub(crate) fn cancel_palette(&mut self, cx: &mut Context<Self>) {
        if self.interaction == BrowserInteraction::Palette {
            self.interaction = BrowserInteraction::RestoringPageFocus;
            cx.notify();
        }
    }

    #[cfg(target_os = "linux")]
    fn ensure_platform_pump(&mut self, window: &Window, cx: &Context<Self>) {
        if self.platform_task.is_some() {
            return;
        }
        self.platform_task = Some(cx.spawn_in(window, async move |owner, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let live =
                    owner.update(cx, |this, _| this.tabs.iter().any(|tab| tab.view.is_some()));
                if !matches!(live, Ok(true)) {
                    _ = owner.update(cx, |this, _| this.platform_task = None);
                    break;
                }
                bootty_browser::poll_platform_events();
            }
        }));
    }

    fn stop_annotation(&mut self) {
        if self.interaction == BrowserInteraction::Selecting {
            if let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref()) {
                _ = view.set_annotation_mode(false);
            }
            self.interaction = BrowserInteraction::Idle;
        } else if matches!(
            self.interaction,
            BrowserInteraction::Palette | BrowserInteraction::RestoringPageFocus
        ) {
            self.interaction = BrowserInteraction::Idle;
        }
    }

    fn toggle_annotation(&mut self, cx: &mut Context<Self>) {
        let enabled = self.interaction != BrowserInteraction::Selecting;
        let result = self
            .selected_tab()
            .and_then(|tab| tab.view.as_ref())
            .map(|view| view.set_annotation_mode(enabled));
        match result {
            Some(Ok(())) => {
                self.interaction = if enabled {
                    BrowserInteraction::Selecting
                } else {
                    BrowserInteraction::Idle
                }
            }
            Some(Err(error)) => {
                if let Some(tab) = self.selected_mut() {
                    tab.error = Some(error.to_string());
                }
            }
            None => {}
        }
        cx.notify();
    }

    fn edit_annotation(
        &mut self,
        element: BrowserElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;
        self.interaction = BrowserInteraction::Editing;
        self.sync_visibility(cx);
        let owner = cx.weak_entity();
        let editor =
            cx.new(|cx| annotation::AnnotationEditor::new(element, owner.clone(), window, cx));
        let cancel_owner = owner.clone();
        let close_owner = owner.clone();
        let content = editor.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let content = content.clone();
            let cancel_owner = cancel_owner.clone();
            let close_owner = close_owner.clone();
            dialog
                .title("Annotate element")
                .w(gpui_kit::px(f32::from(window.rem_size()) * 30.0))
                .on_cancel(move |_, window, cx| {
                    _ = cancel_owner.update(cx, |panel, cx| panel.finish_dialog(window, cx));
                    true
                })
                .on_close(move |_, window, cx| {
                    _ = close_owner.update(cx, |panel, cx| panel.finish_dialog(window, cx));
                })
                .content(move |body, _, _| body.child(content.clone()))
        });
        cx.defer_in(window, move |_, window, cx| {
            crate::window::restore_keyboard_focus(window);
            editor.focus_handle(cx).focus(window, cx);
        });
    }

    fn finish_dialog(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.interaction = BrowserInteraction::Idle;
        self.sync_visibility(cx);
        cx.defer_in(window, |this, window, cx| {
            crate::window::restore_keyboard_focus(window);
            this.address.focus_handle(cx).focus(window, cx);
        });
    }

    fn receive_element(
        &mut self,
        id: u64,
        element: BrowserElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .selected_tab()
            .and_then(|tab| tab.view.as_ref())
            .and_then(|view| view.current_address().ok());
        // Frame selections need explicit frame identity before they can be accepted.
        if id == self.selected
            && self.interaction == BrowserInteraction::Selecting
            && current.as_deref() == Some(&element.url)
        {
            self.edit_annotation(element, window, cx);
        }
    }

    fn receive_event(
        &mut self,
        id: u64,
        event: BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        if let BrowserEvent::ElementPicked(element) = event {
            self.receive_element(id, element, window, cx);
            return;
        }
        if matches!(event, BrowserEvent::PageFocused) {
            if id == self.selected && self.visible && self.host_visible {
                // Clear GPUI shortcut dispatch without changing native keyboard focus.
                window.blur(cx);
            }
            return;
        }
        if let BrowserEvent::Shortcut(shortcut) = event {
            if id == self.selected && self.visible {
                match shortcut {
                    BrowserShortcut::Palette => {
                        self.stop_annotation();
                        self.interaction = BrowserInteraction::Palette;
                        cx.emit(BrowserPaletteRequested);
                    }
                    BrowserShortcut::Address => self.address.update(cx, |input, cx| {
                        crate::window::restore_keyboard_focus(window);
                        input.focus(window, cx);
                        input.select_all(window, cx);
                    }),
                    BrowserShortcut::Reload => self.navigate(BrowserView::reload, window, cx),
                    BrowserShortcut::NewTab => self.new_tab(None, window, cx),
                    BrowserShortcut::CloseTab => self.close_tab(self.selected, window, cx),
                }
            }
            return;
        }
        if let BrowserEvent::OpenTab(address) = event {
            if id == self.selected && self.visible && normalize_address(&address).is_ok() {
                self.new_tab(Some(&address), window, cx);
            }
            return;
        }
        let started = matches!(&event, BrowserEvent::LoadStarted(_));
        // Native page focus does not clear GPUI focus. Preserve edits by value.
        let sync_address = id == self.selected
            && self.selected_tab().is_some_and(|tab| {
                self.address.read(cx).value().as_ref()
                    == if tab.address == "about:blank" {
                        ""
                    } else {
                        tab.address.as_str()
                    }
            });
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            match event {
                BrowserEvent::LoadStarted(address) => {
                    tab.address = address;
                    tab.loading = true;
                    tab.error = None;
                }
                BrowserEvent::LoadFinished(address) => {
                    tab.address = address;
                    tab.loading = false;
                    tab.load_timeout = None;
                }
                BrowserEvent::TitleChanged(title) => tab.title = title,
                BrowserEvent::Notice(message) => tab.error = Some(message),
                BrowserEvent::PageFocused
                | BrowserEvent::ElementPicked(_)
                | BrowserEvent::OpenTab(_)
                | BrowserEvent::Shortcut(_) => {}
            }
            if sync_address {
                let address = tab.address.clone();
                self.address
                    .update(cx, |input, cx| input.set_value(address, window, cx));
            }
        }
        if started {
            self.interaction = BrowserInteraction::Idle;
            self.watch_load(id, window, cx);
        }
        cx.notify();
    }

    fn edit_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let Some(tab) = self.selected_tab() else {
            return;
        };
        let Some(address) = tab
            .view
            .as_ref()
            .and_then(|view| view.current_address().ok())
        else {
            return;
        };
        let Ok(origin) = bootty_browser::login_origin(&address) else {
            if let Some(tab) = self.selected_mut() {
                tab.error = Some("Saved logins require HTTPS or a local development page.".into());
            }
            cx.notify();
            return;
        };
        let Ok(service) = self.profile.credential_service(&address) else {
            return;
        };
        let target = LoginTarget {
            tab: tab.id,
            revision: tab.load_revision,
            origin,
        };
        self.stop_annotation();
        self.interaction = BrowserInteraction::ManagingLogin;
        self.sync_visibility(cx);
        let owner = cx.weak_entity();
        let editor =
            cx.new(|cx| login::LoginEditor::new(target, service, owner.clone(), window, cx));
        let cancel_owner = owner.clone();
        let content = editor.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let content = content.clone();
            let cancel_owner = cancel_owner.clone();
            let close_owner = owner.clone();
            dialog
                .title("Saved logins")
                .w(gpui_kit::px(f32::from(window.rem_size()) * 30.0))
                .on_cancel(move |_, window, cx| {
                    _ = cancel_owner.update(cx, |panel, cx| panel.finish_dialog(window, cx));
                    true
                })
                .on_close(move |_, window, cx| {
                    _ = close_owner.update(cx, |panel, cx| panel.finish_dialog(window, cx));
                })
                .content(move |body, _, _| body.child(content.clone()))
        });
        cx.defer_in(window, move |_, window, cx| {
            crate::window::restore_keyboard_focus(window);
            editor.focus_handle(cx).focus(window, cx);
        });
    }

    fn fill_saved_login(
        &self,
        target: &LoginTarget,
        username: &str,
        password: &[u8],
    ) -> Result<async_channel::Receiver<Result<(), String>>, NativeBrowserError> {
        let tab = self
            .selected_tab()
            .filter(|tab| tab.id == target.tab && tab.load_revision == target.revision)
            .ok_or_else(|| {
                NativeBrowserError::Platform("The page changed. Open saved logins again.".into())
            })?;
        let view = tab
            .view
            .as_ref()
            .ok_or_else(|| NativeBrowserError::Platform("The page closed.".into()))?;
        view.fill_login(&target.origin, username, password)
    }

    fn annotation_button(&self, has_view: bool, cx: &Context<Self>) -> Button {
        Button::new("browser-annotate")
            .icon(gpui_kit::assets::IconName::MousePointer2)
            .ghost()
            .small()
            .size_6()
            .disabled(!has_view)
            .when(
                self.interaction == BrowserInteraction::Selecting,
                ButtonVariants::primary,
            )
            .accessibility_label("Annotate page")
            .tooltip("Select an element and add feedback")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_annotation(cx)))
    }

    fn render_page_actions(
        &self,
        has_view: bool,
        can_open_external: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.annotation_button(has_view, cx))
            .child(
                Button::new("browser-logins")
                    .icon(gpui_kit::assets::IconName::KeyRound)
                    .ghost()
                    .small()
                    .size_6()
                    .disabled(!has_view)
                    .accessibility_label("Saved logins")
                    .tooltip("Saved logins")
                    .on_click(cx.listener(|this, _, window, cx| this.edit_login(window, cx))),
            )
            .child(
                Button::new("browser-settings")
                    .icon(IconName::Settings)
                    .ghost()
                    .small()
                    .size_6()
                    .accessibility_label("Browser settings")
                    .tooltip("Browser settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserSettingsRequested))),
            )
            .child(
                Button::new("browser-external")
                    .icon(IconName::ExternalLink)
                    .ghost()
                    .small()
                    .size_6()
                    .disabled(!can_open_external)
                    .accessibility_label("Open in default browser")
                    .tooltip("Open in default browser")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(tab) = this.selected_tab() {
                            cx.open_url(&tab.address);
                        }
                    })),
            )
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let tab = self.selected_tab();
        let can_back = tab
            .and_then(|tab| tab.view.as_ref())
            .is_some_and(BrowserView::can_go_back);
        let can_forward = tab
            .and_then(|tab| tab.view.as_ref())
            .is_some_and(BrowserView::can_go_forward);
        let has_view = tab.is_some_and(|tab| tab.view.is_some());
        let can_open_external = tab.is_some_and(|tab| {
            tab.address != "about:blank" && normalize_address(&tab.address).is_ok()
        });
        let loading = tab.is_some_and(|tab| tab.loading);
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("browser-back")
                    .icon(IconName::ArrowLeft)
                    .ghost()
                    .small()
                    .size_6()
                    .disabled(!can_back)
                    .accessibility_label("Back")
                    .tooltip("Back")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate(BrowserView::back, window, cx);
                    })),
            )
            .child(
                Button::new("browser-forward")
                    .icon(IconName::ArrowRight)
                    .ghost()
                    .small()
                    .size_6()
                    .disabled(!can_forward)
                    .accessibility_label("Forward")
                    .tooltip("Forward")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate(BrowserView::forward, window, cx);
                    })),
            )
            .child(
                div().flex_1().min_w_0().child(crate::gpui::focus_input(
                    &self.address,
                    Input::new(&self.address)
                        .accessibility_id("browser.address")
                        .aria_label("Web address")
                        .small()
                        .suffix(
                            Button::new("browser-reload")
                                .icon(IconName::RotateCw)
                                .ghost()
                                .xsmall()
                                .disabled(!has_view)
                                .loading(loading)
                                .accessibility_label("Reload page")
                                .tooltip("Reload page")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.navigate(BrowserView::reload, window, cx);
                                })),
                        ),
                )),
            )
            .child(self.render_page_actions(has_view, can_open_external, cx))
    }
}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.address.focus_handle(cx)
    }
}
impl EventEmitter<PanelEvent> for BrowserPanel {}
impl EventEmitter<BrowserPaletteRequested> for BrowserPanel {}
impl EventEmitter<BrowserClosed> for BrowserPanel {}
impl EventEmitter<BrowserSettingsRequested> for BrowserPanel {}
impl EventEmitter<BrowserFeedbackReady> for BrowserPanel {}
impl BasePanel for BrowserPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.browser"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.stop_annotation();
        self.active = false;
        self.sync_visibility(cx);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if !active {
            self.stop_annotation();
        }
        self.active = active;
        self.sync_visibility(cx);
    }
}
impl Panel for BrowserPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Browser".into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Browser"
    }
}
impl Render for BrowserPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let error = self.selected_tab().and_then(|tab| tab.error.clone());
        let empty = self
            .selected_tab()
            .is_none_or(|tab| tab.address == "about:blank");
        div()
            .id("browser-panel")
            .on_action(cx.listener(Self::dispatch_chrome_command))
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .child(self.render_toolbar(cx))
            .when_some(error, |body, message| {
                body.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when(empty, |body| {
                        body.child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .text_color(cx.theme().muted_foreground)
                                .child(Icon::new(IconName::Globe).large())
                                .child("Search the web or open your development server")
                                .child(div().text_sm().child("Enter a search or address above.")),
                        )
                    })
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, cx| {
                                let clipped = bounds.intersect(&window.content_mask().bounds);
                                let bounds = BrowserBounds {
                                    x: f64::from(f32::from(clipped.origin.x)),
                                    y: f64::from(f32::from(clipped.origin.y)),
                                    width: f64::from(f32::from(clipped.size.width)),
                                    height: f64::from(f32::from(clipped.size.height)),
                                    scale_factor: f64::from(window.scale_factor()),
                                };
                                window.defer(cx, move |window, cx| {
                                    _ = owner.update(cx, |this, cx| {
                                        this.layout_webview(bounds, window, cx);
                                    });
                                });
                            },
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
    }
}
