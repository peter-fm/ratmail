//! Mail protocol integration (IMAP/SMTP) skeleton.

use anyhow::{anyhow, Result};
use lettre::{
    message::{Mailbox, Message},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum MailCommand {
    SyncFolder(i64),
    FetchMessageBody(i64),
    SetFlag { message_id: i64, seen: bool },
    SendMessage {
        to: String,
        subject: String,
        body: String,
    },
}

#[derive(Debug, Clone)]
pub enum MailEvent {
    SyncStarted(i64),
    SyncCompleted(i64),
    SyncFailed { folder_id: i64, reason: String },
    BodyFetched(i64),
    FlagUpdated { message_id: i64, seen: bool },
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

#[derive(Clone)]
pub struct MailEngine {
    tx: mpsc::UnboundedSender<MailCommand>,
}

impl MailEngine {
    pub fn start(smtp: Option<SmtpConfig>) -> (Self, mpsc::UnboundedReceiver<MailEvent>) {
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
                    MailCommand::FetchMessageBody(message_id) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        let _ = evt_tx.send(MailEvent::BodyFetched(message_id));
                    }
                    MailCommand::SetFlag { message_id, seen } => {
                        let _ = evt_tx.send(MailEvent::FlagUpdated { message_id, seen });
                    }
                    MailCommand::SendMessage { to, subject, body } => {
                        let _ = evt_tx.send(MailEvent::SendStarted);
                        let result = send_smtp(smtp.clone(), &to, &subject, &body).await;
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
    subject: &str,
    body: &str,
) -> Result<()> {
    let smtp = smtp.ok_or_else(|| anyhow!("SMTP not configured"))?;
    let from_addr = parse_mailbox(&smtp.from)?;
    let to_addr = parse_mailbox(to)?;

    let email = Message::builder()
        .from(from_addr)
        .to(to_addr)
        .subject(subject)
        .body(body.to_string())?;

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
