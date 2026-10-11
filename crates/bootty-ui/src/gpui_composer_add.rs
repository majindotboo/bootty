//! The Add menu uses the same captured catalog and input owner as keyboard completions.
use super::*;
use gpui_kit::component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
};
use gpui_kit::{EntityInputHandler as _, WeakEntity, assets::IconName};

type ChooseFiles = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct ComposerFileActions {
    editor: Entity<TextareaState>,
    choose: ChooseFiles,
}
impl ComposerFileActions {
    pub(crate) fn new(
        editor: &Entity<TextareaState>,
        choose: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            editor: editor.clone(),
            choose: std::rc::Rc::new(choose),
        }
    }
}

#[derive(Clone)]
pub enum ComposerAddAction {
    Plan(bool),
    Goal(String),
}
impl EventEmitter<ComposerAddAction> for ComposerCompletion {}

impl ComposerCompletion {
    pub(crate) fn add_button(
        owner: Option<&Entity<Self>>,
        id: &'static str,
        disabled: bool,
        plan: bool,
        draft_goal: Option<String>,
        files: ComposerFileActions,
    ) -> gpui_kit::AnyElement {
        let button = Button::new(id)
            .debug_selector(move || id.into())
            .icon(IconName::Plus)
            .small()
            .ghost()
            .accessibility_label("Add to message")
            .tooltip("Add to message")
            .when(plan, |button| button.label("Plan"))
            .disabled(disabled);
        let Some(owner) = owner else {
            return button
                .dropdown_menu(move |menu, window, _| file_menu(menu, &files, window))
                .anchor(gpui_kit::Anchor::BottomLeft)
                .into_any_element();
        };
        let owner = owner.downgrade();
        button
            .dropdown_menu(move |menu, window, cx| {
                let popup = cx.weak_entity();
                let catalog = owner
                    .update(cx, |this, cx| {
                        this.plan_mode = plan;
                        this.draft_goal.clone_from(&draft_goal);
                        this.add_menus.retain(|(menu, _)| menu.upgrade().is_some());
                        this.add_menus.push((popup, files.clone()));
                        this.load_add_catalog(window, cx);
                        (
                            this.catalog.clone(),
                            this.applications.clone(),
                            this.error.is_some(),
                            this.plan_mode,
                            this.draft_goal.clone(),
                        )
                    })
                    .ok();
                build_menu(menu, &owner, catalog, &files, window, cx)
            })
            .anchor(gpui_kit::Anchor::BottomLeft)
            .into_any_element()
    }

    fn load_add_catalog(&mut self, window: &Window, cx: &Context<Self>) {
        self.error = None;
        if !self.pending.contains_key("catalog") {
            // Each opening refreshes account capabilities; installed apps can change externally.
            self.catalog = None;
            self.fetch(
                self.scope.catalog.clone(),
                "catalog",
                self.request,
                window,
                cx,
            );
        }
        if self.scope.applications
            && cfg!(target_os = "macos")
            && !self.pending.contains_key("applications")
        {
            self.applications = None;
            self.fetch(
                CommandInvocation::new("computer.targets", Vec::new(), Caller::Internal),
                "applications",
                self.request,
                window,
                cx,
            );
        }
    }

    pub(super) fn refresh_add_menus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let catalog = Some((
            self.catalog.clone(),
            self.applications.clone(),
            self.error.is_some(),
            self.plan_mode,
            self.draft_goal.clone(),
        ));
        self.add_menus.retain(|(popup, files)| {
            let Some(popup) = popup.upgrade() else {
                return false;
            };
            popup.update(cx, |menu, cx| {
                menu.rebuild(window, cx, |menu, window, cx| {
                    build_menu(menu, &owner, catalog.clone(), files, window, cx)
                });
            });
            true
        });
    }

    fn add_provider(
        &mut self,
        option: &NativeCompletionOption,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismissed = None;
        self.trigger = None;
        let token = if option.kind == NativeCompletionKind::Skill {
            InlineToken::new(
                format!("skill:{}", option.name),
                format!("${}", option.name),
            )
        } else {
            let id = format!("provider:{}", option.path.as_deref().unwrap_or_default());
            self.provider_mentions.insert(id.clone(), option.clone());
            InlineToken::new(id, format!("@{}", option.display_name))
        };
        self.editor.update(cx, |input, cx| {
            if let Err(error) = input.replace_with_token(token, window, cx) {
                self.error = Some(error.to_string());
            }
            input.focus_handle(cx).focus(window, cx);
        });
        cx.notify();
    }
}

type AddCatalog = Option<(
    Option<NativeCompletionCatalog>,
    Option<Vec<bootty_computer::ComputerTarget>>,
    bool,
    bool,
    Option<String>,
)>;

fn build_menu(
    mut menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    state: AddCatalog,
    files: &ComposerFileActions,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = file_menu(menu, files, window);
    let Some((catalog, applications, failed, plan, draft_goal)) = state else {
        return menu;
    };
    menu = application_menu(menu, owner, applications, window, cx);
    if let Some(catalog) = &catalog {
        menu = task_menu(menu, owner, catalog, plan, draft_goal.as_deref());
        menu = provider_menu(menu, owner, catalog);
    } else {
        menu = menu.separator().item(
            PopupMenuItem::new(if failed {
                "Couldn’t load provider actions"
            } else {
                "Loading provider actions…"
            })
            .disabled(true),
        );
    }
    if failed
        || catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.unavailable.is_empty())
    {
        let app = owner.clone();
        menu = menu.item(
            PopupMenuItem::new("Refresh provider actions")
                .icon(IconName::RefreshCw)
                .on_click(move |_, window, cx| {
                    _ = app.update(cx, |this, cx| this.load_add_catalog(window, cx));
                }),
        );
    }
    menu
}

fn file_menu(mut menu: PopupMenu, actions: &ComposerFileActions, window: &Window) -> PopupMenu {
    let files = actions.choose.clone();
    let reference = actions.editor.clone();
    let paste = actions.editor.clone();
    menu = menu
        .min_w(gpui_kit::px(f32::from(window.rem_size()).mul_add(22., 0.)))
        .max_w(gpui_kit::px(f32::from(window.rem_size()).mul_add(32., 0.)))
        .max_h(gpui_kit::px(f32::from(window.rem_size()).mul_add(28., 0.)))
        .scrollable(true)
        .label("Add")
        .item(
            PopupMenuItem::new("Files and folders…")
                .icon(IconName::Paperclip)
                .on_click(move |_, window, cx| files(window, cx)),
        )
        .item(
            PopupMenuItem::new("Reference files and folders…")
                .icon(IconName::Folder)
                .on_click(move |_, window, cx| {
                    reference.update(cx, |input, cx| {
                        input.replace_text_in_range(None, "@", window, cx);
                        input.focus_handle(cx).focus(window, cx);
                    });
                }),
        )
        .item(
            PopupMenuItem::new("Paste image or text")
                .icon(IconName::Clipboard)
                .on_click(move |_, window, cx| {
                    paste.update(cx, |input, cx| {
                        input.focus_handle(cx).focus(window, cx);
                        window.dispatch_action(Box::new(gpui_kit::component::input::Paste), cx);
                    });
                }),
        );
    menu
}

fn application_menu(
    menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    applications: Option<Vec<bootty_computer::ComputerTarget>>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(applications) = applications.filter(|apps| !apps.is_empty()) else {
        return menu;
    };
    let owner = owner.clone();
    menu.separator().submenu(
        "Apps on this computer",
        window,
        cx,
        move |mut menu, _, _| {
            for target in &applications {
                let app = owner.clone();
                let target = target.clone();
                let name = super::application_name(&target);
                let label = format!("Attach {name}");
                menu = menu.item(PopupMenuItem::new(label).icon(IconName::Monitor).on_click(
                    move |_, _, cx| {
                        _ = app.update(cx, |this, cx| {
                            let range = this.editor.read(cx).selected_range();
                            let original = this
                                .editor
                                .read(cx)
                                .value()
                                .get(range.clone())
                                .unwrap_or_default()
                                .to_owned();
                            cx.emit(CompletionSelection {
                                source: CompletionSource::Application(target.clone()),
                                range,
                                original,
                            });
                        });
                    },
                ));
            }
            menu
        },
    )
}

fn task_menu(
    mut menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    catalog: &NativeCompletionCatalog,
    plan: bool,
    draft_goal: Option<&str>,
) -> PopupMenu {
    if catalog.plan_mode {
        let app = owner.clone();
        menu = menu.separator().item(
            PopupMenuItem::new("Plan mode")
                .icon(IconName::Lightbulb)
                .checked(plan)
                .on_click(move |_, _, cx| {
                    _ = app.update(cx, |_, cx| cx.emit(ComposerAddAction::Plan(!plan)));
                }),
        );
    }
    if catalog.goals {
        let app = owner.clone();
        let goal = draft_goal
            .map(str::to_owned)
            .or_else(|| catalog.goal.clone());
        let initial = goal.clone().unwrap_or_default();
        menu = menu.item(
            PopupMenuItem::new(if goal.as_ref().is_some_and(|goal| !goal.is_empty()) {
                "Edit goal…"
            } else {
                "Goal…"
            })
            .icon(IconName::Target)
            .on_click(move |_, window, cx| {
                use gpui_kit::component::{
                    WindowExt as _,
                    dialog::DialogFooter,
                    input::{Input, InputState},
                };
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder("What should the agent keep pursuing?")
                });
                input.update(cx, |input, cx| input.set_value(initial.clone(), window, cx));
                let owner = app.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    let input = input.clone();
                    let owner = owner.clone();
                    let goal_input = input.clone();
                    let submit = move |cx: &mut App| {
                        let objective = goal_input.read(cx).value().trim().to_owned();
                        if objective.is_empty() || objective.len() > 8192 {
                            return false;
                        }
                        owner
                            .update(cx, |_, cx| cx.emit(ComposerAddAction::Goal(objective)))
                            .is_ok()
                    };
                    let confirm = submit.clone();
                    dialog
                        .title("Conversation goal")
                        .child(Input::new(&input))
                        .footer(
                            DialogFooter::new()
                                .child(
                                    Button::new("cancel-conversation-goal")
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("set-conversation-goal")
                                        .label("Set goal")
                                        .primary()
                                        .on_click(move |_, window, cx| {
                                            if submit(cx) {
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        )
                        .on_ok(move |_, _, cx| confirm(cx))
                });
            }),
        );
        if goal.as_ref().is_some_and(|goal| !goal.is_empty()) {
            let app = owner.clone();
            menu = menu.item(
                PopupMenuItem::new("Remove goal")
                    .icon(IconName::X)
                    .on_click(move |_, _, cx| {
                        _ = app.update(cx, |_, cx| cx.emit(ComposerAddAction::Goal(String::new())));
                    }),
            );
        }
    }
    menu
}

fn provider_menu(
    mut menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    catalog: &NativeCompletionCatalog,
) -> PopupMenu {
    for (kind, title, icon) in [
        (NativeCompletionKind::App, "Apps", "app-window"),
        (NativeCompletionKind::Plugin, "Plugins", "plug"),
        (NativeCompletionKind::Skill, "Skills", "book-open"),
    ] {
        let options: Vec<_> = catalog
            .options
            .iter()
            .filter(|option| option.kind == kind)
            .collect();
        if !options.is_empty() {
            menu = menu.separator().label(title);
            for option in options {
                let app = owner.clone();
                let selection = option.clone();
                let label = option.display_name.clone();
                let description = option.description.clone();
                let icon_url = option.icon_url.clone();
                let key = gpui_kit::SharedString::from(format!(
                    "composer-add-{:?}-{}",
                    kind, option.name
                ));
                menu = menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        div()
                            .id(key.clone())
                            .role(gpui_kit::Role::MenuItem)
                            .aria_label(label.clone())
                            .flex()
                            .items_center()
                            .gap_1()
                            // Paint artwork in the icon slot reserved by PopupMenu.
                            .ml_neg_4()
                            .w_full()
                            .min_w_0()
                            .child(
                                if matches!(
                                    kind,
                                    NativeCompletionKind::App | NativeCompletionKind::Plugin
                                ) {
                                    gpui_kit::component::avatar::Avatar::new()
                                        .name(label.chars().take(1).collect::<String>())
                                        .xsmall()
                                        .size_3()
                                        .when_some(
                                            icon_url.clone(),
                                            gpui_kit::component::avatar::Avatar::src,
                                        )
                                        .into_any_element()
                                } else {
                                    crate::gpui::sized_icon(
                                        icon,
                                        crate::gpui::IconSize::XSmall,
                                        cx.theme().muted_foreground,
                                    )
                                },
                            )
                            .child(div().flex_shrink_0().child(label.clone()))
                            .when_some(description.clone(), |row, text| {
                                row.child(
                                    div()
                                        .ml_1()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(text),
                                )
                            })
                    })
                    .on_click(move |_, window, cx| {
                        _ = app.update(cx, |this, cx| this.add_provider(&selection, window, cx));
                    }),
                );
            }
        }
        if catalog.unavailable.contains(&kind) {
            menu = menu.item(
                PopupMenuItem::new(format!("Couldn’t load {}", title.to_lowercase()))
                    .disabled(true),
            );
        }
    }
    menu
}
