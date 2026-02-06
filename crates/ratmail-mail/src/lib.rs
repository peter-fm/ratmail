//! Mail protocol integration (IMAP/SMTP) skeleton.

use anyhow::{anyhow, Result};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, Message, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};
use mailparse::{addrparse, MailAddr};
use imap::{ClientBuilder, ConnectionMode};
use chrono::{Datelike, Duration, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum MailCommand {
    SyncFolder(i64),
    FetchMessageBody {
        message_id: i64,
        folder_name: String,
        uid: u32,
    },
    SetFlag { message_id: i64, seen: bool },
    SyncAll,
    SyncFolderByName { name: String, mode: SyncMode },
    SendMessage {
        to: String,
        cc: String,
        bcc: String,
        subject: String,
        body: String,
        attachments: Vec<OutgoingAttachment>,
    },
}

#[derive(Debug, Clone)]
pub enum SyncMode {
    Initial { days: i64 },
    Incremental { last_seen_uid: u32 },
    Backfill { before_ts: i64, window_days: i64 },
}

#[derive(Debug, Clone)]
pub enum MailEvent {
    SyncStarted(i64),
    SyncCompleted(i64),
    SyncFailed { folder_id: i64, reason: String },
    BodyFetched(i64),
    FlagUpdated { message_id: i64, seen: bool },
    ImapFolders(Vec<ImapFolder>),
    ImapMessages { folder_name: String, messages: Vec<ImapMessageSummary> },
    ImapBody { message_id: i64, raw: Vec<u8> },
    ImapError { reason: String },
    SendStarted,
    SendCompleted,
    SendFailed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub skip_tls_verify: bool,
    pub initial_sync_days: i64,
    pub fetch_chunk_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapFolder {
    pub name: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapMessageSummary {
    pub uid: u32,
    pub date: String,
    pub from: String,
    pub subject: String,
    pub unread: bool,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime: String,
    pub data: Vec<u8>,
}

static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn log_path() -> Option<PathBuf> {
    if std::env::var("RATMAIL_LOG").is_err() {
        return None;
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Some(base.join("ratmail").join("ratmail.log"))
}

fn log_debug(msg: &str) {
    let Some(path) = log_path() else { return };
    let lock = LOG_FILE.get_or_init(|| {
        let _ = std::fs::create_dir_all(
            path.parent().unwrap_or_else(|| std::path::Path::new("/tmp")),
        );
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Mutex::new(file)
    });
    if let Ok(mut guard) = lock.lock() {
        if let Some(file) = guard.as_mut() {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", ts, msg);
        }
    }
}

#[derive(Clone)]
pub struct MailEngine {
    tx: mpsc::UnboundedSender<MailCommand>,
}

impl MailEngine {
    pub fn start(
        smtp: Option<SmtpConfig>,
        imap: Option<ImapConfig>,
    ) -> (Self, mpsc::UnboundedReceiver<MailEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<MailCommand>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<MailEvent>();

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    MailCommand::SyncFolder(folder_id) => {
                        let _ = evt_tx.send(MailEvent::SyncStarted(folder_id));
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        let _ = evt_tx.send(MailEvent::SyncCompleted(folder_id));
                    }
                    MailCommand::SyncAll => {
                        if let Some(imap) = imap.clone() {
                            let tx = evt_tx.clone();
                            tokio::task::spawn_blocking(move || sync_all_imap(imap, tx));
                        }
                    }
                    MailCommand::SyncFolderByName { name, mode } => {
                        if let Some(imap) = imap.clone() {
                            let tx = evt_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                sync_folder_imap(imap, name, mode, tx)
                            });
                        }
                    }
                    MailCommand::FetchMessageBody {
                        message_id,
                        folder_name,
                        uid,
                    } => {
                        if let Some(imap) = imap.clone() {
                            let tx = evt_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                match fetch_imap_body(&imap, &folder_name, uid) {
                                    Ok(raw) => {
                                        let _ = tx.send(MailEvent::ImapBody { message_id, raw });
                                    }
                                    Err(err) => {
                                        let _ = tx.send(MailEvent::ImapError {
                                            reason: err.to_string(),
                                        });
                                    }
                                }
                            });
                        }
                        let _ = evt_tx.send(MailEvent::BodyFetched(message_id));
                    }
                    MailCommand::SetFlag { message_id, seen } => {
                        let _ = evt_tx.send(MailEvent::FlagUpdated { message_id, seen });
                    }
                    MailCommand::SendMessage {
                        to,
                        cc,
                        bcc,
                        subject,
                        body,
                        attachments,
                    } => {
                        let _ = evt_tx.send(MailEvent::SendStarted);
                        let result = send_smtp(
                            smtp.clone(),
                            &to,
                            &cc,
                            &bcc,
                            &subject,
                            &body,
                            &attachments,
                        )
                        .await;
                        match result {
                            Ok(()) => {
                                let _ = evt_tx.send(MailEvent::SendCompleted);
                            }
                            Err(err) => {
                                let _ = evt_tx.send(MailEvent::SendFailed {
                                    reason: err.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        });

        (Self { tx: cmd_tx }, evt_rx)
    }

    pub fn send(&self, cmd: MailCommand) -> Result<()> {
        self.tx.send(cmd)?;
        Ok(())
    }
}

async fn send_smtp(
    smtp: Option<SmtpConfig>,
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    body: &str,
    attachments: &[OutgoingAttachment],
) -> Result<()> {
    let smtp = smtp.ok_or_else(|| anyhow!("SMTP not configured"))?;
    let from_addr = parse_mailbox(&smtp.from)?;
    let to_addrs = parse_mailbox_list(to)?;
    let cc_addrs = parse_mailbox_list(cc)?;
    let bcc_addrs = parse_mailbox_list(bcc)?;
    if to_addrs.is_empty() && cc_addrs.is_empty() && bcc_addrs.is_empty() {
        return Err(anyhow!("No recipients"));
    }

    let mut builder = Message::builder().from(from_addr).subject(subject);
    for addr in to_addrs {
        builder = builder.to(addr);
    }
    for addr in cc_addrs {
        builder = builder.cc(addr);
    }
    for addr in bcc_addrs {
        builder = builder.bcc(addr);
    }
    let email = if attachments.is_empty() {
        builder.body(body.to_string())?
    } else {
        let mut multipart = MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()));
        for attachment in attachments {
            let mime = ContentType::parse(&attachment.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            multipart = multipart.singlepart(
                Attachment::new(attachment.filename.clone())
                    .body(attachment.data.clone(), mime),
            );
        }
        builder.multipart(multipart)?
    };

    let creds = Credentials::new(smtp.username, smtp.password);
    let builder = if smtp.port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)?
    };
    let mailer = builder.credentials(creds).port(smtp.port).build();

    mailer
        .send(email)
        .await
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(())
}

fn parse_mailbox(input: &str) -> Result<Mailbox> {
    let trimmed = input.trim();
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        let name = trimmed[..start].trim().trim_matches('"');
        let addr = trimmed[start + 1..end].trim();
        return Ok(Mailbox::new(Some(name.to_string()), addr.parse()?));
    }
    Ok(Mailbox::new(None, trimmed.parse()?))
}

fn parse_mailbox_list(input: &str) -> Result<Vec<Mailbox>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let parsed = addrparse(trimmed)?;
    Ok(mailaddrs_to_mailboxes(&parsed))
}

fn mailaddrs_to_mailboxes(addrs: &[MailAddr]) -> Vec<Mailbox> {
    let mut out = Vec::new();
    for addr in addrs {
        match addr {
            MailAddr::Single(info) => {
                if let Ok(parsed) = info.addr.parse() {
                    out.push(Mailbox::new(info.display_name.clone(), parsed));
                }
            }
            MailAddr::Group(group) => {
                for info in &group.addrs {
                    if let Ok(parsed) = info.addr.parse() {
                        out.push(Mailbox::new(info.display_name.clone(), parsed));
                    }
                }
            }
        }
    }
    out
}

fn sync_all_imap(imap: ImapConfig, tx: mpsc::UnboundedSender<MailEvent>) {
    log_debug("imap_sync_all start");
    match fetch_imap_all(&imap, imap.initial_sync_days, imap.fetch_chunk_size) {
        Ok((folders, inbox_messages)) => {
            let _ = tx.send(MailEvent::ImapFolders(folders));
            let _ = tx.send(MailEvent::ImapMessages {
                folder_name: "INBOX".to_string(),
                messages: inbox_messages,
            });
        }
        Err(err) => {
            log_debug(&format!("imap_sync_all error {}", err));
            if is_imap_bye(&err) {
                log_debug("imap_sync_all retry after BYE");
                if let Ok((folders, inbox_messages)) =
                    fetch_imap_all(&imap, imap.initial_sync_days, imap.fetch_chunk_size)
                {
                    let _ = tx.send(MailEvent::ImapFolders(folders));
                    let _ = tx.send(MailEvent::ImapMessages {
                        folder_name: "INBOX".to_string(),
                        messages: inbox_messages,
                    });
                    return;
                }
            }
            let _ = tx.send(MailEvent::ImapError {
                reason: err.to_string(),
            });
        }
    }
}

fn sync_folder_imap(
    imap: ImapConfig,
    folder_name: String,
    mode: SyncMode,
    tx: mpsc::UnboundedSender<MailEvent>,
) {
    log_debug(&format!("imap_sync_folder start folder={}", folder_name));
    match fetch_imap_folder(&imap, &folder_name, mode.clone(), imap.fetch_chunk_size) {
        Ok(messages) => {
            let _ = tx.send(MailEvent::ImapMessages {
                folder_name,
                messages,
            });
        }
        Err(err) => {
            log_debug(&format!("imap_sync_folder error {}", err));
            if is_imap_bye(&err) {
                log_debug("imap_sync_folder retry after BYE");
                if let Ok(messages) =
                    fetch_imap_folder(&imap, &folder_name, mode, imap.fetch_chunk_size)
                {
                    let _ = tx.send(MailEvent::ImapMessages {
                        folder_name,
                        messages,
                    });
                    return;
                }
            }
            let _ = tx.send(MailEvent::ImapError {
                reason: err.to_string(),
            });
        }
    }
}

fn fetch_imap_all(
    imap: &ImapConfig,
    initial_sync_days: i64,
    fetch_chunk_size: usize,
) -> Result<(Vec<ImapFolder>, Vec<ImapMessageSummary>)> {
    log_debug(&format!(
        "imap_fetch_all connect host={} port={}",
        imap.host, imap.port
    ));
    let mut session = imap_connect(imap)?;
    log_debug("imap_fetch_all connected");
    let folders = fetch_imap_folders(&mut session)?;
    log_debug(&format!("imap_fetch_all folders count={}", folders.len()));
    let inbox_messages = fetch_imap_messages(
        &mut session,
        "INBOX",
        SyncMode::Initial {
            days: initial_sync_days,
        },
        fetch_chunk_size,
    )?;
    log_debug(&format!(
        "imap_fetch_all inbox messages count={}",
        inbox_messages.len()
    ));
    let _ = session.logout();
    log_debug("imap_fetch_all logout");
    Ok((folders, inbox_messages))
}

fn fetch_imap_folder(
    imap: &ImapConfig,
    folder: &str,
    mode: SyncMode,
    fetch_chunk_size: usize,
) -> Result<Vec<ImapMessageSummary>> {
    log_debug(&format!(
        "imap_fetch_folder connect host={} port={} folder={}",
        imap.host, imap.port, folder
    ));
    let mut session = imap_connect(imap)?;
    log_debug("imap_fetch_folder connected");
    let messages = fetch_imap_messages(&mut session, folder, mode, fetch_chunk_size)?;
    log_debug(&format!(
        "imap_fetch_folder messages count={} folder={}",
        messages.len(),
        folder
    ));
    let _ = session.logout();
    log_debug("imap_fetch_folder logout");
    Ok(messages)
}

fn imap_connect(imap: &ImapConfig) -> Result<imap::Session<imap::Connection>> {
    log_debug(&format!(
        "imap_connect start host={} port={}",
        imap.host, imap.port
    ));
    let client = ClientBuilder::new(imap.host.as_str(), imap.port)
        .tls_kind(imap::TlsKind::Native)
        .mode(ConnectionMode::AutoTls)
        .danger_skip_tls_verify(imap.skip_tls_verify)
        .connect()?;
    log_debug("imap_connect tcp connected");
    let session = client
        .login(&imap.username, &imap.password)
        .map_err(|e| e.0)?;
    log_debug("imap_connect login ok");
    Ok(session)
}

fn fetch_imap_folders(session: &mut imap::Session<imap::Connection>) -> Result<Vec<ImapFolder>> {
    let mut folders = Vec::new();
    let list = session.list(None, Some("*"))?;
    log_debug(&format!("imap_fetch_folders raw_count={}", list.len()));
    for folder in list.iter() {
        if folder
            .attributes()
            .iter()
            .any(|attr| matches!(attr, imap_proto::NameAttribute::NoSelect))
        {
            continue;
        }
        let name = folder.name().to_string();
        log_debug(&format!("imap_fetch_folders name={}", name));
        let unread = match session.status(&name, "(UNSEEN)") {
            Ok(status) => status.unseen.unwrap_or(0) as u32,
            Err(_) => continue,
        };
        folders.push(ImapFolder { name, unread });
    }
    Ok(folders)
}

fn fetch_imap_messages(
    session: &mut imap::Session<imap::Connection>,
    folder: &str,
    mode: SyncMode,
    fetch_chunk_size: usize,
) -> Result<Vec<ImapMessageSummary>> {
    log_debug(&format!("imap_fetch_messages select folder={}", folder));
    let mailbox = session.select(folder)?;
    let total = mailbox.exists;
    log_debug(&format!(
        "imap_fetch_messages mailbox folder={} exists={}",
        folder, total
    ));
    if total == 0 {
        return Ok(Vec::new());
    }
    let search_query = match mode {
        SyncMode::Incremental { last_seen_uid } => {
            format!("UID {}:*", last_seen_uid.saturating_add(1))
        }
        SyncMode::Backfill {
            before_ts,
            window_days,
        } => {
            let before = imap_date_from_ts(before_ts);
            let since = imap_date_from_ts(
                before_ts.saturating_sub(window_days.saturating_mul(24 * 60 * 60)),
            );
            format!("SINCE {} BEFORE {}", since, before)
        }
        SyncMode::Initial { days } => {
            let since = imap_search_since(days);
            format!("SINCE {}", since)
        }
    };
    log_debug(&format!(
        "imap_fetch_messages uid_list folder={} query={}",
        folder, search_query
    ));
    let uids = session.uid_search(&search_query)?;
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let mut uids_vec: Vec<u32> = uids.into_iter().collect();
    uids_vec.sort_unstable_by(|a, b| b.cmp(a));
    if uids_vec.is_empty() {
        return Ok(Vec::new());
    }
    let tail = &uids_vec[..];
    let mut messages = Vec::new();
    let chunk_size = fetch_chunk_size.max(1);
    for chunk in tail.chunks(chunk_size) {
        let uid_set = chunk
            .iter()
            .map(|uid| uid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        log_debug(&format!(
            "imap_fetch_messages uid_fetch folder={} count={}",
            folder,
            chunk.len()
        ));
        let fetches = session.uid_fetch(uid_set, "(UID FLAGS BODY.PEEK[HEADER])")?;
        for fetch in fetches.iter() {
            let uid = match fetch.uid {
                Some(uid) => uid,
                None => continue,
            };
            let headers = fetch.header().unwrap_or(&[]);
            let subject =
                header_value(headers, "Subject").unwrap_or_else(|| "(no subject)".to_string());
            let date = header_value(headers, "Date")
                .map(|d| format_date_display(&d))
                .unwrap_or_default();
            let from = header_value(headers, "From").unwrap_or_default();
            let unread = !fetch.flags().iter().any(|f| matches!(f, imap::types::Flag::Seen));
            messages.push(ImapMessageSummary {
                uid,
                date,
                from,
                subject: subject.clone(),
                unread,
                preview: subject,
            });
        }
    }
    messages.sort_by_key(|m| std::cmp::Reverse(parse_date_epoch(&m.date)));
    Ok(messages)
}

fn fetch_imap_body(imap: &ImapConfig, folder: &str, uid: u32) -> Result<Vec<u8>> {
    let mut session = imap_connect(imap)?;
    session.select(folder)?;
    let fetches = session.uid_fetch(uid.to_string(), "RFC822")?;
    let body = fetches
        .iter()
        .find_map(|f| f.body().map(|b| b.to_vec()))
        .ok_or_else(|| anyhow!("No body found for UID {}", uid))?;
    let _ = session.logout();
    Ok(body)
}


fn header_value(raw: &[u8], name: &str) -> Option<String> {
    let (headers, _) = mailparse::parse_headers(raw).ok()?;
    for header in headers.iter() {
        if header.get_key_ref().eq_ignore_ascii_case(name) {
            return Some(header.get_value());
        }
    }
    None
}

fn imap_search_since(days_back: i64) -> String {
    let target = Local::now() - Duration::days(days_back);
    imap_date_from_parts(target.year(), target.month(), target.day())
}

fn parse_date_epoch(date: &str) -> i64 {
    mailparse::dateparse(date).unwrap_or(0)
}

fn is_imap_bye(err: &anyhow::Error) -> bool {
    err.to_string().to_lowercase().contains("bye response")
}

fn format_date_display(raw: &str) -> String {
    let trimmed = raw.trim();
    let ok = mailparse::dateparse(trimmed).is_ok();
    if !ok {
        return trimmed.to_string();
    }
    if let Some((dow, remainder)) = trimmed.split_once(',') {
        let remainder = remainder.trim_start();
        let mut parts = remainder.split_whitespace().collect::<Vec<_>>();
        if let Some(day) = parts.get_mut(0) {
            *day = day.trim_start_matches('0');
            if day.is_empty() {
                *day = "0";
            }
        }
        return format!("{}, {}", dow, parts.join(" "));
    }
    trimmed.to_string()
}

fn imap_date_from_ts(ts: i64) -> String {
    let dt = Local.timestamp_opt(ts, 0).single().unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap());
    imap_date_from_parts(dt.year(), dt.month(), dt.day())
}

fn imap_date_from_parts(year: i32, month: u32, day: u32) -> String {
    let month = match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Jan",
    };
    format!("{}-{}-{}", day, month, year)
}
