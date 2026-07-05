use std::{
    fs::File,
    io::Write,
    time::{Duration, Instant},
};

use crate::{app_actions::MuxKeyAction, layout::SplitDirection, mux::command::MuxDirection};

#[derive(Clone, Copy, Debug)]
pub(super) enum DiagnosticAction {
    NewTab,
    NextTab,
    PreviousTab,
    SplitRight,
    SplitDown,
    SelectPaneLeft,
    SelectPaneRight,
    SelectPaneUp,
    SelectPaneDown,
    NextPane,
}

impl DiagnosticAction {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "new_tab" => Some(Self::NewTab),
            "next_tab" => Some(Self::NextTab),
            "previous_tab" => Some(Self::PreviousTab),
            "split_right" => Some(Self::SplitRight),
            "split_down" => Some(Self::SplitDown),
            "select_pane_left" => Some(Self::SelectPaneLeft),
            "select_pane_right" => Some(Self::SelectPaneRight),
            "select_pane_up" => Some(Self::SelectPaneUp),
            "select_pane_down" => Some(Self::SelectPaneDown),
            "next_pane" => Some(Self::NextPane),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NewTab => "new_tab",
            Self::NextTab => "next_tab",
            Self::PreviousTab => "previous_tab",
            Self::SplitRight => "split_right",
            Self::SplitDown => "split_down",
            Self::SelectPaneLeft => "select_pane_left",
            Self::SelectPaneRight => "select_pane_right",
            Self::SelectPaneUp => "select_pane_up",
            Self::SelectPaneDown => "select_pane_down",
            Self::NextPane => "next_pane",
        }
    }

    pub(super) fn mux_action(self) -> MuxKeyAction {
        match self {
            Self::NewTab => MuxKeyAction::NewTab,
            Self::NextTab => MuxKeyAction::NextTab,
            Self::PreviousTab => MuxKeyAction::PreviousTab,
            Self::SplitRight => MuxKeyAction::SplitPane(SplitDirection::Right),
            Self::SplitDown => MuxKeyAction::SplitPane(SplitDirection::Down),
            Self::SelectPaneLeft => MuxKeyAction::SelectPane(MuxDirection::Left),
            Self::SelectPaneRight => MuxKeyAction::SelectPane(MuxDirection::Right),
            Self::SelectPaneUp => MuxKeyAction::SelectPane(MuxDirection::Up),
            Self::SelectPaneDown => MuxKeyAction::SelectPane(MuxDirection::Down),
            Self::NextPane => MuxKeyAction::NextPane,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DiagnosticStep {
    at: Duration,
    action: DiagnosticAction,
}

pub(super) struct DiagnosticRecord<'a> {
    pub(super) phase: &'a str,
    pub(super) action: DiagnosticAction,
    pub(super) action_elapsed_us: u128,
    pub(super) selected_session: Option<&'a str>,
    pub(super) selected_window: Option<&'a str>,
    pub(super) pane_count: usize,
    pub(super) last_error: Option<&'a str>,
}

pub(super) struct DiagnosticActionDriver {
    started_at: Instant,
    steps: Vec<DiagnosticStep>,
    next_step: usize,
    trace: Option<File>,
}

impl DiagnosticActionDriver {
    pub(super) fn from_env() -> Option<Self> {
        let script = std::env::var("BOOTTY_DIAGNOSTIC_ACTIONS").ok()?;
        let mut steps = Vec::new();
        for raw in script.split(',') {
            let (at, action) = raw.split_once(':')?;
            let at = at.trim().parse::<u64>().ok()?;
            let action = DiagnosticAction::parse(action)?;
            steps.push(DiagnosticStep {
                at: Duration::from_millis(at),
                action,
            });
        }
        if steps.is_empty() {
            return None;
        }
        steps.sort_by_key(|step| step.at);
        let mut trace =
            std::env::var_os("BOOTTY_DIAGNOSTIC_TRACE").and_then(|path| File::create(path).ok());
        if let Some(trace) = &mut trace {
            let _ = writeln!(
                trace,
                "elapsed_ms,phase,action,action_elapsed_us,selected_session,selected_window,pane_count,last_error"
            );
        }
        Some(Self {
            started_at: Instant::now(),
            steps,
            next_step: 0,
            trace,
        })
    }

    pub(super) fn due_actions(&mut self, now: Instant) -> Vec<DiagnosticAction> {
        let elapsed = now.duration_since(self.started_at);
        let mut actions = Vec::new();
        while let Some(step) = self.steps.get(self.next_step)
            && elapsed >= step.at
        {
            actions.push(step.action);
            self.next_step += 1;
        }
        actions
    }

    pub(super) fn record(&mut self, record: DiagnosticRecord<'_>) {
        let Some(trace) = &mut self.trace else {
            return;
        };
        let _ = writeln!(
            trace,
            "{},{},{},{},{},{},{},{}",
            self.started_at.elapsed().as_millis(),
            record.phase,
            record.action.label(),
            record.action_elapsed_us,
            record.selected_session.unwrap_or(""),
            record.selected_window.unwrap_or(""),
            record.pane_count,
            record.last_error.unwrap_or("")
        );
    }
}
