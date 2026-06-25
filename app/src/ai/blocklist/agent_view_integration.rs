use warpui::ModelHandle;

use super::agent_view::AgentViewController;

/// Describes whether a surface delegates conversation UI state to Agent View.
#[derive(Clone)]
pub(super) enum AgentViewIntegration {
    Gui(ModelHandle<AgentViewController>),
    #[cfg_attr(not(any(test, feature = "tui")), allow(dead_code))]
    Headless,
}

impl AgentViewIntegration {
    /// Returns the GUI Agent View controller when this is a GUI surface.
    pub(super) fn controller(&self) -> Option<&ModelHandle<AgentViewController>> {
        match self {
            Self::Gui(controller) => Some(controller),
            Self::Headless => None,
        }
    }

    /// Returns whether the surface owns conversation selection without Agent View.
    pub(super) fn is_headless(&self) -> bool {
        matches!(self, Self::Headless)
    }
}
