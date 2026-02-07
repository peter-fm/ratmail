# ratmail

Terminal email client with dual-channel viewing (rendered HTML + high-quality text).

## Features

- IMAP sync with local cache (SQLite)
- Multi-account tabs with per-account databases
- Rendered HTML preview with Kitty/Sixel support
- Fast text view with link and attachment overlays

## Getting started

1. Install from GitHub:

```bash
cargo install --git https://github.com/peter-fm/ratmail.git --locked
```

2. Create config:

```bash
mkdir -p ~/.config/ratmail
cp ratmail.toml.example ~/.config/ratmail/ratmail.toml
```

3. Edit `~/.config/ratmail/ratmail.toml` with your account details (see Configuration below).

4. Run:

```bash
ratmail
```

## Run (dev)

```bash
cargo run -p ratmail
```

Or use the workspace alias:

```bash
cargo run-ratmail
```

## Install (from GitHub)

```bash
cargo install --git https://github.com/peter-fm/ratmail.git --locked
```

## Configuration

Ratmail looks for `ratmail.toml` in the current directory first, then in
`~/.config/ratmail/ratmail.toml` (or `$XDG_CONFIG_HOME/ratmail/ratmail.toml`).
Relative `db_path` values are stored under `~/.local/state/ratmail`
(or `$XDG_STATE_HOME/ratmail`).

Copy `ratmail.toml.example` to `~/.config/ratmail/ratmail.toml` and fill in one or more accounts:

```toml
[[accounts]]
name = "Personal"
# Relative paths are stored under ~/.local/state/ratmail
db_path = "ratmail-personal.db"

[accounts.imap]
host = "imap.example.com"
port = 993
username = "user@example.com"
password = "app-password-or-imap-password"
skip_tls_verify = false
initial_sync_days = 90
fetch_chunk_size = 10

[accounts.smtp]
host = "smtp.example.com"
port = 587
username = "user@example.com"
password = "app-password-or-smtp-password"
from = "Your Name <user@example.com>"

[render]
remote_images = false
width_px = 1000
render_scale = 1.0
tile_height_px_side = 5000
tile_height_px_focus = 120
```

Notes:
- If `db_path` is omitted, it defaults to `ratmail-<account-name>.db`.
- `initial_sync_days` controls the first sync window; older mail can be loaded on demand.
- `fetch_chunk_size` is intentionally small for Proton Bridge reliability.

## Multi-account tabs

- Tabs are shown in the top bar (e.g., `1:Personal 2:Work`).
- Switch accounts with `[` / `]` or by pressing a number key.

## Key bindings (core)

- `Tab` / `h` / `l`: focus panes
- `j` / `k`: move
- `Enter`: open message
- `v`: toggle rendered/text view
- `p`: toggle preview pane
- `o`: load older messages (backfill)
- `?`: toggle help
- `q`: quit

## CLI (JSON output)

Ratmail can run CLI subcommands that return JSON for scripting (jq-friendly). The CLI is **disabled by default** for safety. Enable and scope access in `ratmail.toml`:

```toml
[cli]
enabled = true
mode = "readonly" # "readonly" | "readonly-full-access" | "full-access"
default_account = "Personal"

[cli.acl]
accounts = ["Personal"]
folders = ["INBOX"]
from_allow = ["*@example.com"]
fields = ["id", "folder_id", "date", "from", "subject", "unread"]
```

Security notes:
- Use `from_allow`, `fields`, and a minimal `allow_commands` list to reduce prompt-injection surface for agents.
- `mode = "readonly"` blocks all mutations. `readonly-full-access` allows full read (including bodies/raw/attachments). `full-access` enables mutations if allowed by ACL.
- CLI remains disabled unless `enabled = true` is set.
- CLI outputs include a `schema` field (currently `ratmail.cli.v1`) for stable parsing.
- `--fetch` pulls raw bodies from IMAP when not cached (requires IMAP + UID); otherwise body/raw may be empty.
- `--wait` waits for a best-effort completion signal (IMAP fetch events). It is not a hard guarantee that every message is synced.

Preset: Agent-safe (locked down)

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
allow_body = false
allow_raw = false
allow_attachments = false
allow_move = false
allow_delete = false
allow_mark = false
allow_send = false
```

Preset: Full access (use with care)

```toml
[cli]
enabled = true
mode = "full-access"
default_account = "Personal"

[cli.acl]
accounts = ["*"]
folders = ["*"]
from_allow = ["*"]
```

Examples:

```bash
ratmail accounts list
ratmail folders list --account Personal
ratmail messages list --account Personal --folder INBOX --limit 20
ratmail message get --account Personal --id 123
ratmail message get --account Personal --id 123 --body --fetch
ratmail sync --account Personal --folder INBOX --wait --timeout-secs 60
ratmail send --account Personal --to alice@example.com --subject "Hi" --body "Test" --wait
```

## Proton Mail Bridge (Linux)

Bridge uses a local IMAP/SMTP server with a self-signed cert. Use:

```toml
[accounts.imap]
host = "127.0.0.1"
port = 1143
skip_tls_verify = true
```

Common gotchas:
- Use the Bridge-provided username/password (not your Proton password).
- Match the Bridge IMAP/SMTP ports and security mode (STARTTLS on 1143 is typical).
- If sync stalls, reduce `fetch_chunk_size` (e.g., 5) and keep `initial_sync_days` small.

## SMTP setup (consumer Gmail)

The easiest way to use Gmail SMTP is with an **App Password** (requires 2‑Step Verification).

1. Enable 2‑Step Verification on your Google account.
2. Create an App Password for “Mail”.
3. Copy `ratmail.toml.example` to `~/.config/ratmail/ratmail.toml` and fill it in (supports multiple accounts):

```toml
[[accounts]]
name = "Gmail"
db_path = "ratmail-gmail.db"

[accounts.smtp]
host = "smtp.gmail.com"
port = 587
username = "you@gmail.com"
password = "your-16-char-app-password"
from = "Your Name <you@gmail.com>"
```

Notes:
- Gmail requires an app password for SMTP when 2‑Step Verification is enabled.
- OAuth (browser login) is possible but not implemented yet.
