use std::collections::HashSet;

use super::CliConfig;

pub(crate) fn allowed_fields(_config: &CliConfig) -> HashSet<String> {
    all_message_fields()
}

pub(crate) fn cli_allows_command(_config: &CliConfig, _command_id: &str, _is_mutation: bool) -> bool {
    true
}

pub(crate) fn cli_allows_account(_config: &CliConfig, _account: &str) -> bool {
    true
}

pub(crate) fn cli_allows_folder(_config: &CliConfig, _folder: &str) -> bool {
    true
}

pub(crate) fn cli_allows_from(_config: &CliConfig, _from: &str) -> bool {
    true
}

pub(crate) fn cli_allows_body(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_raw(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_attachments(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_move(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_delete(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_mark(_config: &CliConfig) -> bool {
    true
}

pub(crate) fn cli_allows_send(_config: &CliConfig) -> bool {
    true
}

fn all_message_fields() -> HashSet<String> {
    [
        "id",
        "folder_id",
        "imap_uid",
        "date",
        "from",
        "to",
        "cc",
        "subject",
        "unread",
        "preview",
        "body",
        "raw",
        "attachments",
        "links",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}
