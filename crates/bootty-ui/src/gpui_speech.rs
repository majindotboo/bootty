//! One local recording owner shared by both composers; providers receive text through normal prompts.
use crate::gpui::ElementTooltip as _;
use bootty_host::speech::{SpeechCommand, SpeechEvent, SpeechSession};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    notification::Notification,
};

/// Shows a failure of `action`, such as "Could not start voice mode", with its details a click away.
pub fn notify_error(action: &str, error: &str, window: &mut Window, cx: &mut gpui_kit::App) {
    let notice = crate::error_catalog::ErrorNotice::failed(action, error);
    let details = notice.details().map(str::to_owned);
    window.push_notification(
        Notification::error(notice.to_string())
            .when_some(details, |notification, details| {
                notification.action(move |_, _, _| {
                    let details = details.clone();
                    Button::new("copy-speech-error-details")
                        .label("Copy details")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                details.clone(),
                            ));
                        })
                })
            })
            .id::<SpeechInput>()
            // After the action, which turns hiding off; hovering holds it open.
            .autohide(true),
        cx,
    );
}
use gpui_kit::component::speech::{SpeechEvent as DictationEvent, SpeechState, SpeechStatus};
use gpui_kit::{App, AppContext as _, Entity, Subscription};
use gpui_kit::{
    Context, EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    prelude::*,
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};
mod dictation;

fn helper_program() -> Result<PathBuf, anyhow::Error> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("The local speech helper is unavailable"))?;
    Ok(directory.join(if cfg!(windows) {
        "bootty-voice.exe"
    } else {
        "bootty-voice"
    }))
}

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
    phase: &'static str,
    /// What the captured Codex account and this machine offer.
    availability: bootty_agents::voice::SpeechAvailability,
    /// The composer belongs to a Codex conversation, which can hold a voice call.
    voice_supported: bool,
    enabled: bool,
    account: Option<SpeechCommand>,
    dictation: Entity<SpeechState>,
    dictation_settings: Rc<RefCell<dictation::Settings>>,
    _dictation_subscription: Subscription,
    _dictation_observation: Subscription,
    availability_check: Option<gpui_kit::Task<()>>,
}

impl SpeechInput {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let settings = Rc::new(RefCell::new(dictation::Settings::default()));
        let dictation = cx.new(|cx| {
            SpeechState::new(cx)
                .system_fallback(false)
                .recognizer(dictation::Recognizer(settings.clone()))
                .input(dictation::HelperInput(settings.clone()))
        });
        let subscription = cx.subscribe(&dictation, |_, _, event, cx| match event {
            DictationEvent::Final(text) if !text.is_empty() => cx.emit(SpeechInputEvent::Final {
                text: text.to_string(),
            }),
            DictationEvent::Error(error) => cx.emit(SpeechInputEvent::Error(error.to_string())),
            _ => {}
        });
        let observation = cx.observe(&dictation, |_, _, cx| cx.notify());
        Self {
            session: None,
            epoch: 0,
            phase: "",
            availability: bootty_agents::voice::SpeechAvailability::default(),
            voice_supported: false,
            enabled: true,
            account: None,
            dictation,
            dictation_settings: settings,
            _dictation_subscription: subscription,
            _dictation_observation: observation,
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
        self.dictation_settings
            .borrow_mut()
            .account
            .clone_from(&self.account);
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
                    this.dictation_settings.borrow_mut().availability = availability;
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
        self.dictation.update(cx, SpeechState::cancel);
        if self.session.take().is_some() {
            cx.emit(SpeechInputEvent::VoiceEnded);
        }
        self.phase = "";
        cx.notify();
    }

    pub(crate) fn dictating(&self, cx: &App) -> bool {
        self.dictation.read(cx).status().is_active()
    }

    pub(crate) fn finish_dictation(&mut self, cx: &mut Context<Self>) {
        self.dictation.update(cx, SpeechState::stop);
    }

    pub(crate) const fn voice_active(&self) -> bool {
        self.session.is_some()
    }

    pub(crate) fn voice_epoch(&self) -> Option<u64> {
        self.session.as_ref().map(|_| self.epoch)
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
        if self.voice_active() || mode == SpeechMode::Voice && self.dictating(cx) {
            self.stop(cx);
            return;
        }
        if !self.available(mode) {
            cx.emit(SpeechInputEvent::Error(match mode {
                SpeechMode::Dictation => "Sign in to Codex to use dictation".into(),
                SpeechMode::Voice => "Voice mode isn't available here".into(),
            }));
            return;
        }
        if mode == SpeechMode::Dictation {
            if self.enabled || self.dictating(cx) {
                self.dictation.update(cx, SpeechState::toggle);
            }
            return;
        }
        if !self.enabled {
            return;
        }
        let program = match helper_program() {
            Ok(program) => program,
            Err(error) => {
                cx.emit(SpeechInputEvent::Error(error.to_string()));
                return;
            }
        };
        let (session, mut events) = SpeechSession::start(program);
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.phase = "Connecting voice…";
        self.session = Some(session);
        if let Some(account) = self.account.clone() {
            self.send(account, cx);
        }
        self.send(SpeechCommand::Listen { voice: true }, cx);
        cx.spawn_in(window, async move |owner, cx| {
            while let Some(event) = events.recv().await {
                let active = owner
                    .update(cx, |this, cx| {
                        if this.epoch != epoch || this.session.is_none() {
                            return false;
                        }
                        match event {
                            SpeechEvent::Listening => this.phase = "Listening…",
                            SpeechEvent::Offer { sdp } => cx.emit(SpeechInputEvent::Offer(sdp)),
                            SpeechEvent::Closed => this.stop(cx),
                            SpeechEvent::Error { message } => {
                                cx.emit(SpeechInputEvent::Error(message));
                                this.stop(cx);
                            }
                            SpeechEvent::Ready
                            | SpeechEvent::Stopped
                            | SpeechEvent::Partial { .. }
                            | SpeechEvent::Final { .. }
                            | SpeechEvent::Level { .. } => {}
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

    fn dictation_button(&self, cx: &Context<Self>) -> Button {
        let status = self.dictation.read(cx).status();
        let label = if status.is_capturing() {
            "Finish dictation"
        } else {
            "Dictate"
        };
        // Keep clicks on CommandInvocation; use SpeechButton when it exposes a command callback.
        Button::new("composer-dictation")
            .debug_selector(|| "composer-dictation".into())
            .ghost()
            .small()
            .icon(if status.is_capturing() {
                IconName::Square
            } else {
                IconName::Mic
            })
            .selected(status.is_capturing())
            .loading(status == SpeechStatus::Stopping)
            .disabled(self.voice_active() || !self.enabled && !status.is_active())
            .accessibility_label(label)
            .element_tooltip(label)
            .on_click(cx.listener(|_, _, _, cx| {
                cx.emit(SpeechInputEvent::Toggle(SpeechMode::Dictation));
            }))
    }
}

impl EventEmitter<SpeechInputEvent> for SpeechInput {}
impl Render for SpeechInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let voice = self.voice_active();
        let dictating = self.dictating(cx);
        div()
            .flex()
            .items_center()
            .gap_1()
            .when(dictating, |row| {
                row.child(
                    gpui_kit::component::speech::SpeechWaveform::new(&self.dictation)
                        .small()
                        .w(gpui_kit::rems(4.)),
                )
            })
            .when(voice || dictating, |row| {
                row.child(
                    div()
                        .max_w(gpui_kit::rems(14.))
                        .text_xs()
                        .text_ellipsis()
                        .text_color(cx.theme().muted_foreground)
                        .child(if voice {
                            self.phase.to_owned()
                        } else {
                            self.dictation.read(cx).transcript().to_string()
                        }),
                )
            })
            .when(self.available(SpeechMode::Dictation), |row| {
                row.child(self.dictation_button(cx))
            })
            .when(self.available(SpeechMode::Voice), |row| {
                row.child(
                    Button::new("composer-voice")
                        .debug_selector(|| "composer-voice".into())
                        .ghost()
                        .small()
                        .selected(voice)
                        .disabled(dictating || !voice && !self.enabled)
                        .accessibility_label(if voice {
                            "End voice mode"
                        } else {
                            "Start voice mode"
                        })
                        .element_tooltip(if voice {
                            "End voice mode"
                        } else {
                            "Start voice mode"
                        })
                        .child(crate::gpui::sized_icon(
                            "audio-lines",
                            crate::gpui::IconSize::Small,
                            if voice {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            },
                        ))
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(SpeechInputEvent::Toggle(SpeechMode::Voice));
                        })),
                )
            })
    }
}
