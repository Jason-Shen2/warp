use std::sync::Arc;

use parking_lot::FairMutex;
use warp::tui_export::{
    AIAgentExchangeId, AIConversationId, BlockHeightItem, BlocklistAIHistoryModel, TerminalModel,
};
use warpui::platform::WindowStyle;
use warpui::{AddWindowOptions, App, EntityId};
use warpui_core::elements::tui::{TuiBufferExt, TuiRect};
use warpui_core::presenter::tui::TuiPresenter;

use super::TuiTranscriptView;

#[test]
fn transcript_view_renders_terminal_blocks_from_canonical_order() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| BlocklistAIHistoryModel::default());
        let mut terminal_model = TerminalModel::mock(None, None);
        terminal_model.simulate_block("echo 1", "1\r\n");
        let terminal_model = Arc::new(FairMutex::new(terminal_model));
        let model_for_view = terminal_model.clone();
        let (_, transcript) = app.update(|ctx| {
            ctx.add_tui_window(
                AddWindowOptions {
                    window_style: WindowStyle::NotStealFocus,
                    ..Default::default()
                },
                |ctx| TuiTranscriptView::new(EntityId::new(), model_for_view, ctx),
            )
        });

        let mut presenter = TuiPresenter::new();
        let frame =
            app.update(|ctx| presenter.present(ctx, &transcript, TuiRect::new(0, 0, 80, 20)));
        let text = frame.buffer.to_lines().join("\n");

        assert!(
            text.contains("echo 1"),
            "transcript should render command input:\n{text}"
        );
        assert!(
            text.contains('1'),
            "transcript should render command output:\n{text}"
        );
    });
}

#[test]
fn transcript_agent_block_lifecycle_updates_canonical_rich_content() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| BlocklistAIHistoryModel::default());
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        let model_for_view = terminal_model.clone();
        let (_, transcript) = app.update(|ctx| {
            ctx.add_tui_window(
                AddWindowOptions {
                    window_style: WindowStyle::NotStealFocus,
                    ..Default::default()
                },
                |ctx| TuiTranscriptView::new(EntityId::new(), model_for_view, ctx),
            )
        });
        let original_conversation_id = AIConversationId::new();
        let reassigned_conversation_id = AIConversationId::new();
        let exchange_id = AIAgentExchangeId::new();

        transcript.update(&mut app, |view, ctx| {
            view.insert_agent_block(original_conversation_id, exchange_id, ctx)
        });
        transcript.read(&app, |view, _| {
            assert_eq!(view.agent_blocks.borrow().len(), 1);
            assert_eq!(view.dirty_agent_blocks.borrow().len(), 1);
        });
        assert_eq!(rich_content_count(&terminal_model), 1);

        transcript.update(&mut app, |view, ctx| {
            view.reassign_exchange(exchange_id, reassigned_conversation_id, ctx);
            view.mark_exchange_dirty(exchange_id, ctx);
        });
        transcript.read(&app, |view, _| {
            let registrations = view.agent_blocks.borrow();
            let registration = registrations
                .values()
                .next()
                .expect("agent block should remain registered");
            assert_eq!(registration.conversation_id, reassigned_conversation_id);
        });
        assert_eq!(rich_content_count(&terminal_model), 1);

        transcript.update(&mut app, |view, ctx| {
            view.remove_conversation(reassigned_conversation_id, ctx)
        });
        transcript.read(&app, |view, _| {
            assert!(view.agent_blocks.borrow().is_empty());
        });
        assert_eq!(rich_content_count(&terminal_model), 0);
    });
}

fn rich_content_count(model: &Arc<FairMutex<TerminalModel>>) -> usize {
    model
        .lock()
        .block_list()
        .block_heights()
        .cursor::<(), ()>()
        .filter(|item| matches!(item, BlockHeightItem::RichContent(_)))
        .count()
}
