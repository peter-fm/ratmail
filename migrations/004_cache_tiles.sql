CREATE TABLE cache_tiles (
    message_id INTEGER NOT NULL,
    width_px INTEGER NOT NULL,
    theme TEXT NOT NULL,
    remote_policy TEXT NOT NULL,
    tile_index INTEGER NOT NULL,
    png_bytes BLOB NOT NULL,
    tile_height_px INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (message_id, width_px, theme, remote_policy, tile_index),
    FOREIGN KEY (message_id) REFERENCES messages(id)
);
