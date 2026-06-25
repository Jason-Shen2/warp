use anyhow::Result;

const PROMPT_ENV: &str = "WARP_TUI_PROMPT";
const CONVERSATION_ID_ENV: &str = "WARP_TUI_CONVERSATION_ID";
#[derive(Debug, Default, PartialEq, Eq)]
struct TuiArgs {
    prompt: Option<String>,
    conversation_id: Option<String>,
}

/// Forwards TUI CLI arguments to the headless app initialization environment.
pub(crate) fn forward_args_to_environment() -> Result<()> {
    let args = parse_args(std::env::args().skip(1))?;
    if let Some(prompt) = args.prompt {
        std::env::set_var(PROMPT_ENV, prompt);
    }
    if let Some(conversation_id) = args.conversation_id {
        std::env::set_var(CONVERSATION_ID_ENV, conversation_id);
    }
    Ok(())
}

/// Parses the arguments supported by every channel-specific TUI binary.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<TuiArgs> {
    let mut parsed = TuiArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--prompt" => {
                parsed.prompt = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--prompt requires a value"))?,
                );
            }
            "--conversation-id" => {
                parsed.conversation_id = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--conversation-id requires a value"))?,
                );
            }
            other => return Err(anyhow::anyhow!("Unknown argument: {other}")),
        }
    }
    Ok(parsed)
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
