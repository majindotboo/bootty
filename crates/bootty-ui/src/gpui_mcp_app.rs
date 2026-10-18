//! An MCP app in its own tab: the document an MCP server offers to show one of its tool calls,
//! in a native view bridged to that server through the conversation that made the call. The
//! conversation's provider owns the server. The tab owns the view and the bridge, and asks before
//! the app runs a tool that may change something.

use bootty_agents::{
    McpAppBridge, McpAppDocument, McpAppEffect, McpAppRequestId, McpAppToolCall, NativeMcpApp,
};
use bootty_browser::{BrowserBounds, BrowserEvent, BrowserView, McpAppMessage, McpAppPolicy};
use bootty_control::CommandOutcome;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, WeakEntity, Window, canvas, div, prelude::*,
};
use serde_json::{Map, Value, json};

use crate::gpui_agent_session::NativeAgentSessionView;

/// The app asked to be closed; its dock closes the tab.
pub struct McpAppClose;

/// The conversation and tool call an app shows.
struct AppSource {
    parent: WeakEntity<NativeAgentSessionView>,
    item: String,
    app: NativeMcpApp,
}

/// A tool the app wants to run, waiting for the user to allow it.
struct Approval {
    id: McpAppRequestId,
    tool: String,
    title: String,
    arguments: Value,
}

pub struct McpAppPanel {
    key: String,
    title: String,
    focus: FocusHandle,
    source: Option<AppSource>,
    /// The app's document and what it may reach, once read from its server.
    document: Option<(McpAppDocument, McpAppPolicy)>,
    bridge: Option<McpAppBridge>,
    view: Option<BrowserView>,
    /// Counts native views, so a closed view's buffered events are dropped.
    view_revision: u64,
    approval: Option<Approval>,
    /// Why the app isn't showing, in its place.
    stopped: Option<String>,
    /// A request of the app's that failed on Bootty's side.
    error: Option<String>,
    bounds: Option<BrowserBounds>,
    active: bool,
    /// The dock shows the tab and no overlay covers it.
    host_visible: bool,
}

impl McpAppPanel {
    pub(crate) fn new(
        key: String,
        parent: &Entity<NativeAgentSessionView>,
        item: String,
        app: NativeMcpApp,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self::closed(key, format!("{} app", app.server), None, cx);
        panel.source = Some(AppSource {
            parent: parent.downgrade(),
            item,
            app,
        });
        panel.read_document(window, cx);
        panel
    }

    /// A tab whose app isn't running, such as one saved before Bootty restarted.
    pub(crate) fn closed(
        key: String,
        title: String,
        stopped: Option<String>,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            key,
            title,
            focus: cx.focus_handle(),
            source: None,
            document: None,
            bridge: None,
            view: None,
            view_revision: 0,
            approval: None,
            stopped,
            error: None,
            bounds: None,
            active: false,
            host_visible: false,
        }
    }

    pub(crate) const fn has_view(&self) -> bool {
        self.view.is_some()
    }

    /// The dock hides native views under overlays, which render above the GPU scene.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.host_visible != visible {
            self.host_visible = visible;
            self.sync_view();
            cx.notify();
        }
    }

    /// Hides the app and gives it a moment to wrap up before its document goes.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = false;
        self.host_visible = false;
        self.sync_view();
        self.approval = None;
        let Some(teardown) = self.bridge.as_mut().and_then(McpAppBridge::teardown) else {
            self.finish_close();
            return;
        };
        self.run(teardown, window, cx);
        // The spec gives the app no deadline; two seconds bounds a closed tab's native view.
        let keep = cx.entity();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            _ = this.update(cx, |this, _| this.finish_close());
            drop(keep);
        })
        .detach();
    }

    fn finish_close(&mut self) {
        self.stop("This app closed.");
    }

    fn stop(&mut self, message: &str) {
        self.view = None;
        self.bridge = None;
        self.approval = None;
        self.stopped = Some(message.to_owned());
    }

    fn sync_view(&mut self) {
        if self.active && self.host_visible {
            return;
        }
        if let Some(view) = &mut self.view {
            _ = view.focus_parent();
            _ = view.set_visible(false);
        }
    }

    fn read_document(&self, window: &Window, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let arguments = vec![
            source.app.server.clone(),
            source.app.resource.clone(),
            source.item.clone(),
        ];
        self.request(
            "mcp-app-read",
            arguments,
            window,
            cx,
            |this, result, _, cx| {
                match result.and_then(|value| McpAppDocument::parse(&value)) {
                    Ok(document) => {
                        let policy = McpAppPolicy::new(
                            &document.connect_domains,
                            &document.resource_domains,
                        );
                        let call = this
                            .source
                            .as_ref()
                            .and_then(|source| source.app.call.as_deref().cloned());
                        this.bridge = Some(McpAppBridge::new(
                            env!("CARGO_PKG_VERSION"),
                            policy.sandbox(),
                            this.context(cx),
                            call,
                        ));
                        this.document = Some((document, policy));
                    }
                    Err(message) => this.stopped = Some(message),
                }
                cx.notify();
            },
        );
    }

    /// Sends `agents.native.<operation>` through the app's conversation and hands its result to
    /// `done`.
    fn request(
        &self,
        operation: &str,
        arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<Value, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let receiver = self
            .source
            .as_ref()
            .and_then(|source| source.parent.upgrade())
            .ok_or_else(|| "The conversation that showed this app is closed".to_owned())
            .and_then(|parent| parent.read(cx).native_request(operation, arguments));
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(message) => {
                cx.defer_in(window, move |this, window, cx| {
                    done(this, Err(message), window, cx);
                });
                return;
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            let result = match outcome {
                Ok(CommandOutcome::Success { value, .. }) => Ok(value),
                Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                    .unwrap_or_else(|| "The request failed".to_owned())),
                Err(_) => Err("Bootty stopped before the request finished".to_owned()),
            };
            _ = this.update_in(cx, |this, window, cx| done(this, result, window, cx));
        })
        .detach();
    }

    /// The host context the app reads: theme, size and platform.
    fn context(&self, cx: &App) -> Map<String, Value> {
        let theme = cx.theme();
        let hex = |color: gpui_kit::Hsla| {
            let color = gpui_kit::Rgba::from(color);
            let byte = |channel: f32| {
                num_traits::ToPrimitive::to_u8(&(channel.clamp(0., 1.) * 255.).round()).unwrap_or(0)
            };
            format!(
                "#{:02x}{:02x}{:02x}",
                byte(color.r),
                byte(color.g),
                byte(color.b)
            )
        };
        let (width, height) = self.bounds.map_or((0.0, 0.0), |bounds| {
            (bounds.width.round(), bounds.height.round())
        });
        let context = json!({
            "theme": if theme.is_dark() { "dark" } else { "light" },
            "styles": {"variables": {
                "--color-background-primary": hex(theme.background),
                "--color-background-secondary": hex(theme.secondary),
                "--color-background-tertiary": hex(theme.muted),
                "--color-text-primary": hex(theme.foreground),
                "--color-text-secondary": hex(theme.muted_foreground),
                "--color-text-danger": hex(theme.danger),
                "--color-border-primary": hex(theme.border),
                "--color-ring-primary": hex(theme.ring),
                "--font-sans": theme.font_family.to_string(),
                "--font-mono": theme.mono_font_family.to_string(),
            }},
            "displayMode": "inline",
            "availableDisplayModes": ["inline"],
            "containerDimensions": {"width": width, "height": height},
            "platform": "desktop",
            "userAgent": concat!("bootty/", env!("CARGO_PKG_VERSION")),
            "deviceCapabilities": {"touch": false, "hover": true},
        });
        match context {
            Value::Object(context) => context,
            _ => Map::new(),
        }
    }

    fn layout(&mut self, bounds: BrowserBounds, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || !self.host_visible || bounds.width < 1.0 || bounds.height < 1.0 {
            self.sync_view();
            return;
        }
        self.bounds = Some(bounds);
        if self.view.is_none()
            && let Some((document, policy)) = self.document.clone()
        {
            match self.create_view(&document.html, &policy, bounds, window, cx) {
                Ok(view) => self.view = Some(view),
                Err(error) => {
                    self.stop(&error);
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(view) = &mut self.view
            && let Err(error) = view
                .set_bounds(bounds)
                .and_then(|()| view.set_visible(true))
        {
            self.error = Some(error.to_string());
            cx.notify();
        }
        let context = self.context(cx);
        if let Some(effect) = self
            .bridge
            .as_mut()
            .and_then(|bridge| bridge.set_context(context))
        {
            self.run(effect, window, cx);
        }
    }

    fn create_view(
        &mut self,
        html: &str,
        policy: &McpAppPolicy,
        bounds: BrowserBounds,
        window: &Window,
        cx: &Context<Self>,
    ) -> Result<BrowserView, String> {
        let (sender, receiver) = async_channel::bounded(128);
        let view = BrowserView::mcp_app(window, html, policy, bounds, sender)
            .map_err(|error| format!("Couldn't show the app: {error}"))?;
        self.view_revision = self.view_revision.wrapping_add(1);
        let revision = self.view_revision;
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let delivered = this.update_in(cx, |this, window, cx| {
                    this.receive(revision, event, window, cx);
                });
                if delivered.is_err() {
                    break;
                }
            }
        })
        .detach();
        Ok(view)
    }

    fn receive(
        &mut self,
        revision: u64,
        event: BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if revision != self.view_revision || self.view.is_none() {
            return;
        }
        match event {
            BrowserEvent::App(McpAppMessage::App { text, activated }) => {
                let effects = self
                    .bridge
                    .as_mut()
                    .map_or_else(Vec::new, |bridge| bridge.receive(&text, activated));
                for effect in effects {
                    self.run(effect, window, cx);
                }
            }
            BrowserEvent::App(McpAppMessage::Navigated) => {
                self.stop("The app left its page, so Bootty stopped it.");
                cx.notify();
            }
            // Clear GPUI shortcut dispatch without changing native keyboard focus.
            BrowserEvent::PageFocused if self.active && self.host_visible => window.blur(cx),
            _ => {}
        }
    }

    fn run(&mut self, effect: McpAppEffect, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let (server, item, parent) = (
            source.app.server.clone(),
            source.item.clone(),
            source.parent.clone(),
        );
        match effect {
            McpAppEffect::Post(message) => {
                if let Some(view) = &self.view
                    && let Err(error) = view.post_app_message(&message)
                {
                    self.error = Some(error.to_string());
                    cx.notify();
                }
            }
            McpAppEffect::CallTool {
                id,
                name,
                arguments,
            } => self.call_tool(id, name, arguments, false, window, cx),
            McpAppEffect::ReadResource { id, uri } => {
                self.request(
                    "mcp-app-read",
                    vec![server, uri, item],
                    window,
                    cx,
                    move |this, result, window, cx| {
                        this.answer(&id, result, window, cx);
                    },
                );
            }
            McpAppEffect::OpenLink { id, url } => {
                cx.open_url(&url);
                self.answer(&id, Ok(Value::Null), window, cx);
            }
            McpAppEffect::Message { id, text } => {
                let offered = parent
                    .update(cx, |parent, cx| parent.offer_draft(&text, window, cx))
                    .map_err(|_| "The conversation that showed this app is closed".to_owned());
                self.answer(&id, offered.map(|()| Value::Null), window, cx);
            }
            McpAppEffect::Close => cx.emit(McpAppClose),
            McpAppEffect::TornDown => self.finish_close(),
        }
    }

    fn answer(
        &mut self,
        id: &McpAppRequestId,
        result: Result<Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(effect) = self
            .bridge
            .as_mut()
            .and_then(|bridge| bridge.answer(id, result))
        {
            self.run(effect, window, cx);
        }
    }

    fn call_tool(
        &self,
        id: McpAppRequestId,
        tool: String,
        arguments: Value,
        approved: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = &self.source else {
            return;
        };
        let mut request = vec![
            source.app.server.clone(),
            tool.clone(),
            arguments.to_string(),
        ];
        if approved {
            request.push("approved".to_owned());
        }
        self.request(
            "mcp-app-call",
            request,
            window,
            cx,
            move |this, result, window, cx| {
                let call = result.and_then(|value| {
                    serde_json::from_value::<McpAppToolCall>(value)
                        .map_err(|error| error.to_string())
                });
                match call {
                    Ok(McpAppToolCall::Result(value)) => this.answer(&id, Ok(value), window, cx),
                    Ok(McpAppToolCall::Approve { .. }) if this.approval.is_some() => this.answer(
                        &id,
                        Err("Another tool is waiting for the user to allow it.".to_owned()),
                        window,
                        cx,
                    ),
                    Ok(McpAppToolCall::Approve { title }) => {
                        this.approval = Some(Approval {
                            id,
                            tool,
                            title,
                            arguments,
                        });
                        cx.notify();
                    }
                    Err(message) => this.answer(&id, Err(message), window, cx),
                }
            },
        );
    }

    fn decide(&mut self, allow: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(approval) = self.approval.take() else {
            return;
        };
        if allow {
            self.call_tool(
                approval.id,
                approval.tool,
                approval.arguments,
                true,
                window,
                cx,
            );
        } else {
            self.answer(
                &approval.id,
                Err("Declined by the user.".to_owned()),
                window,
                cx,
            );
        }
        cx.notify();
    }

    fn render_approval(&self, approval: &Approval, cx: &Context<Self>) -> impl IntoElement {
        let server = self
            .source
            .as_ref()
            .map_or("The app", |source| source.app.server.as_str());
        let arguments: String = serde_json::to_string_pretty(&approval.arguments)
            .unwrap_or_default()
            .chars()
            .take(2000)
            .collect();
        div()
            .debug_selector(|| "mcp-app-approval".into())
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.4))
            .child(
                div()
                    .text_sm()
                    .child(format!("Allow {server} to run {}?", approval.title)),
            )
            .child(
                div()
                    .max_h(gpui_kit::rems(8.))
                    .overflow_hidden()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(arguments),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("mcp-app-allow")
                            .debug_selector(|| "mcp-app-allow".into())
                            .small()
                            .primary()
                            .label("Allow")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.decide(true, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("mcp-app-deny")
                            .debug_selector(|| "mcp-app-deny".into())
                            .small()
                            .ghost()
                            .label("Don't allow")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.decide(false, window, cx)),
                            ),
                    ),
            )
    }
}

impl EventEmitter<McpAppClose> for McpAppPanel {}
impl EventEmitter<PanelEvent> for McpAppPanel {}
impl Focusable for McpAppPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for McpAppPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.mcp_app"
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        self.sync_view();
        cx.notify();
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.active = false;
        self.sync_view();
        cx.notify();
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.panel_name().into(),
            children: Vec::new(),
            info: PanelInfo::panel(json!({"key": self.key, "title": self.title})),
        }
    }
}
impl Panel for McpAppPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone().into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .min_w_0()
            .max_w(gpui_kit::rems(10.))
            .text_ellipsis()
            .child(self.title.clone())
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for McpAppPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let placeholder = self.stopped.clone().or_else(|| {
            (self.document.is_none() && self.source.is_some())
                .then(|| "Opening the app…".to_owned())
        });
        div()
            .id("mcp-app")
            .debug_selector(|| "mcp-app".into())
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .when_some(self.approval.as_ref(), |panel, approval| {
                panel.child(self.render_approval(approval, cx))
            })
            .when_some(self.error.clone(), |panel, message| {
                panel.child(
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
                    .when_some(placeholder, |body, message| {
                        body.child(
                            div()
                                .debug_selector(|| "mcp-app-placeholder".into())
                                .size_full()
                                .p_4()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(message),
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
                                    _ = owner
                                        .update(cx, |this, cx| this.layout(bounds, window, cx));
                                });
                            },
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
    }
}
