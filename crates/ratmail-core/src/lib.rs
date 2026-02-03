use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, SqlitePool};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: i64,
    pub folder_id: i64,
    pub date: String,
    pub from: String,
    pub subject: String,
    pub unread: bool,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDetail {
    pub id: i64,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreSnapshot {
    pub account: Account,
    pub folders: Vec<Folder>,
    pub messages: Vec<MessageSummary>,
    pub message_details: HashMap<i64, MessageDetail>,
}

#[async_trait]
pub trait MailStore: Send + Sync {
    async fn load_snapshot(&self, account_id: i64, folder_id: i64) -> Result<StoreSnapshot>;
}

#[derive(Clone)]
pub struct SqliteMailStore {
    pool: SqlitePool,
}

impl SqliteMailStore {
    pub async fn connect(path: &str) -> Result<Self> {
        let url = if path.starts_with("sqlite:") {
            path.to_string()
        } else {
            format!("sqlite:{}", path)
        };
        let options = SqliteConnectOptions::new()
            .filename(url.trim_start_matches("sqlite:"))
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub async fn init(&self) -> Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }

    pub async fn seed_demo_if_empty(&self) -> Result<()> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
            .fetch_one(&self.pool)
            .await?;
        if count.0 > 0 {
            return Ok(());
        }

        sqlx::query(
            "INSERT INTO accounts (id, name, address) VALUES (1, ?, ?)",
        )
        .bind("personal@example.com")
        .bind("personal@example.com")
        .execute(&self.pool)
        .await?;

        let folders = vec![
            (1, "INBOX", 42),
            (2, "Today", 0),
            (3, "Starred", 0),
            (4, "Sent", 0),
            (5, "Drafts", 0),
            (6, "Archive", 0),
            (7, "Work/INBOX", 3),
            (8, "Work/Sent", 0),
        ];

        for (id, name, unread) in folders {
            sqlx::query(
                "INSERT INTO folders (id, account_id, name, unread) VALUES (?, 1, ?, ?)",
            )
            .bind(id)
            .bind(name)
            .bind(unread)
            .execute(&self.pool)
            .await?;
        }

        let messages = vec![
            (1, "2026-02-03 10:31", "Alex Chen", "Re: Proposal", 1, "Thanks - attached is the updated..."),
            (2, "2026-02-03 09:58", "GitHub", "Security alert", 1, "We detected a new sign-in..."),
            (3, "2026-02-03 09:12", "HR", "Benefits 2026", 0, "Open enrollment starts..."),
            (4, "2026-02-03 08:44", "Newsletter", "Weekly digest", 1, "Top stories this week..."),
            (5, "2026-02-02 17:22", "Billing", "Invoice #1931", 0, "Your invoice is ready..."),
            (6, "2026-02-02 14:03", "Sam", "Lunch?", 0, "Want to grab lunch..."),
        ];

        for (id, date, from, subject, unread, preview) in messages {
            sqlx::query(
                "INSERT INTO messages (id, account_id, folder_id, date, from_addr, subject, unread, preview)
                 VALUES (?, 1, 1, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(date)
            .bind(from)
            .bind(subject)
            .bind(unread)
            .bind(preview)
            .execute(&self.pool)
            .await?;
        }

        let bodies = vec![
            (1, "Thanks - this looks good overall.\n\nI've added comments to section 3 regarding timelines."),
            (2, "We detected a new sign-in to your account. If this was you, no action is needed."),
            (3, "Open enrollment starts next week. Please review the benefits guide."),
            (4, "Here is your weekly digest. Top stories and updates inside."),
            (5, "Your invoice is ready. Please remit payment by the due date."),
            (6, "Want to grab lunch today? I am free around noon."),
        ];

        for (message_id, body) in bodies {
            sqlx::query(
                "INSERT INTO cache_text (message_id, width_cols, text, updated_at)
                 VALUES (?, 0, ?, '2026-02-03T12:00:00Z')",
            )
            .bind(message_id)
            .bind(body)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }
}

#[async_trait]
impl MailStore for SqliteMailStore {
    async fn load_snapshot(&self, account_id: i64, folder_id: i64) -> Result<StoreSnapshot> {
        let account = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, name, address FROM accounts WHERE id = ?",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;

        let folders = sqlx::query_as::<_, (i64, i64, String, i64)>(
            "SELECT id, account_id, name, unread FROM folders WHERE account_id = ? ORDER BY id",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let messages = sqlx::query_as::<_, (i64, i64, String, String, String, i64, String)>(
            "SELECT id, folder_id, date, from_addr, subject, unread, preview
             FROM messages WHERE folder_id = ? ORDER BY id",
        )
        .bind(folder_id)
        .fetch_all(&self.pool)
        .await?;

        let message_ids: Vec<i64> = messages.iter().map(|row| row.0).collect();
        let mut message_details = HashMap::new();

        for message_id in message_ids {
            if let Ok((subject, from, date, body)) = sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT m.subject, m.from_addr, m.date, c.text
                 FROM messages m
                 LEFT JOIN cache_text c ON c.message_id = m.id AND c.width_cols = 0
                 WHERE m.id = ?",
            )
            .bind(message_id)
            .fetch_one(&self.pool)
            .await
            {
                message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject,
                        from,
                        date,
                        body,
                    },
                );
            }
        }

        Ok(StoreSnapshot {
            account: Account {
                id: account.0,
                name: account.1,
                address: account.2,
            },
            folders: folders
                .into_iter()
                .map(|row| Folder {
                    id: row.0,
                    account_id: row.1,
                    name: row.2,
                    unread: row.3 as u32,
                })
                .collect(),
            messages: messages
                .into_iter()
                .map(|row| MessageSummary {
                    id: row.0,
                    folder_id: row.1,
                    date: row.2,
                    from: row.3,
                    subject: row.4,
                    unread: row.5 != 0,
                    preview: row.6,
                })
                .collect(),
            message_details,
        })
    }
}
