CREATE UNIQUE INDEX IF NOT EXISTS messages_folder_uid_unique
ON messages(folder_id, imap_uid)
WHERE imap_uid IS NOT NULL;
