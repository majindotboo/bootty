use super::{BrowserPanel, LoginTarget};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputContentType, InputState},
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    WeakEntity, Window, div, prelude::*,
};

pub(super) struct LoginEditor {
    target: LoginTarget,
    service: String,
    owner: WeakEntity<BrowserPanel>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    // One login per origin; add an account picker when multiple logins are supported.
    saved: Option<(String, Vec<u8>)>,
    pending: bool,
    message: Option<String>,
}

impl LoginEditor {
    pub(super) fn new(
        target: LoginTarget,
        service: String,
        owner: WeakEntity<BrowserPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username or email"));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password")
                .masked(true)
        });
        cx.observe(&username, |_, _, cx| cx.notify()).detach();
        cx.observe(&password, |_, _, cx| cx.notify()).detach();
        let read = cx.read_credentials(&service);
        cx.spawn(async move |editor, cx| {
            let result = read.await;
            _ = editor.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(saved) => this.saved = saved,
                    Err(_) => this.message = Some("Could not read the OS credential store.".into()),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            target,
            service,
            owner,
            username,
            password,
            saved: None,
            pending: true,
            message: None,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let username = self.username.read(cx).value().to_string();
        let password = self.password.read(cx).value().to_string();
        if username.is_empty()
            || password.is_empty()
            || username.len() > 4096
            || password.len() > 4096
        {
            return;
        }
        let write = cx.write_credentials(&self.service, &username, password.as_bytes());
        self.pending = true;
        self.message = None;
        cx.notify();
        cx.spawn(async move |editor, cx| {
            let result = write.await;
            _ = editor.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(()) => {
                        this.saved = Some((username, password.into_bytes()));
                        this.message = Some("Saved in the OS credential store.".into());
                    }
                    Err(_) => {
                        this.message = Some("Could not save in the OS credential store.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fill(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some((username, password)) = &self.saved else {
            return;
        };
        let target = self.target.clone();
        let result = self.owner.update(cx, |panel, _| {
            panel.fill_saved_login(&target, username, password)
        });
        let Ok(Ok(receiver)) = result else {
            self.message = Some("The page changed. Open saved logins again.".into());
            cx.notify();
            return;
        };
        self.pending = true;
        self.message = None;
        cx.notify();
        cx.spawn_in(window, async move |editor, cx| {
            let result = receiver.recv().await;
            _ = editor.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(Ok(())) => {
                        window.close_dialog(cx);
                        _ = this
                            .owner
                            .update(cx, |panel, cx| panel.finish_dialog(window, cx));
                    }
                    Ok(Err(message)) => this.message = Some(message),
                    Err(_) => {
                        this.message = Some("The page closed before the form was filled.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Focusable for LoginEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.username.focus_handle(cx)
    }
}

impl Render for LoginEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let username = self.username.read(cx).value();
        let password = self.password.read(cx).value();
        let can_save = !self.pending
            && !username.is_empty()
            && !password.is_empty()
            && username.len() <= 4096
            && password.len() <= 4096;
        div().flex().flex_col().gap_3()
            .child(div().text_sm().font_semibold().child(self.target.origin.clone()))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(match &self.saved {
                Some((username, _)) => format!("Saved login: {username}"),
                None if self.pending => "Reading the OS credential store…".into(),
                None => "No saved login for this website.".into(),
            }))
            .child(Input::new(&self.username).aria_label("Login username"))
            .child(Input::new(&self.password).content_type(InputContentType::Password).aria_label("Login password"))
            .when_some(self.message.clone(), |body, message| body.child(div().text_sm().child(message)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Save a login in your OS credential store, or fill this page using the saved login. Filling never submits the form."))
            .child(div().flex().justify_end().gap_2()
                .child(Button::new("login-save").outline().label("Save login").disabled(!can_save).on_click(cx.listener(|this, _, _, cx| this.save(cx))))
                .child(Button::new("login-fill").primary().label("Fill saved login").disabled(self.pending || self.saved.is_none()).on_click(cx.listener(|this, _, window, cx| this.fill(window, cx)))))
    }
}
