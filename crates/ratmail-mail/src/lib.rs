//! Mail protocol integration (IMAP/SMTP) skeleton.

use anyhow::Result;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum MailCommand {
    SyncFolder(i64),
    FetchMessageBody(i64),
    SetFlag { message_id: i64, seen: bool },
}

#[derive(Debug, Clone)]
pub enum MailEvent {
    SyncStarted(i64),
    SyncCompleted(i64),
    SyncFailed { folder_id: i64, reason: String },
    BodyFetched(i64),
    FlagUpdated { message_id: i64, seen: bool },
}

#[derive(Clone)]
pub struct MailEngine {
    tx: mpsc::UnboundedSender<MailCommand>,
}

impl MailEngine {
    pub fn start() -> (Self, mpsc::UnboundedReceiver<MailEvent>) {
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
