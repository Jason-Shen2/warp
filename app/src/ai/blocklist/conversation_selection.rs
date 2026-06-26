//! Generic next-prompt conversation-selection behavior.

use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};

use super::agent_view::{AgentViewDisplayMode, AgentViewEntryOrigin, EnterAgentViewError};
use super::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::ai::agent::conversation::{
    AIConversation, AIConversationAutoexecuteMode, AIConversationId,
};

/// Handle to a terminal surface's conversation-selection implementation.
pub(crate) type ConversationSelectionHandle = ModelHandle<Box<dyn ConversationSelection>>;

/// The conversation targeted by the next query from a terminal surface.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PendingQueryState {
    /// The next query will continue an existing conversation.
    Existing { conversation_id: AIConversationId },
    New {
        /// Autoexecute override for the new conversation to be started.
        autoexecute_override: AIConversationAutoexecuteMode,
    },
}

impl Default for PendingQueryState {
    fn default() -> Self {
        Self::New {
            autoexecute_override: AIConversationAutoexecuteMode::default(),
        }
    }
}

/// Events emitted by a surface's conversation-selection implementation.
#[derive(Clone, Debug)]
pub enum ConversationSelectionEvent {
    Changed,
    AgentViewEntered {
        display_mode: AgentViewDisplayMode,
        origin: AgentViewEntryOrigin,
    },
    AgentViewExited {
        conversation_id: AIConversationId,
        final_exchange_count: usize,
        is_exit_before_new_entrance: bool,
    },
}

/// Object-safe next-prompt conversation-selection contract implemented by each terminal surface.
pub(crate) trait ConversationSelection {
    /// Returns the conversation targeted by the next query.
    fn selected_conversation_id(&self, app: &AppContext) -> Option<AIConversationId>;

    /// Returns whether this surface presents a selected conversation as active.
    fn is_agent_view_active(&self, app: &AppContext) -> bool;

    /// Returns whether this surface presents a selected conversation fullscreen.
    fn is_agent_view_fullscreen(&self, app: &AppContext) -> bool;

    /// Selects an existing conversation for the next query.
    fn select_existing_conversation(
        &mut self,
        conversation_id: AIConversationId,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Box<dyn ConversationSelection>>,
    );

    /// Selects the new-conversation state for the next query.
    fn select_new_conversation(
        &mut self,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Box<dyn ConversationSelection>>,
    );

    /// Starts and selects a new conversation for this surface.
    fn try_start_new_conversation(
        &mut self,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Box<dyn ConversationSelection>>,
    ) -> Result<AIConversationId, EnterAgentViewError>;

    /// Returns the autoexecute override for the pending query.
    fn pending_query_autoexecute_override(&self, app: &AppContext)
        -> AIConversationAutoexecuteMode;

    /// Toggles the autoexecute override for the pending query.
    fn toggle_pending_query_autoexecute(
        &mut self,
        ctx: &mut ModelContext<Box<dyn ConversationSelection>>,
    );

    /// Reconciles selection after a terminal-surface-scoped history event.
    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Box<dyn ConversationSelection>>,
    );

    /// Returns the selected conversation, if it is loaded.
    fn selected_conversation<'a>(&self, app: &'a AppContext) -> Option<&'a AIConversation> {
        self.selected_conversation_id(app)
            .as_ref()
            .and_then(|conversation_id| {
                BlocklistAIHistoryModel::as_ref(app).conversation(conversation_id)
            })
    }
}

impl Entity for Box<dyn ConversationSelection> {
    type Event = ConversationSelectionEvent;
}
