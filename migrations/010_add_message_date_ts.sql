ALTER TABLE messages ADD COLUMN date_ts INTEGER;
CREATE INDEX messages_date_ts_idx ON messages(date_ts);
