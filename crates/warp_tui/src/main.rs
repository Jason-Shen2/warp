//! Entry point for `warp-tui`, the headless terminal-UI front-end.
//!
//! This crate is the driver for the TUI: it configures the channel and then
//! hands off to [`warp::run_tui`], which boots the real (headless) Warp app and
//! runs the TUI init flow (see `app/src/tui.rs`). It is a console application
//! (no GUI window, no app bundle), so unlike the GUI binaries it sets no
//! `windows_subsystem` attribute and embeds no `Info.plist`.

use anyhow::Result;
use warp_core::channel::{Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig};
use warp_core::AppId;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--prompt" => {
                let prompt = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--prompt requires a value"))?;
                std::env::set_var("WARP_TUI_PROMPT", prompt);
            }
            "--conversation-id" => {
                let conversation_id = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--conversation-id requires a value"))?;
                std::env::set_var("WARP_TUI_CONVERSATION_ID", conversation_id);
            }
            other => return Err(anyhow::anyhow!("Unknown argument: {other}")),
        }
    }

    // TODO(follow-up): mirror the GUI app's per-channel setup. The GUI ships
    // separate binaries (`app/src/bin/{local,dev,preview,stable,oss}.rs`), each
    // selecting a `Channel` + channel-specific `ChannelConfig`, feature-flag set
    // (LOCAL/DOGFOOD/PREVIEW flags), and build profile, with `cargo run`
    // defaulting to the local channel. The TUI currently hardcodes a single
    // production `Oss` channel; give it the same local/dev/preview/stable
    // channels (e.g. per-channel `warp_tui` binaries or a `--channel` flag).
    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new("dev", "warp", "WarpTui"),
            logfile_name: "warp-tui.log".into(),
            server_config: WarpServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    if cfg!(debug_assertions) {
        state = state.with_additional_features(warp_core::features::DEBUG_FLAGS);
    }
    ChannelState::set(state);

    warp::run_tui()
}
