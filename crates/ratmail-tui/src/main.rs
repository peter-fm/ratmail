use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use html_escape::{encode_double_quoted_attribute, encode_safe};
use linkify::{LinkFinder, LinkKind};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap},
};

use image::{DynamicImage, GenericImageView};
use ratatui_explorer::FileExplorer;
use ratatui_image::{
    Image, Resize, StatefulImage, picker::Picker, protocol::ImageSource, protocol::Protocol,
    protocol::StatefulProtocol,
};
use ratmail_content::extract_display;
use ratmail_core::{
    AttachmentMeta, DEFAULT_TEXT_WIDTH, Folder, FolderSyncState, LinkInfo, MailStore,
    MessageDetail, MessageSummary, SqliteMailStore, StoreSnapshot, TileMeta, log_debug,
};
use ratmail_mail::{ImapConfig, MailEngine, MailEvent, SmtpConfig};
use ratmail_render::{ChromiumRenderer, NullRenderer, Renderer, detect_image_support};
use shell_words::split as shell_split;
use spellbook::Dictionary;
use std::sync::Arc;
use unicode_width::UnicodeWidthChar as _;

mod app_lifecycle_mod;
mod cli;
mod compose_actions_mod;
mod compose_buffer_mod;
mod compose_mod;
mod input_compose_mod;
mod input_main_mod;
mod input_overlay_mod;
mod list_state_mod;
mod message_actions_mod;
mod message_parse_mod;
mod multi_app_mod;
mod overlay_mod;
mod picker_actions_mod;
mod render_mod;
mod render_state_mod;
mod sync_mod;
mod ui_theme_mod;
mod util_mod;

use crate::cli::{
    Cli, CliCommand, from_matches_filter, load_render_config, load_send_config, load_spell_config,
    load_ui_config, output_error, parse_from_addrs, resolve_cli_command, run_cli,
};
use crate::compose_buffer_mod::compose_buffer_from_body;
use crate::compose_mod::render_compose_overlay;
use crate::message_parse_mod::{
    build_forward, build_reply, cc_from_raw, draft_headers_from_raw, extract_email,
    mailaddrs_to_emails, to_from_raw,
};
use crate::overlay_mod::{
    render_attach_overlay, render_bulk_action_overlay, render_bulk_move_overlay,
    render_confirm_delete_overlay, render_confirm_draft_overlay, render_confirm_link_overlay,
    render_links_overlay, render_picker_overlay, render_search_overlay, render_spellcheck_overlay,
};
use crate::render_mod::{RenderEvent, RenderRequest, render_worker};
use crate::util_mod::{
    format_size, picker_meta_lines, render_pdf_first_page, safe_filename, text_preview_from_bytes,
    zip_directory,
};

const TICK_RATE: Duration = Duration::from_millis(200);
const PROTOCOL_CACHE_LIMIT: usize = 16;
const TILE_CACHE_BUDGET_BYTES: i64 = 256 * 1024 * 1024;
const CLI_SCHEMA_VERSION: &str = "ratmail.cli.v1";
const IMAP_SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
const RAT_SPINNER_FRAMES: [[&str; 6]; 8] = [
    [
        "██████╗  █████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "▓█████╗  █████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  ▓████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  █████╗ ████████╗▓██╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  █████╗ ████████╗███╗   ███╗ █████╗ ▓█╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  █████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗▓█║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  █████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║▓██████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    ],
    [
        "██████╗  █████╗ ████████╗███╗   ███╗ █████╗ ██╗██╗",
        "██╔══██╗██╔══██╗╚══██╔══╝████╗ ████║██╔══██╗██║██║",
        "██████╔╝███████║   ██║   ██╔████╔██║███████║██║██║",
        "██╔══██╗██╔══██║   ██║   ██║╚██╔╝██║██╔══██║██║██║",
        "██║  ██║██║  ██║   ██║   ██║ ╚═╝ ██║██║  ██║██║███████╗",
        "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝▓══════╝",
    ],
];
const PICKER_PREVIEW_MAX_BYTES: usize = 64 * 1024;
const PICKER_PREVIEW_MAX_LINES: usize = 200;
const PICKER_IMAGE_PREVIEW_MAX_BYTES: u64 = 10 * 1024 * 1024;
const PICKER_PDF_PREVIEW_MAX_BYTES: u64 = 20 * 1024 * 1024;
const STORE_UPDATE_QUEUE_CAPACITY: usize = 128;
const STORE_SNAPSHOT_QUEUE_CAPACITY: usize = 32;
const RENDER_EVENT_QUEUE_CAPACITY: usize = 64;
static SPELL_DICTIONARY: OnceLock<Option<Dictionary>> = OnceLock::new();
static SPELL_CONFIG: OnceLock<SpellConfig> = OnceLock::new();
static SPELL_IGNORE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(hi), Some(lo)) = (from_hex(hi), from_hex(lo)) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_mailto(link: &str) -> Option<(String, Option<String>, Option<String>)> {
    if !link.to_lowercase().starts_with("mailto:") {
        return None;
    }
    let rest = &link["mailto:".len()..];
    let (addr_raw, query) = rest.split_once('?').unwrap_or((rest, ""));
    let addr = percent_decode(addr_raw).trim().to_string();
    if addr.is_empty() {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    if !query.is_empty() {
        for part in query.split('&') {
            let (key, val) = part.split_once('=').unwrap_or((part, ""));
            match key.to_ascii_lowercase().as_str() {
                "subject" => subject = Some(percent_decode(val)),
                "body" => body = Some(percent_decode(val)),
                _ => {}
            }
        }
    }
    Some((addr, subject, body))
}

fn looks_like_email(link: &str) -> bool {
    let trimmed = link.trim();
    if trimmed.is_empty() || trimmed.contains(' ') || trimmed.contains("://") {
        return false;
    }
    let mut parts = trimmed.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    !local.is_empty() && domain.contains('.')
}

#[derive(Debug, Clone)]
enum StoreUpdate {
    Folders {
        account_id: i64,
        folders: Vec<Folder>,
    },
    AppendMessages {
        account_id: i64,
        folder_name: String,
        messages: Vec<MessageSummary>,
        sync_update: Option<SyncUpdate>,
    },
    RawBody {
        account_id: i64,
        message_id: i64,
        raw: Vec<u8>,
        cached_text: Option<String>,
    },
    MoveMessages {
        account_id: i64,
        message_ids: Vec<i64>,
        target_folder_id: i64,
        refresh_folder_id: i64,
    },
    DeleteMessages {
        account_id: i64,
        message_ids: Vec<i64>,
        refresh_folder_id: i64,
    },
    SetMessagesUnread {
        account_id: i64,
        message_ids: Vec<i64>,
        unread: bool,
        refresh_folder_id: i64,
    },
    SaveDraft {
        account_id: i64,
        from_addr: String,
        to: String,
        cc: String,
        bcc: String,
        subject: String,
        body: String,
    },
}

#[derive(Debug, Clone)]
struct SyncUpdate {
    last_seen_uid: Option<i64>,
    oldest_ts: Option<i64>,
    last_sync_ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    View,
    ViewFocus,
    Compose,
    OverlaySearch,
    OverlaySpellcheck,
    OverlayLinks,
    OverlayAttach,
    OverlayBulkAction,
    OverlayBulkMove,
    OverlayConfirmDelete,
    OverlayConfirmLink,
    OverlayConfirmDraft,
}

#[derive(Debug, Clone)]
enum PickerMode {
    Attach,
    Save {
        message_id: i64,
        attachment_index: usize,
        filename: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerFocus {
    Explorer,
    Filename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerPreviewKind {
    Empty,
    Text,
    Image,
    PdfImage,
    Meta,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Folders,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeFocus {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualMove {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpellTarget {
    Subject,
    Body,
}

#[derive(Debug, Clone)]
struct SpellIssue {
    target: SpellTarget,
    start: usize,
    end: usize,
    word: String,
    suggestions: Vec<String>,
}

#[derive(Debug, Clone)]
struct InlineSpellSuggest {
    start: usize,
    end: usize,
    suggestions: Vec<String>,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeVimMode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, Default)]
struct SearchSpec {
    text: String,
    attachment_name: Vec<String>,
    attachment_type: Vec<String>,
    from: Vec<String>,
    subject: Vec<String>,
    to: Vec<String>,
    date: Vec<String>,
    since_ts: Option<i64>,
    before_ts: Option<i64>,
}

impl SearchSpec {
    fn needs_attachments(&self) -> bool {
        !self.attachment_name.is_empty() || !self.attachment_type.is_empty()
    }

    fn needs_raw(&self) -> bool {
        self.needs_attachments() || !self.to.is_empty()
    }
}

fn parse_search_spec(raw: &str) -> SearchSpec {
    let mut spec = SearchSpec::default();
    let mut text_parts = Vec::new();
    for token in raw.split_whitespace() {
        let lowered = token.to_ascii_lowercase();
        let Some((key, value)) = lowered.split_once(':') else {
            text_parts.push(lowered);
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            text_parts.push(lowered);
            continue;
        }
        match key {
            "att" | "file" | "filename" => {
                spec.attachment_name.push(value.to_string());
            }
            "type" | "mime" => {
                let trimmed = value.trim_start_matches('.');
                if !trimmed.is_empty() {
                    spec.attachment_type.push(trimmed.to_string());
                }
            }
            "from" => spec.from.push(value.to_string()),
            "subject" => spec.subject.push(value.to_string()),
            "to" => spec.to.push(value.to_string()),
            "date" => spec.date.push(value.to_string()),
            "since" => {
                if let Ok(ts) = mailparse::dateparse(value) {
                    spec.since_ts = Some(ts);
                } else {
                    text_parts.push(lowered);
                }
            }
            "before" => {
                if let Ok(ts) = mailparse::dateparse(value) {
                    spec.before_ts = Some(ts);
                } else {
                    text_parts.push(lowered);
                }
            }
            _ => {
                text_parts.push(lowered);
            }
        }
    }
    spec.text = text_parts.join(" ").trim().to_string();
    spec
}

fn compose_focus_next(current: ComposeFocus) -> ComposeFocus {
    match current {
        ComposeFocus::To => ComposeFocus::Cc,
        ComposeFocus::Cc => ComposeFocus::Bcc,
        ComposeFocus::Bcc => ComposeFocus::Subject,
        ComposeFocus::Subject => ComposeFocus::Body,
        ComposeFocus::Body => ComposeFocus::To,
    }
}

fn compose_focus_prev(current: ComposeFocus) -> ComposeFocus {
    match current {
        ComposeFocus::To => ComposeFocus::Body,
        ComposeFocus::Cc => ComposeFocus::To,
        ComposeFocus::Bcc => ComposeFocus::Cc,
        ComposeFocus::Subject => ComposeFocus::Bcc,
        ComposeFocus::Body => ComposeFocus::Subject,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Text,
    Rendered,
}

struct App {
    mode: Mode,
    focus: Focus,
    view_mode: ViewMode,
    store: StoreSnapshot,
    folder_index: usize,
    message_index: usize,
    last_tick: Instant,
    sync_status: String,
    engine: MailEngine,
    events: tokio::sync::mpsc::Receiver<MailEvent>,
    store_handle: SqliteMailStore,
    runtime: Arc<tokio::runtime::Runtime>,
    overlay_return: Mode,
    view_scroll: u16,
    render_supported: bool,
    render_tile_count: usize,
    render_tiles: Vec<TileMeta>,
    render_tile_index: usize,
    render_tiles_height_px: i64,
    render_tiles_width_px: i64,
    render_request_id: u64,
    render_message_id: Option<i64>,
    render_pending: bool,
    render_pending_message_id: Option<i64>,
    render_pending_tile_height_px: i64,
    render_pending_width_px: i64,
    pending_body_fetch: HashSet<i64>,
    image_picker: Option<Picker>,
    image_protocol: Option<StatefulProtocol>,
    protocol_cache: HashMap<(i64, usize), StatefulProtocol>,
    protocol_cache_lru: VecDeque<(i64, usize)>,
    protocol_cache_static: HashMap<(i64, usize), Protocol>,
    tile_rows_cache: HashMap<(i64, usize), u16>,
    last_render_area: Option<(u16, u16)>,
    renderer_is_chromium: bool,
    render_error: Option<String>,
    render_no_html: bool,
    render_request_tx: tokio::sync::watch::Sender<RenderRequest>,
    render_events: tokio::sync::mpsc::Receiver<RenderEvent>,
    store_update_tx: tokio::sync::mpsc::Sender<StoreUpdate>,
    store_updates: tokio::sync::mpsc::Receiver<StoreSnapshot>,
    allow_remote_images: bool,
    render_width_px: i64,
    render_tile_height_px: i64,
    render_tile_height_px_focus: i64,
    render_tile_height_px_side: i64,
    render_spinner: usize,
    render_scale: f64,
    ui_theme_name: String,
    ui_theme: Arc<UiTheme>,
    send_config: SendConfig,
    show_preview: bool,
    folder_pane_width: u16,
    show_help: bool,
    link_index: usize,
    attach_index: usize,
    status_message: Option<String>,
    search_query: String,
    search_cursor: usize,
    search_spec: SearchSpec,
    search_attachment_queue: VecDeque<i64>,
    attachment_checked: HashSet<i64>,
    attachment_cache: HashMap<i64, Vec<AttachmentMeta>>,
    selected_message_ids: HashSet<i64>,
    bulk_action_ids: Vec<i64>,
    bulk_folder_index: usize,
    bulk_done_return: Mode,
    confirm_delete_ids: Vec<i64>,
    confirm_delete_return: Mode,
    confirm_link: Option<LinkInfo>,
    confirm_link_external: bool,
    confirm_link_return: Mode,
    picker_mode: Option<PickerMode>,
    picker_focus: PickerFocus,
    picker: Option<FileExplorer>,
    picker_filename: String,
    picker_cursor: usize,
    picker_filter: String,
    picker_preview_path: Option<PathBuf>,
    picker_preview_kind: PickerPreviewKind,
    picker_preview_text: String,
    picker_preview_meta: Vec<String>,
    picker_preview_image: Option<DynamicImage>,
    picker_preview_protocol: Option<StatefulProtocol>,
    picker_pdf_preview_available: Option<bool>,
    picker_preview_error: Option<String>,
    imap_enabled: bool,
    last_folder_sync: Option<(String, Instant)>,
    last_backfill: Option<(String, Instant)>,
    imap_pending: usize,
    imap_spinner: usize,
    imap_status: Option<String>,
    initial_sync_days: i64,
    compose_to: String,
    compose_cc: String,
    compose_bcc: String,
    compose_subject: String,
    compose_body: ComposeBuffer,
    compose_quote: String,
    compose_attachments: Vec<ComposeAttachment>,
    compose_vim_enabled: bool,
    compose_vim_mode: ComposeVimMode,
    compose_vim_pending: Option<char>,
    compose_focus: ComposeFocus,
    compose_cursor_to: usize,
    compose_cursor_cc: usize,
    compose_cursor_bcc: usize,
    compose_cursor_subject: usize,
    compose_body_desired_x: Option<usize>,
    compose_body_area_width: u16,
    compose_body_area_height: u16,
    compose_address_book: HashSet<String>,
    compose_address_list: Vec<String>,
    spell_issues: Vec<SpellIssue>,
    spell_issue_index: usize,
    spell_suggestion_index: usize,
    spell_return: Mode,
    inline_spell_suggest: Option<InlineSpellSuggest>,
}

struct MultiApp {
    apps: Vec<App>,
    current: usize,
}

#[derive(Debug, Clone)]
struct ComposeBuffer {
    text: String,
    cursor: usize,
    tab_len: u8,
    scroll_top: usize,
}

fn build_html_body(text: &str, config: &SendConfig) -> Option<String> {
    if !config.html {
        return None;
    }
    let font_family = config.html_font_family.trim();
    let font_family = if font_family.is_empty() {
        "Arial, sans-serif"
    } else {
        font_family
    };
    let font_size_px = config.html_font_size_px.clamp(8, 72);
    let style = format!(
        "font-family: {}; font-size: {}px; line-height: 1.4; white-space: pre-wrap; margin: 0;",
        font_family, font_size_px
    );
    let style = encode_double_quoted_attribute(&style);
    let escaped = encode_safe(text);
    Some(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><div style=\"{}\">{}</div></body></html>",
        style, escaped
    ))
}

#[derive(Debug, Clone)]
struct ComposeAttachment {
    filename: String,
    mime: String,
    size: usize,
    data: Vec<u8>,
}

fn apply_compose_key(
    target: &mut String,
    cursor: &mut usize,
    key: KeyEvent,
    allow_newline: bool,
) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Backspace => {
            if *cursor > 0 {
                remove_char_at(target, *cursor - 1);
                *cursor -= 1;
                return true;
            }
        }
        KeyCode::Delete => {
            let len = text_char_len(target);
            if *cursor < len {
                remove_char_at(target, *cursor);
                return true;
            }
        }
        KeyCode::Char(c) => {
            let idx = char_to_byte_idx(target, *cursor);
            target.insert_str(idx, c.encode_utf8(&mut [0; 4]));
            *cursor += 1;
            return true;
        }
        KeyCode::Enter => {
            if allow_newline {
                let idx = char_to_byte_idx(target, *cursor);
                target.insert(idx, '\n');
                *cursor += 1;
                return true;
            }
        }
        _ => {}
    }
    *cursor = clamp_cursor(*cursor, target);
    false
}

fn apply_input_key(target: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Backspace => {
            if *cursor > 0 {
                remove_char_at(target, *cursor - 1);
                *cursor -= 1;
                return true;
            }
        }
        KeyCode::Delete => {
            let len = text_char_len(target);
            if *cursor < len {
                remove_char_at(target, *cursor);
                return true;
            }
        }
        KeyCode::Char(c) => {
            let idx = char_to_byte_idx(target, *cursor);
            target.insert_str(idx, c.encode_utf8(&mut [0; 4]));
            *cursor += 1;
            return true;
        }
        _ => {}
    }
    *cursor = clamp_cursor(*cursor, target);
    false
}

fn build_sync_update(items: &[MessageSummary]) -> Option<SyncUpdate> {
    if items.is_empty() {
        return None;
    }
    let mut last_seen_uid: Option<i64> = None;
    let mut oldest_ts: Option<i64> = None;
    for item in items {
        if let Some(uid) = item.imap_uid.map(|v| v as i64) {
            last_seen_uid = Some(last_seen_uid.map_or(uid, |prev| prev.max(uid)));
        }
        if let Ok(ts) = mailparse::dateparse(&item.date) {
            oldest_ts = Some(oldest_ts.map_or(ts, |prev| prev.min(ts)));
        }
    }
    let last_sync_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(SyncUpdate {
        last_seen_uid,
        oldest_ts,
        last_sync_ts,
    })
}

fn text_char_len(text: &str) -> usize {
    text.chars().count()
}

fn clamp_cursor(cursor: usize, text: &str) -> usize {
    cursor.min(text_char_len(text))
}

fn char_to_byte_idx(text: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let mut count = 0usize;
    for (byte_idx, _) in text.char_indices() {
        if count == char_idx {
            return byte_idx;
        }
        count += 1;
    }
    text.len()
}

fn remove_char_at(text: &mut String, char_idx: usize) {
    let start = char_to_byte_idx(text, char_idx);
    let end = char_to_byte_idx(text, char_idx + 1);
    if start < end {
        text.replace_range(start..end, "");
    }
}

fn move_cursor_left(text: &str, cursor: &mut usize) {
    let len = text_char_len(text);
    *cursor = (*cursor).min(len);
    if *cursor > 0 {
        *cursor -= 1;
    }
}

fn move_cursor_right(text: &str, cursor: &mut usize) {
    let len = text_char_len(text);
    *cursor = (*cursor).min(len);
    if *cursor < len {
        *cursor += 1;
    }
}

fn next_index(current: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (current + 1) % len }
}

fn prev_index(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else if current == 0 {
        len - 1
    } else {
        current - 1
    }
}

fn compose_token_at_cursor(text: &str, cursor: usize) -> Option<(usize, usize, String)> {
    let len = text_char_len(text);
    if cursor > len {
        return None;
    }
    let mut start = cursor;
    while start > 0 {
        let ch = text.chars().nth(start - 1)?;
        if ch == ',' || ch == ';' || ch.is_whitespace() {
            break;
        }
        start -= 1;
    }
    let mut end = cursor;
    while end < len {
        let ch = text.chars().nth(end)?;
        if ch == ',' || ch == ';' || ch.is_whitespace() {
            break;
        }
        end += 1;
    }
    if cursor != end {
        return None;
    }
    let start_idx = char_to_byte_idx(text, start);
    let end_idx = char_to_byte_idx(text, cursor);
    if start_idx >= end_idx {
        return None;
    }
    let token = text[start_idx..end_idx].to_string();
    if token.trim().is_empty() {
        return None;
    }
    Some((start, end, token))
}

fn compose_autocomplete_suffix(addresses: &[String], text: &str, cursor: usize) -> Option<String> {
    let (_, _, prefix) = compose_token_at_cursor(text, cursor)?;
    let prefix_lower = prefix.to_ascii_lowercase();
    let suggestion = addresses
        .iter()
        .find(|addr| addr.starts_with(&prefix_lower) && *addr != &prefix_lower)?;
    let suffix = suggestion
        .chars()
        .skip(prefix_lower.chars().count())
        .collect::<String>();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

fn cursor_line_col(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    let mut idx = 0usize;
    let max = clamp_cursor(cursor, text);
    for ch in text.chars() {
        if idx == max {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
        idx += 1;
    }
    (line, col)
}

fn set_cursor_at(frame: &mut ratatui::Frame, area: Rect, text: &str, cursor: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (line, col) = cursor_line_col(text, cursor);
    let max_x = area.width.saturating_sub(1);
    let max_y = area.height.saturating_sub(1);
    let x = area.x + (col as u16).min(max_x);
    let y = area.y + (line as u16).min(max_y);
    frame.set_cursor_position((x, y));
}

fn cursor_from_char_index(text: &str, index: usize) -> (usize, usize) {
    cursor_line_col(text, index)
}

fn char_index_from_row_col(text: &str, row: usize, col: usize) -> usize {
    let mut idx = 0usize;
    for (r, line) in text.lines().enumerate() {
        let line_len = line.chars().count();
        if r == row {
            idx += col.min(line_len);
            return idx;
        }
        idx += line_len + 1;
    }
    idx
}

fn replace_range_chars(text: &mut String, start: usize, end: usize, replacement: &str) {
    let start_idx = char_to_byte_idx(text, start);
    let end_idx = char_to_byte_idx(text, end);
    if start_idx <= end_idx && end_idx <= text.len() {
        text.replace_range(start_idx..end_idx, replacement);
    }
}

fn is_spell_word_char(ch: char) -> bool {
    ch.is_alphabetic() || ch == '\''
}

fn spell_word_misspelled(word: &str, dict: &Dictionary) -> bool {
    let cleaned = word.trim_matches('\'');
    if cleaned.len() < 2 {
        return false;
    }
    if cleaned.chars().all(|c| c.is_uppercase()) {
        return false;
    }
    if spell_ignore_contains(cleaned) {
        return false;
    }
    let lowered = cleaned.to_ascii_lowercase();
    !(dict.check(cleaned) || dict.check(&lowered))
}

fn spell_line_highlights(line: &str, theme: &UiTheme) -> Vec<(usize, usize, Style)> {
    let Some(dict) = spell_dictionary() else {
        return Vec::new();
    };
    let email_ranges = email_link_ranges(line);
    let mut out = Vec::new();
    let mut start_byte: Option<usize> = None;
    let mut current = String::new();
    for (idx, ch) in line.char_indices() {
        if is_spell_word_char(ch) {
            if start_byte.is_none() {
                start_byte = Some(idx);
            }
            current.push(ch);
        } else if let Some(start) = start_byte {
            if !range_overlaps(&email_ranges, start, idx)
                && !is_suffix_after_digit(line, start)
                && spell_word_misspelled(&current, dict)
            {
                out.push((start, idx, theme.spell_error));
            }
            current.clear();
            start_byte = None;
        }
    }
    if let Some(start) = start_byte {
        // Skip highlighting the trailing word at end-of-line to avoid
        // flagging words while they are still being typed.
        let _ = start;
    }
    out
}

fn word_at_col(line: &str, col: usize) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut idx = col.min(chars.len());
    if idx == chars.len() {
        if idx == 0 {
            return None;
        }
        idx -= 1;
    }
    if !is_spell_word_char(chars[idx]) {
        if idx > 0 && is_spell_word_char(chars[idx - 1]) {
            idx -= 1;
        } else {
            return None;
        }
    }
    let mut start = idx;
    while start > 0 && is_spell_word_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && is_spell_word_char(chars[end]) {
        end += 1;
    }
    let word = chars[start..end].iter().collect::<String>();
    Some((start, end, word))
}

fn spell_issue_context_line(
    text: &str,
    start: usize,
    end: usize,
    theme: &UiTheme,
) -> Vec<Span<'static>> {
    let mut before_start = start;
    let mut after_end = end;
    let mut words_before = 0;
    let mut words_after = 0;
    let chars: Vec<char> = text.chars().collect();
    while before_start > 0 && words_before < 4 {
        before_start -= 1;
        let ch = chars[before_start];
        if ch.is_whitespace() {
            words_before += 1;
        }
    }
    while after_end < chars.len() && words_after < 4 {
        let ch = chars[after_end];
        if ch.is_whitespace() {
            words_after += 1;
        }
        after_end += 1;
    }
    let prefix = chars
        .get(before_start..start)
        .unwrap_or(&[])
        .iter()
        .collect::<String>();
    let word = chars
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .collect::<String>();
    let suffix = chars
        .get(end..after_end)
        .unwrap_or(&[])
        .iter()
        .collect::<String>();

    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::raw(prefix));
    }
    spans.push(Span::styled(word, theme.spell_error));
    if !suffix.is_empty() {
        spans.push(Span::raw(suffix));
    }
    spans
}

fn collect_spell_issues(subject: &str, body: &str, dict: &Dictionary) -> Vec<SpellIssue> {
    let mut out = Vec::new();
    out.extend(collect_spell_issues_for_target(
        subject,
        SpellTarget::Subject,
        dict,
    ));
    out.extend(collect_spell_issues_for_target(
        body,
        SpellTarget::Body,
        dict,
    ));
    out
}

fn collect_spell_issues_for_target(
    text: &str,
    target: SpellTarget,
    dict: &Dictionary,
) -> Vec<SpellIssue> {
    let email_ranges = email_link_ranges(text);
    let mut issues = Vec::new();
    let mut current = String::new();
    let mut start: Option<usize> = None;
    let mut pos = 0usize;
    for ch in text.chars() {
        if ch.is_alphabetic() || ch == '\'' {
            if start.is_none() {
                start = Some(pos);
            }
            current.push(ch);
        } else if let Some(start_idx) = start {
            let end_idx = pos;
            let start_byte = char_to_byte_idx(text, start_idx);
            let end_byte = char_to_byte_idx(text, end_idx);
            if !range_overlaps(&email_ranges, start_byte, end_byte)
                && !is_suffix_after_digit(text, start_byte)
            {
                if let Some(issue) =
                    spell_issue_from_word(&current, target, start_idx, end_idx, dict)
                {
                    issues.push(issue);
                }
            }
            current.clear();
            start = None;
        } else {
            current.clear();
        }
        pos += 1;
    }
    if let Some(start_idx) = start {
        let end_idx = pos;
        let start_byte = char_to_byte_idx(text, start_idx);
        let end_byte = char_to_byte_idx(text, end_idx);
        if !range_overlaps(&email_ranges, start_byte, end_byte)
            && !is_suffix_after_digit(text, start_byte)
        {
            if let Some(issue) = spell_issue_from_word(&current, target, start_idx, end_idx, dict) {
                issues.push(issue);
            }
        }
    }
    issues
}

fn email_link_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Email]);
    finder
        .links(text)
        .filter_map(|link| {
            if *link.kind() == LinkKind::Email {
                Some((link.start(), link.end()))
            } else {
                None
            }
        })
        .collect()
}

fn range_overlaps(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|(rstart, rend)| start < *rend && end > *rstart)
}

fn is_suffix_after_digit(text: &str, start_byte: usize) -> bool {
    if start_byte == 0 {
        return false;
    }
    let prev = text[..start_byte].chars().last();
    if matches!(prev, Some(ch) if ch.is_ascii_digit()) {
        return true;
    }
    if matches!(prev, Some('\'')) {
        let before_prev = text[..start_byte].chars().rev().nth(1);
        return matches!(before_prev, Some(ch) if ch.is_ascii_digit());
    }
    false
}

fn spell_issue_from_word(
    word: &str,
    target: SpellTarget,
    start: usize,
    end: usize,
    dict: &Dictionary,
) -> Option<SpellIssue> {
    let cleaned = word.trim_matches('\'');
    if cleaned.len() < 2 {
        return None;
    }
    if cleaned.chars().all(|c| c.is_uppercase()) {
        return None;
    }
    if spell_ignore_contains(cleaned) {
        return None;
    }
    let lowered = cleaned.to_ascii_lowercase();
    if dict.check(cleaned) || dict.check(&lowered) {
        return None;
    }
    let mut suggestions = Vec::new();
    dict.suggest(cleaned, &mut suggestions);
    let suggestions = suggestions
        .into_iter()
        .take(5)
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    Some(SpellIssue {
        target,
        start,
        end,
        word: cleaned.to_string(),
        suggestions,
    })
}

fn spell_dictionary() -> Option<&'static Dictionary> {
    SPELL_DICTIONARY
        .get_or_init(|| load_spell_dictionary())
        .as_ref()
}

fn spell_ignore_contains(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    SPELL_IGNORE
        .get()
        .and_then(|set| set.lock().ok())
        .map(|set| set.contains(&lower))
        .unwrap_or(false)
}

fn spell_ignore_path() -> PathBuf {
    xdg_state_dir().join("ratmail").join("spell-ignore.txt")
}

fn load_spell_ignore_file() -> Vec<String> {
    let path = spell_ignore_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_ascii_lowercase())
        .collect()
}

fn init_spell_ignore() {
    let mut set = HashSet::new();
    if let Some(cfg) = SPELL_CONFIG.get() {
        for word in &cfg.ignore {
            let cleaned = word.trim().to_ascii_lowercase();
            if !cleaned.is_empty() {
                set.insert(cleaned);
            }
        }
    }
    for word in load_spell_ignore_file() {
        if !word.is_empty() {
            set.insert(word);
        }
    }
    let _ = SPELL_IGNORE.set(Mutex::new(set));
}

fn add_spell_ignore_word(word: &str) -> Result<()> {
    let cleaned = word.trim().trim_matches('\'').to_ascii_lowercase();
    if cleaned.len() < 2 {
        return Ok(());
    }
    let set = SPELL_IGNORE
        .get()
        .ok_or_else(|| anyhow::anyhow!("Spell ignore not initialized"))?;
    let mut guard = set
        .lock()
        .map_err(|_| anyhow::anyhow!("Spell ignore lock"))?;
    if guard.contains(&cleaned) {
        return Ok(());
    }
    guard.insert(cleaned.clone());

    let path = spell_ignore_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", cleaned)?;
    Ok(())
}

fn load_spell_dictionary() -> Option<Dictionary> {
    let lang = std::env::var("RATMAIL_SPELL_LANG")
        .ok()
        .or_else(|| SPELL_CONFIG.get().map(|cfg| cfg.lang.clone()))
        .unwrap_or_else(|| "en_US".to_string());
    let mut candidates = Vec::new();
    let dir = std::env::var("RATMAIL_SPELL_DIR")
        .ok()
        .or_else(|| SPELL_CONFIG.get().and_then(|cfg| cfg.dir.clone()));
    if let Some(dir) = dir {
        candidates.push((
            PathBuf::from(&dir).join(format!("{}.aff", lang)),
            PathBuf::from(&dir).join(format!("{}.dic", lang)),
        ));
    }
    candidates.push((
        PathBuf::from("assets/dict").join(format!("{}.aff", lang)),
        PathBuf::from("assets/dict").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/usr/share/hunspell").join(format!("{}.aff", lang)),
        PathBuf::from("/usr/share/hunspell").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/usr/share/myspell").join(format!("{}.aff", lang)),
        PathBuf::from("/usr/share/myspell").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/usr/share/myspell/dicts").join(format!("{}.aff", lang)),
        PathBuf::from("/usr/share/myspell/dicts").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/usr/local/share/hunspell").join(format!("{}.aff", lang)),
        PathBuf::from("/usr/local/share/hunspell").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/opt/homebrew/share/hunspell").join(format!("{}.aff", lang)),
        PathBuf::from("/opt/homebrew/share/hunspell").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/Library/Spelling").join(format!("{}.aff", lang)),
        PathBuf::from("/Library/Spelling").join(format!("{}.dic", lang)),
    ));
    candidates.push((
        PathBuf::from("/System/Library/Spelling").join(format!("{}.aff", lang)),
        PathBuf::from("/System/Library/Spelling").join(format!("{}.dic", lang)),
    ));

    for (aff, dic) in candidates {
        if !aff.exists() || !dic.exists() {
            continue;
        }
        let Ok(aff_text) = std::fs::read_to_string(&aff) else {
            continue;
        };
        let Ok(dic_text) = std::fs::read_to_string(&dic) else {
            continue;
        };
        if let Ok(dict) = Dictionary::new(&aff_text, &dic_text) {
            return Some(dict);
        }
    }

    load_embedded_dictionary(&lang)
}

fn load_embedded_dictionary(lang: &str) -> Option<Dictionary> {
    match lang {
        "en_US" => {
            let aff = include_str!("../../../assets/dict/en_US.aff");
            let dic = include_str!("../../../assets/dict/en_US.dic");
            Dictionary::new(aff, dic).ok()
        }
        "en_GB" => {
            let aff = include_str!("../../../assets/dict/en_GB.aff");
            let dic = include_str!("../../../assets/dict/en_GB.dic");
            Dictionary::new(aff, dic).ok()
        }
        _ => None,
    }
}

fn wrapped_rows(line: &str, width: usize, tab_len: u8) -> usize {
    if width == 0 {
        return 1;
    }
    let spans = word_wrap_spans(line, width, tab_len);
    spans.len().max(1)
}

fn wrapped_cursor_pos(line: &str, col: usize, width: usize, tab_len: u8) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    let spans = word_wrap_spans(line, width, tab_len);
    if spans.is_empty() {
        return (0, 0);
    }
    let line_len = line.chars().count();
    let col = col.min(line_len);
    let mut row = spans.len().saturating_sub(1);
    for (idx, (start, end)) in spans.iter().enumerate() {
        if col < *end || (col == line_len && *end == line_len) || (*start == *end && col == *end) {
            row = idx;
            break;
        }
    }
    let (start, end) = spans[row];
    let display_col = display_width_range(line, start, col, tab_len);
    if col == end && display_col >= width {
        return (row.saturating_add(1), 0);
    }
    (row, display_col.min(width.saturating_sub(1)))
}

fn word_wrap_spans(line: &str, width: usize, tab_len: u8) -> Vec<(usize, usize)> {
    if width == 0 {
        return vec![(0, 0)];
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }

    let mut spans = Vec::new();
    let mut row_start = 0usize;
    let mut row_width = 0usize;
    let mut idx = 0usize;

    while idx < chars.len() {
        let is_ws = chars[idx].is_whitespace();
        let token_start = idx;
        let mut token_end = idx + 1;
        while token_end < chars.len() && chars[token_end].is_whitespace() == is_ws {
            token_end += 1;
        }

        let token_width = display_width_chars(&chars[token_start..token_end], row_width, tab_len);
        if is_ws {
            if row_width == 0 {
                idx = token_end;
                continue;
            }
            if row_width + token_width > width {
                spans.push((row_start, token_start));
                row_start = token_end;
                row_width = 0;
                idx = token_end;
                continue;
            }
        } else if row_width > 0 && row_width + token_width >= width {
            spans.push((row_start, token_start));
            row_start = token_start;
            row_width = 0;
        }

        let mut token_idx = token_start;
        while token_idx < token_end {
            if row_width >= width {
                spans.push((row_start, token_idx));
                row_start = token_idx;
                row_width = 0;
            }

            let fit = fit_chars(&chars[token_idx..token_end], row_width, width, tab_len);
            if fit == 0 {
                let w = char_display_width(chars[token_idx], row_width, tab_len);
                row_width = row_width.saturating_add(w);
                token_idx += 1;
            } else {
                for i in 0..fit {
                    let w = char_display_width(chars[token_idx + i], row_width, tab_len);
                    row_width = row_width.saturating_add(w);
                }
                token_idx += fit;
            }

            if row_width >= width {
                spans.push((row_start, token_idx));
                row_start = token_idx;
                row_width = 0;
            }
        }

        idx = token_end;
    }

    if spans
        .last()
        .map(|(_, end)| *end != chars.len())
        .unwrap_or(true)
    {
        spans.push((row_start, chars.len()));
    }
    if spans.is_empty() {
        spans.push((0, 0));
    }
    spans
}

fn char_display_width(ch: char, current_width: usize, tab_len: u8) -> usize {
    let tab_len = tab_len as usize;
    if ch == '\t' && tab_len > 0 {
        tab_len - (current_width % tab_len)
    } else {
        ch.width().unwrap_or(0)
    }
}

fn display_width_chars(chars: &[char], start_width: usize, tab_len: u8) -> usize {
    let mut width = start_width;
    for &ch in chars {
        width = width.saturating_add(char_display_width(ch, width, tab_len));
    }
    width.saturating_sub(start_width)
}

fn fit_chars(chars: &[char], start_width: usize, max_width: usize, tab_len: u8) -> usize {
    if max_width == 0 {
        return 0;
    }
    let mut width = start_width;
    let mut count = 0usize;
    for &ch in chars {
        let w = char_display_width(ch, width, tab_len);
        if width.saturating_add(w) > max_width {
            break;
        }
        width = width.saturating_add(w);
        count += 1;
    }
    count
}

fn display_width_range(text: &str, start: usize, end: usize, tab_len: u8) -> usize {
    if start >= end {
        return 0;
    }
    let mut width = 0usize;
    let tab_len = tab_len as usize;
    let mut idx = 0usize;
    for ch in text.chars() {
        if idx < start {
            idx += 1;
            continue;
        }
        if idx >= end {
            break;
        }
        if ch == '\t' && tab_len > 0 {
            let len = tab_len - (width % tab_len);
            width += len;
        } else {
            width += ch.width().unwrap_or(0);
        }
        idx += 1;
    }
    width
}

fn char_col_for_display_col_in_span(
    line: &str,
    start: usize,
    end: usize,
    target_display_col: usize,
    tab_len: u8,
) -> usize {
    if start >= end {
        return start;
    }
    let mut width = 0usize;
    let tab_len = tab_len as usize;
    let mut idx = 0usize;
    for ch in line.chars() {
        if idx < start {
            idx += 1;
            continue;
        }
        if idx >= end {
            break;
        }
        let next_width = if ch == '\t' && tab_len > 0 {
            width + (tab_len - (width % tab_len))
        } else {
            width + ch.width().unwrap_or(0)
        };
        if next_width > target_display_col {
            return idx;
        }
        width = next_width;
        idx += 1;
    }
    end
}

fn compose_move_visual(
    buffer: &mut ComposeBuffer,
    direction: VisualMove,
    desired_x: &mut Option<usize>,
    width: u16,
) -> bool {
    let width = width as usize;
    if width == 0 {
        return false;
    }
    let (row, col) = buffer.cursor();
    let lines = buffer.lines();
    if lines.is_empty() {
        return false;
    }
    let tab_len = buffer.tab_len;
    let line = lines[row];
    let (visual_row, visual_x) = wrapped_cursor_pos(line, col, width, tab_len);
    let desired = desired_x.get_or_insert(visual_x);
    let line_rows = wrapped_rows(line, width, tab_len);

    let (target_row, target_visual_row) = match direction {
        VisualMove::Up => {
            if visual_row > 0 {
                (row, visual_row - 1)
            } else if row > 0 {
                let prev = row - 1;
                let prev_rows = wrapped_rows(lines[prev], width, tab_len);
                (prev, prev_rows.saturating_sub(1))
            } else {
                (row, visual_row)
            }
        }
        VisualMove::Down => {
            if visual_row + 1 < line_rows {
                (row, visual_row + 1)
            } else if row + 1 < lines.len() {
                (row + 1, 0)
            } else {
                (row, visual_row)
            }
        }
    };

    let target_line = lines[target_row];
    let spans = word_wrap_spans(target_line, width, tab_len);
    if spans.is_empty() {
        return false;
    }
    let target_row = target_row.min(lines.len().saturating_sub(1));
    let span_idx = target_visual_row.min(spans.len().saturating_sub(1));
    let (start, end) = spans[span_idx];
    let target_col = char_col_for_display_col_in_span(target_line, start, end, *desired, tab_len);
    buffer.set_cursor(target_row, target_col);
    true
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Err(err) = ensure_default_config_exists() {
        log_debug(&format!("config bootstrap failed: {}", err));
    }
    let mut accounts = load_accounts_config();
    let (cli_requested, cli_command) = match resolve_cli_command(cli) {
        Ok(result) => result,
        Err(err) => {
            return output_error(&err.to_string());
        }
    };
    if cli_requested {
        let Some(command) = cli_command else {
            return output_error("No command provided");
        };
        if !matches!(command, CliCommand::Setup(_)) && accounts.is_empty() {
            return output_error("No accounts configured");
        }
        let rt = Arc::new(tokio::runtime::Runtime::new()?);
        if let Err(err) = run_cli(&rt, command, &accounts) {
            return output_error(&err.to_string());
        }
        return Ok(());
    }

    let rt = Arc::new(tokio::runtime::Runtime::new()?);
    if accounts.is_empty() {
        accounts.push(AccountConfig {
            name: "Personal".to_string(),
            db_path: "ratmail-demo-personal.db".to_string(),
            smtp: None,
            imap: None,
        });
        accounts.push(AccountConfig {
            name: "Work".to_string(),
            db_path: "ratmail-demo-work.db".to_string(),
            smtp: None,
            imap: None,
        });
    }
    let render_config = load_render_config();
    let ui_config = load_ui_config();
    let send_config = load_send_config();
    let ui_theme = Arc::new(if ui_config.theme == "custom" {
        UiTheme::from_palette(ui_config.palette.as_ref())
    } else {
        UiTheme::from_name(&ui_config.theme)
    });
    let spell_config = load_spell_config();
    let _ = SPELL_CONFIG.set(spell_config);
    init_spell_ignore();
    let allow_remote_images = render_config.allow_remote_images
        || std::env::var("RATMAIL_REMOTE_IMAGES")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let render_width_px = std::env::var("RATMAIL_RENDER_WIDTH_PX")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(render_config.width_px);
    let render_scale = std::env::var("RATMAIL_RENDER_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(render_config.render_scale)
        .clamp(0.25, 4.0);
    let render_tile_height_px_side = std::env::var("RATMAIL_RENDER_TILE_HEIGHT_PX_SIDE")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(render_config.tile_height_px_side);
    let render_tile_height_px_focus = std::env::var("RATMAIL_RENDER_TILE_HEIGHT_PX_FOCUS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(render_config.tile_height_px_focus);
    let mut apps: Vec<App> = Vec::new();

    // Query terminal capabilities BEFORE entering alternate screen
    let picker = Picker::from_query_stdio().ok();
    let render_supported = picker.is_some() || detect_image_support();
    log_debug(&format!(
        "image_support detected={} picker={} protocol={:?}",
        render_supported,
        picker.is_some(),
        picker.as_ref().map(|p| p.protocol_type())
    ));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let (renderer, renderer_is_chromium): (Arc<dyn Renderer>, bool) =
        match std::env::var("RATMAIL_RENDERER") {
            Ok(value) if value.to_lowercase() == "null" => {
                (Arc::new(NullRenderer::default()), false)
            }
            Ok(value) if value.to_lowercase() == "chromium" => {
                (Arc::new(ChromiumRenderer::default()), true)
            }
            Ok(_) => (Arc::new(ChromiumRenderer::default()), true),
            Err(_) => (Arc::new(ChromiumRenderer::default()), true),
        };

    for account in accounts {
        let (engine, events, store_handle, store, store_update_tx, store_updates) =
            rt.block_on(async {
                let (engine, events) =
                    MailEngine::start(account.smtp.clone(), account.imap.clone());
                let store_handle = SqliteMailStore::connect(&account.db_path).await?;
                store_handle.init().await?;
                if let Some(imap) = &account.imap {
                    store_handle
                        .upsert_account(1, &account.name, &imap.username)
                        .await?;
                } else {
                    store_handle.seed_demo_if_empty(&account.name).await?;
                }
                let initial_folder_id = match store_handle.folder_id_by_name(1, "INBOX").await? {
                    Some(id) => id,
                    None => store_handle.first_folder_id(1).await?.unwrap_or(1),
                };
                let snapshot = store_handle.load_snapshot(1, initial_folder_id).await?;
                let (store_update_tx, mut store_update_rx) =
                    tokio::sync::mpsc::channel::<StoreUpdate>(STORE_UPDATE_QUEUE_CAPACITY);
                let (store_snapshot_tx, store_updates) =
                    tokio::sync::mpsc::channel::<StoreSnapshot>(STORE_SNAPSHOT_QUEUE_CAPACITY);
                let store_for_task = store_handle.clone();
                tokio::spawn(async move {
                    while let Some(update) = store_update_rx.recv().await {
                        let result: Result<StoreSnapshot, anyhow::Error> = (|| async {
                            match update {
                                StoreUpdate::Folders {
                                    account_id,
                                    folders,
                                } => {
                                    store_for_task.upsert_folders(account_id, &folders).await?;
                                    let folder_id = match store_for_task
                                        .folder_id_by_name(account_id, "INBOX")
                                        .await?
                                    {
                                        Some(id) => id,
                                        None => store_for_task
                                            .first_folder_id(account_id)
                                            .await?
                                            .unwrap_or(1),
                                    };
                                    store_for_task.load_snapshot(account_id, folder_id).await
                                }
                                StoreUpdate::AppendMessages {
                                    account_id,
                                    folder_name,
                                    messages,
                                    sync_update,
                                } => {
                                    let mut folder_id = store_for_task
                                        .folder_id_by_name(account_id, &folder_name)
                                        .await?;
                                    if folder_id.is_none() {
                                        let fallback = Folder {
                                            id: 0,
                                            account_id,
                                            name: folder_name.clone(),
                                            unread: 0,
                                        };
                                        store_for_task
                                            .upsert_folders(account_id, &[fallback])
                                            .await?;
                                        folder_id = store_for_task
                                            .folder_id_by_name(account_id, &folder_name)
                                            .await?;
                                    }
                                    if let Some(folder_id) = folder_id {
                                        let items: Vec<MessageSummary> = messages
                                            .into_iter()
                                            .map(|mut m| {
                                                m.folder_id = folder_id;
                                                m
                                            })
                                            .collect();
                                        store_for_task
                                            .upsert_folder_messages_append(
                                                account_id, folder_id, &items,
                                            )
                                            .await?;
                                        if let Some(update) = sync_update {
                                            let existing = store_for_task
                                                .get_folder_sync_state(folder_id)
                                                .await?
                                                .unwrap_or(FolderSyncState {
                                                    folder_id,
                                                    uidvalidity: None,
                                                    uidnext: None,
                                                    last_seen_uid: None,
                                                    last_sync_ts: None,
                                                    oldest_ts: None,
                                                });
                                            let merged = FolderSyncState {
                                                folder_id,
                                                uidvalidity: existing.uidvalidity,
                                                uidnext: existing.uidnext,
                                                last_seen_uid: match (
                                                    existing.last_seen_uid,
                                                    update.last_seen_uid,
                                                ) {
                                                    (Some(a), Some(b)) => Some(a.max(b)),
                                                    (Some(a), None) => Some(a),
                                                    (None, Some(b)) => Some(b),
                                                    (None, None) => None,
                                                },
                                                last_sync_ts: Some(update.last_sync_ts),
                                                oldest_ts: match (
                                                    existing.oldest_ts,
                                                    update.oldest_ts,
                                                ) {
                                                    (Some(a), Some(b)) => Some(a.min(b)),
                                                    (Some(a), None) => Some(a),
                                                    (None, Some(b)) => Some(b),
                                                    (None, None) => None,
                                                },
                                            };
                                            store_for_task
                                                .upsert_folder_sync_state(&merged)
                                                .await?;
                                        }
                                        log_debug(&format!(
                                            "store_update messages folder={} id={} count={}",
                                            folder_name,
                                            folder_id,
                                            items.len()
                                        ));
                                        store_for_task.load_snapshot(account_id, folder_id).await
                                    } else {
                                        log_debug(&format!(
                                            "store_update messages folder_missing folder={}",
                                            folder_name
                                        ));
                                        let folder_id = match store_for_task
                                            .folder_id_by_name(account_id, "INBOX")
                                            .await?
                                        {
                                            Some(id) => id,
                                            None => store_for_task
                                                .first_folder_id(account_id)
                                                .await?
                                                .unwrap_or(1),
                                        };
                                        store_for_task.load_snapshot(account_id, folder_id).await
                                    }
                                }
                                StoreUpdate::RawBody {
                                    account_id,
                                    message_id,
                                    raw,
                                    cached_text,
                                } => {
                                    store_for_task.upsert_raw_body(message_id, &raw).await?;
                                    let text = cached_text.or_else(|| {
                                        extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)
                                            .ok()
                                            .map(|d| d.text)
                                    });
                                    if let Some(text) = text {
                                        let _ = store_for_task
                                            .upsert_cache_text(
                                                message_id,
                                                DEFAULT_TEXT_WIDTH,
                                                &text,
                                            )
                                            .await;
                                    }
                                    let folder_id = match store_for_task
                                        .folder_id_by_name(account_id, "INBOX")
                                        .await?
                                    {
                                        Some(id) => id,
                                        None => store_for_task
                                            .first_folder_id(account_id)
                                            .await?
                                            .unwrap_or(1),
                                    };
                                    store_for_task.load_snapshot(account_id, folder_id).await
                                }
                                StoreUpdate::MoveMessages {
                                    account_id,
                                    message_ids,
                                    target_folder_id,
                                    refresh_folder_id,
                                } => {
                                    store_for_task
                                        .move_messages(&message_ids, target_folder_id)
                                        .await?;
                                    store_for_task
                                        .load_snapshot(account_id, refresh_folder_id)
                                        .await
                                }
                                StoreUpdate::DeleteMessages {
                                    account_id,
                                    message_ids,
                                    refresh_folder_id,
                                } => {
                                    store_for_task.delete_messages(&message_ids).await?;
                                    store_for_task
                                        .load_snapshot(account_id, refresh_folder_id)
                                        .await
                                }
                                StoreUpdate::SetMessagesUnread {
                                    account_id,
                                    message_ids,
                                    unread,
                                    refresh_folder_id,
                                } => {
                                    for message_id in message_ids {
                                        store_for_task
                                            .set_message_unread(message_id, unread)
                                            .await?;
                                    }
                                    store_for_task
                                        .load_snapshot(account_id, refresh_folder_id)
                                        .await
                                }
                                StoreUpdate::SaveDraft {
                                    account_id,
                                    from_addr,
                                    to,
                                    cc,
                                    bcc,
                                    subject,
                                    body,
                                } => {
                                    store_for_task
                                        .save_draft(
                                            account_id, &from_addr, &to, &cc, &bcc, &subject, &body,
                                        )
                                        .await?;
                                    let folder_id = match store_for_task
                                        .folder_id_by_name(account_id, "Drafts")
                                        .await?
                                    {
                                        Some(id) => id,
                                        None => store_for_task
                                            .first_folder_id(account_id)
                                            .await?
                                            .unwrap_or(1),
                                    };
                                    store_for_task.load_snapshot(account_id, folder_id).await
                                }
                            }
                        })(
                        )
                        .await;
                        match result {
                            Ok(snapshot) => {
                                log_debug(&format!(
                                    "store_snapshot folders={} messages={}",
                                    snapshot.folders.len(),
                                    snapshot.messages.len()
                                ));
                                if store_snapshot_tx.send(snapshot).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                log_debug(&format!("store_update error {}", err));
                            }
                        }
                    }
                });
                Ok::<_, anyhow::Error>((
                    engine,
                    events,
                    store_handle,
                    snapshot,
                    store_update_tx,
                    store_updates,
                ))
            })?;

        let (render_request_tx, render_request_rx) = tokio::sync::watch::channel(RenderRequest {
            request_id: 0,
            message_ids: Vec::new(),
            width_px: 800,
            tile_height_px: 120,
            max_tiles: None,
            theme: "default".to_string(),
            remote_policy: "blocked".to_string(),
        });
        let (render_event_tx, render_event_rx) =
            tokio::sync::mpsc::channel(RENDER_EVENT_QUEUE_CAPACITY);
        let store_for_worker = store_handle.clone();
        let renderer_for_worker = renderer.clone();
        let handle = rt.handle().clone();
        handle.spawn(render_worker(
            render_request_rx,
            render_event_tx,
            store_for_worker,
            renderer_for_worker,
        ));

        let initial_sync_days = account
            .imap
            .as_ref()
            .map(|i| i.initial_sync_days)
            .unwrap_or(90);
        let app = App::new(
            store,
            store_handle,
            engine,
            events,
            rt.clone(),
            render_supported,
            picker.clone(),
            renderer_is_chromium,
            render_request_tx,
            render_event_rx,
            store_update_tx,
            store_updates,
            allow_remote_images,
            render_width_px,
            render_tile_height_px_side,
            render_tile_height_px_focus,
            account.imap.is_some(),
            initial_sync_days,
            render_scale,
            ui_config.folder_width_cols,
            ui_config.theme.clone(),
            ui_theme.clone(),
            send_config.clone(),
            ui_config.compose_vim,
        );
        apps.push(app);
    }

    let res = run_app(&mut terminal, MultiApp::new(apps), rt.clone());

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Ok(rt) = Arc::try_unwrap(rt) {
        rt.shutdown_timeout(Duration::from_millis(200));
    }
    res
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut multi: MultiApp,
    _rt: Arc<tokio::runtime::Runtime>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, &mut multi))?;

        multi.drain_all();

        let timeout = TICK_RATE.saturating_sub(multi.current().last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('[') => multi.switch_prev(),
                    KeyCode::Char(']') => multi.switch_next(),
                    KeyCode::Char(c)
                        if c.is_ascii_digit()
                            && multi.current().allow_account_switch_shortcut() =>
                    {
                        let idx = c.to_digit(10).unwrap_or(0);
                        if idx > 0 && (idx as usize) <= multi.apps.len() {
                            multi.current = (idx as usize) - 1;
                        }
                    }
                    _ => {
                        if multi.current_mut().on_key(key) {
                            return Ok(());
                        }
                    }
                }
            }
        }

        if multi.current().last_tick.elapsed() >= TICK_RATE {
            for app in &mut multi.apps {
                app.last_tick = Instant::now();
            }
            multi.tick_all();
        }
    }
}

fn ui(frame: &mut ratatui::Frame, multi: &mut MultiApp) {
    let labels = multi.account_labels();
    let current_idx = multi.current;
    let app = multi.current_mut();
    let area = frame.area();
    frame.render_widget(Block::default().style(app.ui_theme.base), area);
    let help_height = if app.show_help { 3 } else { 2 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(help_height),
        ])
        .split(area);

    render_status_bar(frame, layout[0], app, &labels, current_idx);
    render_main(frame, layout[1], app);
    render_help_bar(frame, layout[2], app);

    match app.mode {
        Mode::OverlayLinks => render_links_overlay(frame, area, app),
        Mode::OverlayAttach => render_attach_overlay(frame, area, app),
        Mode::OverlayBulkAction => render_bulk_action_overlay(frame, area, app),
        Mode::OverlayBulkMove => render_bulk_move_overlay(frame, area, app),
        Mode::OverlayConfirmDelete => render_confirm_delete_overlay(frame, area, app),
        Mode::OverlayConfirmLink => render_confirm_link_overlay(frame, area, app),
        Mode::OverlayConfirmDraft => render_confirm_draft_overlay(frame, area, app),
        Mode::OverlaySpellcheck => render_spellcheck_overlay(frame, area, app),
        Mode::OverlaySearch => render_search_overlay(frame, area, app),
        Mode::Compose => render_compose_overlay(frame, area, app),
        Mode::ViewFocus => render_view_focus(frame, area, app),
        _ => {}
    }

    if app.picker_mode.is_some() {
        render_picker_overlay(frame, area, app);
    }
}

fn render_status_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    labels: &[String],
    current_idx: usize,
) {
    let view_label = match app.view_mode {
        ViewMode::Text => "TEXT",
        ViewMode::Rendered => "RENDERED",
    };

    let mut spans = Vec::new();
    if !labels.is_empty() {
        spans.push(Span::raw(" "));
        for (idx, label) in labels.iter().enumerate() {
            let style = if idx == current_idx {
                app.ui_theme.status_tab_active
            } else {
                app.ui_theme.status_tab_inactive
            };
            spans.push(Span::styled(format!(" {}:{} ", idx + 1, label), style));
        }
    }
    spans.push(Span::raw(format!(" acct: {} ", app.store.account.address)));
    spans.push(Span::raw(format!(" sync: {} ", app.sync_status)));
    spans.push(Span::styled(
        format!(" view: {} (v) ", view_label),
        app.ui_theme.status_view,
    ));
    if app.search_active() {
        let needle = app.search_query.trim();
        let label = format!(" [/] search: {} ", truncate_label(needle, 24));
        spans.push(Span::raw(label));
    } else {
        spans.push(Span::raw(" [/] search "));
    }
    if let Some(msg) = &app.status_message {
        spans.push(Span::raw(format!(" | {}", msg)));
    }
    if let Some(msg) = &app.imap_status {
        spans.push(Span::raw(format!(" | {}", msg)));
    }
    let line = Line::from(spans);
    let block = Block::default()
        .borders(if app.ui_theme.show_bars {
            Borders::BOTTOM
        } else {
            Borders::NONE
        })
        .style(app.ui_theme.bar)
        .border_style(app.ui_theme.border);
    frame.render_widget(
        Paragraph::new(line).style(app.ui_theme.bar).block(block),
        area,
    );
}

fn render_main(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.show_preview {
            vec![
                Constraint::Length(app.folder_pane_width),
                Constraint::Percentage(40),
                Constraint::Percentage(45),
            ]
        } else {
            vec![
                Constraint::Length(app.folder_pane_width),
                Constraint::Percentage(85),
                Constraint::Length(0),
            ]
        })
        .split(area);

    render_folders(frame, columns[0], app);
    render_message_list(frame, columns[1], app);
    if app.show_preview {
        render_message_view(frame, columns[2], app, 0);
    }
}

fn render_folders(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let total = app.store.folders.len();
    let rows_visible = area.height.saturating_sub(1).max(1) as usize;
    let mut start = app
        .folder_index
        .saturating_sub(rows_visible.saturating_sub(1));
    if start + rows_visible > total {
        start = total.saturating_sub(rows_visible);
    }
    let end = (start + rows_visible).min(total);
    let items: Vec<Line> = app.store.folders[start..end]
        .iter()
        .enumerate()
        .map(|(idx, folder)| {
            let global_idx = start + idx;
            let display_name = App::display_folder_name(&folder.name);
            let label = if folder.unread > 0 {
                format!("{}  {}", display_name, folder.unread)
            } else {
                display_name
            };
            let style = if global_idx == app.folder_index {
                if app.focus == Focus::Folders {
                    app.ui_theme.focus_bg
                } else {
                    app.ui_theme.focus_fg
                }
            } else {
                Style::default()
            };
            Line::from(Span::styled(label, style))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::RIGHT)
        .title("FOLDERS")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(items)
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_message_list(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let header =
        Row::new(vec!["S", "Att", "Time", "From", "Subject"]).style(app.ui_theme.table_header);

    let visible = app.visible_messages();
    let total = visible.len();
    let rows_visible = area.height.saturating_sub(2).max(1) as usize;
    let mut start = app
        .message_index
        .saturating_sub(rows_visible.saturating_sub(1));
    if start + rows_visible > total {
        start = total.saturating_sub(rows_visible);
    }
    let end = (start + rows_visible).min(total);
    let visible_ids: Vec<i64> = visible[start..end].iter().map(|m| m.id).collect();
    drop(visible);
    app.prefetch_visible_attachments(&visible_ids, 5);
    let visible = app.visible_messages();
    let rows: Vec<Row> = visible[start..end]
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let global_idx = start + idx;
            let selected = app.selected_message_ids.contains(&message.id);
            let sel = if selected { "x" } else { " " };
            let unread = if message.unread { "*" } else { " " };
            let marker = format!("{}{}", sel, unread);
            let mut style = if global_idx == app.message_index {
                if app.focus == Focus::Messages {
                    app.ui_theme.focus_bg
                } else {
                    app.ui_theme.focus_fg
                }
            } else {
                Style::default()
            };
            if message.unread {
                style = style.add_modifier(Modifier::BOLD);
            }
            let from_display = format_from_display(&message.from);
            let att = if let Some(attachments) = app.attachment_cache.get(&message.id) {
                if attachments.is_empty() { " " } else { "@" }
            } else if let Some(detail) = app.store.message_details.get(&message.id) {
                if detail.attachments.is_empty() {
                    " "
                } else {
                    "@"
                }
            } else {
                " "
            };
            Row::new(vec![
                marker,
                att.to_string(),
                message.date.clone(),
                from_display,
                message.subject.clone(),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(if app.show_preview {
                Borders::RIGHT
            } else {
                Borders::NONE
            })
            .title("MESSAGE LIST")
            .style(app.ui_theme.base)
            .border_style(app.ui_theme.border),
    )
    .column_spacing(1)
    .style(app.ui_theme.base);

    frame.render_stateful_widget(table, area, &mut TableState::default());

    if visible.is_empty() {
        let msg = if app.search_active() {
            "No messages (search filter)".to_string()
        } else if let Some(status) = &app.imap_status {
            format!("No messages ({})", status)
        } else if app.imap_enabled && app.imap_pending > 0 {
            "No messages (IMAP syncing...)".to_string()
        } else if app.imap_enabled {
            "No messages (IMAP idle)".to_string()
        } else {
            "No messages".to_string()
        };
        let hint_area = Rect {
            x: area.x + 2,
            y: area.y + 2,
            width: area.width.saturating_sub(4),
            height: 1,
        };
        frame.render_widget(Paragraph::new(msg).style(app.ui_theme.base), hint_area);
    }
}

fn format_from_display(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        let name = trimmed[..start].trim().trim_matches('"').trim();
        let email = trimmed[start + 1..end].trim();
        if !name.is_empty() {
            return name.to_string();
        }
        return email.to_string();
    }
    trimmed.to_string()
}

fn should_show_to(to: &str, account_addr: &str) -> bool {
    let trimmed = to.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(',') || trimmed.contains(';') {
        return true;
    }
    let account_email = extract_email(account_addr).to_ascii_lowercase();
    if account_email.is_empty() {
        return true;
    }
    let to_lower = trimmed.to_ascii_lowercase();
    !to_lower.contains(&account_email)
}

fn canonical_folder_name(raw: &str) -> String {
    let mut name = raw.trim();
    if let Some(stripped) = name.strip_prefix("[Gmail]/") {
        name = stripped;
    } else if let Some(stripped) = name.strip_prefix("[Google Mail]/") {
        name = stripped;
    }
    let lowered = name.trim().to_lowercase();
    let mapped = match lowered.as_str() {
        "sent mail" | "sent-mail" => "Sent",
        "all mail" => "All Mail",
        "inbox" => "INBOX",
        "drafts" => "Drafts",
        "archive" => "Archive",
        "spam" => "Spam",
        "trash" => "Trash",
        "starred" => "Starred",
        _ => name.trim(),
    };
    mapped.to_string()
}

fn scale_image(img: image::DynamicImage, scale: f64) -> image::DynamicImage {
    if (scale - 1.0).abs() < 0.01 {
        return img;
    }
    let (w, h) = img.dimensions();
    let new_w = ((w as f64) * scale).round().max(1.0) as u32;
    let new_h = ((h as f64) * scale).round().max(1.0) as u32;
    image::DynamicImage::ImageRgba8(image::imageops::resize(
        &img,
        new_w,
        new_h,
        image::imageops::FilterType::Lanczos3,
    ))
}

fn link_style(theme: &UiTheme) -> Style {
    theme.link
}

fn link_display_label(link: &LinkInfo, index: Option<usize>) -> String {
    if let Some(text) = link.text.as_deref() {
        return text.to_string();
    }
    if link.from_html {
        if let Some(idx) = index {
            return format!("Image Link {}", idx + 1);
        }
        return "Image Link".to_string();
    }
    link.url.clone()
}

const LINK_MARK_START: &str = "\u{1e}";
const LINK_MARK_END: &str = "\u{1f}";

fn apply_link_markers(text: &str, links: &[LinkInfo]) -> String {
    let mut out = text.to_string();
    for (idx, link) in links.iter().enumerate() {
        if link.text.is_none() && !link.from_html {
            continue;
        }
        let label = link_display_label(link, Some(idx));
        let marker = format!("{LINK_MARK_START}{label}{LINK_MARK_END}");
        let label_bracketed = format!("[{}]", label);
        if out.contains(&label_bracketed) {
            out = out.replace(
                &label_bracketed,
                &format!("{LINK_MARK_START}[{}]{LINK_MARK_END}", label),
            );
            continue;
        }
        let token = format!("{} [{}]", label, link.url);
        if out.contains(&token) {
            out = out.replace(&token, &marker);
            continue;
        }
        let bracketed = format!("[{}]", link.url);
        if out.contains(&bracketed) {
            out = out.replace(&bracketed, &marker);
            continue;
        }
        if link.from_html && label.starts_with("Image link ") && out.contains(&label) {
            out = out.replace(&label, &marker);
        }
    }
    out
}

fn spans_for_plain_text(text: &str, labels: &[String], theme: &UiTheme) -> Vec<Span<'static>> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url, LinkKind::Email]);
    let mut ranges: Vec<(usize, usize)> = finder
        .links(text)
        .map(|link| (link.start(), link.end()))
        .collect();
    ranges.extend(find_reference_links(text));
    ranges.extend(find_label_ranges(text, labels));
    if text.is_empty() {
        vec![Span::raw(String::new())]
    } else {
        build_spans_with_ranges_owned(text, ranges, theme)
    }
}

fn find_label_ranges(text: &str, labels: &[String]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for label in labels {
        let needle = format!("[{}]", label);
        let mut start = 0usize;
        while let Some(pos) = text[start..].find(&needle) {
            let s = start + pos;
            out.push((s, s + needle.len()));
            start = s + needle.len();
        }
    }
    out
}

fn copy_with_osc52(text: &str) -> bool {
    let b64 = BASE64_STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", b64);
    if io::stdout().write_all(seq.as_bytes()).is_ok() && io::stdout().flush().is_ok() {
        return true;
    }
    false
}

fn copy_with_command(text: &str) -> bool {
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("pbcopy", &[]),
        ("clip", &[]),
    ];
    for (cmd, args) in candidates {
        let mut child = match std::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

fn find_reference_links(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let text_end = match bytes[i + 1..].iter().position(|&b| b == b']') {
            Some(pos) => i + 1 + pos,
            None => break,
        };
        let idx_start = text_end + 1;
        if idx_start + 1 >= bytes.len() || bytes[idx_start] != b'[' {
            i += 1;
            continue;
        }
        let idx_end = match bytes[idx_start + 1..].iter().position(|&b| b == b']') {
            Some(pos) => idx_start + 1 + pos,
            None => break,
        };
        if idx_end <= idx_start + 1 {
            i += 1;
            continue;
        }
        if bytes[idx_start + 1..idx_end]
            .iter()
            .all(|b| b.is_ascii_digit())
        {
            out.push((i, idx_end + 1));
            i = idx_end + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn build_spans_with_ranges_owned(
    line: &str,
    mut ranges: Vec<(usize, usize)>,
    theme: &UiTheme,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    ranges.sort_by_key(|(start, _)| *start);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some((last_start, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                *last_start = (*last_start).min(start);
                continue;
            }
        }
        merged.push((start, end));
    }

    let mut spans = Vec::new();
    let mut last = 0usize;
    for (start, end) in merged {
        if start > last {
            spans.push(Span::raw(line[last..start].to_string()));
        }
        spans.push(Span::styled(
            line[start..end].to_string(),
            link_style(theme),
        ));
        last = end;
    }
    if last < line.len() {
        spans.push(Span::raw(line[last..].to_string()));
    }
    spans
}

fn build_spans_with_style_ranges_owned(
    line: &str,
    mut ranges: Vec<(usize, usize, Style)>,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    ranges.sort_by_key(|(start, _, _)| *start);
    let mut spans = Vec::new();
    let mut last = 0usize;
    for (start, end, style) in ranges {
        if start > last {
            spans.push(Span::raw(line[last..start].to_string()));
        }
        if end > start {
            spans.push(Span::styled(line[start..end].to_string(), style));
        }
        last = end.max(last);
    }
    if last < line.len() {
        spans.push(Span::raw(line[last..].to_string()));
    }
    spans
}

fn spans_for_spell_line_range(
    line: &str,
    start_col: usize,
    end_col: usize,
    theme: &UiTheme,
) -> Vec<Span<'static>> {
    let start_col = start_col.min(text_char_len(line));
    let end_col = end_col.min(text_char_len(line));
    if start_col >= end_col {
        return vec![Span::raw(String::new())];
    }

    let start_byte = char_to_byte_idx(line, start_col);
    let end_byte = char_to_byte_idx(line, end_col);
    let segment = &line[start_byte..end_byte];
    let ranges = spell_line_highlights(line, theme)
        .into_iter()
        .filter_map(|(start, end, style)| {
            let overlap_start = start.max(start_byte);
            let overlap_end = end.min(end_byte);
            if overlap_start < overlap_end {
                Some((overlap_start - start_byte, overlap_end - start_byte, style))
            } else {
                None
            }
        })
        .collect();
    build_spans_with_style_ranges_owned(segment, ranges)
}

fn text_with_link_style(text: &str, links: &[LinkInfo], theme: &UiTheme) -> Text<'static> {
    let mut lines = Vec::new();
    let marked = apply_link_markers(text, links);
    let labels: Vec<String> = links
        .iter()
        .enumerate()
        .map(|(idx, link)| link_display_label(link, Some(idx)))
        .collect();

    for line in marked.split('\n') {
        let mut spans = Vec::new();
        let mut rest = line;
        while let Some(start) = rest.find(LINK_MARK_START) {
            let before = &rest[..start];
            spans.extend(spans_for_plain_text(before, &labels, theme));
            let after_start = start + LINK_MARK_START.len();
            let Some(end_rel) = rest[after_start..].find(LINK_MARK_END) else {
                spans.extend(spans_for_plain_text(&rest[start..], &labels, theme));
                rest = "";
                break;
            };
            let end = after_start + end_rel;
            let label = &rest[after_start..end];
            spans.push(Span::styled(label.to_string(), link_style(theme)));
            rest = &rest[end + LINK_MARK_END.len()..];
        }
        if !rest.is_empty() {
            spans.extend(spans_for_plain_text(rest, &labels, theme));
        }
        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

fn render_logo_palette(theme: &UiTheme) -> [Color; 6] {
    [
        theme.link.fg.unwrap_or(Color::LightBlue),
        theme.label_focus.fg.unwrap_or(Color::Cyan),
        theme.status_view.fg.unwrap_or(Color::Yellow),
        theme.table_header.fg.unwrap_or(Color::White),
        theme.border.fg.unwrap_or(Color::Gray),
        theme.base.fg.unwrap_or(Color::White),
    ]
}

fn render_message_view(frame: &mut ratatui::Frame, area: Rect, app: &mut App, scroll: u16) {
    let detail = app.selected_detail().cloned();
    let view_mode = app.view_mode;
    let render_supported = app.render_supported;
    let render_pending = app.render_pending;
    let render_tile_count = app.render_tile_count;
    let render_spinner = app.render_spinner;
    let renderer_is_chromium = app.renderer_is_chromium;
    let render_error = app.render_error.clone();
    let render_no_html = app.render_no_html;
    let protocol_available = app.image_picker.is_some();

    if let Some(detail) = detail {
        let message_id = detail.id;
        let render_matches_selected = app.render_message_id == Some(message_id);
        let attachments = app
            .attachment_cache
            .get(&message_id)
            .map(|a| a.as_slice())
            .unwrap_or(detail.attachments.as_slice());
        let mut meta_lines = vec![
            Line::from(Span::styled(
                format!("Subject: {}", detail.subject),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("From: {}", detail.from)),
        ];
        if should_show_to(&detail.to, &app.store.account.address) {
            meta_lines.push(Line::from(format!("To: {}", detail.to)));
        }
        if !detail.cc.trim().is_empty() {
            meta_lines.push(Line::from(format!("Cc: {}", detail.cc)));
        }
        meta_lines.push(Line::from(format!("Date: {}", detail.date)));
        if !attachments.is_empty() {
            let list = attachments
                .iter()
                .map(|att| format!("{} ({})", att.filename, format_size(att.size)))
                .collect::<Vec<_>>()
                .join(", ");
            meta_lines.push(Line::from(format!("Attachments: {}", list)));
        }
        let meta_block = Text::from(meta_lines);

        let content_text = match view_mode {
            ViewMode::Rendered => {
                if !render_supported {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode disabled."),
                        Line::from("Terminal image support not detected."),
                    ])
                } else if render_pending {
                    let spinner = RAT_SPINNER_FRAMES[render_spinner % RAT_SPINNER_FRAMES.len()];
                    let palette = render_logo_palette(&app.ui_theme);
                    let mode_label = if app.mode == Mode::ViewFocus {
                        "Rendering focus view tiles"
                    } else {
                        "Rendering preview tile"
                    };
                    let mut lines = vec![Line::from(""), Line::from(mode_label), Line::from("")];
                    lines.extend(spinner.iter().enumerate().map(|(idx, line)| {
                        let color = palette[(idx + render_spinner) % palette.len()];
                        let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                        Line::from(Span::styled(*line, style))
                    }));
                    lines.push(Line::from(""));
                    lines.push(Line::from("Links: [l]  Attach: [a]"));
                    Text::from(lines)
                } else if let Some(err) = render_error {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode failed."),
                        Line::from(format!("Error: {}", err)),
                        Line::from("Try setting RATMAIL_CHROME_PATH=/usr/bin/chromium"),
                        Line::from("Or RATMAIL_CHROME_NO_SANDBOX=1"),
                    ])
                } else if render_no_html {
                    text_with_link_style(detail.body.as_str(), &detail.links, &app.ui_theme)
                } else if render_tile_count == 0 && renderer_is_chromium {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode ready, but no tiles were produced."),
                        Line::from("If Chromium is not installed, install it and retry."),
                        Line::from("Or set RATMAIL_RENDERER=null to disable rendering."),
                    ])
                } else {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("(HTML email rendered as image tiles via kitty/sixel)"),
                        Line::from(""),
                        Line::from(format!("Tiles: {}  (PgDn/PgUp)", render_tile_count)),
                        Line::from("Links: [l]  Attach: [a]"),
                    ])
                }
            }
            ViewMode::Text => {
                text_with_link_style(detail.body.as_str(), &detail.links, &app.ui_theme)
            }
        };

        let content_block = Paragraph::new(content_text)
            .style(app.ui_theme.base)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        let meta_height = meta_block.lines.len().max(1) as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(meta_height),
                Constraint::Length(1),
                Constraint::Min(8),
            ])
            .split(area);
        if app.ui_theme.show_bars {
            let separator = Block::default()
                .borders(Borders::TOP)
                .border_style(app.ui_theme.border)
                .style(app.ui_theme.base);
            frame.render_widget(separator, chunks[1]);
        }
        let content_area = chunks[2];
        if app.update_render_geometry(content_area)
            && app.view_mode == ViewMode::Rendered
            && app.render_supported
        {
            app.schedule_render();
        }

        if view_mode == ViewMode::Rendered
            && render_supported
            && protocol_available
            && render_matches_selected
        {
            let area_key = (content_area.width, content_area.height);
            if app.last_render_area != Some(area_key) {
                app.protocol_cache.clear();
                app.protocol_cache_lru.clear();
                app.protocol_cache_static.clear();
                app.tile_rows_cache.clear();
                app.last_render_area = Some(area_key);
                app.image_protocol = None;
            }

            if app.mode == Mode::ViewFocus {
                // Always draw text fallback first so focus view is never blank while tiles load.
                frame.render_widget(content_block.clone(), content_area);
                if app.render_tile_count > 0 {
                    let max = (app.render_tile_count - 1) as u16;
                    if app.view_scroll > max {
                        app.view_scroll = max;
                    }
                }
                let mut idx = app.view_scroll as usize;
                let (_fw, fh) = app
                    .image_picker
                    .as_ref()
                    .map(|p| p.font_size())
                    .unwrap_or((10, 20));
                let tile_rows = ((app.render_tile_height_px as f64 * app.render_scale) / fh as f64)
                    .ceil()
                    .max(1.0) as u16;

                let mut y = content_area.y;
                let end_y = content_area.y + content_area.height;
                while y < end_y && idx < app.render_tiles.len() {
                    let remaining = end_y - y;
                    let rect_height = tile_rows.min(remaining);
                    if rect_height < tile_rows {
                        break; // avoid shrinking bottom tile
                    }
                    let rect = Rect {
                        x: content_area.x,
                        y,
                        width: content_area.width,
                        height: rect_height,
                    };

                    if !app.protocol_cache_static.contains_key(&(message_id, idx)) {
                        if let Some(bytes) = app.render_tiles.get(idx).map(|t| t.bytes.clone()) {
                            if let Ok(img) = image::load_from_memory(&bytes) {
                                if let Some(picker) = app.image_picker.as_mut() {
                                    let img = scale_image(img, app.render_scale);
                                    let source = ImageSource::new(
                                        img.clone(),
                                        picker.font_size(),
                                        image::Rgba([0, 0, 0, 0]),
                                    );
                                    app.tile_rows_cache
                                        .insert((message_id, idx), source.desired.height);
                                    if let Ok(protocol) =
                                        picker.new_protocol(img, rect, Resize::Crop(None))
                                    {
                                        app.protocol_cache_static
                                            .insert((message_id, idx), protocol);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(protocol) = app.protocol_cache_static.get(&(message_id, idx)) {
                        frame.render_widget(Image::new(protocol), rect);
                    }

                    y = y.saturating_add(tile_rows);
                    idx += 1;
                }
            } else {
                let desired_idx = 0;
                if app.render_tile_index != desired_idx || app.image_protocol.is_none() {
                    if let Some(protocol) = app.take_cached_protocol(message_id, desired_idx) {
                        app.image_protocol = Some(protocol);
                        app.render_tile_index = desired_idx;
                    } else if let Some(bytes) =
                        app.render_tiles.get(desired_idx).map(|t| t.bytes.clone())
                    {
                        if let Ok(img) = image::load_from_memory(&bytes) {
                            if let Some(picker) = app.image_picker.as_mut() {
                                let img = scale_image(img, app.render_scale);
                                let protocol = picker.new_resize_protocol(img);
                                app.render_tile_index = desired_idx;
                                app.store_protocol_cache(message_id, desired_idx, protocol);
                                app.image_protocol =
                                    app.take_cached_protocol(message_id, desired_idx);
                            }
                        }
                    }
                }

                if let Some(protocol) = app.image_protocol.as_mut() {
                    frame.render_stateful_widget(StatefulImage::default(), content_area, protocol);
                } else {
                    frame.render_widget(content_block, content_area);
                }
            }
        } else {
            frame.render_widget(content_block, content_area);
        }
        frame.render_widget(
            Paragraph::new(meta_block).style(app.ui_theme.base),
            chunks[0],
        );
    } else {
        frame.render_widget(
            Paragraph::new("No message selected").style(app.ui_theme.base),
            area,
        );
    }
}

fn render_help_bar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut help = if app.show_help {
        String::from(
            "Tab/h/l focus  j/k move  Space select+next  Enter open/actions  v toggle view  p preview  s sync  o older  / search  [ ] switch acct  ? help  Esc clear\n\
Ctrl+S/F5 send  r reply  R reply-all  f forward  m move  d delete  (bulk: r read)  q quit",
        )
    } else {
        String::from(
            "Tab/h/l focus  j/k move  Space select+next  Enter open/actions  v view  p preview  s sync  o older  / search  [ ] acct  ? help  Esc clear  q quit",
        )
    };
    if let Some(msg) = &app.status_message {
        help.push_str("  |  ");
        help.push_str(msg);
    }
    let block = Block::default()
        .borders(if app.ui_theme.show_bars {
            Borders::TOP
        } else {
            Borders::NONE
        })
        .style(app.ui_theme.bar)
        .border_style(app.ui_theme.border);
    frame.render_widget(
        Paragraph::new(help).style(app.ui_theme.bar).block(block),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn truncate_label(label: &str, max_len: usize) -> String {
    let mut text = label.replace('\n', " ").replace('\r', " ");
    if text.len() <= max_len {
        return text;
    }
    if max_len <= 3 {
        return text.chars().take(max_len).collect();
    }
    text.truncate(max_len - 3);
    text.push_str("...");
    text
}

fn render_view_focus(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);
    let outer = Block::default()
        .borders(Borders::ALL)
        .title("MESSAGE")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);
    render_message_view(frame, inner, app, app.view_scroll);
}

#[derive(Debug, Clone)]
struct AccountConfig {
    name: String,
    db_path: String,
    smtp: Option<SmtpConfig>,
    imap: Option<ImapConfig>,
}

fn xdg_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg_state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_path_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("ratmail.toml"),
        xdg_config_dir().join("ratmail").join("ratmail.toml"),
    ]
}

fn load_config_text() -> Option<String> {
    for path in config_path_candidates() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }
    None
}

fn default_config_template() -> &'static str {
    r#"# Auto-generated by ratmail on first run.
# Edit this file to add accounts.

[cli]
enabled = true
default_account = "Personal"
"#
}

fn ensure_default_config_exists() -> Result<()> {
    if load_config_text().is_some() {
        return Ok(());
    }
    let path = xdg_config_dir().join("ratmail").join("ratmail.toml");
    write_text_atomic(&path, default_config_template())
}

fn write_text_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Preserve ownership when updating an existing user-owned config file.
    if path.exists() {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn default_db_dir() -> PathBuf {
    let dir = xdg_state_dir().join("ratmail");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn resolve_db_path(raw: &str) -> String {
    let path = Path::new(raw);
    if path.is_absolute() {
        raw.to_string()
    } else {
        default_db_dir().join(path).to_string_lossy().to_string()
    }
}

fn load_accounts_config() -> Vec<AccountConfig> {
    let content = match load_config_text() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if let Some(accounts) = value.get("accounts").and_then(|v| v.as_array()) {
        return accounts
            .iter()
            .enumerate()
            .filter_map(|(idx, acct)| parse_account_config(acct, idx))
            .collect();
    }
    let smtp = parse_smtp_config(&value);
    let imap = parse_imap_config(&value);
    if smtp.is_none() && imap.is_none() {
        return Vec::new();
    }
    let name = imap
        .as_ref()
        .map(|i| i.username.clone())
        .or_else(|| smtp.as_ref().map(|s| s.username.clone()))
        .unwrap_or_else(|| "account".to_string());
    let db_path = resolve_db_path(&format!("ratmail-{}.db", slugify_name(&name)));
    vec![AccountConfig {
        name,
        db_path,
        smtp,
        imap,
    }]
}

fn parse_account_config(value: &toml::Value, index: usize) -> Option<AccountConfig> {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let smtp = value.get("smtp").and_then(parse_smtp_table);
    let imap = value.get("imap").and_then(parse_imap_table);
    let derived = name
        .clone()
        .or_else(|| imap.as_ref().map(|i| i.username.clone()))
        .or_else(|| smtp.as_ref().map(|s| s.username.clone()))
        .unwrap_or_else(|| format!("account-{}", index + 1));
    let db_path = value
        .get("db_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("ratmail-{}.db", slugify_name(&derived)));
    Some(AccountConfig {
        name: derived,
        db_path: resolve_db_path(&db_path),
        smtp,
        imap,
    })
}

fn parse_smtp_config(value: &toml::Value) -> Option<SmtpConfig> {
    value.get("smtp").and_then(parse_smtp_table)
}

fn parse_smtp_table(smtp: &toml::Value) -> Option<SmtpConfig> {
    Some(SmtpConfig {
        host: smtp.get("host")?.as_str()?.to_string(),
        port: smtp.get("port").and_then(|v| v.as_integer()).unwrap_or(587) as u16,
        username: smtp.get("username")?.as_str()?.to_string(),
        password: smtp.get("password")?.as_str()?.to_string(),
        from: smtp.get("from")?.as_str()?.to_string(),
        skip_tls_verify: smtp
            .get("skip_tls_verify")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

fn parse_imap_config(value: &toml::Value) -> Option<ImapConfig> {
    value.get("imap").and_then(parse_imap_table)
}

fn parse_imap_table(imap: &toml::Value) -> Option<ImapConfig> {
    Some(ImapConfig {
        host: imap.get("host")?.as_str()?.to_string(),
        port: imap.get("port").and_then(|v| v.as_integer()).unwrap_or(993) as u16,
        username: imap.get("username")?.as_str()?.to_string(),
        password: imap.get("password")?.as_str()?.to_string(),
        skip_tls_verify: imap
            .get("skip_tls_verify")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        initial_sync_days: imap
            .get("initial_sync_days")
            .and_then(|v| v.as_integer())
            .map(|v| v.max(1) as i64)
            .unwrap_or(90),
        fetch_chunk_size: imap
            .get("fetch_chunk_size")
            .and_then(|v| v.as_integer())
            .map(|v| v.clamp(1, 50) as usize)
            .unwrap_or(10),
    })
}

fn slugify_name(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed
    }
}

struct RenderConfig {
    allow_remote_images: bool,
    width_px: i64,
    render_scale: f64,
    tile_height_px_side: i64,
    tile_height_px_focus: i64,
}

#[derive(Debug, Clone)]
struct SendConfig {
    html: bool,
    html_font_family: String,
    html_font_size_px: u16,
}

struct UiConfig {
    folder_width_cols: u16,
    theme: String,
    palette: Option<UiPalette>,
    compose_vim: bool,
}

#[derive(Debug, Clone)]
struct UiPalette {
    base_fg: Option<Color>,
    base_bg: Option<Color>,
    border: Option<Color>,
    bar_fg: Option<Color>,
    bar_bg: Option<Color>,
    accent: Option<Color>,
    warn: Option<Color>,
    error: Option<Color>,
    selection_bg: Option<Color>,
    selection_fg: Option<Color>,
    link: Option<Color>,
    muted: Option<Color>,
}

#[derive(Debug, Clone)]
struct UiTheme {
    base: Style,
    border: Style,
    bar: Style,
    show_bars: bool,
    status_tab_active: Style,
    status_tab_inactive: Style,
    status_view: Style,
    focus_bg: Style,
    focus_fg: Style,
    table_header: Style,
    link: Style,
    link_selected: Style,
    label: Style,
    label_focus: Style,
    suffix: Style,
    separator: Style,
    overlay_select: Style,
    spell_error: Style,
}

fn style_with_colors(fg: Option<Color>, bg: Option<Color>) -> Style {
    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    style
}

fn normalize_ui_theme(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    let normalized = if lowered.is_empty() {
        "default"
    } else {
        lowered.as_str()
    };
    match normalized {
        "default" | "ratmail" | "nord" | "gruvbox" | "solarized-dark" | "solarized-light"
        | "dracula" | "catppuccin-mocha" | "catppuccin-latte" | "custom" => normalized.to_string(),
        _ => {
            log_debug(&format!(
                "config warn unknown ui theme='{}', using default",
                raw
            ));
            "default".to_string()
        }
    }
}

fn parse_ui_palette(value: &toml::Value) -> Option<UiPalette> {
    let table = value.as_table()?;
    Some(UiPalette {
        base_fg: table.get("base_fg").and_then(parse_toml_color),
        base_bg: table.get("base_bg").and_then(parse_toml_color),
        border: table.get("border").and_then(parse_toml_color),
        bar_fg: table.get("bar_fg").and_then(parse_toml_color),
        bar_bg: table.get("bar_bg").and_then(parse_toml_color),
        accent: table.get("accent").and_then(parse_toml_color),
        warn: table.get("warn").and_then(parse_toml_color),
        error: table.get("error").and_then(parse_toml_color),
        selection_bg: table.get("selection_bg").and_then(parse_toml_color),
        selection_fg: table.get("selection_fg").and_then(parse_toml_color),
        link: table.get("link").and_then(parse_toml_color),
        muted: table.get("muted").and_then(parse_toml_color),
    })
}

fn parse_toml_color(value: &toml::Value) -> Option<Color> {
    value.as_str().and_then(parse_hex_color)
}

fn parse_hex_color(raw: &str) -> Option<Color> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[derive(Clone)]
struct SpellConfig {
    lang: String,
    dir: Option<String>,
    ignore: Vec<String>,
}
