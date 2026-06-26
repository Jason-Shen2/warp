//! Headless terminal-UI frontend for Warp.

use anyhow::Result;

mod args;
mod conversation_model;
mod conversation_selection;
mod prompt_stream;

use args::TuiArgs;

/// Runs the TUI frontend or dispatches a Warp worker invocation.
pub fn run() -> Result<()> {
    if let Some(result) = warp::run_tui_worker_if_requested() {
        return result;
    }
    let args = TuiArgs::from_env()?;
    warp::run_tui(move |ctx| prompt_stream::start(args, ctx))
}
