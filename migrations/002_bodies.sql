CREATE TABLE bodies (
    message_id INTEGER PRIMARY KEY,
    raw_bytes BLOB NOT NULL,
    FOREIGN KEY (message_id) REFERENCES messages(id)
);
