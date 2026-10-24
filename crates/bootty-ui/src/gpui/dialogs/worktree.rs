//! Searchable checkout and base-ref selection using Kit's retained select behavior.
use super::*;
use gpui_kit::component::{
    searchable_list::{SearchableListDelegate, SearchableListItem},
    select::{Select, SelectEvent, SelectState},
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct RefChoice {
    value: String,
    custom: bool,
    base_ref: bool,
    badge: Option<&'static str>,
}

impl SearchableListItem for RefChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        if self.value == "HEAD" {
            "Current revision".into()
        } else {
            self.value.clone().into()
        }
    }
    fn value(&self) -> &String {
        &self.value
    }
    fn display_title(&self) -> Option<gpui_kit::AnyElement> {
        Some(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::GitBranch).small(),
                )
                .child(div().min_w_0().text_ellipsis().child(if self.base_ref {
                    SharedString::from(format!("From {}", self.title()))
                } else {
                    self.title()
                }))
                .into_any_element(),
        )
    }
    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .id(SharedString::from(format!("branch-choice-{}", self.value)))
            .role(gpui_kit::Role::Label)
            .aria_label(self.badge.map_or_else(
                || self.title(),
                |badge| format!("{}, {badge}", self.title()).into(),
            ))
            .w_full()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(div().flex_1().min_w_0().text_ellipsis().child(self.title()))
            .when_some(self.badge, |row, badge| {
                row.child(
                    div()
                        .debug_selector({
                            let name = self.value.clone();
                            move || format!("branch-badge-{name}-{badge}")
                        })
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(badge),
                )
            })
            .when(self.custom, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Use ref"),
                )
            })
    }
}

pub(super) struct RefChoices {
    all: Vec<RefChoice>,
    matches: Vec<RefChoice>,
    custom_refs: bool,
}

impl RefChoices {
    fn new(
        values: &[String],
        custom_refs: bool,
        branches: &[bootty_git::changes::GitBranch],
        project: Option<&str>,
    ) -> Self {
        let all = values
            .iter()
            .map(|value| RefChoice {
                value: value.clone(),
                custom: false,
                base_ref: custom_refs,
                badge: branches
                    .iter()
                    .find(|branch| branch.name == *value)
                    .and_then(|branch| {
                        if branch.current {
                            Some("current")
                        } else if branch
                            .worktree_path
                            .as_deref()
                            .zip(project)
                            .is_some_and(|(path, project)| path != project)
                        {
                            Some("worktree")
                        } else {
                            None
                        }
                    }),
            })
            .collect::<Vec<_>>();
        Self {
            matches: all.clone(),
            all,
            custom_refs,
        }
    }
}

impl SearchableListDelegate for RefChoices {
    type Item = RefChoice;
    fn render_item(
        &self,
        _: IndexPath,
        item: &RefChoice,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui_kit::AnyElement> {
        Some(item.render(window, cx).into_any_element())
    }
    fn items_count(&self, _: usize) -> usize {
        self.matches.len()
    }
    fn item(&self, ix: IndexPath) -> Option<&RefChoice> {
        self.matches.get(ix.row)
    }
    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        RefChoice: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.matches
            .iter()
            .position(|item| item.value() == value)
            .map(IndexPath::new)
    }
    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> gpui_kit::Task<()> {
        self.matches = self
            .all
            .iter()
            .filter(|item| item.matches(query))
            .cloned()
            .collect();
        let query = query.trim();
        if self.custom_refs && !query.is_empty() && !self.all.iter().any(|item| item.value == query)
        {
            self.matches.push(RefChoice {
                value: query.to_owned(),
                custom: true,
                base_ref: true,
                badge: None,
            });
        }
        gpui_kit::Task::ready(())
    }
}

pub(super) struct WorktreeRefPicker {
    state: Entity<SelectState<RefChoices>>,
    choices: Vec<RefChoice>,
    value: String,
    field: String,
    _subscription: Subscription,
}

impl DialogView {
    pub(super) fn sync_worktree_ref_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.spec.as_ref() else {
            self.worktree_ref_picker = None;
            return;
        };
        let Some(field) = spec
            .fields
            .iter()
            .find(|field| matches!(field.id.as_str(), "start-ref" | "checkout-branch"))
        else {
            self.worktree_ref_picker = None;
            return;
        };
        let DialogFieldKind::Choice(options) = &field.kind else {
            return;
        };
        let mut values = options.clone();
        let custom_refs = field.id == "start-ref";
        if self
            .worktree_ref_picker
            .as_ref()
            .is_some_and(|picker| picker.field != field.id)
        {
            self.worktree_ref_picker = None;
        }
        if !field.value.is_empty() && !values.contains(&field.value) {
            values.insert(0, field.value.clone());
        }
        let items = RefChoices::new(
            &values,
            custom_refs,
            &spec.branches,
            spec.selected_project.as_deref(),
        );
        let choices = items.all.clone();
        if let Some(picker) = &mut self.worktree_ref_picker {
            if picker.choices != choices || picker.value != field.value {
                picker.state.update(cx, |state, cx| {
                    state.set_items(items, window, cx);
                    state.set_selected_value(&field.value, window, cx);
                });
                picker.choices = choices;
                picker.value.clone_from(&field.value);
            }
            return;
        }
        let selected = values
            .iter()
            .position(|value| value == &field.value)
            .map(IndexPath::new);
        let state = cx.new(|cx| SelectState::new(items, selected, window, cx).searchable(true));
        let field_id = field.id.clone();
        let subscription = cx.subscribe(&state, move |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(value)) = event
                && let Some(spec) = &this.spec
            {
                // Kit has applied the request locally; the next owner snapshot may reject it.
                if let Some(picker) = &mut this.worktree_ref_picker {
                    picker.value.clone_from(value);
                }
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: field_id.clone(),
                    value: value.clone(),
                });
            }
        });
        self.worktree_ref_picker = Some(WorktreeRefPicker {
            state,
            choices,
            value: field.value.clone(),
            field: field.id.clone(),
            _subscription: subscription,
        });
    }

    pub(super) fn render_worktree_ref_picker(
        &self,
        spec: &DialogSpec,
        field: &DialogField,
        compact: bool,
        _cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let picker = self.worktree_ref_picker.as_ref()?;
        let checkout = field.id == "checkout-branch";
        let control = public_selector(
            if checkout {
                "dialog-checkout-branch"
            } else {
                "dialog-worktree-base-branch"
            },
            Select::new(&picker.state)
                .w_full()
                .placeholder(field.placeholder.clone())
                .accessibility_label(field.label.clone())
                .search_placeholder(if checkout {
                    "Search branches…"
                } else {
                    "Search branches, tags or commits…"
                })
                .menu_width(rems(24.))
                .menu_max_h(rems(18.))
                .when(compact || checkout, |select| {
                    select.small().appearance(false)
                })
                .disabled(spec.busy),
        );
        if checkout || compact {
            return Some(control);
        }
        Some(Self::prompt_control_row(
            spec,
            "dialog-field-start-ref",
            Some(&field.label),
            control,
        ))
    }
}
