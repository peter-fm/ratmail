---
name: ratmail-cli
description: Operate the ratmail CLI and JSON output. Use when running ratmail CLI subcommands (accounts/folders/messages/message/sync/send/attachment-save), enabling CLI access in ratmail.toml, and explaining CLI output/schema.
---

# Ratmail CLI

## Goals

- Operate the ratmail CLI safely and predictably.
- Configure `ratmail.toml` correctly.
- Produce or consume JSON output reliably.

## CLI Enablement (Required)

1. Confirm `ratmail.toml` exists in the current directory or `~/.config/ratmail/ratmail.toml`.
2. Add/verify:

```toml
[cli]
enabled = true
default_account = "Personal"
```

3. Run setup if needed:

```bash
ratmail setup
```

## Core Commands

Prefer explicit `--account` and `--folder` for reproducibility.

- `ratmail accounts list`
- `ratmail folders list --account Personal`
- `ratmail messages list --account Personal --folder INBOX --limit 20`
- `ratmail messages list --account Personal --folder INBOX --query "from:alice subject:invoice type:pdf"`
- `ratmail message get --account Personal --id 123`
- `ratmail message get --account Personal --id 123 --body --fetch`
- `ratmail message attachment-save --account Personal --id 123 --index 0 --path /tmp/invoice.pdf --fetch`
- `ratmail sync --account Personal --folder INBOX --wait --timeout-secs 60`
- `ratmail send --account Personal --to alice@example.com --subject "Hi" --body "Test" --wait`

## Output Expectations

- CLI output is JSON with schema `ratmail.cli.v1`.
- Failures are returned as JSON (`ok: false`) with an `error` string.
- Missing bodies/attachments can mean "not fetched" unless `--fetch` is used.

## Troubleshooting

- If commands fail: verify `[cli].enabled = true` and account configuration.
- If CLI reports disabled on startup: check the active `ratmail.toml` path and syntax.
- If data is missing: verify account selection, folder selection, and whether `--fetch` is required.
- If Proton Bridge is used: set `skip_tls_verify = true` and Bridge ports/credentials.
