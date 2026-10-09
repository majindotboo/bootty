//! Browser tabs own native child webviews; GPUI owns their chrome and visibility.
use bootty_browser::{
    BrowserBounds, BrowserEvent, BrowserProfile, BrowserShortcut, BrowserView, NativeBrowserError,
    normalize_address, resolve_address,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    dock::{BasePanel, Panel, PanelEvent},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InputEvent as _, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, canvas, div, prelude::*,
};
use num_traits::ToPrimitive as _;

use crate::commands::BrowserAction;
mod annotations;
mod attachment;
mod capture;
mod snapshot;
use annotations::BrowserAnnotations;
pub use annotations::BrowserAnnotationsChanged;

#[derive(Clone)]
pub struct BrowserPageSnapshot {
    pub id: u64,
    pub title: String,
    pub icon: IconName,
}

struct BrowserTab {
    id: u64,
    address: String,
    title: String,
    loading: bool,
    load_revision: u64,
    view_revision: u64,
    load_timeout: Option<gpui_kit::Task<()>>,
    error: Option<String>,
    view: Option<BrowserView>,
}

pub struct BrowserClosed;
pub struct BrowserPagesChanged;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BrowserInteraction {
    #[default]
    Idle,
    Palette,
    RestoringPageFocus,
}

// Native visibility, host activation, and site-data reset/confirmation are independent states.
#[allow(clippy::struct_excessive_bools)]
pub struct BrowserPanel {
    tabs: Vec<BrowserTab>,
    reset_view: Option<BrowserView>,
    profile: BrowserProfile,
    annotations: BrowserAnnotations,
    config: bootty_config::config::BrowserConfig,
    conversation_target: Option<bootty_control::CommandTarget>,
    attachment: bootty_agents::NativeBrowserAccess,
    attaching: bool,
    resetting: bool,
    capture_request: Option<capture::PendingCapture>,
    confirm_reset: bool,
    sender: bootty_control::BoundAppCommandSender,
    window_target: Option<bootty_control::CommandTarget>,
    selected: u64,
    next_id: u64,
    address: Entity<InputState>,
    webview_bounds: Option<BrowserBounds>,
    interaction: BrowserInteraction,
    interaction_generation: u64,
    visible: bool,
    active: bool,
    host_visible: bool,
    #[cfg(target_os = "linux")]
    platform_task: Option<gpui_kit::Task<()>>,
}

impl BrowserPanel {
    pub(crate) fn configure(
        &mut self,
        config: bootty_config::config::BrowserConfig,
        conversation_target: Option<bootty_control::CommandTarget>,
        window_target: Option<bootty_control::CommandTarget>,
        attachment: bootty_agents::NativeBrowserAccess,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.config == config
            && self.conversation_target == conversation_target
            && self.window_target == window_target
            && self.attachment == attachment
        {
            return;
        }
        self.invalidate_capture();
        if self.config.persist_site_data != config.persist_site_data {
            let restore = self.visible && window.focused(cx).is_none();
            self.focus_host(cx);
            // Drop the old policy's views; creating new views must never reuse private site data.
            self.clear_annotation_scopes();
            self.release_views(true);
            if restore {
                self.interaction = BrowserInteraction::RestoringPageFocus;
            }
        }
        self.config = config;
        self.conversation_target = conversation_target;
        self.window_target = window_target;
        self.attachment = attachment;
        if let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref()) {
            _ = view.set_annotation_conversation(self.annotation_recipient_available());
        }
        cx.notify();
    }

    fn release_views(&mut self, loading: bool) {
        self.invalidate_capture();
        for tab in &mut self.tabs {
            // Invalidate buffered events immediately, before another native view can own this page.
            tab.view_revision = tab.view_revision.wrapping_add(1);
            tab.load_revision = tab.load_revision.saturating_add(1);
            tab.view = None;
            tab.load_timeout = None;
            tab.loading = loading && tab.address != "about:blank";
            tab.error = None;
        }
    }

    pub(crate) fn reset_site_data(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<async_channel::Receiver<Result<(), String>>, NativeBrowserError> {
        if !BrowserProfile::supports_site_data_reset() {
            return Err(NativeBrowserError::Platform(
                "Confirmed site-data reset is unavailable on this platform.".into(),
            ));
        }
        if self.resetting {
            return Err(NativeBrowserError::Platform(
                "Site-data reset is already in progress.".into(),
            ));
        }
        self.focus_host(cx);
        self.clear_annotation_scopes();
        self.release_views(false);
        let reset = match self.profile.request_reset(window) {
            Ok(reset) => reset,
            Err(error) => {
                for tab in &mut self.tabs {
                    tab.error = Some(error.to_string());
                }
                cx.notify();
                return Err(error);
            }
        };
        self.reset_view = reset.view;
        self.resetting = true;
        self.confirm_reset = false;
        #[cfg(target_os = "linux")]
        self.ensure_platform_pump(window, cx);
        let (completion, receiver) = async_channel::bounded(1);
        cx.spawn_in(window, async move |owner, cx| {
            let observed = reset
                .completion
                .recv()
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
            _ = owner.update(cx, |this, cx| {
                this.reset_view = None;
                this.resetting = false;
                for tab in &mut this.tabs {
                    tab.error.clone_from(&observed.as_ref().err().cloned());
                }
                cx.notify();
            });
            _ = completion.try_send(observed);
        })
        .detach();
        cx.notify();
        Ok(receiver)
    }

    pub(crate) fn execute(
        &mut self,
        action: BrowserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        if let BrowserAction::Open(address) = &action {
            self.record_interaction();
            return self.open_url(address, window, cx);
        }
        if let BrowserAction::CloseTab(page) = &action {
            return self.execute_on_page(*page, action, window, cx);
        }
        self.execute_on_page(self.selected, action, window, cx)
    }

    /// Executes against the captured page without redirecting work to the selected page.
    pub(crate) fn execute_on_page(
        &mut self,
        page: u64,
        action: BrowserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        if !matches!(action, BrowserAction::NewTab(_))
            && !self.tabs.iter().any(|tab| tab.id == page)
        {
            return Err(NativeBrowserError::Platform(
                "This browser page is closed.".into(),
            ));
        }
        self.record_interaction();
        match action {
            BrowserAction::ResetSiteData
            | BrowserAction::Capture
            | BrowserAction::CaptureAnnotation(_)
            | BrowserAction::Snapshot(_) => {
                return Err(NativeBrowserError::Platform(
                    "This browser action must await its observed completion.".into(),
                ));
            }
            BrowserAction::Open(address) => self.open_page(page, &address, window, cx)?,
            BrowserAction::NewTab(address) => self.new_tab(address.as_deref(), window, cx)?,
            BrowserAction::CloseTab(id) => {
                if id != page {
                    return Err(NativeBrowserError::Platform(
                        "The browser page target changed.".into(),
                    ));
                }
                self.close_tab(page, window, cx);
            }
            BrowserAction::Address => {
                self.reset_interaction();
                if page != self.selected {
                    return Err(NativeBrowserError::Platform(
                        "Select this browser page before editing its address.".into(),
                    ));
                }
                if let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref()) {
                    view.focus_parent()?;
                }
                self.address.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.select_all(window, cx);
                });
            }
            BrowserAction::Back => self.navigate(page, BrowserView::back, window, cx)?,
            BrowserAction::Input(action) => {
                if page != self.selected {
                    return Err(NativeBrowserError::Platform(
                        "Select this browser page and dismiss overlays before sending input".into(),
                    ));
                }
                self.tabs
                    .iter()
                    .find(|tab| tab.id == page)
                    .and_then(|tab| tab.view.as_ref())
                    .ok_or_else(|| {
                        NativeBrowserError::Platform("The browser page is unavailable".into())
                    })?
                    .input(&action)?;
            }
            BrowserAction::Forward => self.navigate(page, BrowserView::forward, window, cx)?,
            BrowserAction::Reload => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == page)
                    && tab.view.is_none()
                {
                    let address = tab.address.clone();
                    self.open_page(page, &address, window, cx)?;
                } else {
                    self.navigate(page, BrowserView::reload, window, cx)?;
                }
            }
            BrowserAction::Stop => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == page) {
                    if let Some(view) = &tab.view {
                        view.stop()?;
                    }
                    tab.loading = false;
                    tab.load_timeout = None;
                }
                cx.notify();
            }
            BrowserAction::OpenExternal => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == page) {
                    cx.open_url(&normalize_address(&tab.address)?);
                }
            }
        }
        Ok(())
    }

    fn submit(
        &mut self,
        command: &str,
        mut arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(window_target) = self.window_target.clone() else {
            if let Some(tab) = self.selected_mut() {
                tab.error = Some("This application window is no longer available.".into());
            }
            cx.notify();
            return;
        };
        self.record_interaction();
        if command.starts_with("browser.")
            && !matches!(command, "browser.new_tab" | "browser.close_tab")
        {
            arguments.push(self.selected.to_string());
        }
        let mut invocation = bootty_control::CommandInvocation::new(
            command,
            arguments,
            bootty_control::Caller::Internal,
        );
        invocation.target = Some(window_target);
        if command == "browser.reset_site_data" {
            invocation.confirmation = Some(invocation.confirmation());
        }
        let page = self.selected;
        let now = std::time::Instant::now();
        let receiver = match self.sender.submit(
            invocation,
            now.checked_add(std::time::Duration::from_secs(30))
                .unwrap_or(now),
            bootty_control::CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                if let Some(tab) = self.selected_mut() {
                    tab.error = Some(format!("Command unavailable: {error:?}"));
                }
                cx.notify();
                return;
            }
        };
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update(cx, |this, cx| {
                let error = result.map_or_else(
                    |error| Some(error.to_string()),
                    |outcome| crate::commands::command_outcome_message(&outcome),
                );
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.id == page) {
                    tab.error = error;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn shortcut(&mut self, shortcut: BrowserShortcut, window: &Window, cx: &mut Context<Self>) {
        match shortcut {
            BrowserShortcut::Palette => {
                self.interaction = BrowserInteraction::Palette;
                self.submit("command_palette", Vec::new(), window, cx);
            }
            BrowserShortcut::NewSession => {
                self.interaction = BrowserInteraction::Palette;
                self.submit("new_mux_session", Vec::new(), window, cx);
            }
            BrowserShortcut::Address => self.submit("browser.address", Vec::new(), window, cx),
            BrowserShortcut::Reload => self.submit("browser.reload", Vec::new(), window, cx),
            BrowserShortcut::NewTab => self.submit("browser.new_tab", Vec::new(), window, cx),
            BrowserShortcut::CloseTab => self.submit(
                "browser.close_tab",
                vec![self.selected.to_string()],
                window,
                cx,
            ),
        }
    }

    pub(crate) fn new(
        profile_directory: std::path::PathBuf,
        sender: bootty_control::BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter an address"));
        cx.subscribe_in(&address, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let address = this.address.read(cx).value().to_string();
                this.submit("browser.open", vec![address], window, cx);
            }
        })
        .detach();
        let panel = Self {
            tabs: Vec::new(),
            reset_view: None,
            annotations: BrowserAnnotations::new(&profile_directory),
            profile: BrowserProfile::new(profile_directory),
            config: bootty_config::config::BrowserConfig::default(),
            conversation_target: None,
            attachment: bootty_agents::NativeBrowserAccess::default(),
            attaching: false,
            resetting: false,
            capture_request: None,
            confirm_reset: false,
            sender,
            window_target: None,
            selected: 0,
            next_id: 1,
            address,
            webview_bounds: None,
            interaction: BrowserInteraction::Idle,
            interaction_generation: 0,
            visible: false,
            active: false,
            host_visible: false,
            #[cfg(target_os = "linux")]
            platform_task: None,
        };
        panel.load_annotations(window, cx);
        panel
    }

    pub(crate) fn open_url(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        let address = resolve_address(address, self.config.search_engine.address())?;
        if self.tabs.is_empty() {
            self.new_tab(None, window, cx)?;
        }
        self.open_page(self.selected, &address, window, cx)
    }

    fn open_page(
        &mut self,
        page: u64,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        let address = resolve_address(address, self.config.search_engine.address())?;
        let tab = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == page)
            .ok_or_else(|| NativeBrowserError::Platform("This browser page is closed.".into()))?;
        if let Some(view) = &tab.view
            && let Err(error) = view.navigate(&address)
        {
            tab.error = Some(error.to_string());
            tab.loading = false;
            cx.notify();
            return Err(error);
        }
        tab.error = None;
        tab.address.clone_from(&address);
        tab.loading = address != "about:blank";
        if address == "about:blank" {
            if let Some(view) = &mut tab.view {
                view.set_visible(false)?;
            }
        } else {
            self.watch_load(page, window, cx);
        }
        if page == self.selected {
            if address != "about:blank" {
                self.interaction = BrowserInteraction::RestoringPageFocus;
            }
            self.address
                .update(cx, |input, cx| input.set_value(address, window, cx));
        }
        cx.notify();
        Ok(())
    }

    /// The shell also hides native children for overlays, which render above the GPU scene.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.host_visible = visible;
        self.sync_visibility(cx);
    }

    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        let visible = self.host_visible && self.active;
        if self.visible == visible {
            return;
        }
        if !visible {
            self.invalidate_capture();
            if let Some(tab) = self.selected_mut()
                && let Some(view) = &tab.view
                && let Err(error) = view.focus_parent()
            {
                tab.error = Some(error.to_string());
            }
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

    fn selected_tab(&self) -> Option<&BrowserTab> {
        self.tabs.iter().find(|tab| tab.id == self.selected)
    }

    fn selected_mut(&mut self) -> Option<&mut BrowserTab> {
        self.tabs.iter_mut().find(|tab| tab.id == self.selected)
    }

    pub(crate) fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        if id != self.selected {
            self.invalidate_capture();
            self.annotation_navigation(self.selected);
        }
        self.reset_interaction();
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
    ) -> Result<(), NativeBrowserError> {
        self.invalidate_capture();
        let address = address
            .map(|address| resolve_address(address, self.config.search_engine.address()))
            .transpose()?;
        self.reset_interaction();
        // Bound native resources; revisit when suspended tabs exist.
        if self.tabs.len() >= 32 {
            return Err(NativeBrowserError::Platform(
                "Close a browser page before opening another; up to 32 pages can stay open.".into(),
            ));
        }
        let id = self.next_id;
        let Some(next_id) = id.checked_add(1) else {
            return Err(NativeBrowserError::Platform(
                "No browser page identities remain.".into(),
            ));
        };
        self.next_id = next_id;
        self.tabs.push(BrowserTab {
            id,
            address: "about:blank".to_owned(),
            title: "New tab".to_owned(),
            loading: false,
            load_revision: 0,
            view_revision: 0,
            load_timeout: None,
            error: None,
            view: None,
        });
        self.select_tab(id, window, cx);
        cx.emit(BrowserPagesChanged);
        if let Some(address) = address {
            self.open_url(&address, window, cx)?;
        } else {
            self.address.update(cx, |input, cx| input.focus(window, cx));
        }
        Ok(())
    }

    pub(crate) fn restore_page(
        &mut self,
        id: u64,
        address: &str,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        if self.tabs.iter().any(|tab| tab.id == id) {
            return Ok(());
        }
        let address = normalize_address(address)?;
        let next_id = id.checked_add(1).filter(|_| id != 0).ok_or_else(|| {
            NativeBrowserError::Platform("This saved browser page identity is invalid.".into())
        })?;
        self.new_tab(Some(&address), window, cx)?;
        let created = self.selected;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == created) {
            tab.id = id;
            tab.title = title.chars().take(256).collect();
            tab.load_timeout = None;
        }
        self.selected = id;
        self.next_id = self.next_id.max(next_id);
        self.watch_load(id, window, cx);
        cx.emit(BrowserPagesChanged);
        Ok(())
    }

    pub(crate) fn pages(&self) -> Vec<BrowserPageSnapshot> {
        self.tabs
            .iter()
            .map(|tab| BrowserPageSnapshot {
                id: tab.id,
                title: tab.title.clone(),
                icon: IconName::Globe,
            })
            .collect()
    }

    pub(crate) fn has_native_views(&self) -> bool {
        self.tabs.iter().any(|tab| tab.view.is_some())
    }

    pub(crate) const fn selected(&self) -> u64 {
        self.selected
    }

    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if id == self.selected {
            self.invalidate_capture();
        }
        self.reset_interaction();
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.selected == id
            && let Some(tab) = self.tabs.get_mut(index)
            && let Some(view) = &mut tab.view
        {
            _ = view.set_visible(false);
        }
        self.annotation_navigation(id);
        self.tabs.remove(index);
        cx.emit(BrowserPagesChanged);
        if self.tabs.is_empty() {
            self.selected = 0;
            cx.emit(BrowserClosed);
            cx.emit(BrowserPagesChanged);
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
        page: u64,
        operation: impl FnOnce(&BrowserView) -> Result<(), NativeBrowserError>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        let tab = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == page)
            .ok_or_else(|| NativeBrowserError::Platform("This browser page is closed.".into()))?;
        let view = tab.view.as_ref().ok_or_else(|| {
            NativeBrowserError::Platform("This browser page is unavailable.".into())
        })?;
        if let Err(error) = operation(view) {
            tab.error = Some(error.to_string());
            tab.loading = false;
            cx.notify();
            return Err(error);
        }
        tab.error = None;
        tab.loading = true;
        self.watch_load(page, window, cx);
        cx.notify();
        Ok(())
    }

    fn watch_load(&mut self, id: u64, window: &Window, cx: &Context<Self>) {
        if id == self.selected {
            self.invalidate_capture();
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        // Hidden restored pages have not started loading until a native view exists.
        if tab.view.is_none() {
            return;
        }
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

    fn create_webview(
        &mut self,
        bounds: BrowserBounds,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeBrowserError> {
        let page = self.selected;
        let address = self
            .selected_tab()
            .map(|tab| tab.address.clone())
            .ok_or_else(|| NativeBrowserError::Platform("This browser page is closed.".into()))?;
        let (sender, receiver) = async_channel::bounded(128);
        let view = BrowserView::new(
            window,
            &address,
            bounds,
            sender,
            &mut self.profile,
            self.config.persist_site_data,
        )?;
        let tab = self
            .selected_mut()
            .ok_or_else(|| NativeBrowserError::Platform("This browser page is closed.".into()))?;
        tab.view_revision = tab.view_revision.wrapping_add(1);
        let revision = tab.view_revision;
        tab.view = Some(view);
        self.watch_load(page, window, cx);
        #[cfg(target_os = "linux")]
        self.ensure_platform_pump(window, cx);
        cx.spawn_in(window, async move |owner, cx| {
            while let Ok(event) = receiver.recv().await {
                let result = cx.update(|window, cx| {
                    owner.update(cx, |this, cx| {
                        this.receive_event(page, revision, event, window, cx);
                    })
                });
                if !matches!(result, Ok(Ok(()))) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
        Ok(())
    }

    fn layout_webview(
        &mut self,
        bounds: BrowserBounds,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible
            || self.resetting
            || self
                .selected_tab()
                .is_none_or(|tab| tab.address == "about:blank")
            || bounds.width < 1.0
            || bounds.height < 1.0
        {
            self.webview_bounds = None;
            self.invalidate_capture();
            if let Some(tab) = self.selected_mut()
                && let Some(view) = &mut tab.view
            {
                _ = view.set_visible(false);
            }
            return;
        }
        self.webview_bounds = Some(bounds);
        let create = self.selected_tab().is_some_and(|tab| {
            tab.view.is_none() && tab.error.is_none() && tab.address != "about:blank"
        });
        if create && let Err(error) = self.create_webview(bounds, window, cx) {
            if let Some(tab) = self.selected_mut() {
                tab.error = Some(error.to_string());
                tab.loading = false;
            }
            cx.notify();
            return;
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
        #[cfg(target_os = "macos")]
        self.invalidate_capture_geometry();
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
                let live = owner.update(cx, |this, _| {
                    this.reset_view.is_some() || this.tabs.iter().any(|tab| tab.view.is_some())
                });
                if !matches!(live, Ok(true)) {
                    _ = owner.update(cx, |this, _| this.platform_task = None);
                    break;
                }
                bootty_browser::poll_platform_events();
            }
        }));
    }

    pub(crate) const fn record_interaction(&mut self) {
        self.interaction_generation = self.interaction_generation.wrapping_add(1);
    }

    const fn record_host_interaction(&mut self) {
        self.record_interaction();
        self.reset_interaction();
    }

    pub(crate) const fn interaction_generation(&self) -> u64 {
        self.interaction_generation
    }

    pub(crate) fn focus_host(&mut self, cx: &mut Context<Self>) {
        self.reset_interaction();
        if let Some(tab) = self.selected_mut()
            && let Some(view) = &tab.view
            && let Err(error) = view.focus_parent()
        {
            tab.error = Some(error.to_string());
            cx.notify();
        }
    }

    const fn reset_interaction(&mut self) {
        self.interaction = BrowserInteraction::Idle;
    }

    fn receive_event(
        &mut self,
        id: u64,
        revision: u64,
        event: BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .tabs
            .iter()
            .any(|tab| tab.id == id && tab.view_revision == revision && tab.view.is_some())
        {
            return;
        }
        if let BrowserEvent::Annotation(event) = event {
            self.receive_annotation(id, event, window, cx);
            return;
        }
        if matches!(event, BrowserEvent::PageFocused) {
            self.record_interaction();
            if id == self.selected && self.visible && self.host_visible {
                // Clear GPUI shortcut dispatch without changing native keyboard focus.
                window.blur(cx);
            }
            return;
        }
        if matches!(&event, BrowserEvent::PointerEntered) {
            if id == self.selected && self.visible && self.host_visible {
                self.dismiss_gpui_hover(window, cx);
            }
            return;
        }
        if let BrowserEvent::Shortcut(shortcut) = event {
            if id == self.selected && self.visible {
                self.shortcut(shortcut, window, cx);
            }
            return;
        }
        if let BrowserEvent::OpenTab(address) = event {
            if id == self.selected && self.visible && normalize_address(&address).is_ok() {
                self.submit("browser.new_tab", vec![address], window, cx);
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
                BrowserEvent::TitleChanged(title) => {
                    tab.title = title;
                    cx.emit(BrowserPagesChanged);
                }
                BrowserEvent::Notice(message) => tab.error = Some(message),
                BrowserEvent::PageFocused
                | BrowserEvent::PointerEntered
                | BrowserEvent::Annotation(_)
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
            self.annotation_navigation(id);
            self.watch_load(id, window, cx);
        }
        cx.notify();
    }

    fn dismiss_gpui_hover(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bounds) = self.webview_bounds else {
            return;
        };
        let Some((x, y)) = bounds
            .width
            .mul_add(0.5, bounds.x)
            .to_f32()
            .filter(|value| value.is_finite())
            .zip(
                bounds
                    .height
                    .mul_add(0.5, bounds.y)
                    .to_f32()
                    .filter(|value| value.is_finite()),
            )
        else {
            return;
        };
        // Native WebView pointer events do not reach GPUI's hover listeners.
        let position = gpui_kit::point(gpui_kit::px(x), gpui_kit::px(y));
        window.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    }

    fn render_page_actions(&self, can_open_external: bool, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("browser-external")
                    .icon(IconName::ExternalLink)
                    .ghost()
                    .small()
                    .size_6()
                    .disabled(!can_open_external)
                    .accessibility_label("Open in default browser")
                    .tooltip("Open in default browser")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit("browser.open_external", Vec::new(), window, cx);
                    })),
            )
            .when(
                !self
                    .profile
                    .persists_site_data(self.config.persist_site_data),
                |toolbar| {
                    toolbar.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Private"),
                    )
                },
            )
    }

    fn render_browser_menu(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let owner = cx.entity();
        let reset_unavailable = self.resetting || !BrowserProfile::supports_site_data_reset();
        div().flex().flex_shrink_0().child(
            Button::new("browser-menu")
                .icon(IconName::EllipsisVertical)
                .ghost()
                .small()
                .size_6()
                .accessibility_label("Browser options")
                .tooltip("Browser options")
                .dropdown_menu(move |menu, _, _| {
                    let settings = owner.clone();
                    let reset = owner.clone();
                    menu.item(PopupMenuItem::new("Browser settings").on_click(
                        move |_, window, cx| {
                            settings.update(cx, |this, cx| {
                                this.submit(
                                    "open_setting",
                                    vec!["browser.search-engine".to_owned()],
                                    window,
                                    cx,
                                );
                            });
                        },
                    ))
                    .separator()
                    .item(
                        PopupMenuItem::new("Reset saved site data")
                            .disabled(reset_unavailable)
                            .on_click(move |_, _, cx| {
                                reset.update(cx, |this, cx| {
                                    this.invalidate_capture();
                                    this.confirm_reset = true;
                                    cx.notify();
                                });
                            }),
                    )
                }),
        )
    }

    fn render_reset_confirmation(cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .text_sm()
            .child("Reset this app identity’s saved site data?")
            .child(
                Button::new("browser-reset-cancel")
                    .label("Cancel")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_reset = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("browser-reset-confirm")
                    .label("Reset")
                    .danger()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit("browser.reset_site_data", Vec::new(), window, cx);
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
                        this.submit("browser.back", Vec::new(), window, cx);
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
                        this.submit("browser.forward", Vec::new(), window, cx);
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
                                .icon(if loading {
                                    IconName::Close
                                } else {
                                    IconName::RotateCw
                                })
                                .ghost()
                                .xsmall()
                                .disabled(!can_open_external)
                                .accessibility_label(if loading {
                                    "Stop loading"
                                } else {
                                    "Reload page"
                                })
                                .tooltip(if loading {
                                    "Stop loading"
                                } else {
                                    "Reload page"
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit(
                                        if this.selected_tab().is_some_and(|tab| tab.loading) {
                                            "browser.stop"
                                        } else {
                                            "browser.reload"
                                        },
                                        Vec::new(),
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                )),
            )
            .child(self.render_page_actions(can_open_external, cx))
            .child(self.render_annotation_actions(cx))
            .child(self.render_capture_action(cx))
            .child(self.render_attachment_action(cx))
            .child(self.render_browser_menu(cx))
    }
}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.address.focus_handle(cx)
    }
}
impl EventEmitter<BrowserClosed> for BrowserPanel {}
impl EventEmitter<BrowserPagesChanged> for BrowserPanel {}
impl Render for BrowserPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let error = self.selected_tab().and_then(|tab| tab.error.clone());
        let empty = self
            .selected_tab()
            .is_none_or(|tab| tab.address == "about:blank");
        div()
            .id("browser-panel")
            .key_context("BoottyBrowser")
            .on_action(cx.listener(
                |this, _: &crate::gpui_actions::CloseBrowserTab, window, cx| {
                    this.shortcut(BrowserShortcut::CloseTab, window, cx);
                    cx.stop_propagation();
                },
            ))
            .capture_key_down(cx.listener(|this, _, _, _| this.record_host_interaction()))
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.record_host_interaction()))
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .child(self.render_toolbar(cx))
            .when(self.confirm_reset, |body| {
                body.child(Self::render_reset_confirmation(cx))
            })
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

/// One page in the right sidebar strip; the browser session owns every native view.
pub struct BrowserPagePanel {
    browser: Entity<BrowserPanel>,
    page: u64,
    unavailable: Option<(gpui_kit::component::dock::PanelState, String)>,
}

impl BrowserPagePanel {
    pub(crate) fn new(browser: Entity<BrowserPanel>, page: u64, cx: &mut Context<Self>) -> Self {
        cx.observe(&browser, |_, _, cx| cx.notify()).detach();
        Self {
            browser,
            page,
            unavailable: None,
        }
    }

    pub(crate) fn unavailable(
        browser: Entity<BrowserPanel>,
        page: u64,
        state: gpui_kit::component::dock::PanelState,
        error: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self::new(browser, page, cx);
        panel.unavailable = Some((state, error));
        panel
    }
}

impl Focusable for BrowserPagePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.browser.focus_handle(cx)
    }
}
impl EventEmitter<PanelEvent> for BrowserPagePanel {}
impl BasePanel for BrowserPagePanel {
    fn panel_name(&self) -> &'static str {
        "bootty.browser.page"
    }
    fn dump(&self, cx: &App) -> gpui_kit::component::dock::PanelState {
        if let Some((state, _)) = &self.unavailable {
            return state.clone();
        }
        let browser = self.browser.read(cx);
        let tab = browser.tabs.iter().find(|tab| tab.id == self.page);
        gpui_kit::component::dock::PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: gpui_kit::component::dock::PanelInfo::panel(serde_json::json!({
                "page": self.page,
                "address": tab.map_or("about:blank", |tab| tab.address.as_str()),
                "title": tab.map_or("Browser", |tab| tab.title.as_str()),
            })),
        }
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.unavailable.is_some() {
            return;
        }
        self.browser.update(cx, |browser, cx| {
            if active {
                browser.select_tab(self.page, window, cx);
                browser.active = true;
                if browser
                    .selected_tab()
                    .is_some_and(|tab| tab.address != "about:blank")
                    // Revealing the right dock must not steal focus from a new center surface.
                    && browser.focus_handle(cx).contains_focused(window, cx)
                {
                    browser.interaction = BrowserInteraction::RestoringPageFocus;
                }
            } else if browser.selected == self.page {
                browser.active = false;
            }
            browser.sync_visibility(cx);
        });
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| {
            if browser.selected == self.page {
                browser.active = false;
                browser.sync_visibility(cx);
            }
        });
    }
}
impl Panel for BrowserPagePanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        self.browser
            .read(cx)
            .pages()
            .into_iter()
            .find(|page| page.id == self.page)
            .map(|page| page.title.into())
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_else(|| "Browser".into())
    }
}
impl Render for BrowserPagePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some((_, error)) = &self.unavailable {
            div()
                .size_full()
                .p_4()
                .child(error.clone())
                .into_any_element()
        } else {
            self.browser.clone().into_any_element()
        }
    }
}
