-- Add tile_height_px to PRIMARY KEY to prevent Side/Focus view cache collisions
-- Side view uses 1200px tiles, Focus view uses 120px tiles
-- Without tile_height_px in the key, they overwrite each other causing cache misses

CREATE TABLE cache_tiles_new (
    message_id INTEGER NOT NULL,
    width_px INTEGER NOT NULL,
    tile_height_px INTEGER NOT NULL,
    theme TEXT NOT NULL,
    remote_policy TEXT NOT NULL,
    tile_index INTEGER NOT NULL,
    png_bytes BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (message_id, width_px, tile_height_px, theme, remote_policy, tile_index),
    FOREIGN KEY (message_id) REFERENCES messages(id)
);

INSERT OR IGNORE INTO cache_tiles_new
SELECT message_id, width_px, tile_height_px, theme, remote_policy, tile_index, png_bytes, updated_at
FROM cache_tiles;

DROP TABLE cache_tiles;
ALTER TABLE cache_tiles_new RENAME TO cache_tiles;
