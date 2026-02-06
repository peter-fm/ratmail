use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use mailparse::dateparse;
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
pub struct FolderSyncState {
    pub folder_id: i64,
    pub uidvalidity: Option<i64>,
    pub uidnext: Option<i64>,
    pub last_seen_uid: Option<i64>,
    pub last_sync_ts: Option<i64>,
    pub oldest_ts: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: i64,
    pub folder_id: i64,
    pub imap_uid: Option<u32>,
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
    pub links: Vec<String>,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreSnapshot {
    pub account: Account,
    pub folders: Vec<Folder>,
    pub messages: Vec<MessageSummary>,
    pub message_details: HashMap<i64, MessageDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime: String,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileMeta {
    pub tile_index: i64,
    pub height_px: i64,
    pub bytes: Vec<u8>,
}

pub const DEFAULT_TEXT_WIDTH: i64 = 80;

fn parse_date_ts(date: &str) -> i64 {
    dateparse(date).unwrap_or(0)
}

#[async_trait]
pub trait MailStore: Send + Sync {
    async fn load_snapshot(&self, account_id: i64, folder_id: i64) -> Result<StoreSnapshot>;
    async fn get_raw_body(&self, message_id: i64) -> Result<Option<Vec<u8>>>;
    async fn upsert_raw_body(&self, message_id: i64, raw: &[u8]) -> Result<()>;
    async fn upsert_cache_text(&self, message_id: i64, width_cols: i64, text: &str) -> Result<()>;
    async fn get_cache_html(&self, message_id: i64, remote_policy: &str) -> Result<Option<String>>;
    async fn upsert_cache_html(&self, message_id: i64, remote_policy: &str, html: &str)
        -> Result<()>;
    async fn get_cache_tiles(
        &self,
        message_id: i64,
        width_px: i64,
        tile_height_px: i64,
        theme: &str,
        remote_policy: &str,
    ) -> Result<Vec<TileMeta>>;
    async fn upsert_cache_tiles(
        &self,
        message_id: i64,
        width_px: i64,
        tile_height_px: i64,
        theme: &str,
        remote_policy: &str,
        tiles: &[TileMeta],
    ) -> Result<()>;
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

    pub async fn upsert_account(&self, id: i64, name: &str, address: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO accounts (id, name, address) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, address = excluded.address",
        )
        .bind(id)
        .bind(name)
        .bind(address)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_account_data(&self, account_id: i64) -> Result<()> {
        let message_ids: Vec<i64> = sqlx::query_as::<_, (i64,)>(
            "SELECT id FROM messages WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| row.0)
        .collect();

        if !message_ids.is_empty() {
            let placeholders = message_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            for table in ["bodies", "cache_text", "cache_html", "cache_tiles"] {
                let query = format!("DELETE FROM {} WHERE message_id IN ({})", table, placeholders);
                let mut q = sqlx::query(&query);
                for id in &message_ids {
                    q = q.bind(id);
                }
                q.execute(&self.pool).await?;
            }
        }

        sqlx::query("DELETE FROM messages WHERE account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM folders WHERE account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_folders(
        &self,
        account_id: i64,
        folders: &[Folder],
    ) -> Result<Vec<Folder>> {
        let existing = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, name FROM folders WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let mut by_name: HashMap<String, i64> = HashMap::new();
        for (id, name) in existing {
            by_name.insert(name, id);
        }

        let mut kept_names: Vec<String> = Vec::new();
        let mut output = Vec::new();
        for folder in folders {
            let id = if let Some(id) = by_name.get(&folder.name) {
                sqlx::query("UPDATE folders SET unread = ? WHERE id = ?")
                    .bind(folder.unread as i64)
                    .bind(*id)
                    .execute(&self.pool)
                    .await?;
                *id
            } else {
                let result = sqlx::query(
                    "INSERT INTO folders (account_id, name, unread) VALUES (?, ?, ?)",
                )
                .bind(account_id)
                .bind(&folder.name)
                .bind(folder.unread as i64)
                .execute(&self.pool)
                .await?;
                result.last_insert_rowid()
            };
            kept_names.push(folder.name.clone());
            output.push(Folder {
                id,
                account_id,
                name: folder.name.clone(),
                unread: folder.unread,
            });
        }

        if kept_names.is_empty() {
            sqlx::query("DELETE FROM folders WHERE account_id = ?")
                .bind(account_id)
                .execute(&self.pool)
                .await?;
        } else {
            let placeholders = kept_names.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let query = format!(
                "DELETE FROM folders WHERE account_id = ? AND name NOT IN ({})",
                placeholders
            );
            let mut q = sqlx::query(&query).bind(account_id);
            for name in &kept_names {
                q = q.bind(name);
            }
            q.execute(&self.pool).await?;
        }

        Ok(output)
    }

    pub async fn replace_folder_messages(
        &self,
        account_id: i64,
        folder_id: i64,
        messages: &[MessageSummary],
    ) -> Result<()> {
        let existing: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT id, imap_uid FROM messages WHERE folder_id = ?",
        )
        .bind(folder_id)
        .fetch_all(&self.pool)
        .await?;

        let incoming_uids: Vec<i64> = messages
            .iter()
            .filter_map(|m| m.imap_uid.map(|v| v as i64))
            .collect();

        if !existing.is_empty() {
            let mut to_delete = Vec::new();
            for (id, uid) in existing {
                if let Some(uid) = uid {
                    if !incoming_uids.contains(&uid) {
                        to_delete.push(id);
                    }
                } else {
                    to_delete.push(id);
                }
            }

            if !to_delete.is_empty() {
                let placeholders = to_delete.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                for table in ["bodies", "cache_text", "cache_html", "cache_tiles"] {
                    let query = format!(
                        "DELETE FROM {} WHERE message_id IN ({})",
                        table, placeholders
                    );
                    let mut q = sqlx::query(&query);
                    for id in &to_delete {
                        q = q.bind(id);
                    }
                    q.execute(&self.pool).await?;
                }
                let query = format!(
                    "DELETE FROM messages WHERE id IN ({})",
                    placeholders
                );
                let mut q = sqlx::query(&query);
                for id in &to_delete {
                    q = q.bind(id);
                }
                q.execute(&self.pool).await?;
            }
        }

        for msg in messages {
            let uid = msg.imap_uid.map(|v| v as i64);

            // Check if message exists by folder_id + imap_uid
            // Note: ON CONFLICT doesn't work with partial unique indexes (migration 006)
            let existing_id: Option<i64> = if let Some(uid_val) = uid {
                sqlx::query_as::<_, (i64,)>(
                    "SELECT id FROM messages WHERE folder_id = ? AND imap_uid = ?",
                )
                .bind(folder_id)
                .bind(uid_val)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.0)
            } else {
                None
            };

            if let Some(existing_id) = existing_id {
                // UPDATE existing message
                sqlx::query(
                    "UPDATE messages SET date = ?, from_addr = ?, subject = ?, unread = ?, preview = ?
                     WHERE id = ?",
                )
                .bind(&msg.date)
                .bind(&msg.from)
                .bind(&msg.subject)
                .bind(if msg.unread { 1 } else { 0 })
                .bind(&msg.preview)
                .bind(existing_id)
                .execute(&self.pool)
                .await?;
            } else {
                // INSERT new message
                sqlx::query(
                    "INSERT INTO messages (account_id, folder_id, imap_uid, date, from_addr, subject, unread, preview)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(account_id)
                .bind(folder_id)
                .bind(uid)
                .bind(&msg.date)
                .bind(&msg.from)
                .bind(&msg.subject)
                .bind(if msg.unread { 1 } else { 0 })
                .bind(&msg.preview)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn upsert_folder_messages_append(
        &self,
        account_id: i64,
        folder_id: i64,
        messages: &[MessageSummary],
    ) -> Result<()> {
        for msg in messages {
            let uid = msg.imap_uid.map(|v| v as i64);
            let exists = if let Some(uid) = uid {
                sqlx::query_as::<_, (i64,)>(
                    "SELECT id FROM messages WHERE folder_id = ? AND imap_uid = ?",
                )
                .bind(folder_id)
                .bind(uid)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.0)
            } else {
                None
            };
            if let Some(id) = exists {
                sqlx::query(
                    "UPDATE messages SET date = ?, date_ts = ?, from_addr = ?, subject = ?, unread = ?, preview = ?
                     WHERE id = ?",
                )
                .bind(&msg.date)
                .bind(parse_date_ts(&msg.date))
                .bind(&msg.from)
                .bind(&msg.subject)
                .bind(if msg.unread { 1 } else { 0 })
                .bind(&msg.preview)
                .bind(id)
                .execute(&self.pool)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO messages (account_id, folder_id, imap_uid, date, date_ts, from_addr, subject, unread, preview)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(account_id)
                .bind(folder_id)
                .bind(uid)
                .bind(&msg.date)
                .bind(parse_date_ts(&msg.date))
                .bind(&msg.from)
                .bind(&msg.subject)
                .bind(if msg.unread { 1 } else { 0 })
                .bind(&msg.preview)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn folder_id_by_name(&self, account_id: i64, name: &str) -> Result<Option<i64>> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT id FROM folders WHERE account_id = ? AND name = ?",
        )
        .bind(account_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn first_folder_id(&self, account_id: i64) -> Result<Option<i64>> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT id FROM folders WHERE account_id = ? ORDER BY id LIMIT 1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn get_folder_sync_state(
        &self,
        folder_id: i64,
    ) -> Result<Option<FolderSyncState>> {
        let row = sqlx::query_as::<_, (i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)>(
            "SELECT folder_id, uidvalidity, uidnext, last_seen_uid, last_sync_ts, oldest_ts
             FROM folder_sync_state WHERE folder_id = ?",
        )
        .bind(folder_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| FolderSyncState {
            folder_id: r.0,
            uidvalidity: r.1,
            uidnext: r.2,
            last_seen_uid: r.3,
            last_sync_ts: r.4,
            oldest_ts: r.5,
        }))
    }

    pub async fn upsert_folder_sync_state(
        &self,
        state: &FolderSyncState,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO folder_sync_state
             (folder_id, uidvalidity, uidnext, last_seen_uid, last_sync_ts, oldest_ts)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(folder_id) DO UPDATE SET
               uidvalidity = excluded.uidvalidity,
               uidnext = excluded.uidnext,
               last_seen_uid = excluded.last_seen_uid,
               last_sync_ts = excluded.last_sync_ts,
               oldest_ts = excluded.oldest_ts",
        )
        .bind(state.folder_id)
        .bind(state.uidvalidity)
        .bind(state.uidnext)
        .bind(state.last_seen_uid)
        .bind(state.last_sync_ts)
        .bind(state.oldest_ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn seed_demo_if_empty(&self) -> Result<()> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
            .fetch_one(&self.pool)
            .await?;
        if count.0 == 0 {
            sqlx::query(
                "INSERT INTO accounts (id, name, address) VALUES (1, ?, ?)",
            )
            .bind("personal@example.com")
            .bind("personal@example.com")
            .execute(&self.pool)
            .await?;
        } else {
            let demo = sqlx::query_as::<_, (String,)>(
                "SELECT name FROM accounts WHERE id = 1",
            )
            .fetch_one(&self.pool)
            .await?
            .0 == "personal@example.com";
            if demo {
                sqlx::query("DELETE FROM cache_tiles").execute(&self.pool).await?;
                sqlx::query("DELETE FROM cache_html").execute(&self.pool).await?;
                sqlx::query("DELETE FROM cache_text").execute(&self.pool).await?;
                sqlx::query("DELETE FROM bodies").execute(&self.pool).await?;
                sqlx::query("DELETE FROM messages").execute(&self.pool).await?;
                sqlx::query("DELETE FROM folders").execute(&self.pool).await?;
            }
        }

        let folders = vec![
            (1, "INBOX", 42),
            (2, "Sent", 0),
            (3, "Drafts", 1),
            (4, "Archive", 0),
        ];

        for (id, name, unread) in folders {
            sqlx::query(
                "INSERT INTO folders (id, account_id, name, unread) VALUES (?, 1, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, unread = excluded.unread",
            )
            .bind(id)
            .bind(name)
            .bind(unread)
            .execute(&self.pool)
            .await?;
        }

        let messages = vec![
            (1, 1, "2026-02-03 10:31", "Alex Chen", "Re: Proposal", 1, "Thanks - attached is the updated..."),
            (2, 1, "2026-02-03 09:58", "GitHub", "Security alert", 1, "We detected a new sign-in..."),
            (3, 1, "2026-02-03 09:12", "HR", "Benefits 2026", 0, "Open enrollment starts..."),
            (4, 1, "2026-02-03 08:44", "Newsletter", "Weekly digest", 1, "Top stories this week..."),
            (5, 1, "2026-02-02 17:22", "Billing", "Invoice #1931", 0, "Your invoice is ready..."),
            (6, 1, "2026-02-02 14:03", "Sam", "Lunch?", 0, "Want to grab lunch..."),
            (7, 3, "2026-02-03 11:11", "Me", "Draft: Proposal follow-up", 0, "Draft message..."),
        ];

        for (id, folder_id, date, from, subject, unread, preview) in messages {
            sqlx::query(
                "INSERT INTO messages (id, account_id, folder_id, imap_uid, date, date_ts, from_addr, subject, unread, preview)
                 VALUES (?, 1, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET folder_id = excluded.folder_id, date = excluded.date, date_ts = excluded.date_ts,
                 from_addr = excluded.from_addr, subject = excluded.subject, unread = excluded.unread,
                 preview = excluded.preview",
            )
            .bind(id)
            .bind(folder_id)
            .bind(None::<i64>)
            .bind(date)
            .bind(parse_date_ts(date))
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
            (7, "Draft message..."),
        ];

        for (message_id, body) in bodies {
            sqlx::query(
                "INSERT INTO cache_text (message_id, width_cols, text, updated_at)
                 VALUES (?, ?, ?, '2026-02-03T12:00:00Z')
                 ON CONFLICT(message_id, width_cols)
                 DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
            )
            .bind(message_id)
            .bind(DEFAULT_TEXT_WIDTH)
            .bind(body)
            .execute(&self.pool)
            .await?;
        }

        let raw_messages = vec![
            (
                1,
                "From: Alex Chen <alex@example.com>\r\n\
Subject: Re: Proposal\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary42\"\r\n\
\r\n\
--boundary42\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Thanks - this looks good overall.\r\n\
\r\n\
I've added comments to section 3 regarding timelines.\r\n\
\r\n\
--boundary42\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"proposal-v3.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQKJcTl8uXr\r\n\
--boundary42--\r\n",
            ),
            (
                2,
                "From: GitHub <security@example.com>\r\n\
Subject: Security alert\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body>\r\n\
<p>We detected a new sign-in to your account.</p>\r\n\
<a href=\"https://github.com/settings/security\">Review security settings</a>\r\n\
</body></html>\r\n",
            ),
            (
                3,
                "From: HR <hr@example.com>\r\n\
Subject: Benefits 2026\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Open enrollment starts next week. Please review the benefits guide.\r\n",
            ),
            (
                4,
                "From: Newsletter <news@example.com>\r\n\
Subject: Weekly digest\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body>\r\n\
<p>Top stories this week.</p>\r\n\
<a href=\"https://news.example.com/story\">Read more</a>\r\n\
</body></html>\r\n",
            ),
            (
                5,
                "From: Billing <billing@example.com>\r\n\
Subject: Invoice #1931\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your invoice is ready. Please remit payment by the due date.\r\n",
            ),
            (
                6,
                "From: Sam <sam@example.com>\r\n\
Subject: Lunch?\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Want to grab lunch today? I am free around noon.\r\n",
            ),
            (
                7,
                "From: Me <me@example.com>\r\n\
Subject: Draft: Proposal follow-up\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Draft message...\r\n",
            ),
        ];

        for (message_id, raw) in raw_messages {
            sqlx::query(
                "INSERT INTO bodies (message_id, raw_bytes) VALUES (?, ?)
                 ON CONFLICT(message_id) DO UPDATE SET raw_bytes = excluded.raw_bytes",
            )
            .bind(message_id)
            .bind(raw.as_bytes())
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    pub async fn touch_cache_tiles(
        &self,
        message_id: i64,
        width_px: i64,
        tile_height_px: i64,
        theme: &str,
        remote_policy: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE cache_tiles SET updated_at = datetime('now')
             WHERE message_id = ? AND width_px = ? AND tile_height_px = ? AND theme = ? AND remote_policy = ?",
        )
        .bind(message_id)
        .bind(width_px)
        .bind(tile_height_px)
        .bind(theme)
        .bind(remote_policy)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cache_tiles_total_bytes(&self) -> Result<i64> {
        let row = sqlx::query_as::<_, (Option<i64>,)>(
            "SELECT SUM(LENGTH(png_bytes)) FROM cache_tiles",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0.unwrap_or(0))
    }

    pub async fn prune_cache_tiles(&self, max_bytes: i64) -> Result<()> {
        let mut total = self.cache_tiles_total_bytes().await?;
        if total <= max_bytes {
            return Ok(());
        }

        while total > max_bytes {
            let deleted = sqlx::query(
                "DELETE FROM cache_tiles
                 WHERE rowid IN (
                    SELECT rowid FROM cache_tiles
                    ORDER BY updated_at ASC
                    LIMIT 50
                 )",
            )
            .execute(&self.pool)
            .await?
            .rows_affected() as i64;

            if deleted == 0 {
                break;
            }

            total = self.cache_tiles_total_bytes().await?;
        }
        Ok(())
    }
}

#[async_trait]
impl MailStore for SqliteMailStore {
    async fn load_snapshot(&self, account_id: i64, _folder_id: i64) -> Result<StoreSnapshot> {
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

        let messages = sqlx::query_as::<_, (i64, i64, Option<i64>, String, String, String, i64, String)>(
            "SELECT id, folder_id, imap_uid, date, from_addr, subject, unread, preview
             FROM messages WHERE account_id = ? ORDER BY COALESCE(date_ts, 0) DESC, id DESC",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;

        let message_ids: Vec<i64> = messages.iter().map(|row| row.0).collect();
        let mut message_details = HashMap::new();

        for message_id in message_ids {
            if let Ok((subject, from, date, body)) = sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT m.subject, m.from_addr, m.date, c.text
                 FROM messages m
                 LEFT JOIN cache_text c ON c.message_id = m.id AND c.width_cols = ?
                 WHERE m.id = ?",
            )
            .bind(DEFAULT_TEXT_WIDTH)
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
                        links: Vec::new(),
                        attachments: Vec::new(),
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
                    imap_uid: row.2.map(|v| v as u32),
                    date: row.3,
                    from: row.4,
                    subject: row.5,
                    unread: row.6 != 0,
                    preview: row.7,
                })
                .collect(),
            message_details,
        })
    }

    async fn get_raw_body(&self, message_id: i64) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT raw_bytes FROM bodies WHERE message_id = ?",
        )
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    async fn upsert_raw_body(&self, message_id: i64, raw: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO bodies (message_id, raw_bytes) VALUES (?, ?)
             ON CONFLICT(message_id) DO UPDATE SET raw_bytes = excluded.raw_bytes",
        )
        .bind(message_id)
        .bind(raw)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn upsert_cache_text(&self, message_id: i64, width_cols: i64, text: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO cache_text (message_id, width_cols, text, updated_at)
             VALUES (?, ?, ?, datetime('now'))
             ON CONFLICT(message_id, width_cols)
             DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
        )
        .bind(message_id)
        .bind(width_cols)
        .bind(text)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_cache_html(&self, message_id: i64, remote_policy: &str) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT prepared_html FROM cache_html WHERE message_id = ? AND remote_policy = ?",
        )
        .bind(message_id)
        .bind(remote_policy)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    async fn upsert_cache_html(
        &self,
        message_id: i64,
        remote_policy: &str,
        html: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO cache_html (message_id, remote_policy, prepared_html, updated_at)
             VALUES (?, ?, ?, datetime('now'))
             ON CONFLICT(message_id, remote_policy)
             DO UPDATE SET prepared_html = excluded.prepared_html, updated_at = excluded.updated_at",
        )
        .bind(message_id)
        .bind(remote_policy)
        .bind(html)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_cache_tiles(
        &self,
        message_id: i64,
        width_px: i64,
        tile_height_px: i64,
        theme: &str,
        remote_policy: &str,
    ) -> Result<Vec<TileMeta>> {
        let rows = sqlx::query_as::<_, (i64, i64, Vec<u8>)>(
            "SELECT tile_index, tile_height_px, png_bytes
             FROM cache_tiles
             WHERE message_id = ? AND width_px = ? AND tile_height_px = ? AND theme = ? AND remote_policy = ?
             ORDER BY tile_index",
        )
        .bind(message_id)
        .bind(width_px)
        .bind(tile_height_px)
        .bind(theme)
        .bind(remote_policy)
        .fetch_all(&self.pool)
        .await?;

        if !rows.is_empty() {
            self.touch_cache_tiles(message_id, width_px, tile_height_px, theme, remote_policy)
                .await?;
        }

        Ok(rows
            .into_iter()
            .map(|row| TileMeta {
                tile_index: row.0,
                height_px: row.1,
                bytes: row.2,
            })
            .collect())
    }

    async fn upsert_cache_tiles(
        &self,
        message_id: i64,
        width_px: i64,
        tile_height_px: i64,
        theme: &str,
        remote_policy: &str,
        tiles: &[TileMeta],
    ) -> Result<()> {
        for tile in tiles {
            sqlx::query(
                "INSERT INTO cache_tiles (message_id, width_px, tile_height_px, theme, remote_policy, tile_index, png_bytes, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now'))
                 ON CONFLICT(message_id, width_px, tile_height_px, theme, remote_policy, tile_index)
                 DO UPDATE SET png_bytes = excluded.png_bytes, updated_at = excluded.updated_at",
            )
            .bind(message_id)
            .bind(width_px)
            .bind(tile_height_px)
            .bind(theme)
            .bind(remote_policy)
            .bind(tile.tile_index)
            .bind(&tile.bytes)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }
}
