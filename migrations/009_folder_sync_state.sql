CREATE TABLE folder_sync_state (
    folder_id INTEGER PRIMARY KEY,
    uidvalidity INTEGER,
    uidnext INTEGER,
    last_seen_uid INTEGER,
    last_sync_ts INTEGER,
    oldest_ts INTEGER,
    FOREIGN KEY (folder_id) REFERENCES folders(id)
);
