CREATE UNIQUE INDEX messages_folder_uid_unique_full
ON messages(folder_id, imap_uid);
