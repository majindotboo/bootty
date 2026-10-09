//! One local recording owner shared by both composers; providers receive text through normal prompts.
use bootty_host::speech::{SpeechCommand, SpeechEvent, SpeechSession};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    notification::Notification,
};

pub fn notify_error(error: &str, window: &mut Window, cx: &mut gpui_kit::App) {
    window.push_notification(
        Notification::error(crate::error_catalog::ErrorNotice::from_text(error).to_string())
            .id::<SpeechInput>(),
        cx,
    );
}
use gpui_kit::{
    Context, EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    prelude::*,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpeechMode {
    Dictation,
    Voice,
}

#[derive(Clone)]
pub enum SpeechInputEvent {
    Toggle(SpeechMode),
    Final { text: String, mode: SpeechMode },
    Error(String),
}

pub struct SpeechInput {
    session: Option<SpeechSession>,
    epoch: u64,
    mode: SpeechMode,
    phase: &'static str,
    partial: String,
    voice_available: bool,
    enabled: bool,
    account: Option<SpeechCommand>,
}

impl SpeechInput {
    pub(crate) const fn new(voice_available: bool) -> Self {
        Self {
            session: None,
            epoch: 0,
            mode: SpeechMode::Dictation,
            phase: "",
            partial: String::new(),
            voice_available,
            enabled: true,
            account: None,
        }
    }

    pub(crate) fn set_account(
        &mut self,
        preferences: &bootty_config::config::AgentProviderConfig,
        provider: &str,
    ) {
        self.set_captured_account(
            provider,
            if preferences.program.is_empty() {
                provider.into()
            } else {
                preferences.program.clone()
            },
            preferences
                .selected_profile()
                .and_then(|profile| profile.directory.clone()),
        );
    }

    pub(crate) fn set_captured_account(
        &mut self,
        provider: &str,
        program: String,
        directory: Option<String>,
    ) {
        self.account = Some(SpeechCommand::Account {
            provider: provider.into(),
            program,
            directory,
        });
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled != enabled {
            self.enabled = enabled;
            cx.notify();
        }
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        self.session = None;
        self.partial.clear();
        self.phase = "";
        cx.notify();
    }

    pub(crate) fn dictating(&self) -> bool {
        self.session.is_some() && self.mode == SpeechMode::Dictation
    }
    pub(crate) fn finish_dictation(&mut self, cx: &mut Context<Self>) {
        if self.phase == "Starting dictation…" {
            self.stop(cx);
        } else {
            self.send(SpeechCommand::Stop, cx);
        }
    }

    pub(crate) fn voice_active(&self) -> bool {
        self.session.is_some() && self.mode == SpeechMode::Voice
    }

    pub(crate) fn speak(&mut self, text: String, cx: &mut Context<Self>) {
        self.send(SpeechCommand::Speak { text }, cx);
    }

    fn send(&mut self, command: SpeechCommand, cx: &mut Context<Self>) {
        if let Some(session) = &self.session
            && let Err(error) = session.send(command)
        {
            cx.emit(SpeechInputEvent::Error(error));
            self.stop(cx);
        }
    }

    pub(crate) fn toggle(&mut self, mode: SpeechMode, window: &Window, cx: &mut Context<Self>) {
        if self.session.is_some() {
            if self.mode == SpeechMode::Dictation && mode == self.mode {
                self.finish_dictation(cx);
            } else {
                self.stop(cx);
            }
            return;
        }
        if !self.enabled || mode == SpeechMode::Voice && !self.voice_available {
            return;
        }
        let Some(program) = std::env::current_exe().ok().and_then(|path| {
            Some(path.parent()?.join(if cfg!(windows) {
                "bootty-voice.exe"
            } else {
                "bootty-voice"
            }))
        }) else {
            cx.emit(SpeechInputEvent::Error(
                "The local speech helper is unavailable".into(),
            ));
            return;
        };
        let (session, mut events) = SpeechSession::start(program);
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.mode = mode;
        self.phase = if mode == SpeechMode::Voice {
            "Connecting voice…"
        } else {
            "Starting dictation…"
        };
        self.session = Some(session);
        if let Some(account) = self.account.clone() {
            self.send(account, cx);
        }
        self.send(
            SpeechCommand::Listen {
                voice: mode == SpeechMode::Voice,
            },
            cx,
        );
        cx.spawn_in(window, async move |owner, cx| {
            while let Some(event) = events.recv().await {
                let active = owner
                    .update(cx, |this, cx| {
                        if this.epoch != epoch || this.session.is_none() {
                            return false;
                        }
                        match event {
                            SpeechEvent::Listening => this.phase = "Listening…",
                            SpeechEvent::Partial { text } => this.partial = text,
                            SpeechEvent::Final { text } => {
                                if this.mode == SpeechMode::Dictation {
                                    this.stop(cx);
                                }
                                this.partial.clear();
                                this.phase = if this.mode == SpeechMode::Voice {
                                    "Waiting for reply…"
                                } else {
                                    "Finishing dictation…"
                                };
                                cx.emit(SpeechInputEvent::Final {
                                    text,
                                    mode: this.mode,
                                });
                            }
                            SpeechEvent::Speaking => this.phase = "Speaking…",
                            SpeechEvent::Finished if this.mode == SpeechMode::Voice => {
                                this.phase = "Listening…";
                            }
                            SpeechEvent::Stopped if this.mode == SpeechMode::Dictation => {
                                this.stop(cx);
                            }
                            SpeechEvent::Closed => this.stop(cx),
                            SpeechEvent::Error { message } => {
                                cx.emit(SpeechInputEvent::Error(message));
                                this.stop(cx);
                            }
                            SpeechEvent::Ready | SpeechEvent::Finished | SpeechEvent::Stopped => {}
                        }
                        cx.notify();
                        this.session.is_some()
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }
}

impl EventEmitter<SpeechInputEvent> for SpeechInput {}
impl Render for SpeechInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.session.is_some();
        div()
            .flex()
            .items_center()
            .gap_1()
            .when(active, |row| {
                row.child(
                    div()
                        .max_w(gpui_kit::rems(14.))
                        .text_xs()
                        .text_ellipsis()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.partial.is_empty() {
                            self.phase.to_owned()
                        } else {
                            self.partial.clone()
                        }),
                )
            })
            .children(
                [SpeechMode::Dictation, SpeechMode::Voice]
                    .into_iter()
                    .filter(|mode| *mode != SpeechMode::Voice || self.voice_available)
                    .map(|mode| {
                        let selected = active && self.mode == mode;
                        let label = match (mode, selected) {
                            (SpeechMode::Dictation, false) => "Dictate",
                            (SpeechMode::Dictation, true) => "Finish dictation",
                            (SpeechMode::Voice, false) => "Start voice mode",
                            (SpeechMode::Voice, true) => "End voice mode",
                        };
                        Button::new(match mode {
                            SpeechMode::Dictation => "composer-dictation",
                            SpeechMode::Voice => "composer-voice",
                        })
                        .ghost()
                        .small()
                        .selected(selected)
                        .disabled(!active && !self.enabled || active && self.mode != mode)
                        .accessibility_label(label)
                        .tooltip(label)
                        .child(crate::gpui::sized_icon(
                            match mode {
                                SpeechMode::Dictation => "mic",
                                SpeechMode::Voice => "audio-lines",
                            },
                            crate::gpui::IconSize::Small,
                            if selected {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            },
                        ))
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(SpeechInputEvent::Toggle(mode))),
                        )
                    }),
            )
    }
}
