//! Temporary preview key bindings for exercising transcript interleaving.

use warpui_core::elements::tui::{TuiColumn, TuiEventHandler};

/// Temporary preview actions removed when the real TUI input view lands.
#[derive(Debug)]
pub(super) enum PreviewAction {
    RunShellCommand,
    SendAgentPrompt,
}

/// Attaches the removable `s`/`a` preview bindings at one root call site.
pub(super) fn wrap(child: TuiColumn) -> TuiEventHandler {
    TuiEventHandler::new(child)
        .on_key("s", |_, ctx, _| {
            ctx.dispatch_typed_action(PreviewAction::RunShellCommand)
        })
        .on_key("a", |_, ctx, _| {
            ctx.dispatch_typed_action(PreviewAction::SendAgentPrompt)
        })
}
