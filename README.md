# ratmail

Terminal email client with dual-channel viewing (rendered HTML + high-quality text).

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
ratmail
```

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
