//! Dev-channel `warp-tui` binary (internal nightly builds).
//!
//! Mirrors `app/src/bin/dev.rs`: loads the internal `dev` channel config and
//! layers the dev feature flags, then hands off to the shared TUI entry point.

use anyhow::Result;
use warp_core::channel::{Channel, ChannelState};
use warp_core::features;
mod args;

fn main() -> Result<()> {
    args::forward_args_to_environment()?;
    ChannelState::set(
        ChannelState::new(Channel::Dev, warp_channel_config::load_config!("dev"))
            .with_additional_features(features::DEBUG_FLAGS)
            .with_additional_features(features::DOGFOOD_FLAGS)
            .with_additional_features(features::PREVIEW_FLAGS),
    );

    warp::run_tui()
}
