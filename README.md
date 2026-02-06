# ratmail

Terminal email client with dual-channel viewing (rendered HTML + high-quality text).

## Features

- IMAP sync with local cache (SQLite)
- Multi-account tabs with per-account databases
- Rendered HTML preview with Kitty/Sixel support
- Fast text view with link and attachment overlays

## Getting started

1. Install from crates.io:

```bash
cargo install ratmail
```

2. Create config:

```bash
cp ratmail.toml.example ratmail.toml
```

3. Edit `ratmail.toml` with your account details (see Configuration below).

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

## Install (from crates.io)

```bash
cargo install ratmail
```

## Install (from GitHub)

```bash
cargo install --git https://github.com/peter-fm/ratmail.git --locked
```

## Configuration

Copy `ratmail.toml.example` to `ratmail.toml` and fill in one or more accounts:

```toml
[[accounts]]
name = "Personal"
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
3. Copy `ratmail.toml.example` to `ratmail.toml` and fill it in (supports multiple accounts):

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
