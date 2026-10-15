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
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpeechMode {
    Dictation,
    Voice,
}

#[derive(Clone)]
pub enum SpeechInputEvent {
    Toggle(SpeechMode),
    /// Dictated text for the composer.
    Final {
        text: String,
    },
    /// Voice mode's offer, for the conversation to take to Codex.
    Offer(String),
    /// A voice call ended here, so the conversation ends Codex's side.
    VoiceEnded,
    Error(String),
}

/// Speech runs on the Codex sign-in already on this machine. Without one, dictation runs on device
/// where the OS can, and otherwise its buttons don't show.
pub struct SpeechInput {
    session: Option<SpeechSession>,
    epoch: u64,
    mode: SpeechMode,
    phase: &'static str,
    partial: String,
    /// What the captured Codex account and this machine offer.
    availability: bootty_agents::voice::SpeechAvailability,
    /// The composer belongs to a Codex conversation, which can hold a voice call.
    voice_supported: bool,
    enabled: bool,
    account: Option<SpeechCommand>,
    availability_check: Option<gpui_kit::Task<()>>,
}

impl SpeechInput {
    pub(crate) const fn new() -> Self {
        Self {
            session: None,
            epoch: 0,
            mode: SpeechMode::Dictation,
            phase: "",
            partial: String::new(),
            availability: bootty_agents::voice::SpeechAvailability {
                codex: false,
                on_device: false,
            },
            voice_supported: false,
            enabled: true,
            account: None,
            availability_check: None,
        }
    }

    /// Use the selected Codex account from the configuration.
    pub(crate) fn set_account(
        &mut self,
        codex: &bootty_config::config::AgentProviderConfig,
        cx: &Context<Self>,
    ) {
        self.set_captured_account(
            if codex.program.is_empty() {
                "codex".into()
            } else {
                codex.program.clone()
            },
            codex
                .selected_profile()
                .and_then(|profile| profile.directory.clone()),
            cx,
        );
    }

    /// Use a Codex account captured elsewhere, such as by a Codex conversation.
    pub(crate) fn set_captured_account(
        &mut self,
        program: String,
        directory: Option<String>,
        cx: &Context<Self>,
    ) {
        let check = directory.clone();
        self.account = Some(SpeechCommand::Account {
            provider: "codex".into(),
            program,
            directory,
        });
        // Reading the sign-in is file work; the buttons appear once it's known.
        self.availability_check = Some(cx.spawn(async move |owner, cx| {
            let availability = cx
                .background_executor()
                .spawn(async move {
                    bootty_agents::voice::availability(check.as_deref().map(Path::new))
                })
                .await;
            _ = owner.update(cx, |this, cx| {
                if this.availability != availability {
                    this.availability = availability;
                    cx.notify();
                }
            });
        }));
    }

    /// Voice calls run on a Codex conversation's own session.
    pub(crate) fn set_voice_supported(&mut self, supported: bool, cx: &mut Context<Self>) {
        if self.voice_supported != supported {
            self.voice_supported = supported;
            cx.notify();
        }
    }

    const fn available(&self, mode: SpeechMode) -> bool {
        match mode {
            SpeechMode::Dictation => self.availability.dictation(),
            SpeechMode::Voice => self.voice_supported && self.availability.codex,
        }
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
        if self.mode == SpeechMode::Voice {
            cx.emit(SpeechInputEvent::VoiceEnded);
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

    /// Connect the voice call with Codex's answer to its offer.
    pub(crate) fn answer(&mut self, sdp: String, cx: &mut Context<Self>) {
        self.send(SpeechCommand::Answer { sdp }, cx);
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
        if !self.available(mode) {
            cx.emit(SpeechInputEvent::Error(match mode {
                SpeechMode::Dictation => "Sign in to Codex to use dictation".into(),
                SpeechMode::Voice => "Voice mode isn't available here".into(),
            }));
            return;
        }
        if !self.enabled {
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
        // Dictation prefers the Codex sign-in; the recognizer on this machine stands in without one.
        self.send(
            if mode == SpeechMode::Dictation && !self.availability.codex {
                SpeechCommand::ListenOnDevice
            } else {
                SpeechCommand::Listen {
                    voice: mode == SpeechMode::Voice,
                }
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
                            // Only dictation produces text; Codex speaks voice mode's replies.
                            SpeechEvent::Final { text } => {
                                this.stop(cx);
                                this.partial.clear();
                                cx.emit(SpeechInputEvent::Final { text });
                            }
                            SpeechEvent::Offer { sdp } => cx.emit(SpeechInputEvent::Offer(sdp)),
                            SpeechEvent::Stopped if this.mode == SpeechMode::Dictation => {
                                this.stop(cx);
                            }
                            SpeechEvent::Closed => this.stop(cx),
                            SpeechEvent::Error { message } => {
                                cx.emit(SpeechInputEvent::Error(message));
                                this.stop(cx);
                            }
                            SpeechEvent::Ready | SpeechEvent::Stopped => {}
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
                    .filter(|mode| self.available(*mode))
                    .map(|mode| {
                        let selected = active && self.mode == mode;
                        let label = match (mode, selected) {
                            (SpeechMode::Dictation, false) => "Dictate",
                            (SpeechMode::Dictation, true) => "Finish dictation",
                            (SpeechMode::Voice, false) => "Start voice mode",
                            (SpeechMode::Voice, true) => "End voice mode",
                        };
                        let id = match mode {
                            SpeechMode::Dictation => "composer-dictation",
                            SpeechMode::Voice => "composer-voice",
                        };
                        Button::new(id)
                            .debug_selector(move || id.to_owned())
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
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SpeechInputEvent::Toggle(mode));
                            }))
                    }),
            )
    }
}
