use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mime_guess::MimeGuess;
use ratmail_content::{extract_attachment_data, extract_attachments, extract_display};
use ratmail_core::{AttachmentMeta, DEFAULT_TEXT_WIDTH, Folder, SqliteMailStore};
use ratmail_mail::{MailCommand, MailEngine, MailEvent, OutgoingAttachment};
use serde_json::{Value as JsonValue, json};

use super::{
    AccountConfig, AccountsCommand, CliCommand, FoldersCommand, MessageCommand, MessagesCommand,
    account_id_for, cli_allows_account, cli_allows_attachments, cli_allows_body,
    cli_allows_command, cli_allows_delete, cli_allows_folder, cli_allows_from, cli_allows_mark,
    cli_allows_move, cli_allows_raw, cli_allows_send, allowed_fields, build_html_body, cc_from_raw,
    filter_summary_to_json, load_cli_config, load_send_config, map_folder_names, maybe_fetch_raw,
    output_error, output_ok, parse_before_ts, parse_from_addrs, parse_search_spec, parse_since_ts,
    resolve_account, run_setup_wizard, spec_matches_attachments_cli, spec_matches_text_fields_cli,
    to_from_raw,
};

pub(crate) fn run_cli(
    rt: &Arc<tokio::runtime::Runtime>,
    command: CliCommand,
    accounts: &[AccountConfig],
) -> Result<()> {
    if let CliCommand::Setup(_) = command {
        return run_setup_wizard(accounts, !io::stdout().is_terminal());
    }
    let config = load_cli_config();
    if !config.enabled {
        if let Some(err) = config.load_error.as_deref() {
            return output_error(&format!("CLI disabled: {}", err));
        }
        return output_error("CLI disabled (set [cli].enabled = true)");
    }
    let allowed = allowed_fields(&config);
    let send_config = load_send_config();

    match command {
        CliCommand::Accounts(cmd) => match cmd.command {
            AccountsCommand::List => {
                if !cli_allows_command(&config, "accounts.list", false) {
                    return output_error("Command not allowed");
                }
                let mut out = Vec::new();
                for acct in accounts {
                    if !cli_allows_account(&config, &acct.name) {
                        continue;
                    }
                    let address = acct
                        .imap
                        .as_ref()
                        .map(|i| i.username.clone())
                        .or_else(|| acct.smtp.as_ref().map(|s| s.username.clone()))
                        .unwrap_or_default();
                    out.push(json!({
                        "name": acct.name,
                        "address": address,
                        "db_path": acct.db_path,
                        "imap": acct.imap.is_some(),
                        "smtp": acct.smtp.is_some(),
                    }));
                }
                return output_ok(json!(out));
            }
        },
        CliCommand::Folders(cmd) => match cmd.command {
            FoldersCommand::List(args) => {
                if !cli_allows_command(&config, "folders.list", false) {
                    return output_error("Command not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let folders = rt.block_on(store.list_folders(account_id))?;
                let filtered: Vec<Folder> = folders
                    .into_iter()
                    .filter(|f| cli_allows_folder(&config, &f.name))
                    .collect();
                return output_ok(json!(filtered));
            }
        },
        CliCommand::Messages(cmd) => match cmd.command {
            MessagesCommand::List(args) => {
                if !cli_allows_command(&config, "messages.list", false) {
                    return output_error("Command not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let folder_id = if let Some(folder) = &args.folder {
                    if !cli_allows_folder(&config, folder) {
                        return output_error("Folder not allowed");
                    }
                    let id = rt
                        .block_on(store.folder_id_by_name(account_id, folder))?
                        .ok_or_else(|| anyhow::anyhow!("Folder not found"))?;
                    Some(id)
                } else {
                    None
                };
                let mut spec = args
                    .query
                    .as_deref()
                    .map(parse_search_spec)
                    .unwrap_or_default();
                if let Some(from) = args.from.as_deref() {
                    spec.from.push(from.to_ascii_lowercase());
                }
                if let Some(subject) = args.subject.as_deref() {
                    spec.subject.push(subject.to_ascii_lowercase());
                }
                if let Some(to) = args.to.as_deref() {
                    spec.to.push(to.to_ascii_lowercase());
                }
                if let Some(date) = args.date.as_deref() {
                    spec.date.push(date.to_ascii_lowercase());
                }
                if !args.att_name.is_empty() {
                    spec.attachment_name
                        .extend(args.att_name.iter().map(|s| s.to_ascii_lowercase()));
                }
                if !args.att_type.is_empty() {
                    spec.attachment_type
                        .extend(args.att_type.iter().map(|s| s.to_ascii_lowercase()));
                }
                if let Some(ts) = args.before_ts {
                    spec.before_ts = Some(ts);
                } else if let Some(before) = &args.before {
                    spec.before_ts = Some(parse_before_ts(before)?);
                }
                let since_ts = if let Some(ts) = args.since_ts {
                    Some(ts)
                } else if let Some(since) = &args.since {
                    Some(parse_since_ts(since)?)
                } else {
                    None
                };
                if spec.since_ts.is_none() {
                    spec.since_ts = since_ts;
                }
                let since_ts = spec.since_ts;
                let unread = if args.unread { Some(true) } else { None };
                let messages = rt.block_on(store.list_messages(
                    account_id,
                    folder_id,
                    unread,
                    since_ts,
                    Some(args.limit as i64),
                ))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if spec.needs_attachments() && !cli_allows_attachments(&config) {
                    return output_error("Attachments access not allowed");
                }
                let mut out = Vec::new();
                for message in messages {
                    if let Some(folder_name) = folder_map.get(&message.folder_id) {
                        if !cli_allows_folder(&config, folder_name) {
                            continue;
                        }
                    }
                    let from_emails = parse_from_addrs(&message.from);
                    if !from_emails
                        .iter()
                        .any(|addr| cli_allows_from(&config, addr))
                        && !cli_allows_from(&config, &message.from)
                    {
                        continue;
                    }
                    let mut to_val: Option<String> = None;
                    let mut cc_val: Option<String> = None;
                    let mut attachments: Option<Vec<AttachmentMeta>> = None;
                    if spec.needs_raw() {
                        let raw = maybe_fetch_raw(
                            rt,
                            &store,
                            account.imap.as_ref(),
                            folder_map.get(&message.folder_id).map(|s| s.as_str()),
                            message.imap_uid,
                            message.id,
                            args.fetch,
                        )?;
                        let Some(raw) = raw else { continue };
                        if !spec.to.is_empty() {
                            to_val = to_from_raw(&raw);
                            cc_val = cc_from_raw(&raw);
                        }
                        if spec.needs_attachments() {
                            if let Ok(list) = extract_attachments(&raw) {
                                attachments = Some(list);
                            } else {
                                attachments = Some(Vec::new());
                            }
                        }
                    }
                    if !spec_matches_text_fields_cli(
                        &spec,
                        &message,
                        to_val.as_deref(),
                        cc_val.as_deref(),
                    ) {
                        continue;
                    }
                    if spec.needs_attachments() {
                        let Some(list) = attachments.as_ref() else {
                            continue;
                        };
                        if !spec_matches_attachments_cli(&spec, list) {
                            continue;
                        }
                    }
                    out.push(filter_summary_to_json(&message, &allowed));
                }
                return output_ok(json!(out));
            }
        },
        CliCommand::Message(cmd) => match cmd.command {
            MessageCommand::Get(args) => {
                if !cli_allows_command(&config, "message.get", false) {
                    return output_error("Command not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                let from_emails = parse_from_addrs(&summary.from);
                if !from_emails
                    .iter()
                    .any(|addr| cli_allows_from(&config, addr))
                    && !cli_allows_from(&config, &summary.from)
                {
                    return output_error("Sender not allowed");
                }
                let mut obj = match filter_summary_to_json(&summary, &allowed) {
                    JsonValue::Object(map) => map,
                    _ => serde_json::Map::new(),
                };
                if allowed.contains("to") {
                    if let Ok(Some(to)) = rt.block_on(store.get_message_to(summary.id)) {
                        if !to.trim().is_empty() {
                            obj.insert("to".to_string(), json!(to));
                        }
                    }
                }
                if allowed.contains("cc") {
                    if let Ok(Some(cc)) = rt.block_on(store.get_message_cc(summary.id)) {
                        if !cc.trim().is_empty() {
                            obj.insert("cc".to_string(), json!(cc));
                        }
                    }
                }

                if args.body && cli_allows_body(&config) && allowed.contains("body") {
                    let raw = maybe_fetch_raw(
                        rt,
                        &store,
                        account.imap.as_ref(),
                        folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                        summary.imap_uid,
                        summary.id,
                        args.fetch,
                    )?;
                    let body = if let Some(body) =
                        rt.block_on(store.get_message_text(summary.id, DEFAULT_TEXT_WIDTH))?
                    {
                        body
                    } else if let Some(raw) = raw {
                        extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)
                            .map(|d| d.text)
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    obj.insert("body".to_string(), json!(body));
                }

                if args.raw && cli_allows_raw(&config) && allowed.contains("raw") {
                    if let Some(raw) = maybe_fetch_raw(
                        rt,
                        &store,
                        account.imap.as_ref(),
                        folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                        summary.imap_uid,
                        summary.id,
                        args.fetch,
                    )? {
                        let encoded = BASE64_STANDARD.encode(raw);
                        obj.insert("raw".to_string(), json!(encoded));
                    }
                }

                if args.attachments
                    && cli_allows_attachments(&config)
                    && allowed.contains("attachments")
                {
                    if let Some(raw) = maybe_fetch_raw(
                        rt,
                        &store,
                        account.imap.as_ref(),
                        folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                        summary.imap_uid,
                        summary.id,
                        args.fetch,
                    )? {
                        if let Ok(attachments) = extract_attachments(&raw) {
                            obj.insert("attachments".to_string(), json!(attachments));
                        }
                    }
                }

                return output_ok(JsonValue::Object(obj));
            }
            MessageCommand::AttachmentSave(args) => {
                if !cli_allows_command(&config, "message.attachment.save", false) {
                    return output_error("Command not allowed");
                }
                if !cli_allows_attachments(&config) {
                    return output_error("Attachments access not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                let from_emails = parse_from_addrs(&summary.from);
                if !from_emails
                    .iter()
                    .any(|addr| cli_allows_from(&config, addr))
                    && !cli_allows_from(&config, &summary.from)
                {
                    return output_error("Sender not allowed");
                }
                let raw = maybe_fetch_raw(
                    rt,
                    &store,
                    account.imap.as_ref(),
                    folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                    summary.imap_uid,
                    summary.id,
                    args.fetch,
                )?;
                let Some(raw) = raw else {
                    return output_error("Attachment not cached (use --fetch)");
                };
                let Some(attachment) = extract_attachment_data(&raw, args.index)? else {
                    return output_error("Attachment not found");
                };
                std::fs::write(&args.path, &attachment.data)?;
                return output_ok(json!({
                    "id": summary.id,
                    "index": args.index,
                    "filename": attachment.filename,
                    "mime": attachment.mime,
                    "size": attachment.data.len(),
                    "path": args.path,
                }));
            }
            MessageCommand::Body(args) => {
                if !cli_allows_command(&config, "message.body", false) {
                    return output_error("Command not allowed");
                }
                if !cli_allows_body(&config) || !allowed.contains("body") {
                    return output_error("Body access not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                let from_emails = parse_from_addrs(&summary.from);
                if !from_emails
                    .iter()
                    .any(|addr| cli_allows_from(&config, addr))
                    && !cli_allows_from(&config, &summary.from)
                {
                    return output_error("Sender not allowed");
                }
                let raw = maybe_fetch_raw(
                    rt,
                    &store,
                    account.imap.as_ref(),
                    folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                    summary.imap_uid,
                    summary.id,
                    args.fetch,
                )?;
                let body = if let Some(body) =
                    rt.block_on(store.get_message_text(summary.id, DEFAULT_TEXT_WIDTH))?
                {
                    body
                } else if let Some(raw) = raw {
                    extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)
                        .map(|d| d.text)
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                return output_ok(json!({ "id": summary.id, "body": body }));
            }
            MessageCommand::Raw(args) => {
                if !cli_allows_command(&config, "message.raw", false) {
                    return output_error("Command not allowed");
                }
                if !cli_allows_raw(&config) || !allowed.contains("raw") {
                    return output_error("Raw access not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                let from_emails = parse_from_addrs(&summary.from);
                if !from_emails
                    .iter()
                    .any(|addr| cli_allows_from(&config, addr))
                    && !cli_allows_from(&config, &summary.from)
                {
                    return output_error("Sender not allowed");
                }
                if let Some(raw) = maybe_fetch_raw(
                    rt,
                    &store,
                    account.imap.as_ref(),
                    folder_map.get(&summary.folder_id).map(|s| s.as_str()),
                    summary.imap_uid,
                    summary.id,
                    args.fetch,
                )? {
                    let encoded = BASE64_STANDARD.encode(raw);
                    return output_ok(json!({ "id": summary.id, "raw": encoded }));
                }
                return output_ok(json!({ "id": summary.id, "raw": null }));
            }
            MessageCommand::Move(args) => {
                if !cli_allows_command(&config, "message.move", true) || !cli_allows_move(&config) {
                    return output_error("Command not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                if !cli_allows_folder(&config, &args.folder) {
                    return output_error("Target folder not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Source folder not allowed");
                    }
                }
                let target_id = rt
                    .block_on(store.folder_id_by_name(account_id, &args.folder))?
                    .ok_or_else(|| anyhow::anyhow!("Target folder not found"))?;
                rt.block_on(store.move_messages(&[summary.id], target_id))?;
                if let (Some(imap), Some(uid), Some(src_name)) = (
                    account.imap.clone(),
                    summary.imap_uid,
                    folder_map.get(&summary.folder_id),
                ) {
                    let (engine, _events) =
                        rt.block_on(async { MailEngine::start(None, Some(imap)) });
                    let _ = engine.send(MailCommand::MoveMessages {
                        folder_name: src_name.clone(),
                        target_folder: args.folder.clone(),
                        uids: vec![uid],
                    });
                }
                return output_ok(json!({ "id": summary.id, "moved_to": args.folder }));
            }
            MessageCommand::Delete(args) => {
                if !cli_allows_command(&config, "message.delete", true)
                    || !cli_allows_delete(&config)
                {
                    return output_error("Command not allowed");
                }
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                rt.block_on(store.delete_messages(&[summary.id]))?;
                if let (Some(imap), Some(uid), Some(src_name)) = (
                    account.imap.clone(),
                    summary.imap_uid,
                    folder_map.get(&summary.folder_id),
                ) {
                    let (engine, _events) =
                        rt.block_on(async { MailEngine::start(None, Some(imap)) });
                    let _ = engine.send(MailCommand::DeleteMessages {
                        folder_name: src_name.clone(),
                        uids: vec![uid],
                    });
                }
                return output_ok(json!({ "id": summary.id, "deleted": true }));
            }
            MessageCommand::Mark(args) => {
                if !cli_allows_command(&config, "message.mark", true) || !cli_allows_mark(&config) {
                    return output_error("Command not allowed");
                }
                if args.read == args.unread {
                    return output_error("Specify exactly one of --read or --unread");
                }
                let unread = args.unread;
                let account = resolve_account(&config, accounts, args.account.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !cli_allows_account(&config, &account.name) {
                    return output_error("Account not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let summary = rt
                    .block_on(store.get_message_summary(args.id))?
                    .ok_or_else(|| anyhow::anyhow!("Message not found"))?;
                let folders = rt
                    .block_on(store.list_folders(account_id))
                    .unwrap_or_default();
                let folder_map = map_folder_names(&folders);
                if let Some(folder_name) = folder_map.get(&summary.folder_id) {
                    if !cli_allows_folder(&config, folder_name) {
                        return output_error("Folder not allowed");
                    }
                }
                rt.block_on(store.set_message_unread(summary.id, unread))?;
                if let Some(imap) = account.imap.clone() {
                    let (engine, _events) =
                        rt.block_on(async { MailEngine::start(None, Some(imap)) });
                    let _ = engine.send(MailCommand::SetFlag {
                        message_id: summary.id,
                        seen: !unread,
                    });
                }
                return output_ok(json!({ "id": summary.id, "unread": unread }));
            }
        },
        CliCommand::Sync(cmd) => {
            if !cli_allows_command(&config, "sync", true) {
                return output_error("Command not allowed");
            }
            let account = resolve_account(&config, accounts, cmd.account.as_deref())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if !cli_allows_account(&config, &account.name) {
                return output_error("Account not allowed");
            }
            let Some(imap) = account.imap.clone() else {
                return output_error("IMAP not configured");
            };
            let (engine, mut events) = rt.block_on(async { MailEngine::start(None, Some(imap)) });
            if let Some(folder_name) = cmd.folder {
                if !cli_allows_folder(&config, &folder_name) {
                    return output_error("Folder not allowed");
                }
                let store = rt.block_on(SqliteMailStore::connect(&account.db_path))?;
                let account_id = account_id_for(rt, &store, &account.name);
                let folder_id = rt
                    .block_on(store.folder_id_by_name(account_id, &folder_name))?
                    .ok_or_else(|| anyhow::anyhow!("Folder not found"))?;
                let state = rt
                    .block_on(store.get_folder_sync_state(folder_id))
                    .ok()
                    .flatten();
                let mode = if cmd.backfill {
                    let before_ts = state.and_then(|s| s.oldest_ts).unwrap_or(0);
                    ratmail_mail::SyncMode::Backfill {
                        before_ts,
                        window_days: cmd.days.unwrap_or(
                            account
                                .imap
                                .as_ref()
                                .map(|i| i.initial_sync_days)
                                .unwrap_or(90),
                        ),
                    }
                } else if let Some(uid) = state.and_then(|s| s.last_seen_uid).map(|v| v as u32) {
                    ratmail_mail::SyncMode::Incremental { last_seen_uid: uid }
                } else {
                    ratmail_mail::SyncMode::Initial {
                        days: cmd.days.unwrap_or(
                            account
                                .imap
                                .as_ref()
                                .map(|i| i.initial_sync_days)
                                .unwrap_or(90),
                        ),
                    }
                };
                let _ = engine.send(MailCommand::SyncFolderByName {
                    name: folder_name,
                    mode,
                });
                if cmd.wait {
                    let deadline = Instant::now() + Duration::from_secs(cmd.timeout_secs);
                    loop {
                        if Instant::now() > deadline {
                            return output_error("Sync timeout");
                        }
                        if let Ok(event) = events.try_recv() {
                            match event {
                                MailEvent::ImapMessages { .. } => {
                                    return output_ok(json!({ "synced": true }));
                                }
                                MailEvent::ImapError { reason, .. } => {
                                    return output_error(&format!("Sync failed: {}", reason));
                                }
                                _ => {}
                            }
                        } else {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
                return output_ok(json!({ "queued": true }));
            }
            let _ = engine.send(MailCommand::SyncAll);
            if cmd.wait {
                let deadline = Instant::now() + Duration::from_secs(cmd.timeout_secs);
                loop {
                    if Instant::now() > deadline {
                        return output_error("Sync timeout");
                    }
                    if let Ok(event) = events.try_recv() {
                        match event {
                            MailEvent::ImapMessages { .. } => {
                                return output_ok(json!({ "synced": true }));
                            }
                            MailEvent::ImapError { reason, .. } => {
                                return output_error(&format!("Sync failed: {}", reason));
                            }
                            _ => {}
                        }
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            return output_ok(json!({ "queued": true }));
        }
        CliCommand::Send(cmd) => {
            if !cli_allows_command(&config, "send", true) || !cli_allows_send(&config) {
                return output_error("Command not allowed");
            }
            let account = resolve_account(&config, accounts, cmd.account.as_deref())
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if !cli_allows_account(&config, &account.name) {
                return output_error("Account not allowed");
            }
            let Some(smtp) = account.smtp.clone() else {
                return output_error("SMTP not configured");
            };
            let attachments = cmd
                .attach
                .iter()
                .filter_map(|path| std::fs::read(path).ok().map(|data| (path, data)))
                .map(|(path, data)| OutgoingAttachment {
                    filename: Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "attachment".to_string()),
                    mime: MimeGuess::from_path(path)
                        .first_or_octet_stream()
                        .to_string(),
                    data,
                })
                .collect::<Vec<_>>();
            let (engine, mut events) = rt.block_on(async { MailEngine::start(Some(smtp), None) });
            let body_html = build_html_body(&cmd.body, &send_config);
            let _ = engine.send(MailCommand::SendMessage {
                to: cmd.to,
                cc: cmd.cc.unwrap_or_default(),
                bcc: cmd.bcc.unwrap_or_default(),
                subject: cmd.subject,
                body: cmd.body,
                body_html,
                attachments,
            });
            if cmd.wait {
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(cmd.timeout_secs) {
                    if let Ok(event) = events.try_recv() {
                        match event {
                            MailEvent::SendCompleted => {
                                return output_ok(json!({ "sent": true }));
                            }
                            MailEvent::SendFailed { reason } => {
                                return output_error(&format!("Send failed: {}", reason));
                            }
                            _ => {}
                        }
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                return output_error("Send timeout");
            }
            return output_ok(json!({ "queued": true }));
        }
        CliCommand::Setup(_) => unreachable!("setup handled before dispatch"),
    }
}
