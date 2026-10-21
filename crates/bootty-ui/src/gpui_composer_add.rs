//! The Add menu uses the same captured catalog and input owner as keyboard completions.
use super::*;
use crate::gpui::ElementTooltip as _;
use gpui_kit::component::{
    Disableable as _,
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
            .ghost()
            .accessibility_label("Add to message")
            .element_tooltip("Add to message")
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
                        this.add_menu_state()
                    })
                    .ok();
                build_menu(menu, &owner, catalog, &files, window, cx)
            })
            .anchor(gpui_kit::Anchor::BottomLeft)
            .into_any_element()
    }

    /// Each opening refreshes account capabilities and open apps. The last results stay on
    /// screen until fresh ones arrive, so the menu keeps its shape instead of popping in.
    pub(super) fn load_add_catalog(&mut self, window: &Window, cx: &Context<Self>) {
        self.error = None;
        if !self.pending.contains_key("catalog") {
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
            self.fetch(
                CommandInvocation::new("computer.targets", Vec::new(), Caller::Internal),
                "applications",
                self.request,
                window,
                cx,
            );
        }
    }

    fn add_menu_state(&self) -> AddMenuState {
        AddMenuState {
            catalog: self.catalog.clone(),
            applications: if !self.scope.applications || !cfg!(target_os = "macos") {
                AddApplications::Unsupported
            } else {
                self.applications
                    .clone()
                    .map_or(AddApplications::Loading, AddApplications::Loaded)
            },
            failed: self.error.is_some(),
            plan: self.plan_mode,
            draft_goal: self.draft_goal.clone(),
        }
    }

    pub(super) fn refresh_add_menus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let catalog = Some(self.add_menu_state());
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
        if option.kind == NativeCompletionKind::Command {
            self.editor.update(cx, |input, cx| {
                input.replace(format!("/{} ", option.name), window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            cx.notify();
            return;
        }
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

#[derive(Clone)]
struct AddMenuState {
    catalog: Option<NativeCompletionCatalog>,
    applications: AddApplications,
    failed: bool,
    plan: bool,
    draft_goal: Option<String>,
}

#[derive(Clone)]
enum AddApplications {
    Unsupported,
    Loading,
    Loaded(Vec<bootty_computer::ComputerTarget>),
}

fn build_menu(
    mut menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    state: Option<AddMenuState>,
    files: &ComposerFileActions,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = file_menu(menu, files, window);
    let Some(AddMenuState {
        catalog,
        applications,
        failed,
        plan,
        draft_goal,
    }) = state
    else {
        return menu;
    };
    menu = application_menu(menu, owner, applications, window, cx);
    if let Some(catalog) = &catalog {
        menu = task_menu(menu, owner, catalog, plan, draft_goal.as_deref());
        menu = provider_menu(menu, owner, catalog, window, cx);
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
    applications: AddApplications,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if matches!(applications, AddApplications::Unsupported) {
        return menu;
    }
    let owner = owner.clone();
    menu.separator().submenu_with_icon(
        Some(gpui_kit::component::Icon::new(IconName::Monitor)),
        "Apps on this computer",
        window,
        cx,
        move |menu, window, _| {
            let mut menu = bounded_submenu(menu, window);
            let targets = match &applications {
                AddApplications::Loaded(targets) if !targets.is_empty() => targets,
                AddApplications::Loaded(_) => {
                    return menu.item(PopupMenuItem::new("No open app windows").disabled(true));
                }
                _ => return menu.item(PopupMenuItem::new("Loading apps…").disabled(true)),
            };
            for target in targets {
                menu = menu.item(application_item(&owner, target));
            }
            menu
        },
    )
}

/// Long lists scroll inside the window instead of running off-screen.
fn bounded_submenu(menu: PopupMenu, window: &Window) -> PopupMenu {
    menu.max_h(gpui_kit::px(f32::from(window.rem_size()).mul_add(24., 0.)))
        .scrollable(true)
}

/// The app's icon and name, with the window title so several windows of one app stay distinct.
fn application_item(
    owner: &WeakEntity<ComposerCompletion>,
    target: &bootty_computer::ComputerTarget,
) -> PopupMenuItem {
    let app = owner.clone();
    let selection = target.clone();
    let name = super::application_name(target).to_owned();
    let bundle_id = target.bundle_id.clone();
    let title = target
        .title
        .clone()
        .filter(|title| !title.trim().is_empty());
    let key = gpui_kit::SharedString::from(format!(
        "composer-add-app-{}-{}",
        target.process_id, target.window_id
    ));
    PopupMenuItem::element(move |_, cx| {
        div()
            .id(key.clone())
            .role(gpui_kit::Role::MenuItem)
            .aria_label(name.clone())
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(crate::gpui_application_icons::render(
                &bundle_id,
                crate::gpui::IconSize::XSmall,
                cx.theme().muted_foreground,
                cx,
            ))
            .child(div().flex_shrink_0().child(name.clone()))
            .when_some(title.clone(), |row, title| {
                row.child(
                    div()
                        .ml_1()
                        .min_w_0()
                        .text_ellipsis()
                        .text_color(cx.theme().muted_foreground)
                        .child(title),
                )
            })
    })
    .on_click(move |_, _, cx| {
        _ = app.update(cx, |this, cx| {
            let range = this.editor.read(cx).selected_range();
            cx.emit(CompletionSelection {
                source: CompletionSource::Application(selection.clone()),
                range,
            });
        });
    })
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
                        .child(Input::new(&input).context_menu(crate::gpui::text_menu(&input)))
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

/// Provider entries use submenus to keep the root menu short.
fn provider_menu(
    mut menu: PopupMenu,
    owner: &WeakEntity<ComposerCompletion>,
    catalog: &NativeCompletionCatalog,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if !catalog.options.is_empty() || !catalog.unavailable.is_empty() {
        menu = menu.separator();
    }
    for (kind, title, icon) in [
        (
            NativeCompletionKind::Command,
            "Commands",
            IconName::SquareTerminal,
        ),
        (NativeCompletionKind::App, "Connected apps", IconName::Globe),
        (NativeCompletionKind::Plugin, "Plugins", IconName::Wrench),
        (NativeCompletionKind::Skill, "Skills", IconName::FileText),
    ] {
        let options: Vec<NativeCompletionOption> = catalog
            .options
            .iter()
            .filter(|option| option.kind == kind)
            .cloned()
            .collect();
        if !options.is_empty() {
            let owner = owner.clone();
            menu = menu.submenu_with_icon(
                Some(gpui_kit::component::Icon::new(icon)),
                title,
                window,
                cx,
                move |menu, window, _| {
                    let mut menu = bounded_submenu(menu, window);
                    for option in &options {
                        menu = menu.item(provider_item(&owner, option));
                    }
                    menu
                },
            );
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

/// The provider's artwork, or a kind glyph, with the name and a muted description.
fn provider_item(
    owner: &WeakEntity<ComposerCompletion>,
    option: &NativeCompletionOption,
) -> PopupMenuItem {
    let app = owner.clone();
    let selection = option.clone();
    let label = option.display_name.clone();
    let description = option.description.clone();
    let artwork = option.icon_url.clone();
    let glyph = match option.kind {
        NativeCompletionKind::App => "app-window",
        NativeCompletionKind::Plugin => "plug",
        NativeCompletionKind::Command => "square-terminal",
        NativeCompletionKind::Skill => "book-open",
    };
    let key =
        gpui_kit::SharedString::from(format!("composer-add-{:?}-{}", option.kind, option.name));
    PopupMenuItem::element(move |_, cx| {
        div()
            .id(key.clone())
            .debug_selector({
                let key = key.clone();
                move || key.to_string()
            })
            .role(gpui_kit::Role::MenuItem)
            .aria_label(label.clone())
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .max_w(gpui_kit::rems(28.))
            .child(artwork.as_deref().map_or_else(
                || {
                    crate::gpui::sized_icon(
                        glyph,
                        crate::gpui::IconSize::XSmall,
                        cx.theme().muted_foreground,
                    )
                },
                |url| {
                    crate::gpui_application_icons::render(
                        url,
                        crate::gpui::IconSize::XSmall,
                        cx.theme().muted_foreground,
                        cx,
                    )
                },
            ))
            .child(div().flex_shrink_0().child(label.clone()))
            .when_some(description.clone(), |row, text| {
                row.child(
                    div()
                        .min_w_0()
                        .text_ellipsis()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
            })
    })
    .on_click(move |_, window, cx| {
        _ = app.update(cx, |this, cx| this.add_provider(&selection, window, cx));
    })
}
