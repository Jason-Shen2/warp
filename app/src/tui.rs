//! Headless TUI entry point and conversation-streaming smoke surface.

use std::any::Any;

use anyhow::anyhow;
use pathfinder_geometry::vector::Vector2F;
use warpui::elements::Empty;
use warpui::platform::{TerminationMode, WindowStyle};
use warpui::{
    AddWindowOptions, AppContext, Element, Entity, EntityId, ModelContext, ModelHandle,
    SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::AIAgentTextSection;
use crate::ai::blocklist::agent_view::AgentViewEntryOrigin;
use crate::ai::blocklist::{
    BlocklistAIActionModel, BlocklistAIContextEvent, BlocklistAIContextModel,
    BlocklistAIController, BlocklistAIHistoryEvent, BlocklistAIHistoryModel, BlocklistAIInputModel,
    ConversationStatusUpdate,
};
use crate::ai::get_relevant_files::controller::GetRelevantFilesController;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::AuthStateProvider;
use crate::banner::BannerState;
use crate::terminal::event::AfterBlockCompletedEvent;
use crate::terminal::local_tty::{
    TerminalManager as LocalTtyTerminalManager, TerminalSurfaceInit, TerminalSurfaceResult,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::terminal_model::BlockIndex;
use crate::terminal::shared_session::IsSharedSessionCreator;
use crate::terminal::{
    PtyIntent, PtyIntentEvent, ShellLaunchData, TerminalManager as TerminalManagerTrait,
    TerminalModel, TerminalSurface,
};

const PROMPT_ENV: &str = "WARP_TUI_PROMPT";
const CONVERSATION_ID_ENV: &str = "WARP_TUI_CONVERSATION_ID";

struct TuiHostView;

impl Entity for TuiHostView {
    type Event = ();
}

impl View for TuiHostView {
    fn ui_name() -> &'static str {
        "TuiHostView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for TuiHostView {
    type Action = ();
}

/// Events emitted by a TUI conversation model for presentation layers.
#[derive(Clone, Debug)]
enum TuiConversationModelEvent {
    SelectedConversationChanged {
        conversation_id: Option<AIConversationId>,
    },
    ConversationStarted {
        conversation_id: AIConversationId,
    },
    ConversationUpdated {
        conversation_id: AIConversationId,
    },
    ConversationStatusChanged {
        conversation_id: AIConversationId,
        status: ConversationStatus,
        update: ConversationStatusUpdate,
    },
    Error {
        message: String,
    },
}

/// Per-surface conversation/composer model for a future interactive TUI.
///
/// This model deliberately owns no transcript widgets. It coordinates selected
/// conversation state, conversation restore/create operations, prompt
/// submission, and history-backed stream events for one TUI surface.
struct TuiConversationModel {
    owner_id: EntityId,
    context_model: ModelHandle<BlocklistAIContextModel>,
    ai_controller: ModelHandle<BlocklistAIController>,
}

impl TuiConversationModel {
    /// Creates a TUI conversation model around the shared production AI models.
    fn new(
        owner_id: EntityId,
        context_model: ModelHandle<BlocklistAIContextModel>,
        ai_controller: ModelHandle<BlocklistAIController>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&context_model, |model, _, event, ctx| {
            if matches!(event, BlocklistAIContextEvent::PendingQueryStateUpdated) {
                ctx.emit(TuiConversationModelEvent::SelectedConversationChanged {
                    conversation_id: model.selected_conversation_id(ctx),
                });
            }
        });
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |model, _, event, ctx| model.handle_history_event(event, ctx),
        );
        Self {
            owner_id,
            context_model,
            ai_controller,
        }
    }

    /// Returns this surface's currently selected next-prompt target.
    fn selected_conversation_id(&self, ctx: &AppContext) -> Option<AIConversationId> {
        self.context_model.as_ref(ctx).selected_conversation_id(ctx)
    }

    /// Selects a live conversation as this surface's next-prompt target.
    fn select_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<()> {
        let is_live = BlocklistAIHistoryModel::as_ref(ctx)
            .all_live_conversations_for_owner(self.owner_id)
            .any(|conversation| conversation.id() == conversation_id);
        if !is_live {
            return Err(anyhow!(
                "Conversation {conversation_id} is not live for TUI owner {}",
                self.owner_id
            ));
        }
        self.context_model.update(ctx, |context_model, ctx| {
            context_model.set_pending_query_state_for_existing_conversation(
                conversation_id,
                AgentViewEntryOrigin::Cli,
                ctx,
            );
        });
        Ok(())
    }

    /// Creates and selects an empty conversation for this TUI surface.
    fn start_new_conversation(
        &mut self,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<AIConversationId> {
        self.context_model
            .update(ctx, |context_model, ctx| {
                context_model
                    .try_enter_agent_view_for_new_conversation(AgentViewEntryOrigin::Cli, ctx)
            })
            .map_err(Into::into)
    }

    /// Sends a prompt to this surface's selected conversation, creating one if needed.
    fn send_prompt(&mut self, prompt: String, ctx: &mut ModelContext<Self>) {
        let conversation_id = match self.selected_conversation_id(ctx) {
            Some(conversation_id) => conversation_id,
            None => match self.start_new_conversation(ctx) {
                Ok(conversation_id) => conversation_id,
                Err(error) => {
                    self.emit_error(error, ctx);
                    return;
                }
            },
        };
        self.send_prompt_to_selected(prompt, conversation_id, ctx);
    }

    /// Restores, selects, and sends a prompt to an existing conversation.
    fn send_prompt_to_conversation(
        &mut self,
        prompt: String,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let history = BlocklistAIHistoryModel::handle(ctx);
        let is_live = history
            .as_ref(ctx)
            .all_live_conversations_for_owner(self.owner_id)
            .any(|conversation| conversation.id() == conversation_id);
        if is_live {
            if let Err(error) = self.select_conversation(conversation_id, ctx) {
                self.emit_error(error, ctx);
                return;
            }
            self.send_prompt_to_selected(prompt, conversation_id, ctx);
            return;
        }
        if let Some(conversation) = history.as_ref(ctx).conversation(&conversation_id).cloned() {
            history.update(ctx, |history, ctx| {
                history.restore_conversations(self.owner_id, vec![conversation], ctx);
            });
            if let Err(error) = self.select_conversation(conversation_id, ctx) {
                self.emit_error(error, ctx);
                return;
            }
            self.send_prompt_to_selected(prompt, conversation_id, ctx);
            return;
        }

        let future = history
            .as_ref(ctx)
            .load_conversation_data(conversation_id, ctx);
        ctx.spawn(future, move |model, conversation, ctx| {
            let Some(crate::ai::blocklist::history_model::CloudConversationData::Oz(conversation)) =
                conversation
            else {
                model.emit_error(
                    anyhow!("Failed to load local conversation {conversation_id}"),
                    ctx,
                );
                return;
            };
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                history.restore_conversations(model.owner_id, vec![*conversation], ctx);
            });
            if let Err(error) = model.select_conversation(conversation_id, ctx) {
                model.emit_error(error, ctx);
                return;
            }
            model.send_prompt_to_selected(prompt, conversation_id, ctx);
        });
    }

    /// Sends a prompt after the target has been selected for this surface.
    fn send_prompt_to_selected(
        &mut self,
        prompt: String,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.ai_controller.update(ctx, |controller, ctx| {
            controller.send_user_query_in_conversation(prompt, conversation_id, None, ctx);
        });
    }

    /// Converts owner-scoped history events into TUI presentation events.
    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if event
            .owner_id()
            .is_some_and(|owner_id| owner_id != self.owner_id)
        {
            return;
        }
        match event {
            BlocklistAIHistoryEvent::StartedNewConversation {
                new_conversation_id,
                ..
            } => ctx.emit(TuiConversationModelEvent::ConversationStarted {
                conversation_id: *new_conversation_id,
            }),
            BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                conversation_id, ..
            } => ctx.emit(TuiConversationModelEvent::ConversationUpdated {
                conversation_id: *conversation_id,
            }),
            BlocklistAIHistoryEvent::UpdatedConversationStatus {
                conversation_id,
                new_status,
                update,
                ..
            } => ctx.emit(TuiConversationModelEvent::ConversationStatusChanged {
                conversation_id: *conversation_id,
                status: new_status.clone(),
                update: update.clone(),
            }),
            _ => {}
        }
    }

    /// Emits a presentation-safe model error.
    fn emit_error(&self, error: anyhow::Error, ctx: &mut ModelContext<Self>) {
        ctx.emit(TuiConversationModelEvent::Error {
            message: format!("{error:#}"),
        });
    }
}

impl Entity for TuiConversationModel {
    type Event = TuiConversationModelEvent;
}

struct TuiConversationSurface {
    conversation_model: ModelHandle<TuiConversationModel>,
    last_output: String,
}

impl TuiConversationSurface {
    /// Builds the conversation-capable surface from manager-owned terminal session handles.
    fn new(surface_init: TerminalSurfaceInit, ctx: &mut ViewContext<Self>) -> Self {
        let TerminalSurfaceInit {
            model,
            sessions,
            model_events,
            ..
        } = surface_init;
        let owner_id: EntityId = ctx.view_id();
        let active_session =
            ctx.add_model(|ctx| ActiveSession::new(sessions.clone(), model_events.clone(), ctx));
        let context_model = ctx.add_model(|ctx| {
            BlocklistAIContextModel::new(
                sessions,
                &model_events,
                model.clone(),
                owner_id,
                None,
                ctx,
            )
        });
        let input_model = ctx.add_model(|ctx| {
            BlocklistAIInputModel::new(model.clone(), None, context_model.clone(), owner_id, ctx)
        });
        let get_relevant_files_controller = ctx.add_model(GetRelevantFilesController::new);
        let action_model = ctx.add_model(|ctx| {
            BlocklistAIActionModel::new(
                model.clone(),
                active_session.clone(),
                &model_events,
                get_relevant_files_controller,
                owner_id,
                ctx,
            )
        });
        let ai_controller = ctx.add_model(|ctx| {
            BlocklistAIController::new(
                input_model,
                context_model.clone(),
                action_model,
                active_session,
                None,
                model,
                owner_id,
                ctx,
            )
        });
        let conversation_model = ctx.add_model(|ctx| {
            TuiConversationModel::new(owner_id, context_model, ai_controller, ctx)
        });
        ctx.subscribe_to_model(&conversation_model, |surface, _, event, ctx| {
            surface.handle_conversation_event(event, ctx)
        });
        Self {
            conversation_model,
            last_output: String::new(),
        }
    }

    /// Submits a new prompt or restores and follows up in an existing conversation.
    fn submit_prompt(
        &mut self,
        prompt: String,
        conversation_id: Option<AIConversationId>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.conversation_model.update(ctx, |model, ctx| {
            if let Some(conversation_id) = conversation_id {
                model.send_prompt_to_conversation(prompt, conversation_id, ctx);
            } else {
                model.send_prompt(prompt, ctx);
            }
        });
    }

    /// Adapts production model events to the cargo-runnable smoke output.
    fn handle_conversation_event(
        &mut self,
        event: &TuiConversationModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            TuiConversationModelEvent::SelectedConversationChanged { conversation_id } => {
                let _ = conversation_id;
                self.last_output.clear();
            }
            TuiConversationModelEvent::ConversationStarted { conversation_id } => {
                println!("conversation_id={conversation_id}");
            }
            TuiConversationModelEvent::ConversationUpdated { conversation_id } => {
                self.print_stream_snapshot(*conversation_id, ctx);
            }
            TuiConversationModelEvent::ConversationStatusChanged {
                conversation_id,
                status,
                update: ConversationStatusUpdate::Changed { .. },
            } => {
                self.print_stream_snapshot(*conversation_id, ctx);
                if !status.is_in_progress() {
                    println!("status={status:?}");
                    ctx.terminate_app(TerminationMode::ForceTerminate, None);
                }
            }
            TuiConversationModelEvent::ConversationStatusChanged {
                update: ConversationStatusUpdate::Restored,
                ..
            } => {}
            TuiConversationModelEvent::Error { message } => {
                self.terminate_with_error(anyhow!("{message}"), ctx);
            }
        }
    }

    /// Prints the latest plain-text output when it changes.
    fn print_stream_snapshot(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some((has_actions, text)) = (|| {
            let exchange = BlocklistAIHistoryModel::as_ref(ctx)
                .conversation(&conversation_id)
                .and_then(|conversation| conversation.latest_exchange())?;
            let output = exchange.output_status.output()?;
            let output = output.get();
            let has_actions = output.actions().next().is_some();
            let text = output
                .text_from_agent_output()
                .flat_map(|text| text.sections.iter())
                .filter_map(|section| match section {
                    AIAgentTextSection::PlainText { text } => Some(text.text()),
                    AIAgentTextSection::Code { .. }
                    | AIAgentTextSection::Table { .. }
                    | AIAgentTextSection::Image { .. }
                    | AIAgentTextSection::MermaidDiagram { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some((has_actions, text))
        })() else {
            return;
        };
        if has_actions {
            self.terminate_with_error(anyhow!("TUI smoke mode does not support tool actions"), ctx);
            return;
        }
        if text != self.last_output {
            println!("{text}");
            self.last_output = text;
        }
    }

    /// Terminates smoke mode with a user-visible error.
    fn terminate_with_error(&self, error: anyhow::Error, ctx: &mut ViewContext<Self>) {
        ctx.terminate_app(TerminationMode::ForceTerminate, Some(Err(error)));
    }
}

impl Entity for TuiConversationSurface {
    type Event = ();
}

impl PtyIntentEvent for () {
    fn pty_intent(&self) -> Option<PtyIntent> {
        None
    }
}

impl TerminalSurface for TuiConversationSurface {
    fn should_start_pty(&self) -> bool {
        false
    }

    #[cfg(unix)]
    fn should_start_password_prompt_polling(&self, _command: &str, _ctx: &AppContext) -> bool {
        false
    }

    #[cfg(unix)]
    fn should_stop_password_prompt_polling(&self, _completed: &AfterBlockCompletedEvent) -> bool {
        false
    }

    fn on_shell_determined(&mut self, _ctx: &mut ViewContext<Self>) {}

    fn on_active_shell_launch_data_updated(
        &mut self,
        _shell_launch_data: Option<ShellLaunchData>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn on_pty_spawn_failed(&mut self, error: anyhow::Error, _ctx: &mut ViewContext<Self>) {
        log::warn!("Unexpected PTY spawn failure for no-PTY TUI surface: {error:#}");
    }

    #[cfg(unix)]
    fn on_possible_password_prompt(
        &mut self,
        _block_index: Option<BlockIndex>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    #[cfg(unix)]
    fn on_polled_block_completed(
        &mut self,
        _completed: &AfterBlockCompletedEvent,
        _ctx: &mut ViewContext<Self>,
    ) {
    }
}

impl View for TuiConversationSurface {
    fn ui_name() -> &'static str {
        "TuiConversationSurface"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for TuiConversationSurface {
    type Action = ();
}

impl TerminalManagerTrait for LocalTtyTerminalManager<TuiConversationSurface> {
    fn model(&self) -> std::sync::Arc<parking_lot::FairMutex<TerminalModel>> {
        self.model()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

struct TuiConversationSession {
    _manager: ModelHandle<Box<dyn TerminalManagerTrait>>,
    _surface: ViewHandle<TuiConversationSurface>,
}

impl Entity for TuiConversationSession {
    type Event = ();
}

impl SingletonEntity for TuiConversationSession {}

/// Entry point invoked once the headless app is initialized.
pub fn init(ctx: &mut AppContext) {
    let prompt = std::env::var(PROMPT_ENV).ok();
    let conversation_id = std::env::var(CONVERSATION_ID_ENV)
        .ok()
        .map(AIConversationId::try_from)
        .transpose();
    let conversation_id = match conversation_id {
        Ok(conversation_id) => conversation_id,
        Err(error) => {
            ctx.terminate_app(
                TerminationMode::ForceTerminate,
                Some(Err(anyhow!("Invalid conversation ID: {error}"))),
            );
            return;
        }
    };
    let auth_state = AuthStateProvider::as_ref(ctx).get();
    if auth_state.is_logged_in() {
        finish_auth_and_run(prompt, conversation_id, ctx);
        return;
    }

    println!("Welcome to Warp TUI. Let's get you logged in.");
    ctx.subscribe_to_model(&AuthManager::handle(ctx), move |_, event, ctx| match event {
        AuthManagerEvent::ReceivedDeviceAuthorizationCode {
            verification_url,
            verification_url_complete,
            user_code,
        } => {
            let url_to_open = verification_url_complete
                .as_deref()
                .unwrap_or(verification_url.as_str());
            println!(
                "Opening your browser to log in.\nIf it doesn't open, visit {verification_url} and enter this code: {user_code}"
            );
            ctx.open_url(url_to_open);
        }
        AuthManagerEvent::AuthComplete => {
            finish_auth_and_run(prompt.clone(), conversation_id, ctx);
        }
        AuthManagerEvent::AuthFailed(error) => {
            ctx.terminate_app(
                TerminationMode::ForceTerminate,
                Some(Err(anyhow!("Authentication failed: {error:#}"))),
            );
        }
        _ => {}
    });
    AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
        auth_manager.authorize_device(ctx);
    });
}

/// Prints identity for auth-only mode or starts the conversation smoke path.
fn finish_auth_and_run(
    prompt: Option<String>,
    conversation_id: Option<AIConversationId>,
    ctx: &mut AppContext,
) {
    let Some(prompt) = prompt else {
        print_user_id_and_exit(ctx);
        return;
    };
    start_prompt_smoke(prompt, conversation_id, ctx);
}

/// Builds a manager-owned terminal session and submits the smoke prompt.
fn start_prompt_smoke(
    prompt: String,
    conversation_id: Option<AIConversationId>,
    ctx: &mut AppContext,
) {
    let (window_id, _) = ctx.add_window(
        AddWindowOptions {
            window_style: WindowStyle::NotStealFocus,
            ..Default::default()
        },
        |_ctx| TuiHostView,
    );
    let banner = ctx.add_model(|_| BannerState::default());
    let terminal_manager = LocalTtyTerminalManager::<TuiConversationSurface>::create_model(
        std::env::current_dir().ok(),
        std::env::vars_os().collect(),
        IsSharedSessionCreator::No,
        None,
        banner,
        Vector2F::new(120., 24.),
        None,
        None,
        ctx,
        move |surface_init, ctx| {
            let surface = ctx.add_typed_action_view(window_id, |ctx| {
                TuiConversationSurface::new(surface_init, ctx)
            });
            TerminalSurfaceResult {
                surface,
                post_wire: |_manager: &mut LocalTtyTerminalManager<TuiConversationSurface>,
                            _surface: &ViewHandle<TuiConversationSurface>,
                            _ctx: &mut AppContext| {},
            }
        },
    );
    let manager = terminal_manager.manager;
    let surface = terminal_manager.surface;
    surface.update(ctx, |surface, ctx| {
        surface.submit_prompt(prompt, conversation_id, ctx);
    });
    ctx.add_singleton_model(|_| TuiConversationSession {
        _manager: manager,
        _surface: surface,
    });
}

/// Prints the authenticated user's ID to stdout, then terminates the app.
fn print_user_id_and_exit(ctx: &mut AppContext) {
    let auth_state = AuthStateProvider::as_ref(ctx).get();
    match auth_state.user_id() {
        Some(user_id) => {
            let uid_full = user_id.as_string();
            let uid = uid_full
                .strip_prefix("serviceAccount:")
                .unwrap_or(uid_full.as_str());
            println!("Logged in. User ID: {uid}");
            ctx.terminate_app(TerminationMode::ForceTerminate, None);
        }
        None => {
            ctx.terminate_app(
                TerminationMode::ForceTerminate,
                Some(Err(anyhow!("Could not determine user ID after login."))),
            );
        }
    }
}
