use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Local;
use mailparse::dateparse;
use serde::{Deserialize, Serialize};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions};

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
    pub to: String,
    pub cc: String,
    pub date: String,
    pub body: String,
    pub links: Vec<LinkInfo>,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkInfo {
    pub url: String,
    pub text: Option<String>,
    pub from_html: bool,
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
static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

pub fn log_debug(msg: &str) {
    if std::env::var("RATMAIL_LOG").is_err() {
        return;
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = base.join("ratmail").join("ratmail.log");
    let lock = LOG_FILE.get_or_init(|| {
        let _ = std::fs::create_dir_all(
            path.parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp")),
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

fn parse_date_ts(date: &str) -> i64 {
    dateparse(date).unwrap_or(0)
}

fn draft_preview(body: &str) -> String {
    let first = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    first.trim().chars().take(200).collect()
}

fn draft_raw(
    from_addr: &str,
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    body: &str,
    date_rfc2822: String,
) -> Vec<u8> {
    let mut raw = String::new();
    raw.push_str(&format!("From: {}\r\n", from_addr));
    if !to.trim().is_empty() {
        raw.push_str(&format!("To: {}\r\n", to.trim()));
    }
    if !cc.trim().is_empty() {
        raw.push_str(&format!("Cc: {}\r\n", cc.trim()));
    }
    if !bcc.trim().is_empty() {
        raw.push_str(&format!("Bcc: {}\r\n", bcc.trim()));
    }
    raw.push_str(&format!("Subject: {}\r\n", subject));
    raw.push_str(&format!("Date: {}\r\n", date_rfc2822));
    raw.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    raw.push_str("Content-Transfer-Encoding: 8bit\r\n");
    raw.push_str("\r\n");
    raw.push_str(body);
    raw.push_str("\r\n");
    raw.into_bytes()
}

#[async_trait]
pub trait MailStore: Send + Sync {
    async fn load_snapshot(&self, account_id: i64, folder_id: i64) -> Result<StoreSnapshot>;
    async fn get_raw_body(&self, message_id: i64) -> Result<Option<Vec<u8>>>;
    async fn upsert_raw_body(&self, message_id: i64, raw: &[u8]) -> Result<()>;
    async fn upsert_cache_text(&self, message_id: i64, width_cols: i64, text: &str) -> Result<()>;
    async fn get_cache_html(&self, message_id: i64, remote_policy: &str) -> Result<Option<String>>;
    async fn upsert_cache_html(
        &self,
        message_id: i64,
        remote_policy: &str,
        html: &str,
    ) -> Result<()>;
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

    pub async fn save_draft(
        &self,
        account_id: i64,
        from_addr: &str,
        to: &str,
        cc: &str,
        bcc: &str,
        subject: &str,
        body: &str,
    ) -> Result<i64> {
        let folder_id = if let Some(id) = self.folder_id_by_name(account_id, "Drafts").await? {
            id
        } else {
            let result =
                sqlx::query("INSERT INTO folders (account_id, name, unread) VALUES (?, ?, 0)")
                    .bind(account_id)
                    .bind("Drafts")
                    .execute(&self.pool)
                    .await?;
            result.last_insert_rowid()
        };

        let now = Local::now();
        let date = now.format("%Y-%m-%d %H:%M").to_string();
        let preview = draft_preview(body);

        let result = sqlx::query(
            "INSERT INTO messages (account_id, folder_id, imap_uid, date, date_ts, from_addr, to_addr, cc, subject, unread, preview)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account_id)
        .bind(folder_id)
        .bind(None::<i64>)
        .bind(&date)
        .bind(parse_date_ts(&date))
        .bind(from_addr)
        .bind(to)
        .bind(cc)
        .bind(subject)
        .bind(0)
        .bind(preview)
        .execute(&self.pool)
        .await?;

        let message_id = result.last_insert_rowid();
        self.upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, body)
            .await?;
        let raw = draft_raw(from_addr, to, cc, bcc, subject, body, now.to_rfc2822());
        self.upsert_raw_body(message_id, &raw).await?;
        Ok(message_id)
    }

    pub async fn clear_account_data(&self, account_id: i64) -> Result<()> {
        let message_ids: Vec<i64> =
            sqlx::query_as::<_, (i64,)>("SELECT id FROM messages WHERE account_id = ?")
                .bind(account_id)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|row| row.0)
                .collect();

        if !message_ids.is_empty() {
            let placeholders = message_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(", ");
            for table in ["bodies", "cache_text", "cache_html", "cache_tiles"] {
                let query = format!(
                    "DELETE FROM {} WHERE message_id IN ({})",
                    table, placeholders
                );
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

    pub async fn move_messages(&self, message_ids: &[i64], target_folder_id: i64) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let placeholders = placeholders(message_ids.len());

        let select_query = format!(
            "SELECT DISTINCT folder_id FROM messages WHERE id IN ({})",
            placeholders
        );
        let mut select = sqlx::query_as::<_, (i64,)>(&select_query);
        for id in message_ids {
            select = select.bind(id);
        }
        let mut affected_folders: Vec<i64> = select
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect();
        if !affected_folders.contains(&target_folder_id) {
            affected_folders.push(target_folder_id);
        }

        let update_query = format!(
            "UPDATE messages SET folder_id = ? WHERE id IN ({})",
            placeholders
        );
        let mut update = sqlx::query(&update_query).bind(target_folder_id);
        for id in message_ids {
            update = update.bind(id);
        }
        update.execute(&self.pool).await?;

        self.update_folder_unread_counts(&affected_folders).await?;
        Ok(())
    }

    pub async fn delete_messages(&self, message_ids: &[i64]) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let placeholders = placeholders(message_ids.len());

        let select_query = format!(
            "SELECT DISTINCT folder_id FROM messages WHERE id IN ({})",
            placeholders
        );
        let mut select = sqlx::query_as::<_, (i64,)>(&select_query);
        for id in message_ids {
            select = select.bind(id);
        }
        let affected_folders: Vec<i64> = select
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect();

        for table in ["bodies", "cache_text", "cache_html", "cache_tiles"] {
            let query = format!(
                "DELETE FROM {} WHERE message_id IN ({})",
                table, placeholders
            );
            let mut q = sqlx::query(&query);
            for id in message_ids {
                q = q.bind(id);
            }
            q.execute(&self.pool).await?;
        }

        let delete_query = format!("DELETE FROM messages WHERE id IN ({})", placeholders);
        let mut delete = sqlx::query(&delete_query);
        for id in message_ids {
            delete = delete.bind(id);
        }
        delete.execute(&self.pool).await?;

        self.update_folder_unread_counts(&affected_folders).await?;
        Ok(())
    }

    async fn update_folder_unread_counts(&self, folder_ids: &[i64]) -> Result<()> {
        for folder_id in folder_ids {
            let row = sqlx::query_as::<_, (i64,)>(
                "SELECT COUNT(*) FROM messages WHERE folder_id = ? AND unread = 1",
            )
            .bind(folder_id)
            .fetch_one(&self.pool)
            .await?;
            sqlx::query("UPDATE folders SET unread = ? WHERE id = ?")
                .bind(row.0)
                .bind(folder_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn upsert_folders(&self, account_id: i64, folders: &[Folder]) -> Result<Vec<Folder>> {
        let existing =
            sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM folders WHERE account_id = ?")
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
                let result =
                    sqlx::query("INSERT INTO folders (account_id, name, unread) VALUES (?, ?, ?)")
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
            let placeholders = kept_names
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(", ");
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
        let existing: Vec<(i64, Option<i64>)> =
            sqlx::query_as("SELECT id, imap_uid FROM messages WHERE folder_id = ?")
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
                let query = format!("DELETE FROM messages WHERE id IN ({})", placeholders);
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
                    "UPDATE messages SET date = ?, date_ts = ?, from_addr = ?, subject = ?, unread = ?, preview = ?
                     WHERE id = ?",
                )
                .bind(&msg.date)
                .bind(parse_date_ts(&msg.date))
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
                    "INSERT INTO messages (account_id, folder_id, imap_uid, date, date_ts, from_addr, to_addr, cc, subject, unread, preview)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(account_id)
                .bind(folder_id)
                .bind(uid)
                .bind(&msg.date)
                .bind(parse_date_ts(&msg.date))
                .bind(&msg.from)
                .bind("")
                .bind("")
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
                    "INSERT INTO messages (account_id, folder_id, imap_uid, date, date_ts, from_addr, to_addr, cc, subject, unread, preview)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(account_id)
                .bind(folder_id)
                .bind(uid)
                .bind(&msg.date)
                .bind(parse_date_ts(&msg.date))
                .bind(&msg.from)
                .bind("")
                .bind("")
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
        let row =
            sqlx::query_as::<_, (i64,)>("SELECT id FROM folders WHERE account_id = ? AND name = ?")
                .bind(account_id)
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn list_folders(&self, account_id: i64) -> Result<Vec<Folder>> {
        let folders = sqlx::query_as::<_, (i64, i64, String, i64)>(
            "SELECT id, account_id, name, unread FROM folders WHERE account_id = ? ORDER BY id",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(folders
            .into_iter()
            .map(|row| Folder {
                id: row.0,
                account_id: row.1,
                name: row.2,
                unread: row.3 as u32,
            })
            .collect())
    }

    pub async fn list_messages(
        &self,
        account_id: i64,
        folder_id: Option<i64>,
        unread: Option<bool>,
        since_ts: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Vec<MessageSummary>> {
        let mut query = String::from(
            "SELECT id, folder_id, imap_uid, date, from_addr, subject, unread, preview
             FROM messages WHERE account_id = ?",
        );
        if folder_id.is_some() {
            query.push_str(" AND folder_id = ?");
        }
        if unread.is_some() {
            query.push_str(" AND unread = ?");
        }
        if since_ts.is_some() {
            query.push_str(" AND COALESCE(date_ts, 0) >= ?");
        }
        query.push_str(" ORDER BY COALESCE(date_ts, 0) DESC, id DESC");
        if limit.is_some() {
            query.push_str(" LIMIT ?");
        }

        let mut q =
            sqlx::query_as::<_, (i64, i64, Option<i64>, String, String, String, i64, String)>(
                &query,
            )
            .bind(account_id);
        if let Some(folder_id) = folder_id {
            q = q.bind(folder_id);
        }
        if let Some(unread) = unread {
            q = q.bind(if unread { 1 } else { 0 });
        }
        if let Some(since_ts) = since_ts {
            q = q.bind(since_ts);
        }
        if let Some(limit) = limit {
            q = q.bind(limit);
        }

        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
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
            .collect())
    }

    pub async fn get_message_summary(&self, message_id: i64) -> Result<Option<MessageSummary>> {
        let row =
            sqlx::query_as::<_, (i64, i64, Option<i64>, String, String, String, i64, String)>(
                "SELECT id, folder_id, imap_uid, date, from_addr, subject, unread, preview
             FROM messages WHERE id = ?",
            )
            .bind(message_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| MessageSummary {
            id: row.0,
            folder_id: row.1,
            imap_uid: row.2.map(|v| v as u32),
            date: row.3,
            from: row.4,
            subject: row.5,
            unread: row.6 != 0,
            preview: row.7,
        }))
    }

    pub async fn get_message_text(
        &self,
        message_id: i64,
        width_cols: i64,
    ) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT text FROM cache_text WHERE message_id = ? AND width_cols = ?",
        )
        .bind(message_id)
        .bind(width_cols)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn get_message_cc(&self, message_id: i64) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>("SELECT cc FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn get_message_to(&self, message_id: i64) -> Result<Option<String>> {
        let row = sqlx::query_as::<_, (String,)>("SELECT to_addr FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn update_message_to(&self, message_id: i64, to: &str) -> Result<()> {
        sqlx::query("UPDATE messages SET to_addr = ? WHERE id = ?")
            .bind(to)
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_message_cc(&self, message_id: i64, cc: &str) -> Result<()> {
        sqlx::query("UPDATE messages SET cc = ? WHERE id = ?")
            .bind(cc)
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_message_unread(&self, message_id: i64, unread: bool) -> Result<()> {
        let folder_id = sqlx::query_as::<_, (i64,)>("SELECT folder_id FROM messages WHERE id = ?")
            .bind(message_id)
            .fetch_optional(&self.pool)
            .await?
            .map(|row| row.0);
        sqlx::query("UPDATE messages SET unread = ? WHERE id = ?")
            .bind(if unread { 1 } else { 0 })
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        if let Some(folder_id) = folder_id {
            self.update_folder_unread_counts(&[folder_id]).await?;
        }
        Ok(())
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

    pub async fn account_id_by_name(&self, name: &str) -> Result<Option<i64>> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT id FROM accounts WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn first_account_id(&self) -> Result<Option<i64>> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT id FROM accounts ORDER BY id LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn get_folder_sync_state(&self, folder_id: i64) -> Result<Option<FolderSyncState>> {
        let row = sqlx::query_as::<
            _,
            (
                i64,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            ),
        >(
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

    pub async fn upsert_folder_sync_state(&self, state: &FolderSyncState) -> Result<()> {
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

    pub async fn seed_demo_if_empty(&self, account_label: &str) -> Result<()> {
        let trimmed_label = account_label.trim();
        let lower_label = trimmed_label.to_ascii_lowercase();
        let is_work_demo = lower_label.contains("work");
        let expected_address = if is_work_demo {
            "work@ratmail-demo.local"
        } else {
            "personal@ratmail-demo.local"
        };
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
            .fetch_one(&self.pool)
            .await?;
        let should_reset_demo = if count.0 == 0 {
            true
        } else {
            let account = sqlx::query_as::<_, (String, String)>(
                "SELECT name, address FROM accounts WHERE id = 1",
            )
            .fetch_optional(&self.pool)
            .await?;
            match account {
                Some((name, address)) => {
                    name == "personal@example.com"
                        || address == "personal@example.com"
                        || name.starts_with("Ratmail Demo")
                        || (trimmed_label.eq_ignore_ascii_case("personal")
                            || trimmed_label.eq_ignore_ascii_case("work"))
                            && address != expected_address
                        || address.ends_with("@ratmail-demo.local")
                }
                None => false,
            }
        };
        if !should_reset_demo {
            return Ok(());
        }

        if count.0 > 0 {
            sqlx::query("DELETE FROM cache_tiles")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM cache_html")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM cache_text")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM bodies")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM messages")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM folder_sync_state")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM folders")
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM accounts")
                .execute(&self.pool)
                .await?;
        }

        let account_name = if trimmed_label.is_empty() {
            "Ratmail Demo".to_string()
        } else {
            trimmed_label.to_string()
        };
        let account_address = expected_address;
        let to_header = format!("{} <{}>", account_name, account_address);

        sqlx::query("INSERT INTO accounts (id, name, address) VALUES (1, ?, ?)")
            .bind(&account_name)
            .bind(account_address)
            .execute(&self.pool)
            .await?;

        let folders = vec![
            (1, "INBOX", 4),
            (2, "Sent", 0),
            (3, "Drafts", 1),
            (4, "Archive", 0),
            (5, "Promotions", 2),
            (6, "Orders", 1),
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

        let mut messages = vec![
            (
                101,
                6,
                "2026-02-14 09:42",
                "Northstar Outfitters <orders@northstar-outfitters.com>",
                to_header.as_str(),
                "",
                "Your order NS-20419 has shipped",
                1,
                "Track your shipment and view order details.",
            ),
            (
                102,
                1,
                "2026-02-14 08:15",
                "Orbit Weekly <editor@orbitweekly.com>",
                to_header.as_str(),
                "",
                "The Friday Brief: product launches, retail trends, and growth playbooks",
                1,
                "A polished newsletter with top stories and market signals.",
            ),
            (
                103,
                1,
                "2026-02-13 17:28",
                "Acorn Payments <billing@acornpayments.com>",
                to_header.as_str(),
                "finance@northstar-outfitters.com",
                "Invoice 8842 paid successfully",
                0,
                "Payment confirmed. Receipt and breakdown attached.",
            ),
            (
                104,
                5,
                "2026-02-13 13:52",
                "Northstar Studio <hello@northstar-outfitters.com>",
                to_header.as_str(),
                "",
                "48-hour Winter Edit: premium picks up to 30% off",
                1,
                "Store campaign with product cards and image-rich layout.",
            ),
            (
                105,
                1,
                "2026-02-12 16:10",
                "Ratmail Team <product@ratmail.dev>",
                to_header.as_str(),
                "",
                "Ratmail 0.7 release notes and roadmap preview",
                0,
                "Terminal rendering upgrades, compose improvements, and CLI policy updates.",
            ),
            (
                106,
                1,
                "2026-02-12 10:04",
                "Security Desk <security@workspace.example>",
                to_header.as_str(),
                "",
                "New login detected from San Diego, CA",
                1,
                "Sign-in alert with security review link.",
            ),
            (
                107,
                2,
                "2026-02-11 21:44",
                to_header.as_str(),
                "Jordan Park <jordan@partnerstudio.io>",
                "",
                "Re: Q2 co-marketing timeline",
                0,
                "Shared timeline and creative milestones for Q2 launch.",
            ),
            (
                108,
                3,
                "2026-02-11 14:03",
                to_header.as_str(),
                "marketing@northstar-outfitters.com",
                "",
                "Draft: Spring campaign concept",
                0,
                "Drafting launch copy and hero section options.",
            ),
        ];
        if !is_work_demo {
            messages[0] = (
                101,
                1,
                "2026-02-14 18:22",
                "Maya Lin <maya.lin@friendsmail.com>",
                to_header.as_str(),
                "",
                "Dinner on Friday?",
                1,
                "Italian or sushi? I booked us for 7 if you're free.",
            );
            messages[1] = (
                102,
                1,
                "2026-02-14 11:04",
                "SkyBridge Airlines <updates@skybridge-air.com>",
                to_header.as_str(),
                "",
                "Trip confirmed: Austin, Mar 3",
                1,
                "Gate details, baggage allowance, and check-in timeline.",
            );
            messages[2] = (
                103,
                1,
                "2026-02-13 20:19",
                "River Bank <alerts@riverbank.com>",
                to_header.as_str(),
                "",
                "Your February statement is ready",
                0,
                "Statement available in secure inbox.",
            );
            messages[3] = (
                104,
                5,
                "2026-02-13 08:40",
                "Neighborhood Makers <hello@makers-district.org>",
                to_header.as_str(),
                "",
                "Weekend events near you",
                1,
                "Food popups, gallery night, and live jazz picks.",
            );
            messages[4] = (
                105,
                1,
                "2026-02-12 21:12",
                "Lena Park <lena.park@photoshare.app>",
                to_header.as_str(),
                "",
                "Photos from Tahoe are up",
                0,
                "Shared album with 64 new photos.",
            );
            messages[5] = (
                106,
                1,
                "2026-02-12 09:18",
                "Google Account <no-reply@accounts.google.com>",
                to_header.as_str(),
                "",
                "Password changed successfully",
                0,
                "Security confirmation for your account.",
            );
            messages[6] = (
                107,
                2,
                "2026-02-11 17:31",
                to_header.as_str(),
                "Noah Rivera <noah.rivera@friendsmail.com>",
                "",
                "Re: Mom's birthday plan",
                0,
                "I can pick up the cake and decorations Saturday morning.",
            );
            messages[7] = (
                108,
                3,
                "2026-02-10 22:06",
                to_header.as_str(),
                "travel@notes.local",
                "",
                "Draft: Packing list for Austin",
                0,
                "Carry-on checklist and hotel confirmation notes.",
            );
        }

        for (id, folder_id, date, from, to, cc, subject, unread, preview) in messages {
            sqlx::query(
                "INSERT INTO messages (id, account_id, folder_id, imap_uid, date, date_ts, from_addr, to_addr, cc, subject, unread, preview)
                 VALUES (?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET folder_id = excluded.folder_id, date = excluded.date, date_ts = excluded.date_ts,
                 from_addr = excluded.from_addr, to_addr = excluded.to_addr, cc = excluded.cc, subject = excluded.subject,
                 unread = excluded.unread, preview = excluded.preview",
            )
            .bind(id)
            .bind(folder_id)
            .bind(None::<i64>)
            .bind(date)
            .bind(parse_date_ts(date))
            .bind(from)
            .bind(to)
            .bind(cc)
            .bind(subject)
            .bind(unread)
            .bind(preview)
            .execute(&self.pool)
            .await?;
        }

        let mut text_bodies = vec![
            (
                101,
                "Your Northstar order NS-20419 is on the way.\nCarrier: ParcelPro\nTracking: PP203948112\nETA: Monday, Feb 17\n\nTrack package: https://northstar-outfitters.com/track/PP203948112",
            ),
            (
                102,
                "Orbit Weekly Friday Brief\n\n1) Retail benchmark report: conversion +12% for mobile-first checkouts.\n2) Creative teardown: what made this week's top lifecycle campaign work.\n3) Product playbook: shipping polished HTML newsletters in under a day.\n\nRead full issue: https://orbitweekly.com/brief/friday",
            ),
            (
                103,
                "Invoice 8842 has been paid.\nAmount: $1,284.00\nMethod: ACH\nPaid: 2026-02-13\n\nReceipt attached in your billing portal.",
            ),
            (
                104,
                "Winter Edit sale is live for 48 hours.\nSelected items are up to 30% off.\nShop now: https://northstar-outfitters.com/winter-edit",
            ),
            (
                105,
                "Ratmail 0.7 highlights:\n- Better HTML rendering stability\n- Faster list redraws\n- Expanded CLI policy controls\n\nRoadmap and release notes: https://ratmail.dev/changelog/0-7",
            ),
            (
                106,
                "We detected a new login from San Diego, CA on 2026-02-12 10:02 PST.\nIf this was you, no action is required.\nIf not, reset your password immediately:\nhttps://workspace.example/security",
            ),
            (
                107,
                "Works for us. We can lock creative on March 3 and start media on March 10.\nI'll share final assets in one folder by end of week.",
            ),
            (
                108,
                "Hero concept A: product-first photography with concise CTA.\nHero concept B: editorial story style with founder quote.\nPending final copy.",
            ),
        ];
        if !is_work_demo {
            text_bodies[0] = (
                101,
                "Are you free Friday at 7?\nI booked us at Luca if you're in.\nIf not, we can switch to sushi downtown.",
            );
            text_bodies[1] = (
                102,
                "Flight SB231 to Austin is confirmed.\nDeparture: Mar 3, 08:25\nCheck-in opens 24 hours before departure.\n\nManage trip: https://skybridge-air.com/manage/SB231",
            );
            text_bodies[2] = (
                103,
                "Your February statement is ready.\nSecure message center: https://riverbank.com/inbox",
            );
            text_bodies[3] = (
                104,
                "Weekend picks near you:\n- Friday: Gallery night (7 PM)\n- Saturday: Food market\n- Sunday: Jazz set at Green Room",
            );
            text_bodies[4] = (
                105,
                "Uploaded 64 photos from Tahoe. Favorite set: sunrise + lake trail.\nAlbum link: https://photoshare.app/a/tahoe-feb",
            );
            text_bodies[5] = (
                106,
                "Your password was changed on Feb 12 at 09:17.\nIf this wasn't you, review security settings immediately.",
            );
            text_bodies[6] = (
                107,
                "I can pick up the cake and decorations on Saturday morning.\nLet's do dinner at 6:30 so everyone can make it.",
            );
            text_bodies[7] = (
                108,
                "Packing list draft:\n- Jacket\n- Sneakers\n- Chargers\n- Camera\n- Tripod\n- Toiletries",
            );
        }

        for (message_id, body) in text_bodies {
            sqlx::query(
                "INSERT INTO cache_text (message_id, width_cols, text, updated_at)
                 VALUES (?, ?, ?, '2026-02-14T12:00:00Z')
                 ON CONFLICT(message_id, width_cols)
                 DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
            )
            .bind(message_id)
            .bind(DEFAULT_TEXT_WIDTH)
            .bind(body)
            .execute(&self.pool)
            .await?;
        }

        let mut raw_messages = vec![
            (
                101,
                format!(
                    "From: Northstar Outfitters <orders@northstar-outfitters.com>\r\n\
To: {to_header}\r\n\
Subject: Your order NS-20419 has shipped\r\n\
Date: Sat, 14 Feb 2026 09:42:00 -0800\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"rm-order-20419\"\r\n\
\r\n\
--rm-order-20419\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your order NS-20419 has shipped. Track package PP203948112.\r\n\
\r\n\
--rm-order-20419\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#f3f6fb;font-family:Arial,sans-serif;color:#111827;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#f3f6fb;padding:24px 0;\">\r\n\
<tr><td align=\"center\">\r\n\
<table role=\"presentation\" width=\"680\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:680px;background:#ffffff;border-radius:16px;overflow:hidden;box-shadow:0 14px 40px rgba(15,23,42,0.12);\">\r\n\
<tr><td style=\"background:#0f172a;padding:22px 28px;color:#ffffff;font-size:20px;font-weight:700;\">Northstar Outfitters</td></tr>\r\n\
<tr><td><img src=\"https://images.unsplash.com/photo-1523381210434-271e8be1f52b?auto=format&amp;fit=crop&amp;w=1400&amp;q=80\" alt=\"Order banner\" style=\"display:block;width:100%;height:auto;border:0;\" /></td></tr>\r\n\
<tr><td style=\"padding:28px;\">\r\n\
<p style=\"margin:0 0 8px 0;font-size:13px;letter-spacing:0.08em;text-transform:uppercase;color:#475569;\">Shipment update</p>\r\n\
<h1 style=\"margin:0 0 14px 0;font-size:30px;line-height:1.2;color:#0f172a;\">Your order is on the way</h1>\r\n\
<p style=\"margin:0 0 18px 0;font-size:16px;line-height:1.6;color:#334155;\">Order <strong>NS-20419</strong> left our warehouse and is scheduled to arrive <strong>Monday, Feb 17</strong>.</p>\r\n\
<p style=\"margin:0 0 24px 0;\"><a href=\"https://northstar-outfitters.com/track/PP203948112\" style=\"display:inline-block;background:#2563eb;color:#ffffff;text-decoration:none;padding:12px 22px;border-radius:10px;font-weight:700;\">Track Package</a></p>\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"border-top:1px solid #e2e8f0;padding-top:18px;\">\r\n\
<tr>\r\n\
<td width=\"50%\" style=\"padding-right:10px;\"><img src=\"https://images.unsplash.com/photo-1460353581641-37baddab0fa2?auto=format&amp;fit=crop&amp;w=900&amp;q=80\" alt=\"Travel jacket\" style=\"display:block;width:100%;border-radius:10px;\" /></td>\r\n\
<td width=\"50%\" style=\"font-size:14px;line-height:1.6;color:#475569;vertical-align:top;\">Carrier: <strong>ParcelPro</strong><br/>Tracking: <strong>PP203948112</strong><br/>Service: Priority 2-Day</td>\r\n\
</tr>\r\n\
</table>\r\n\
</td></tr>\r\n\
</table></td></tr></table></body></html>\r\n\
--rm-order-20419--\r\n"
                ),
            ),
            (
                102,
                format!(
                    "From: Orbit Weekly <editor@orbitweekly.com>\r\n\
To: {to_header}\r\n\
Subject: The Friday Brief: product launches, retail trends, and growth playbooks\r\n\
Date: Sat, 14 Feb 2026 08:15:00 -0800\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#f8fafc;font-family:Arial,sans-serif;color:#0f172a;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#f8fafc;padding:28px 0;\">\r\n\
<tr><td align=\"center\" style=\"text-align:center;\">\r\n\
<table role=\"presentation\" width=\"700\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:700px;margin:0 auto;background:#ffffff;border:1px solid #e2e8f0;border-radius:14px;overflow:hidden;\">\r\n\
<tr><td style=\"padding:18px 24px;background:#111827;color:#ffffff;font-size:12px;letter-spacing:0.08em;text-transform:uppercase;\">Orbit Weekly  |  Friday Brief</td></tr>\r\n\
<tr><td><img src=\"https://images.unsplash.com/photo-1505238680356-667803448bb6?auto=format&amp;fit=crop&amp;w=1400&amp;q=80\" alt=\"Newsletter cover\" style=\"display:block;width:100%;height:auto;border:0;\" /></td></tr>\r\n\
<tr><td style=\"padding:28px 28px 12px 28px;text-align:center;\">\r\n\
<h1 style=\"margin:0 0 10px 0;font-size:30px;line-height:1.2;\">What shipped this week</h1>\r\n\
<p style=\"margin:0 0 20px 0;font-size:16px;line-height:1.6;color:#334155;\">Three stories worth forwarding to your team before Monday standup.</p>\r\n\
</td></tr>\r\n\
<tr><td style=\"padding:0 28px 28px 28px;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"text-align:center;\">\r\n\
<tr>\r\n\
<td style=\"padding-bottom:16px;\"><img src=\"https://images.unsplash.com/photo-1460925895917-afdab827c52f?auto=format&amp;fit=crop&amp;w=900&amp;q=80\" alt=\"Analytics\" style=\"display:block;width:100%;border-radius:10px;\" /></td>\r\n\
</tr>\r\n\
<tr><td style=\"font-size:18px;font-weight:700;padding-bottom:8px;\">Retail benchmark: checkout conversion climbs 12%</td></tr>\r\n\
<tr><td style=\"font-size:15px;line-height:1.6;color:#475569;padding-bottom:20px;\">Teams reducing friction in shipping and payment options are winning the quarter.</td></tr>\r\n\
<tr><td style=\"font-size:18px;font-weight:700;padding-bottom:8px;\">Lifecycle teardown: the campaign everyone copied</td></tr>\r\n\
<tr><td style=\"font-size:15px;line-height:1.6;color:#475569;padding-bottom:20px;\">A practical breakdown of flow timing, segmentation, and creative hierarchy.</td></tr>\r\n\
<tr><td style=\"font-size:18px;font-weight:700;padding-bottom:8px;\">Playbook: shipping polished HTML newsletters fast</td></tr>\r\n\
<tr><td style=\"font-size:15px;line-height:1.6;color:#475569;padding-bottom:24px;\">Templates, QA checklists, and image guidelines used by top growth teams.</td></tr>\r\n\
<tr><td><a href=\"https://orbitweekly.com/brief/friday\" style=\"display:inline-block;background:#0ea5e9;color:#ffffff;text-decoration:none;padding:12px 20px;border-radius:10px;font-weight:700;\">Read Full Issue</a></td></tr>\r\n\
</table>\r\n\
</td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            ),
            (
                103,
                format!(
                    "From: Acorn Payments <billing@acornpayments.com>\r\n\
To: {to_header}\r\n\
Cc: finance@northstar-outfitters.com\r\n\
Subject: Invoice 8842 paid successfully\r\n\
Date: Fri, 13 Feb 2026 17:28:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Invoice 8842 has been paid in full.\r\n\
Amount: $1,284.00\r\n\
Method: ACH\r\n\
Paid: 2026-02-13\r\n\
\r\n\
Receipt: https://billing.acornpayments.com/receipts/8842\r\n"
                ),
            ),
            (
                104,
                format!(
                    "From: Northstar Studio <hello@northstar-outfitters.com>\r\n\
To: {to_header}\r\n\
Subject: 48-hour Winter Edit: premium picks up to 30% off\r\n\
Date: Fri, 13 Feb 2026 13:52:00 -0800\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#eef2ff;font-family:Arial,sans-serif;color:#0f172a;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#eef2ff;padding:24px 0;\">\r\n\
<tr><td align=\"center\" style=\"text-align:center;\">\r\n\
<table role=\"presentation\" width=\"700\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:700px;margin:0 auto;background:#ffffff;border-radius:16px;overflow:hidden;\">\r\n\
<tr><td style=\"padding:16px 24px;background:#312e81;color:#c7d2fe;font-size:12px;letter-spacing:0.08em;text-transform:uppercase;\">Winter Edit  |  48 hours only</td></tr>\r\n\
<tr><td><img src=\"https://images.unsplash.com/photo-1441986300917-64674bd600d8?auto=format&amp;fit=crop&amp;w=1400&amp;q=80\" alt=\"Store hero\" style=\"display:block;width:100%;height:auto;border:0;\" /></td></tr>\r\n\
<tr><td style=\"padding:28px;\">\r\n\
<h1 style=\"margin:0 0 12px 0;font-size:32px;line-height:1.2;color:#1e1b4b;\">Elevate your cold-weather kit</h1>\r\n\
<p style=\"margin:0 0 20px 0;font-size:16px;line-height:1.7;color:#475569;\">Premium layers, weather-ready shells, and everyday staples. Prices drop up to 30% through Sunday night.</p>\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\">\r\n\
<tr>\r\n\
<td width=\"33.33%\" style=\"padding-right:8px;\"><img src=\"https://images.unsplash.com/photo-1483985988355-763728e1935b?auto=format&amp;fit=crop&amp;w=700&amp;q=80\" alt=\"Outerwear\" style=\"display:block;width:100%;border-radius:10px;\" /></td>\r\n\
<td width=\"33.33%\" style=\"padding:0 4px;\"><img src=\"https://images.unsplash.com/photo-1529139574466-a303027c1d8b?auto=format&amp;fit=crop&amp;w=700&amp;q=80\" alt=\"Boots\" style=\"display:block;width:100%;border-radius:10px;\" /></td>\r\n\
<td width=\"33.33%\" style=\"padding-left:8px;\"><img src=\"https://images.unsplash.com/photo-1521572163474-6864f9cf17ab?auto=format&amp;fit=crop&amp;w=700&amp;q=80\" alt=\"Basics\" style=\"display:block;width:100%;border-radius:10px;\" /></td>\r\n\
</tr>\r\n\
</table>\r\n\
<p style=\"margin:24px 0 0 0;\"><a href=\"https://northstar-outfitters.com/winter-edit\" style=\"display:inline-block;background:#4338ca;color:#ffffff;text-decoration:none;padding:12px 22px;border-radius:10px;font-weight:700;\">Shop The Edit</a></p>\r\n\
</td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            ),
            (
                105,
                format!(
                    "From: Ratmail Team <product@ratmail.dev>\r\n\
To: {to_header}\r\n\
Subject: Ratmail 0.7 release notes and roadmap preview\r\n\
Date: Thu, 12 Feb 2026 16:10:00 -0800\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#0d0515;font-family:Arial,sans-serif;color:#e8e8ff;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#0d0515;padding:24px 0;\">\r\n\
<tr><td align=\"center\" style=\"text-align:center;\">\r\n\
<table role=\"presentation\" width=\"700\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:700px;margin:0 auto;background:#1a0a2e;border:1px solid #b026ff;border-radius:14px;overflow:hidden;\">\r\n\
<tr><td style=\"padding:16px 24px;background:linear-gradient(90deg,#b026ff,#ff2d95);color:#ffffff;font-size:12px;letter-spacing:0.08em;text-transform:uppercase;\">Ratmail Update  |  Totally Rad Terminal Email</td></tr>\r\n\
<tr><td style=\"padding:24px 24px 8px 24px;text-align:center;\"><img src=\"https://raw.githubusercontent.com/peter-fm/ratmail/main/crates/ratmail-core/assets/demo/logo.png\" alt=\"Ratmail\" style=\"display:block;margin:0 auto;width:260px;max-width:100%;height:auto;border:0;\" /></td></tr>\r\n\
<tr><td style=\"padding:8px 24px 0 24px;text-align:center;\"><img src=\"https://raw.githubusercontent.com/peter-fm/ratmail/main/crates/ratmail-core/assets/demo/eric.png\" alt=\"Eric the Ratmail mascot\" style=\"display:block;margin:0 auto;width:170px;max-width:100%;height:auto;border:0;\" /></td></tr>\r\n\
<tr><td style=\"padding:18px 30px 8px 30px;text-align:center;\">\r\n\
<h1 style=\"margin:0 0 12px 0;font-size:32px;line-height:1.2;color:#e8e8ff;\">Ratmail v0.2 is live</h1>\r\n\
<p style=\"margin:0 0 18px 0;font-size:16px;line-height:1.7;color:#a8a8d8;\">Synthwave UI, sharper rendering, and safer AI CLI workflows for teams automating mail from the terminal.</p>\r\n\
</td></tr>\r\n\
<tr><td style=\"padding:8px 30px 24px 30px;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#0d0515;border:1px solid #5858a8;border-radius:10px;\">\r\n\
<tr><td style=\"padding:18px 18px 8px 18px;font-size:18px;line-height:1.4;color:#00fff7;font-weight:700;\">What's new</td></tr>\r\n\
<tr><td style=\"padding:0 18px 18px 18px;font-size:15px;line-height:1.8;color:#e8e8ff;\">\r\n\
• Better HTML render stability in Kitty/Sixel terminals<br/>\r\n\
• Faster message list redraws in busy inboxes<br/>\r\n\
• Expanded CLI policy controls for agent safety\r\n\
</td></tr>\r\n\
</table>\r\n\
</td></tr>\r\n\
<tr><td style=\"padding:0 30px 26px 30px;text-align:center;\">\r\n\
<a href=\"https://ratmail.dev/changelog/0-7\" style=\"display:inline-block;background:#ff2d95;color:#0d0515;text-decoration:none;padding:12px 22px;border-radius:10px;font-weight:700;\">Read Release Notes</a>\r\n\
<span style=\"display:inline-block;width:10px;\">&nbsp;</span>\r\n\
<a href=\"https://github.com/peter-fm/ratmail\" style=\"display:inline-block;background:#00d4ff;color:#0d0515;text-decoration:none;padding:12px 22px;border-radius:10px;font-weight:700;\">Get Ratmail</a>\r\n\
</td></tr>\r\n\
<tr><td style=\"padding:0 30px 26px 30px;\"><img src=\"https://raw.githubusercontent.com/peter-fm/ratmail/main/crates/ratmail-core/assets/demo/electric_guitar.png\" alt=\"Ratmail rocks\" style=\"display:block;width:100%;height:auto;border:0;border-radius:10px;\" /></td></tr>\r\n\
<tr><td style=\"padding:0 30px 28px 30px;font-size:13px;line-height:1.6;color:#a8a8d8;text-align:center;\">Install: <span style=\"color:#00fff7;\">cargo install --git https://github.com/peter-fm/ratmail.git --locked</span></td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            ),
            (
                106,
                format!(
                    "From: Security Desk <security@workspace.example>\r\n\
To: {to_header}\r\n\
Subject: New login detected from San Diego, CA\r\n\
Date: Thu, 12 Feb 2026 10:04:00 -0800\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body>\r\n\
<p>We detected a new login on <strong>Feb 12, 2026 at 10:02 PST</strong> from <strong>San Diego, CA</strong>.</p>\r\n\
<p>If this was not you, secure your account now:</p>\r\n\
<p><a href=\"https://workspace.example/security\">Review security activity</a></p>\r\n\
</body></html>\r\n"
                ),
            ),
            (
                107,
                format!(
                    "From: {to_header}\r\n\
To: Jordan Park <jordan@partnerstudio.io>\r\n\
Subject: Re: Q2 co-marketing timeline\r\n\
Date: Wed, 11 Feb 2026 21:44:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Works for us. We can lock creative on March 3 and start media on March 10.\r\n\
I'll share final assets in one folder by end of week.\r\n"
                ),
            ),
            (
                108,
                format!(
                    "From: {to_header}\r\n\
To: marketing@northstar-outfitters.com\r\n\
Subject: Draft: Spring campaign concept\r\n\
Date: Wed, 11 Feb 2026 14:03:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hero concept A: product-first photography with concise CTA.\r\n\
Hero concept B: editorial story style with founder quote.\r\n\
Pending final copy.\r\n"
                ),
            ),
        ];
        if !is_work_demo {
            raw_messages[0] = (
                101,
                format!(
                    "From: Maya Lin <maya.lin@friendsmail.com>\r\n\
To: {to_header}\r\n\
Subject: Dinner on Friday?\r\n\
Date: Sat, 14 Feb 2026 18:22:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Are you free Friday at 7?\r\n\
I booked Luca if you're in.\r\n\
If not, we can switch to sushi downtown.\r\n"
                ),
            );
            raw_messages[1] = (
                102,
                format!(
                    "From: SkyBridge Airlines <updates@skybridge-air.com>\r\n\
To: {to_header}\r\n\
Subject: Trip confirmed: Austin, Mar 3\r\n\
Date: Sat, 14 Feb 2026 11:04:00 -0800\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#eef2ff;font-family:Arial,sans-serif;color:#0f172a;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"padding:24px 0;background:#eef2ff;\">\r\n\
<tr><td align=\"center\" style=\"text-align:center;\">\r\n\
<table role=\"presentation\" width=\"700\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:700px;margin:0 auto;background:#fff;border:1px solid #c7d2fe;border-radius:14px;overflow:hidden;\">\r\n\
<tr><td style=\"padding:16px 24px;background:#1e3a8a;color:#dbeafe;font-size:12px;letter-spacing:0.08em;text-transform:uppercase;\">SkyBridge Itinerary</td></tr>\r\n\
<tr><td><img src=\"https://images.unsplash.com/photo-1436491865332-7a61a109cc05?auto=format&amp;fit=crop&amp;w=1400&amp;q=80\" alt=\"Flight\" style=\"display:block;width:100%;\" /></td></tr>\r\n\
<tr><td style=\"padding:26px;text-align:center;\">\r\n\
<h1 style=\"margin:0 0 10px 0;font-size:30px;\">Austin trip confirmed</h1>\r\n\
<p style=\"margin:0 0 18px 0;font-size:16px;line-height:1.6;color:#475569;\">Flight <strong>SB231</strong> departs Mar 3 at 08:25. Check-in opens 24 hours before departure.</p>\r\n\
<img src=\"https://images.unsplash.com/photo-1474302770737-173ee21bab63?auto=format&amp;fit=crop&amp;w=1200&amp;q=80\" alt=\"Airport\" style=\"display:block;width:100%;border-radius:10px;margin-bottom:16px;\" />\r\n\
<p style=\"margin:0;font-size:15px;line-height:1.7;color:#334155;\">Carry-on included. Gate details available after check-in.</p>\r\n\
</td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            );
            raw_messages[2] = (
                103,
                format!(
                    "From: River Bank <alerts@riverbank.com>\r\n\
To: {to_header}\r\n\
Subject: Your February statement is ready\r\n\
Date: Fri, 13 Feb 2026 20:19:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your February statement is ready in secure inbox.\r\n\
Open: https://riverbank.com/inbox\r\n"
                ),
            );
            raw_messages[3] = (
                104,
                format!(
                    "From: Neighborhood Makers <hello@makers-district.org>\r\n\
To: {to_header}\r\n\
Subject: Weekend events near you\r\n\
Date: Fri, 13 Feb 2026 08:40:00 -0800\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#fff7ed;font-family:Arial,sans-serif;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"padding:20px 0;background:#fff7ed;\">\r\n\
<tr><td align=\"center\">\r\n\
<table role=\"presentation\" width=\"680\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:680px;background:#ffffff;border-radius:12px;overflow:hidden;\">\r\n\
<tr><td style=\"padding:16px 24px;background:#ea580c;color:#ffedd5;font-size:12px;text-transform:uppercase;letter-spacing:0.08em;\">Neighborhood Picks</td></tr>\r\n\
<tr><td style=\"padding:24px;\">\r\n\
<h1 style=\"margin:0 0 12px 0;font-size:28px;color:#1e293b;\">This weekend in your area</h1>\r\n\
<p style=\"margin:0 0 18px 0;color:#475569;font-size:15px;line-height:1.7;\">Food popups, gallery night, and Sunday jazz. Three events we think you'll love.</p>\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\r\n\
<td width=\"50%\" style=\"padding-right:8px;\"><img src=\"https://images.unsplash.com/photo-1414235077428-338989a2e8c0?auto=format&amp;fit=crop&amp;w=900&amp;q=80\" alt=\"Food\" style=\"display:block;width:100%;border-radius:8px;\" /></td>\r\n\
<td width=\"50%\" style=\"padding-left:8px;\"><img src=\"https://images.unsplash.com/photo-1511671782779-c97d3d27a1d4?auto=format&amp;fit=crop&amp;w=900&amp;q=80\" alt=\"Jazz\" style=\"display:block;width:100%;border-radius:8px;\" /></td>\r\n\
</tr></table>\r\n\
</td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            );
            raw_messages[4] = (
                105,
                format!(
                    "From: Lena Park <lena.park@photoshare.app>\r\n\
To: {to_header}\r\n\
Subject: Photos from Tahoe are up\r\n\
Date: Thu, 12 Feb 2026 21:12:00 -0800\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body style=\"margin:0;padding:0;background:#f0f9ff;font-family:Arial,sans-serif;\">\r\n\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"padding:20px 0;background:#f0f9ff;\">\r\n\
<tr><td align=\"center\"><table role=\"presentation\" width=\"680\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:680px;background:#ffffff;border-radius:12px;overflow:hidden;\">\r\n\
<tr><td><img src=\"https://images.unsplash.com/photo-1508261303786-ef342f9cf3b6?auto=format&amp;fit=crop&amp;w=1400&amp;q=80\" alt=\"Tahoe\" style=\"display:block;width:100%;\" /></td></tr>\r\n\
<tr><td style=\"padding:24px;\"><h1 style=\"margin:0 0 10px 0;font-size:28px;color:#0f172a;\">Tahoe album is live</h1><p style=\"margin:0;color:#475569;line-height:1.7;\">Uploaded 64 photos from the trip. The sunrise shots came out great.</p></td></tr>\r\n\
</table></td></tr></table></body></html>\r\n"
                ),
            );
            raw_messages[5] = (
                106,
                format!(
                    "From: Google Account <no-reply@accounts.google.com>\r\n\
To: {to_header}\r\n\
Subject: Password changed successfully\r\n\
Date: Thu, 12 Feb 2026 09:18:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your password was changed on Feb 12 at 09:17.\r\n\
If this wasn't you, review recent security activity.\r\n"
                ),
            );
            raw_messages[6] = (
                107,
                format!(
                    "From: {to_header}\r\n\
To: Noah Rivera <noah.rivera@friendsmail.com>\r\n\
Subject: Re: Mom's birthday plan\r\n\
Date: Wed, 11 Feb 2026 17:31:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
I can pick up the cake and decorations Saturday morning.\r\n\
Let's do dinner at 6:30 so everyone can make it.\r\n"
                ),
            );
            raw_messages[7] = (
                108,
                format!(
                    "From: {to_header}\r\n\
To: travel@notes.local\r\n\
Subject: Draft: Packing list for Austin\r\n\
Date: Tue, 10 Feb 2026 22:06:00 -0800\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Packing list:\r\n\
- Jacket\r\n\
- Sneakers\r\n\
- Chargers\r\n\
- Camera\r\n\
- Toiletries\r\n"
                ),
            );
        }

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
        let row =
            sqlx::query_as::<_, (Option<i64>,)>("SELECT SUM(LENGTH(png_bytes)) FROM cache_tiles")
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

fn placeholders(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(", ")
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

        let messages =
            sqlx::query_as::<_, (i64, i64, Option<i64>, String, String, String, i64, String)>(
                "SELECT id, folder_id, imap_uid, date, from_addr, subject, unread, preview
             FROM messages WHERE account_id = ? ORDER BY COALESCE(date_ts, 0) DESC, id DESC",
            )
            .bind(account_id)
            .fetch_all(&self.pool)
            .await?;

        let message_ids: Vec<i64> = messages.iter().map(|row| row.0).collect();
        let mut message_details = HashMap::new();

        for message_id in message_ids {
            if let Ok((subject, from, to, cc, date, body)) =
                sqlx::query_as::<_, (String, String, String, String, String, String)>(
                    "SELECT m.subject, m.from_addr, m.to_addr, m.cc, m.date, c.text
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
                        to,
                        cc,
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
        let row =
            sqlx::query_as::<_, (Vec<u8>,)>("SELECT raw_bytes FROM bodies WHERE message_id = ?")
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{MailStore, SqliteMailStore};

    fn temp_db_path() -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "ratmail-core-migrate-{}-{}.db",
            std::process::id(),
            ts
        ))
    }

    #[tokio::test]
    async fn init_applies_full_migration_chain_on_fresh_db() -> anyhow::Result<()> {
        let db_path = temp_db_path();
        let _ = std::fs::remove_file(&db_path);

        let store = SqliteMailStore::connect(
            db_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid temp db path"))?,
        )
        .await?;
        store.init().await?;

        let rows = sqlx::query_as::<_, (i64, String, String, i64, Option<String>, i64)>(
            "PRAGMA table_info(messages)",
        )
        .fetch_all(&store.pool)
        .await?;
        let columns: HashSet<String> = rows.into_iter().map(|row| row.1).collect();

        for required in [
            "id",
            "account_id",
            "folder_id",
            "imap_uid",
            "date",
            "date_ts",
            "from_addr",
            "to_addr",
            "cc",
            "subject",
            "unread",
            "preview",
        ] {
            assert!(columns.contains(required), "missing column {}", required);
        }

        let date_ts_idx = sqlx::query_as::<_, (String,)>(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'messages_date_ts_idx'",
        )
        .fetch_optional(&store.pool)
        .await?;
        assert!(date_ts_idx.is_some(), "missing messages_date_ts_idx");

        let _ = std::fs::remove_file(&db_path);
        Ok(())
    }

    #[tokio::test]
    async fn fresh_db_supports_draft_save_and_snapshot_load() -> anyhow::Result<()> {
        let db_path = temp_db_path();
        let _ = std::fs::remove_file(&db_path);

        let store = SqliteMailStore::connect(
            db_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid temp db path"))?,
        )
        .await?;
        store.init().await?;
        store
            .upsert_account(1, "Personal", "owner@example.com")
            .await?;

        let msg_id = store
            .save_draft(
                1,
                "Owner <owner@example.com>",
                "to@example.com",
                "cc@example.com",
                "bcc@example.com",
                "Draft Subject",
                "Draft body line",
            )
            .await?;
        assert!(msg_id > 0);

        let drafts_folder_id = store
            .folder_id_by_name(1, "Drafts")
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing Drafts folder"))?;
        let snapshot = store.load_snapshot(1, drafts_folder_id).await?;

        assert_eq!(snapshot.account.id, 1);
        assert!(snapshot.folders.iter().any(|f| f.name == "Drafts"));
        assert!(snapshot.messages.iter().any(|m| m.id == msg_id));

        let detail = snapshot
            .message_details
            .get(&msg_id)
            .ok_or_else(|| anyhow::anyhow!("missing draft detail"))?;
        assert_eq!(detail.subject, "Draft Subject");
        assert!(detail.body.contains("Draft body line"));
        assert_eq!(detail.to, "to@example.com");
        assert_eq!(detail.cc, "cc@example.com");

        let _ = std::fs::remove_file(&db_path);
        Ok(())
    }
}
