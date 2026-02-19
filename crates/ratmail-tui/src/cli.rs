use clap::{Args, Parser, Subcommand};

use super::{
    AccountConfig, CLI_SCHEMA_VERSION, RenderConfig, SearchSpec, SendConfig, SpellConfig,
    UiConfig, build_html_body, cc_from_raw, extract_display, extract_email, load_config_text,
    mailaddrs_to_emails, normalize_ui_theme, parse_search_spec, parse_ui_palette, shell_split,
    to_from_raw,
};

#[path = "cli_guards.rs"]
mod cli_guards;
#[path = "cli_command_handlers.rs"]
mod cli_command_handlers;
#[path = "cli_config.rs"]
mod cli_config;
#[path = "cli_message_filters.rs"]
mod cli_message_filters;
#[path = "cli_runtime_helpers.rs"]
mod cli_runtime_helpers;
#[path = "cli_setup.rs"]
mod cli_setup;
pub(crate) use cli_guards::{
    cli_allows_account, cli_allows_attachments, cli_allows_body, cli_allows_command,
    cli_allows_delete, cli_allows_folder, cli_allows_from, cli_allows_mark, cli_allows_move,
    cli_allows_raw, cli_allows_send, allowed_fields,
};
pub(crate) use cli_command_handlers::run_cli;
pub(crate) use cli_config::{
    load_cli_config, load_render_config, load_send_config, load_spell_config, load_ui_config,
};
pub(crate) use cli_message_filters::{
    account_id_for, from_matches_filter, map_folder_names, maybe_fetch_raw, parse_before_ts,
    parse_from_addrs, parse_since_ts, spec_matches_attachments_cli, spec_matches_text_fields_cli,
};
pub(crate) use cli_runtime_helpers::{
    filter_summary_to_json, output_error, output_ok, resolve_account, resolve_cli_command,
};
pub(crate) use cli_setup::run_setup_wizard;

#[derive(Parser, Debug)]
#[command(name = "ratmail", version, about = "Terminal email client")]
pub(crate) struct Cli {
    #[arg(short = 'c', long = "cmd")]
    cmd: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CliCommand {
    Setup(SetupCmd),
    Accounts(AccountsCmd),
    Folders(FoldersCmd),
    Messages(MessagesCmd),
    Message(MessageCmd),
    Sync(SyncCmd),
    Send(SendCmd),
}

#[derive(Args, Debug)]
pub(crate) struct SetupCmd {}

#[derive(Args, Debug)]
pub(crate) struct AccountsCmd {
    #[command(subcommand)]
    command: AccountsCommand,
}

#[derive(Subcommand, Debug)]
pub(crate) enum AccountsCommand {
    List,
}

#[derive(Args, Debug)]
pub(crate) struct FoldersCmd {
    #[command(subcommand)]
    command: FoldersCommand,
}

#[derive(Subcommand, Debug)]
pub(crate) enum FoldersCommand {
    List(FoldersList),
}

#[derive(Args, Debug)]
pub(crate) struct FoldersList {
    #[arg(long)]
    account: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct MessagesCmd {
    #[command(subcommand)]
    command: MessagesCommand,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MessagesCommand {
    List(MessagesList),
}

#[derive(Args, Debug)]
pub(crate) struct MessagesList {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    folder: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    unread: bool,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    subject: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    date: Option<String>,
    #[arg(long)]
    query: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long = "since-ts")]
    since_ts: Option<i64>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long = "before-ts")]
    before_ts: Option<i64>,
    #[arg(long = "att")]
    att_name: Vec<String>,
    #[arg(long = "att-type")]
    att_type: Vec<String>,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MessageCmd {
    #[command(subcommand)]
    command: MessageCommand,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MessageCommand {
    Get(MessageGet),
    Body(MessageBody),
    Raw(MessageRaw),
    AttachmentSave(MessageAttachmentSave),
    Move(MessageMove),
    Delete(MessageDelete),
    Mark(MessageMark),
}

#[derive(Args, Debug)]
pub(crate) struct MessageGet {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    body: bool,
    #[arg(long)]
    raw: bool,
    #[arg(long)]
    attachments: bool,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MessageBody {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MessageRaw {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MessageAttachmentSave {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    index: usize,
    #[arg(long)]
    path: String,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MessageMove {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    folder: String,
}

#[derive(Args, Debug)]
pub(crate) struct MessageDelete {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
}

#[derive(Args, Debug)]
pub(crate) struct MessageMark {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    id: i64,
    #[arg(long)]
    read: bool,
    #[arg(long)]
    unread: bool,
}

#[derive(Args, Debug)]
pub(crate) struct SyncCmd {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    folder: Option<String>,
    #[arg(long)]
    backfill: bool,
    #[arg(long)]
    days: Option<i64>,
    #[arg(long)]
    wait: bool,
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

#[derive(Args, Debug)]
pub(crate) struct SendCmd {
    #[arg(long)]
    account: Option<String>,
    #[arg(long)]
    to: String,
    #[arg(long)]
    cc: Option<String>,
    #[arg(long)]
    bcc: Option<String>,
    #[arg(long)]
    subject: String,
    #[arg(long)]
    body: String,
    #[arg(long)]
    attach: Vec<String>,
    #[arg(long)]
    wait: bool,
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CliConfig {
    enabled: bool,
    default_account: Option<String>,
    load_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::{word_wrap_spans, wrapped_cursor_pos};

    #[test]
    fn test_wrap_exact_fit_moves_word_to_next_row() {
        let spans = word_wrap_spans("12345 abcde", 11, 4);
        assert_eq!(spans, vec![(0, 6), (6, 11)]);
    }

    #[test]
    fn test_wrap_exact_fit_cursor_stays_on_word_row() {
        let (row, col) = wrapped_cursor_pos("12345 abcde", 11, 11, 4);
        assert_eq!((row, col), (1, 5));
    }
}
