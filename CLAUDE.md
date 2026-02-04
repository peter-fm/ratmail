# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo run -p ratmail        # Run main application
cargo run-ratmail           # Alias for above
cargo build --release -p ratmail  # Release build
cargo check                 # Fast compile check
```

## Code Quality

```bash
cargo fmt                   # Format code
cargo clippy --all-targets --all-features  # Lint
```

## Testing

No tests exist yet. When added:
```bash
cargo test --workspace      # All tests
cargo test -p ratmail-core  # Single crate
```

## Environment Variables

- `RATMAIL_LOG=1` - Enable debug logging to `~/.local/state/ratmail/ratmail.log`
- `RATMAIL_CHROME_PATH` - Custom Chrome/Chromium path for HTML rendering
- `RATMAIL_CHROME_NO_SANDBOX=1` - Disable Chrome sandbox (containers)
- `RATMAIL_FORCE_IMAGES=1` - Force image rendering in unsupported terminals

## Architecture

Five-crate workspace with layered dependencies:

```
ratmail-tui (binary)
    ├── ratmail-render (HTML→image tiles via headless Chrome)
    ├── ratmail-content (email parsing, HTML sanitization)
    ├── ratmail-mail (IMAP/SMTP protocols, actor pattern)
    └── ratmail-core (SQLite storage, data models)
```

**ratmail-core** - Data models (`Account`, `Folder`, `MessageSummary`, `MessageDetail`), `MailStore` trait for persistence, SQLite implementation with caching (text, HTML, rendered tiles).

**ratmail-mail** - `MailEngine` actor with mpsc channels. `MailCommand`/`MailEvent` enums for async IMAP sync and SMTP sending.

**ratmail-content** - `extract_display()` for plain text, `prepare_html()` for sanitized HTML. Handles MIME parsing, link extraction, CID image inlining, remote image blocking.

**ratmail-render** - `Renderer` trait with `ChromiumRenderer` (headless Chrome) and `NullRenderer` fallback. Generates PNG tiles for terminal display.

**ratmail-tui** - Main binary. Ratatui-based UI with modes (List, View, ViewFocus, Compose). LRU caches for render tiles and protocol handlers. Async workers for mail, rendering, storage.

## Database

SQLite at `ratmail.db`. Migrations in `/migrations/`. Tables: accounts, folders, messages, bodies, cache_text, cache_html, cache_tiles.

## Configuration

Copy `ratmail.toml.example` to `ratmail.toml` for IMAP/SMTP credentials.
