CREATE TABLE cache_html (
    message_id INTEGER NOT NULL,
    prepared_html TEXT NOT NULL,
    remote_policy TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (message_id, remote_policy),
    FOREIGN KEY (message_id) REFERENCES messages(id)
);
