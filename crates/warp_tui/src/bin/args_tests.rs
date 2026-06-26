use super::{parse_args, should_forward_args, TuiArgs};

#[test]
fn forwards_frontend_args_but_not_worker_args() {
    assert!(should_forward_args(&[
        "--prompt".to_owned(),
        "hello".to_owned()
    ]));
    assert!(!should_forward_args(&["minidump-server".to_owned()]));
    #[cfg(unix)]
    assert!(!should_forward_args(&[
        warp_cli::terminal_server_subcommand()
    ]));
}
#[test]
fn parses_prompt_and_conversation_id() {
    assert_eq!(
        parse_args(
            ["--conversation-id", "conversation-1", "--prompt", "hello",].map(str::to_owned),
        )
        .unwrap(),
        TuiArgs {
            prompt: Some("hello".to_owned()),
            conversation_id: Some("conversation-1".to_owned()),
        }
    );
}

#[test]
fn rejects_missing_argument_value() {
    let error = parse_args(["--prompt".to_owned()]).unwrap_err();
    assert_eq!(error.to_string(), "--prompt requires a value");
}

#[test]
fn rejects_unknown_argument() {
    let error = parse_args(["--unknown".to_owned()]).unwrap_err();
    assert_eq!(error.to_string(), "Unknown argument: --unknown");
}
