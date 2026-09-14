#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputFocus {
    #[default]
    Terminal,
    Sidebar,
    Find,
}

impl InputFocus {
    #[must_use]
    pub const fn terminal_owns_input(self) -> bool {
        matches!(self, Self::Terminal)
    }
}
