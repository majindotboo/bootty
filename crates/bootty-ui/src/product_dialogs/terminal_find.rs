//! Host-neutral terminal find-bar state.

use bootty_terminal::terminal_search::TerminalSearchOptions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFindResult {
    pub found: bool,
    pub active_index: Option<usize>,
    pub match_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindDirection {
    Current,
    Previous,
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalFindIntent {
    SetQuery(String),
    ToggleRegex,
    ToggleCaseSensitive,
    Submit,
    SubmitPrevious,
    Previous,
    Next,
    FocusFind,
    FocusTerminal,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalFindOutput {
    Search {
        query: String,
        direction: FindDirection,
    },
    FocusFind,
    FocusTerminal,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFindModel {
    query: String,
    options: TerminalSearchOptions,
    error: Option<String>,
    result: Option<TerminalFindResult>,
    enter_direction: FindDirection,
}

impl TerminalFindModel {
    #[must_use]
    pub fn new(query: String) -> Self {
        Self::with_enter_direction(query, FindDirection::Next)
    }

    #[must_use]
    pub fn with_enter_direction(query: String, enter_direction: FindDirection) -> Self {
        Self {
            query,
            options: TerminalSearchOptions::default(),
            error: None,
            result: None,
            enter_direction,
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub const fn result(&self) -> Option<TerminalFindResult> {
        self.result
    }

    #[must_use]
    pub const fn options(&self) -> TerminalSearchOptions {
        self.options
    }

    pub const fn set_options(&mut self, options: TerminalSearchOptions) {
        self.options = options;
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    pub fn set_result(&mut self, result: TerminalFindResult) {
        self.error = None;
        self.result = Some(result);
    }

    #[must_use]
    pub fn count_text(&self) -> String {
        match self.result {
            Some(result) if result.match_count > 0 => {
                let index = result.active_index.unwrap_or(1);
                format!("{index}/{}", result.match_count)
            }
            Some(_) => "0/0".to_owned(),
            None => String::new(),
        }
    }

    pub fn apply(&mut self, intent: TerminalFindIntent) -> Option<TerminalFindOutput> {
        match intent {
            TerminalFindIntent::ToggleRegex => {
                self.options.regex = !self.options.regex;
                Some(self.search(FindDirection::Current))
            }
            TerminalFindIntent::ToggleCaseSensitive => {
                self.options.case_sensitive = !self.options.case_sensitive;
                Some(self.search(FindDirection::Current))
            }
            TerminalFindIntent::SetQuery(query) => {
                self.query = query;
                Some(self.search(FindDirection::Current))
            }
            TerminalFindIntent::Submit if !self.query.is_empty() => {
                Some(self.search(self.enter_direction))
            }
            TerminalFindIntent::SubmitPrevious | TerminalFindIntent::Previous => {
                (!self.query.is_empty()).then(|| self.search(FindDirection::Previous))
            }
            TerminalFindIntent::Next => {
                (!self.query.is_empty()).then(|| self.search(FindDirection::Next))
            }
            TerminalFindIntent::Submit => None,
            TerminalFindIntent::FocusFind => Some(TerminalFindOutput::FocusFind),
            TerminalFindIntent::FocusTerminal => Some(TerminalFindOutput::FocusTerminal),
            TerminalFindIntent::Close => Some(TerminalFindOutput::Close),
        }
    }

    fn search(&self, direction: FindDirection) -> TerminalFindOutput {
        TerminalFindOutput::Search {
            query: self.query.clone(),
            direction,
        }
    }
}
