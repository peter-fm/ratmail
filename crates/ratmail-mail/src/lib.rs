//! Mail protocol integration (IMAP/SMTP) skeleton.

use anyhow::{anyhow, Result};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, Message, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};
use mailparse::{addrparse, MailAddr};
use imap::{ClientBuilder, ConnectionMode};
use chrono::{Datelike, Duration, Local};
use imap_proto::types::Address as ProtoAddress;
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
    SyncFolderByName { name: String },
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
                    MailCommand::SyncFolderByName { name } => {
                        if let Some(imap) = imap.clone() {
                            let tx = evt_tx.clone();
                            tokio::task::spawn_blocking(move || sync_folder_imap(imap, name, tx));
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
    match fetch_imap_all(&imap) {
        Ok((folders, inbox_messages)) => {
            let _ = tx.send(MailEvent::ImapFolders(folders));
            let _ = tx.send(MailEvent::ImapMessages {
                folder_name: "INBOX".to_string(),
                messages: inbox_messages,
            });
        }
        Err(err) => {
            log_debug(&format!("imap_sync_all error {}", err));
            let _ = tx.send(MailEvent::ImapError {
                reason: err.to_string(),
            });
        }
    }
}

fn sync_folder_imap(
    imap: ImapConfig,
    folder_name: String,
    tx: mpsc::UnboundedSender<MailEvent>,
) {
    log_debug(&format!("imap_sync_folder start folder={}", folder_name));
    match fetch_imap_folder(&imap, &folder_name) {
        Ok(messages) => {
            let _ = tx.send(MailEvent::ImapMessages {
                folder_name,
                messages,
            });
        }
        Err(err) => {
            log_debug(&format!("imap_sync_folder error {}", err));
            let _ = tx.send(MailEvent::ImapError {
                reason: err.to_string(),
            });
        }
    }
}

fn fetch_imap_all(imap: &ImapConfig) -> Result<(Vec<ImapFolder>, Vec<ImapMessageSummary>)> {
    log_debug(&format!(
        "imap_fetch_all connect host={} port={}",
        imap.host, imap.port
    ));
    let mut session = imap_connect(imap)?;
    log_debug("imap_fetch_all connected");
    let folders = fetch_imap_folders(&mut session)?;
    log_debug(&format!("imap_fetch_all folders count={}", folders.len()));
    let inbox_messages = fetch_imap_messages(&mut session, "INBOX")?;
    log_debug(&format!(
        "imap_fetch_all inbox messages count={}",
        inbox_messages.len()
    ));
    let _ = session.logout();
    log_debug("imap_fetch_all logout");
    Ok((folders, inbox_messages))
}

fn fetch_imap_folder(imap: &ImapConfig, folder: &str) -> Result<Vec<ImapMessageSummary>> {
    log_debug(&format!(
        "imap_fetch_folder connect host={} port={} folder={}",
        imap.host, imap.port, folder
    ));
    let mut session = imap_connect(imap)?;
    log_debug("imap_fetch_folder connected");
    let messages = fetch_imap_messages(&mut session, folder)?;
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
    let since = imap_search_since(365);
    let search_query = format!("SINCE {}", since);
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
    for chunk in tail.chunks(10) {
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
            let date = header_value(headers, "Date").unwrap_or_default();
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

fn format_imap_address(addr: &ProtoAddress) -> String {
    let name = addr
        .name
        .as_ref()
        .map(|s| decode_header_value(s))
        .unwrap_or_default();
    let mailbox = addr
        .mailbox
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_default();
    let host = addr
        .host
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).to_string())
        .unwrap_or_default();
    let email = if mailbox.is_empty() || host.is_empty() {
        String::new()
    } else {
        format!("{}@{}", mailbox, host)
    };
    if !name.is_empty() && !email.is_empty() {
        format!("{} <{}>", name, email)
    } else if !email.is_empty() {
        email
    } else {
        name
    }
}

fn decode_header_value(raw: &[u8]) -> String {
    let mut buf = Vec::with_capacity(raw.len() + 3);
    buf.extend_from_slice(b"X: ");
    buf.extend_from_slice(raw);
    match mailparse::parse_header(&buf) {
        Ok((header, _)) => header.get_value(),
        Err(_) => String::from_utf8_lossy(raw).to_string(),
    }
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
    let month = match target.month() {
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
    format!("{}-{}-{}", target.day(), month, target.year())
}

fn parse_date_epoch(date: &str) -> i64 {
    mailparse::dateparse(date).unwrap_or(0)
}
