---
name: ratmail-cli
description: Operate the ratmail CLI and configure safe JSON output for scripting. Use when running ratmail CLI subcommands (accounts/folders/messages/message/sync/send), enabling or adjusting CLI access in ratmail.toml, troubleshooting CLI access or data visibility, or explaining CLI output/schema.
---

# Ratmail CLI

## Goals

- Operate the ratmail CLI safely and predictably.
- Configure `ratmail.toml` to allow only the required CLI access.
- Produce or consume JSON output reliably.

## Safety First

- Prefer `mode = "readonly"` unless the task explicitly needs mutation.
- Keep `allow_commands` tight and fields minimal to reduce prompt injection risk.
- Avoid `allow_body`, `allow_raw`, `allow_attachments` unless needed.

## Defaults When Enabled

- `mode` defaults to `readonly` if not set.
- Reads are unscoped unless you set `cli.acl` (headers across all accounts/folders are accessible).
- Bodies, raw, and attachments remain blocked unless explicitly allowed.

## CLI Enablement (Required)

1. Confirm `ratmail.toml` exists in the current directory or `~/.config/ratmail/ratmail.toml`.
2. Add/verify the `cli` section:

```toml
[cli]
enabled = true
mode = "readonly" # "readonly" | "readonly-full-access" | "full-access"
default_account = "Personal"

[cli.acl]
accounts = ["Personal"]
folders = ["INBOX"]
from_allow = ["*@trusted.com"]
fields = ["id", "folder_id", "date", "from", "subject", "unread"]
allow_commands = ["accounts.list", "folders.list", "messages.list", "message.get"]
allow_body = false
allow_raw = false
allow_attachments = false
allow_move = false
allow_delete = false
allow_mark = false
allow_send = false
```

3. If additional commands are needed, expand only the relevant `allow_*` and `allow_commands` entries.

## Core Commands

Prefer explicit `--account` and `--folder` for reproducibility.

- `ratmail accounts list`
- `ratmail folders list --account Personal`
- `ratmail messages list --account Personal --folder INBOX --limit 20`
- `ratmail message get --account Personal --id 123`
- `ratmail message get --account Personal --id 123 --body --fetch`
- `ratmail sync --account Personal --folder INBOX --wait --timeout-secs 60`
- `ratmail send --account Personal --to alice@example.com --subject "Hi" --body "Test" --wait`

## Output Expectations

- CLI output is JSON and includes a `schema` field (currently `ratmail.cli.v1`).
- Treat missing bodies as “not fetched” unless `--fetch` is used and the message is cached.

## Troubleshooting

- If commands fail: check `cli.enabled = true`, `mode`, and `allow_commands`.
- If data is missing: check `cli.acl` scoping for `accounts`, `folders`, `from_allow`, and `fields`.
- If `--fetch` is slow: reduce `fetch_chunk_size` in IMAP config or reduce `initial_sync_days`.
- If Proton Bridge is used: set `skip_tls_verify = true` and use Bridge credentials and ports.

## Minimal Readonly Preset

Use this when an agent only needs to list accounts/folders and read headers:

```toml
[cli]
enabled = true
mode = "readonly"
default_account = "Personal"

[cli.acl]
accounts = ["Personal"]
folders = ["INBOX"]
from_allow = ["*@trusted.com"]
fields = ["id", "folder_id", "date", "from", "subject", "unread"]
allow_commands = ["accounts.list", "folders.list", "messages.list", "message.get"]
```
