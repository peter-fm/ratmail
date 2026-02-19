use std::collections::HashSet;

use anyhow::Result;
use clap::Parser;
use ratmail_core::MessageSummary;
use serde_json::{Value as JsonValue, json};

use super::{AccountConfig, CLI_SCHEMA_VERSION, Cli, CliCommand, CliConfig, shell_split};

pub(crate) fn output_ok(value: JsonValue) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema": CLI_SCHEMA_VERSION,
            "ok": true,
            "result": value
        }))?
    );
    Ok(())
}

pub(crate) fn output_error(message: &str) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema": CLI_SCHEMA_VERSION,
            "ok": false,
            "error": message
        }))?
    );
    Ok(())
}

pub(crate) fn resolve_cli_command(cli: Cli) -> Result<(bool, Option<CliCommand>)> {
    let cli_requested = cli.cmd.is_some() || cli.command.is_some();
    if let Some(cmd) = cli.cmd {
        let parts = shell_split(&cmd).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if parts.is_empty() {
            return Ok((true, None));
        }
        let mut args = Vec::with_capacity(parts.len() + 1);
        args.push("ratmail".to_string());
        args.extend(parts);
        let parsed = Cli::try_parse_from(args).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        return Ok((true, parsed.command));
    }
    Ok((cli_requested, cli.command))
}

pub(crate) fn resolve_account<'a>(
    config: &CliConfig,
    accounts: &'a [AccountConfig],
    requested: Option<&str>,
) -> Result<&'a AccountConfig> {
    let selected = if let Some(name) = requested {
        accounts.iter().find(|acct| acct.name == name)
    } else if let Some(default_name) = config.default_account.as_deref() {
        accounts.iter().find(|acct| acct.name == default_name)
    } else if accounts.len() == 1 {
        accounts.first()
    } else {
        None
    };
    selected.ok_or_else(|| anyhow::anyhow!("Account not found or not specified"))
}

pub(crate) fn filter_summary_to_json(
    summary: &MessageSummary,
    allowed: &HashSet<String>,
) -> JsonValue {
    let mut map = serde_json::Map::new();
    if allowed.contains("id") {
        map.insert("id".to_string(), json!(summary.id));
    }
    if allowed.contains("folder_id") {
        map.insert("folder_id".to_string(), json!(summary.folder_id));
    }
    if allowed.contains("imap_uid") {
        map.insert("imap_uid".to_string(), json!(summary.imap_uid));
    }
    if allowed.contains("date") {
        map.insert("date".to_string(), json!(summary.date));
    }
    if allowed.contains("from") {
        map.insert("from".to_string(), json!(summary.from));
    }
    if allowed.contains("subject") {
        map.insert("subject".to_string(), json!(summary.subject));
    }
    if allowed.contains("unread") {
        map.insert("unread".to_string(), json!(summary.unread));
    }
    if allowed.contains("preview") {
        map.insert("preview".to_string(), json!(summary.preview));
    }
    JsonValue::Object(map)
}
