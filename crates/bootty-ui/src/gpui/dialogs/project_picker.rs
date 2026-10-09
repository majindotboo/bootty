//! Registered projects stay in the composer while their anchored picker owns search and focus.
use super::*;
use gpui_kit::component::{
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProjectChoice(bootty_git::ProjectPickerEntry, String);

impl SearchableListItem for ProjectChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.1.clone().into()
    }

    fn value(&self) -> &String {
        &self.0.path
    }

    fn matches(&self, query: &str) -> bool {
        self.0.path.to_lowercase().contains(&query.to_lowercase())
            || self.1.to_lowercase().contains(&query.to_lowercase())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .id(SharedString::from(format!(
                "new-session-project:{}",
                self.0.path
            )))
            .role(gpui_kit::Role::ListBoxOption)
            .aria_label(self.title())
            .min_w_0()
            .gap_2()
            .items_center()
            .child(super::super::sized_icon(
                "folder",
                super::super::IconSize::Small,
                cx.theme().muted_foreground,
            ))
            .child(div().flex_1().min_w_0().text_ellipsis().child(self.title()))
    }
}

pub(super) struct NewProjectPicker {
    pub(super) state: Entity<SelectState<SearchableVec<ProjectChoice>>>,
    projects: Vec<ProjectChoice>,
    _subscription: Subscription,
}

impl DialogView {
    pub(super) fn sync_new_project_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        if !spec.rows.iter().any(|row| row.id.0 == "choose-project") {
            self.project_picker = None;
            return;
        }
        let projects = spec
            .projects
            .iter()
            .map(|project| {
                let label = spec
                    .project_labels
                    .get(&project.path)
                    .cloned()
                    .unwrap_or_else(|| {
                        project
                            .path
                            .trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(&project.path)
                            .to_owned()
                    });
                ProjectChoice(project.clone(), label)
            })
            .collect::<Vec<_>>();
        let selected = spec.selected_project.clone();
        if let Some(picker) = &mut self.project_picker {
            picker.state.update(cx, |state, cx| {
                if picker.projects != projects {
                    state.set_items(SearchableVec::new(projects.clone()), window, cx);
                }
                if let Some(selected) = &selected {
                    state.set_selected_value(selected, window, cx);
                }
            });
            picker.projects = projects;
            return;
        }
        let selected_index = projects
            .iter()
            .position(|project| Some(&project.0.path) == selected.as_ref())
            .map(IndexPath::new);
        let state = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(projects.clone()),
                selected_index,
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription = cx.subscribe_in(&state, window, |this, _, event, window, cx| {
            if let SelectEvent::Confirm(Some(value)) = event
                && let Some(spec) = &this.spec
            {
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: "project".into(),
                    value: value.clone(),
                });
                let dialog = spec.id.clone();
                // Select restores its trigger after Confirm; transfer focus after that.
                cx.defer_in(window, move |this, window, cx| {
                    if this.spec.as_ref().is_some_and(|spec| spec.id == dialog) {
                        this.prompt_textarea
                            .read(cx)
                            .focus_handle(cx)
                            .focus(window, cx);
                    }
                });
            }
        });
        self.project_picker = Some(NewProjectPicker {
            state,
            projects,
            _subscription: subscription,
        });
    }

    pub(super) fn render_new_project_picker(
        &self,
        spec: &DialogSpec,
        choice: &DialogRow,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(picker) = &self.project_picker else {
            return div().child(choice.label.clone()).into_any_element();
        };
        public_selector(
            "new-session-header-project",
            Select::new(&picker.state)
                .when(Self::is_agent_session_spec(spec), |select| {
                    select.text_xl().h_auto().text_color(cx.theme().foreground)
                })
                .appearance(false)
                .placeholder(choice.label.trim_end_matches('…').to_owned())
                .search_placeholder("Find project…")
                .accessibility_label("Project")
                .menu_width(rems(24.0))
                .menu_max_h(rems(18.0))
                .empty(|_, cx| {
                    div()
                        .p_3()
                        .text_color(cx.theme().muted_foreground)
                        .child("No registered projects")
                })
                .disabled(spec.busy || !choice.enabled),
        )
        .into_any_element()
    }
}
