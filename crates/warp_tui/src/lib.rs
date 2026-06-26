//! Headless terminal-UI frontend for Warp.

use anyhow::Result;
mod agent_block;

mod args;
mod conversation_model;
mod conversation_selection;
mod grid_render;
mod preview_shim;
mod prompt_stream;
mod root;
mod terminal_block;
mod terminal_history_index;
mod transcript_view;

use args::TuiArgs;

/// Runs the TUI frontend or dispatches a Warp worker invocation.
pub fn run() -> Result<()> {
    if let Some(result) = warp::run_tui_worker_if_requested() {
        return result;
    }
    let args = TuiArgs::from_env()?;
    warp::run_tui(move |ctx| {
        if !prompt_stream::start(args, ctx) {
            root::start(ctx);
        }
        true
    })
}
