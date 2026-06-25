//! Stable-channel `warp-tui` binary.
//!
//! Mirrors `app/src/bin/stable.rs`: loads the `stable` channel config with no
//! additional feature flags, then hands off to the shared TUI entry point.

use anyhow::Result;
use warp_core::channel::{Channel, ChannelState};
mod args;

fn main() -> Result<()> {
    args::forward_args_to_environment()?;
    ChannelState::set(ChannelState::new(
        Channel::Stable,
        warp_channel_config::load_config!("stable"),
    ));

    warp::run_tui()
}
