use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use linkify::{LinkFinder, LinkKind};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap},
};

use image::GenericImageView;
use mailparse::{MailAddr, MailHeaderMap, addrparse_header};
use mime_guess::MimeGuess;
use ratatui_explorer::{FileExplorer, Input as ExplorerInput, Theme as ExplorerTheme};
use ratatui_image::{
    Image, Resize, StatefulImage, picker::Picker, protocol::ImageSource, protocol::Protocol,
    protocol::StatefulProtocol,
};
use ratmail_content::{
    extract_attachment_data, extract_attachments, extract_display, prepare_html,
};
use ratmail_core::{
    DEFAULT_TEXT_WIDTH, Folder, FolderSyncState, MailStore, MessageDetail, MessageSummary,
    SqliteMailStore, StoreSnapshot, TileMeta,
};
use ratmail_mail::{
    ImapConfig, MailCommand, MailEngine, MailEvent, OutgoingAttachment, SmtpConfig,
};
use ratmail_render::{
    ChromiumRenderer, NullRenderer, RemotePolicy, Renderer, detect_image_support,
};
use std::io::Cursor;
use std::sync::Arc;
use zip::{ZipWriter, write::FileOptions};

const TICK_RATE: Duration = Duration::from_millis(200);
const PROTOCOL_CACHE_LIMIT: usize = 16;
const TILE_CACHE_BUDGET_BYTES: i64 = 256 * 1024 * 1024;

static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn log_path() -> Option<PathBuf> {
    if std::env::var("RATMAIL_LOG").is_err() {
        return None;
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Some(base.join("ratmail").join("ratmail.log"))
}

fn log_debug(msg: &str) {
    let Some(path) = log_path() else { return };
    let lock = LOG_FILE.get_or_init(|| {
        let _ = std::fs::create_dir_all(
            path.parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp")),
        );
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Mutex::new(file)
    });
    if let Ok(mut guard) = lock.lock() {
        if let Some(file) = guard.as_mut() {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", ts, msg);
        }
    }
}

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
struct RenderRequest {
    request_id: u64,
    message_ids: Vec<i64>,
    width_px: i64,
    tile_height_px: i64,
    max_tiles: Option<usize>,
    theme: String,
    remote_policy: String,
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
}

#[derive(Debug, Clone)]
struct SyncUpdate {
    last_seen_uid: Option<i64>,
    oldest_ts: Option<i64>,
    last_sync_ts: i64,
}

#[derive(Debug, Clone)]
struct RenderEvent {
    message_id: i64,
    tiles: Vec<TileMeta>,
    tile_height_px: i64,
    no_html: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    View,
    ViewFocus,
    Compose,
    OverlayLinks,
    OverlayAttach,
    OverlayBulkAction,
    OverlayBulkMove,
    OverlayConfirmDelete,
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
    events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
    store_handle: SqliteMailStore,
    runtime: Arc<tokio::runtime::Runtime>,
    overlay_return: Mode,
    view_scroll: u16,
    render_supported: bool,
    render_tile_count: usize,
    render_tiles: Vec<TileMeta>,
    render_tile_index: usize,
    render_tiles_height_px: i64,
    render_request_id: u64,
    render_message_id: Option<i64>,
    render_pending: bool,
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
    render_events: tokio::sync::mpsc::UnboundedReceiver<RenderEvent>,
    store_update_tx: tokio::sync::mpsc::UnboundedSender<StoreUpdate>,
    store_updates: tokio::sync::mpsc::UnboundedReceiver<StoreSnapshot>,
    allow_remote_images: bool,
    render_width_px: i64,
    render_tile_height_px: i64,
    render_tile_height_px_focus: i64,
    render_tile_height_px_side: i64,
    render_scale: f64,
    show_preview: bool,
    show_help: bool,
    link_index: usize,
    attach_index: usize,
    status_message: Option<String>,
    selected_message_ids: HashSet<i64>,
    bulk_action_ids: Vec<i64>,
    bulk_folder_index: usize,
    bulk_done_return: Mode,
    confirm_delete_ids: Vec<i64>,
    confirm_delete_return: Mode,
    picker_mode: Option<PickerMode>,
    picker_focus: PickerFocus,
    picker: Option<FileExplorer>,
    picker_filename: String,
    picker_cursor: usize,
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
    compose_body: String,
    compose_quote: String,
    compose_attachments: Vec<ComposeAttachment>,
    compose_focus: ComposeFocus,
    compose_cursor_to: usize,
    compose_cursor_cc: usize,
    compose_cursor_bcc: usize,
    compose_cursor_subject: usize,
    compose_cursor_body: usize,
    compose_body_desired_col: Option<usize>,
}

struct MultiApp {
    apps: Vec<App>,
    current: usize,
}

impl MultiApp {
    fn new(apps: Vec<App>) -> Self {
        Self { apps, current: 0 }
    }

    fn current(&self) -> &App {
        &self.apps[self.current]
    }

    fn current_mut(&mut self) -> &mut App {
        &mut self.apps[self.current]
    }

    fn switch_next(&mut self) {
        if !self.apps.is_empty() {
            self.current = (self.current + 1) % self.apps.len();
        }
    }

    fn switch_prev(&mut self) {
        if !self.apps.is_empty() {
            if self.current == 0 {
                self.current = self.apps.len() - 1;
            } else {
                self.current -= 1;
            }
        }
    }

    fn account_labels(&self) -> Vec<String> {
        self.apps
            .iter()
            .map(|app| app.store.account.name.clone())
            .collect()
    }

    fn drain_all(&mut self) {
        for app in &mut self.apps {
            app.drain_channels();
        }
    }

    fn tick_all(&mut self) {
        for app in &mut self.apps {
            app.on_tick();
        }
    }
}

#[derive(Debug, Clone)]
struct ComposeAttachment {
    filename: String,
    mime: String,
    size: usize,
    data: Vec<u8>,
}

impl App {
    fn new(
        store: StoreSnapshot,
        store_handle: SqliteMailStore,
        engine: MailEngine,
        events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
        runtime: Arc<tokio::runtime::Runtime>,
        render_supported: bool,
        image_picker: Option<Picker>,
        renderer_is_chromium: bool,
        render_request_tx: tokio::sync::watch::Sender<RenderRequest>,
        render_events: tokio::sync::mpsc::UnboundedReceiver<RenderEvent>,
        store_update_tx: tokio::sync::mpsc::UnboundedSender<StoreUpdate>,
        store_updates: tokio::sync::mpsc::UnboundedReceiver<StoreSnapshot>,
        allow_remote_images: bool,
        render_width_px: i64,
        render_tile_height_px_side: i64,
        render_tile_height_px_focus: i64,
        imap_enabled: bool,
        initial_sync_days: i64,
        render_scale: f64,
    ) -> Self {
        let mut app = Self {
            mode: Mode::List,
            focus: Focus::Messages,
            view_mode: ViewMode::Text,
            store,
            folder_index: 0,
            message_index: 0,
            last_tick: Instant::now(),
            sync_status: "idle".to_string(),
            engine,
            events,
            store_handle,
            runtime,
            overlay_return: Mode::View,
            view_scroll: 0,
            render_supported,
            render_tile_count: 0,
            render_tiles: Vec::new(),
            render_tile_index: 0,
            render_tiles_height_px: 0,
            render_request_id: 0,
            render_message_id: None,
            render_pending: false,
            pending_body_fetch: HashSet::new(),
            image_picker,
            image_protocol: None,
            protocol_cache: HashMap::new(),
            protocol_cache_lru: VecDeque::new(),
            protocol_cache_static: HashMap::new(),
            tile_rows_cache: HashMap::new(),
            last_render_area: None,
            renderer_is_chromium,
            render_error: None,
            render_no_html: false,
            render_request_tx,
            render_events,
            store_update_tx,
            store_updates,
            allow_remote_images,
            render_width_px,
            render_tile_height_px: render_tile_height_px_side,
            render_tile_height_px_focus,
            render_tile_height_px_side,
            render_scale,
            show_preview: false,
            show_help: false,
            link_index: 0,
            attach_index: 0,
            status_message: None,
            selected_message_ids: HashSet::new(),
            bulk_action_ids: Vec::new(),
            bulk_folder_index: 0,
            bulk_done_return: Mode::View,
            confirm_delete_ids: Vec::new(),
            confirm_delete_return: Mode::View,
            picker_mode: None,
            picker_focus: PickerFocus::Explorer,
            picker: None,
            picker_filename: String::new(),
            picker_cursor: 0,
            imap_enabled,
            last_folder_sync: None,
            last_backfill: None,
            imap_pending: 0,
            imap_spinner: 0,
            imap_status: None,
            initial_sync_days,
            compose_to: String::new(),
            compose_cc: String::new(),
            compose_bcc: String::new(),
            compose_subject: String::new(),
            compose_body: String::new(),
            compose_quote: String::new(),
            compose_attachments: Vec::new(),
            compose_focus: ComposeFocus::Body,
            compose_cursor_to: 0,
            compose_cursor_cc: 0,
            compose_cursor_bcc: 0,
            compose_cursor_subject: 0,
            compose_cursor_body: 0,
            compose_body_desired_col: None,
        };
        app.select_inbox_if_available();
        app.sort_folders();
        if app.imap_enabled {
            let _ = app.engine.send(MailCommand::SyncAll);
            app.imap_pending = app.imap_pending.saturating_add(2);
            app.imap_status = Some("IMAP syncing...".to_string());
        }
        app
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    fn selected_message(&self) -> Option<&MessageSummary> {
        let messages = self.visible_messages();
        messages.get(self.message_index).copied()
    }

    fn selected_detail(&self) -> Option<&MessageDetail> {
        let message_id = self.selected_message()?.id;
        self.store.message_details.get(&message_id)
    }

    fn selected_folder(&self) -> Option<&Folder> {
        self.store.folders.get(self.folder_index)
    }

    fn select_inbox_if_available(&mut self) {
        if let Some((idx, _)) = self
            .store
            .folders
            .iter()
            .enumerate()
            .find(|(_, f)| f.name == "INBOX")
        {
            self.folder_index = idx;
            self.message_index = 0;
        }
    }

    fn visible_messages(&self) -> Vec<&MessageSummary> {
        let folder_id = self.selected_folder().map(|f| f.id);
        let messages: Vec<&MessageSummary> = self
            .store
            .messages
            .iter()
            .filter(|msg| Some(msg.folder_id) == folder_id)
            .collect();
        // Preserve backend ordering (already sorted by date).
        messages
    }

    fn prune_selected_messages(&mut self) {
        if self.selected_message_ids.is_empty() {
            return;
        }
        let visible: HashSet<i64> = self.visible_messages().iter().map(|m| m.id).collect();
        self.selected_message_ids.retain(|id| visible.contains(id));
        if self.selected_message_ids.is_empty() {
            self.status_message = None;
        }
    }

    fn clear_selected_messages(&mut self) {
        self.selected_message_ids.clear();
        self.bulk_action_ids.clear();
        self.confirm_delete_ids.clear();
        self.status_message = None;
    }

    fn toggle_select_current(&mut self) {
        let message_id = match self.selected_message() {
            Some(message) => message.id,
            None => return,
        };
        if self.selected_message_ids.contains(&message_id) {
            self.selected_message_ids.remove(&message_id);
        } else {
            self.selected_message_ids.insert(message_id);
        }
        let count = self.selected_message_ids.len();
        if count > 0 {
            self.status_message = Some(format!(
                "Selected {} message{}",
                count,
                if count == 1 { "" } else { "s" }
            ));
        } else {
            self.status_message = None;
        }
    }

    fn active_message_ids(&self) -> Vec<i64> {
        if !self.selected_message_ids.is_empty() {
            return self.selected_message_ids.iter().copied().collect();
        }
        self.selected_message()
            .map(|m| vec![m.id])
            .unwrap_or_default()
    }

    fn default_move_folder_index(&self) -> usize {
        let current_id = self.selected_folder().map(|f| f.id);
        self.store
            .folders
            .iter()
            .position(|f| Some(f.id) != current_id)
            .unwrap_or(self.folder_index)
    }

    fn open_bulk_action_overlay(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        self.bulk_action_ids = ids;
        self.bulk_done_return = self.mode;
        self.overlay_return = self.mode;
        self.mode = Mode::OverlayBulkAction;
    }

    fn open_move_overlay(&mut self, ids: Vec<i64>, return_mode: Mode) {
        if ids.is_empty() || self.store.folders.is_empty() {
            return;
        }
        self.bulk_action_ids = ids;
        self.bulk_folder_index = self.default_move_folder_index();
        if return_mode != Mode::OverlayBulkAction {
            self.bulk_done_return = return_mode;
        }
        self.overlay_return = return_mode;
        self.mode = Mode::OverlayBulkMove;
    }

    fn open_confirm_delete(&mut self, ids: Vec<i64>, return_mode: Mode) {
        if ids.is_empty() {
            return;
        }
        self.confirm_delete_ids = ids;
        self.confirm_delete_return = return_mode;
        self.overlay_return = return_mode;
        self.mode = Mode::OverlayConfirmDelete;
    }

    fn collect_imap_uids(&self, ids: &[i64]) -> Vec<u32> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(uid) = self
                .store
                .messages
                .iter()
                .find(|m| m.id == *id)
                .and_then(|m| m.imap_uid)
                .and_then(|uid| u32::try_from(uid).ok())
            {
                out.push(uid);
            }
        }
        out
    }

    fn queue_move_messages(&mut self, ids: Vec<i64>, target_folder_id: i64) {
        if ids.is_empty() {
            return;
        }
        if Some(target_folder_id) == self.selected_folder().map(|f| f.id) {
            self.status_message = Some("Already in that folder".to_string());
            return;
        }
        let account_id = self.store.account.id;
        let refresh_folder_id = self.selected_folder().map(|f| f.id).unwrap_or(1);
        let _ = self.store_update_tx.send(StoreUpdate::MoveMessages {
            account_id,
            message_ids: ids.clone(),
            target_folder_id,
            refresh_folder_id,
        });

        if self.imap_enabled {
            let uids = self.collect_imap_uids(&ids);
            if !uids.is_empty() {
                if let (Some(src_folder), Some(dst_folder)) = (
                    self.selected_folder().map(|f| f.name.clone()),
                    self.store
                        .folders
                        .iter()
                        .find(|f| f.id == target_folder_id)
                        .map(|f| f.name.clone()),
                ) {
                    let _ = self.engine.send(MailCommand::MoveMessages {
                        folder_name: src_folder,
                        target_folder: dst_folder,
                        uids,
                    });
                }
            }
        }

        self.clear_selected_messages();
        self.status_message = Some(format!(
            "Moved {} message{}",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        ));
    }

    fn queue_delete_messages(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        let account_id = self.store.account.id;
        let refresh_folder_id = self.selected_folder().map(|f| f.id).unwrap_or(1);
        let _ = self.store_update_tx.send(StoreUpdate::DeleteMessages {
            account_id,
            message_ids: ids.clone(),
            refresh_folder_id,
        });

        if self.imap_enabled {
            let uids = self.collect_imap_uids(&ids);
            if !uids.is_empty() {
                if let Some(folder_name) = self.selected_folder().map(|f| f.name.clone()) {
                    let _ = self
                        .engine
                        .send(MailCommand::DeleteMessages { folder_name, uids });
                }
            }
        }

        self.clear_selected_messages();
        self.status_message = Some(format!(
            "Deleted {} message{}",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        ));
    }

    fn sort_folders(&mut self) {
        const PRIORITY: [&str; 8] = [
            "All Mail", "INBOX", "Starred", "Sent", "Drafts", "Archive", "Spam", "Trash",
        ];
        self.store.folders.sort_by(|a, b| {
            let a_name = canonical_folder_name(&a.name);
            let b_name = canonical_folder_name(&b.name);
            let a_idx = PRIORITY.iter().position(|p| p == &a_name);
            let b_idx = PRIORITY.iter().position(|p| p == &b_name);
            match (a_idx, b_idx) {
                (Some(ai), Some(bi)) => ai.cmp(&bi),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a_name.cmp(&b_name),
            }
        });
        let mut seen = HashSet::new();
        let system: HashSet<&'static str> = [
            "All Mail", "INBOX", "Starred", "Sent", "Drafts", "Archive", "Spam", "Trash",
        ]
        .into_iter()
        .collect();
        self.store.folders.retain(|f| {
            let canonical = canonical_folder_name(&f.name);
            if !system.contains(canonical.as_str()) {
                return true;
            }
            if seen.contains(&canonical) {
                return false;
            }
            seen.insert(canonical);
            true
        });
    }

    fn display_folder_name(raw: &str) -> String {
        canonical_folder_name(raw)
    }

    fn restore_selection(&mut self, folder_name: Option<String>, message_uid: Option<u32>) {
        if let Some(name) = folder_name {
            if let Some(idx) = self.store.folders.iter().position(|f| f.name == name) {
                self.folder_index = idx;
            }
        }
        if let Some(uid) = message_uid {
            let messages = self.visible_messages();
            if let Some(idx) = messages.iter().position(|m| m.imap_uid == Some(uid)) {
                self.message_index = idx;
            }
        }
        let max_folder = self.store.folders.len().saturating_sub(1);
        if self.folder_index > max_folder {
            self.folder_index = 0;
        }
        let max_msg = self.visible_messages().len().saturating_sub(1);
        if self.message_index > max_msg {
            self.message_index = 0;
        }
    }

    fn request_sync_selected_folder(&mut self) {
        if !self.imap_enabled {
            return;
        }
        let Some(folder) = self.selected_folder() else {
            return;
        };
        let folder_id = folder.id;
        let folder_name = folder.name.clone();
        let now = Instant::now();
        if let Some((name, last)) = &self.last_folder_sync {
            if *name == folder_name && now.duration_since(*last) < Duration::from_secs(2) {
                return;
            }
        }
        self.last_folder_sync = Some((folder_name.clone(), now));
        self.imap_pending = self.imap_pending.saturating_add(1);
        self.imap_status = Some("IMAP syncing...".to_string());
        let last_seen_uid = self.runtime().block_on(async {
            self.store_handle
                .get_folder_sync_state(folder_id)
                .await
                .ok()
                .flatten()
                .and_then(|s| s.last_seen_uid)
                .map(|v| v as u32)
        });
        let mode = match last_seen_uid {
            Some(uid) => ratmail_mail::SyncMode::Incremental { last_seen_uid: uid },
            None => ratmail_mail::SyncMode::Initial {
                days: self.initial_sync_days,
            },
        };
        let _ = self.engine.send(MailCommand::SyncFolderByName {
            name: folder_name,
            mode,
        });
    }

    fn request_backfill_selected_folder(&mut self) {
        if !self.imap_enabled {
            return;
        }
        let Some(folder) = self.selected_folder() else {
            return;
        };
        let folder_id = folder.id;
        let folder_name = folder.name.clone();
        let now = Instant::now();
        if let Some((name, last)) = &self.last_backfill {
            if *name == folder_name && now.duration_since(*last) < Duration::from_secs(3) {
                return;
            }
        }
        self.last_backfill = Some((folder_name.clone(), now));
        let oldest_ts = self.runtime().block_on(async {
            self.store_handle
                .get_folder_sync_state(folder_id)
                .await
                .ok()
                .flatten()
                .and_then(|s| s.oldest_ts)
        });
        let Some(before_ts) = oldest_ts else {
            self.status_message = Some("No older messages cached yet.".to_string());
            return;
        };
        self.imap_pending = self.imap_pending.saturating_add(1);
        self.imap_status = Some("IMAP loading older...".to_string());
        let mode = ratmail_mail::SyncMode::Backfill {
            before_ts,
            window_days: self.initial_sync_days,
        };
        let _ = self.engine.send(MailCommand::SyncFolderByName {
            name: folder_name,
            mode,
        });
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if self.picker_mode.is_some() {
            return self.on_key_picker(key);
        }
        match self.mode {
            Mode::List | Mode::View => self.on_key_main(key),
            Mode::ViewFocus => self.on_key_focus(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::OverlayLinks
            | Mode::OverlayAttach
            | Mode::OverlayBulkAction
            | Mode::OverlayBulkMove
            | Mode::OverlayConfirmDelete => self.on_key_overlay(key),
        }
    }

    fn drain_channels(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.on_event(event);
        }
        while let Ok(snapshot) = self.store_updates.try_recv() {
            let prev_folder = self.selected_folder().map(|f| f.name.clone());
            let prev_uid = self.selected_message().and_then(|m| m.imap_uid);
            self.store = snapshot;
            self.sort_folders();
            self.restore_selection(prev_folder, prev_uid);
            self.prune_selected_messages();
            if self.store.folders.is_empty() {
                self.select_inbox_if_available();
            }
        }
        while let Ok(event) = self.render_events.try_recv() {
            self.on_render_event(event);
        }
    }

    fn start_compose_new(&mut self) {
        self.compose_to.clear();
        self.compose_cc.clear();
        self.compose_bcc.clear();
        self.compose_subject.clear();
        self.compose_body.clear();
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::To;
        self.compose_cursor_to = 0;
        self.compose_cursor_cc = 0;
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = 0;
        self.compose_cursor_body = 0;
        self.compose_body_desired_col = None;
        self.mode = Mode::Compose;
    }

    fn start_compose_to(&mut self, to: String, subject: Option<String>, body: Option<String>) {
        self.start_compose_new();
        self.compose_to = to;
        if let Some(subject) = subject {
            self.compose_subject = subject;
        }
        if let Some(body) = body {
            self.compose_body = body;
        }
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_cursor_body = text_char_len(&self.compose_body);
        self.compose_focus = if !self.compose_body.is_empty() {
            ComposeFocus::Body
        } else if !self.compose_subject.is_empty() {
            ComposeFocus::Subject
        } else {
            ComposeFocus::To
        };
    }

    fn start_compose_reply(&mut self, reply_all: bool) {
        let raw = self.selected_message().and_then(|msg| {
            self.runtime()
                .block_on(async { self.store_handle.get_raw_body(msg.id).await.ok().flatten() })
        });
        let (to, cc, subject, quote) = build_reply(
            self.selected_detail(),
            raw.as_deref(),
            &self.store.account.address,
            reply_all,
        );
        self.compose_to = to;
        self.compose_cc = cc;
        self.compose_bcc.clear();
        self.compose_subject = subject;
        self.compose_body.clear();
        self.compose_quote = quote;
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::Body;
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_cc = text_char_len(&self.compose_cc);
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_cursor_body = 0;
        self.compose_body_desired_col = None;
        self.mode = Mode::Compose;
    }

    fn start_compose_forward(&mut self) {
        let raw = self.selected_message().and_then(|msg| {
            self.runtime()
                .block_on(async { self.store_handle.get_raw_body(msg.id).await.ok().flatten() })
        });
        let (subject, body) = build_forward(self.selected_detail(), raw.as_deref());
        self.compose_to.clear();
        self.compose_cc.clear();
        self.compose_bcc.clear();
        self.compose_subject = subject;
        self.compose_body = body;
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::To;
        self.compose_cursor_to = 0;
        self.compose_cursor_cc = 0;
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_cursor_body = text_char_len(&self.compose_body);
        self.compose_body_desired_col = None;
        self.mode = Mode::Compose;
    }

    fn on_tick(&mut self) {
        if self.imap_enabled && self.last_folder_sync.is_none() {
            self.select_inbox_if_available();
            self.request_sync_selected_folder();
            self.prefetch_raw_bodies(10);
        }
        if self.imap_pending > 0 {
            let frames = ["|", "/", "-", "\\"];
            self.imap_spinner = (self.imap_spinner + 1) % frames.len();
            self.imap_status = Some(format!("IMAP syncing {}", frames[self.imap_spinner]));
        }
    }

    fn schedule_render(&mut self) {
        if self.view_mode != ViewMode::Rendered {
            return;
        }
        if !self.render_supported {
            return;
        }

        let Some(message_id) = self.selected_message().map(|m| m.id) else {
            return;
        };

        self.render_tile_height_px = self.current_tile_height();
        if let Some(picker) = self.image_picker.as_ref() {
            let fh = picker.font_size().1 as i64;
            if fh > 0 {
                self.render_tile_height_px = ((self.render_tile_height_px + fh - 1) / fh) * fh;
            }
        }

        // Already have tiles for this message and tile height?
        if self.render_message_id == Some(message_id)
            && self.render_tile_count > 0
            && self.render_tiles_height_px == self.render_tile_height_px
        {
            return;
        }

        log_debug(&format!(
            "schedule_render msg_id={} tile_h={} width_px={}",
            message_id, self.render_tile_height_px, self.render_width_px
        ));

        // Check if we have the raw body
        if !self.ensure_raw_body_for_render(message_id) {
            log_debug(&format!("render_skip no_raw msg_id={}", message_id));
            return;
        }

        // Try to load from database cache
        if self.try_load_cached_tiles_db(message_id) {
            log_debug(&format!("render_cache db_hit msg_id={}", message_id));
            return;
        }

        // Cache miss - send render request
        log_debug(&format!("render_cache miss msg_id={}", message_id));
        self.send_render_request(message_id);
    }

    fn try_load_cached_tiles_db(&mut self, message_id: i64) -> bool {
        let store_handle = self.store_handle.clone();
        let width_px = self.render_width_px;
        let tile_height_px = self.render_tile_height_px;
        let theme = "default";
        let remote_policy = if self.allow_remote_images {
            "allowed"
        } else {
            "blocked"
        };

        let cached = self.runtime().block_on(async move {
            store_handle
                .get_cache_tiles(message_id, width_px, tile_height_px, theme, remote_policy)
                .await
        });

        let Ok(tiles) = cached else { return false };
        if tiles.is_empty() {
            return false;
        }

        log_debug(&format!(
            "render_cache db_hit msg_id={} tiles={} tile_h={}",
            message_id,
            tiles.len(),
            tile_height_px
        ));
        self.apply_render_tiles(message_id, tiles);
        true
    }

    fn send_render_request(&mut self, message_id: i64) {
        let remote_policy = if self.allow_remote_images {
            "allowed"
        } else {
            "blocked"
        };
        self.render_request_id += 1;
        self.render_pending = true;
        log_debug(&format!(
            "render_enqueue id={} msg_id={} mode={:?} tile_h={} max_tiles={:?}",
            self.render_request_id,
            message_id,
            self.mode,
            self.render_tile_height_px,
            if self.mode == Mode::ViewFocus {
                None
            } else {
                Some(1)
            }
        ));
        let _ = self.render_request_tx.send(RenderRequest {
            request_id: self.render_request_id,
            message_ids: vec![message_id],
            width_px: self.render_width_px,
            tile_height_px: self.render_tile_height_px,
            max_tiles: if self.mode == Mode::ViewFocus {
                None
            } else {
                Some(1)
            },
            theme: "default".to_string(),
            remote_policy: remote_policy.to_string(),
        });
    }

    fn apply_render_tiles(&mut self, message_id: i64, tiles: Vec<TileMeta>) {
        self.render_message_id = Some(message_id);
        self.render_tiles = tiles;
        self.render_tile_count = self.render_tiles.len();
        self.render_tile_index = 0;
        self.render_tiles_height_px = self.render_tile_height_px;
        self.render_error = None;
        self.render_no_html = false;
        self.image_protocol = None; // Will be created on demand when rendering
        self.render_pending = false;
    }

    fn take_cached_protocol(
        &mut self,
        message_id: i64,
        tile_index: usize,
    ) -> Option<StatefulProtocol> {
        let key = (message_id, tile_index);
        let protocol = self.protocol_cache.remove(&key);
        if protocol.is_some() {
            self.touch_protocol_cache(key);
        }
        protocol
    }

    fn store_protocol_cache(
        &mut self,
        message_id: i64,
        tile_index: usize,
        protocol: StatefulProtocol,
    ) {
        let key = (message_id, tile_index);
        self.protocol_cache.insert(key, protocol);
        self.touch_protocol_cache(key);
        self.prune_protocol_cache();
    }

    fn touch_protocol_cache(&mut self, key: (i64, usize)) {
        if let Some(pos) = self.protocol_cache_lru.iter().position(|k| *k == key) {
            self.protocol_cache_lru.remove(pos);
        }
        self.protocol_cache_lru.push_front(key);
    }

    fn prune_protocol_cache(&mut self) {
        while self.protocol_cache_lru.len() > PROTOCOL_CACHE_LIMIT {
            if let Some(key) = self.protocol_cache_lru.pop_back() {
                self.protocol_cache.remove(&key);
            }
        }
    }

    fn current_tile_height(&self) -> i64 {
        if self.mode == Mode::ViewFocus {
            self.render_tile_height_px_focus
        } else {
            self.render_tile_height_px_side
        }
    }

    fn on_render_event(&mut self, event: RenderEvent) {
        let selected_id = self.selected_message().map(|m| m.id);
        let is_current = selected_id == Some(event.message_id)
            && event.tile_height_px == self.render_tile_height_px;

        if let Some(err) = event.error {
            if is_current {
                self.render_error = Some(err.clone());
                self.render_no_html = false;
                self.render_tile_count = 0;
                self.render_tiles.clear();
                self.render_message_id = None;
                self.image_protocol = None;
                self.render_pending = false;
            }
            log_debug(&format!(
                "render_event error msg_id={} err={}",
                event.message_id, err
            ));
            return;
        }

        if event.no_html {
            if is_current {
                self.render_no_html = true;
                self.render_tile_count = 0;
                self.render_tiles.clear();
                self.render_message_id = None;
                self.image_protocol = None;
                self.render_pending = false;
            }
            log_debug(&format!("render_event no_html msg_id={}", event.message_id));
            return;
        }

        if is_current {
            self.apply_render_tiles(event.message_id, event.tiles);
        }
        log_debug(&format!(
            "render_event ok msg_id={} tiles={} tile_h={}",
            event.message_id, self.render_tile_count, event.tile_height_px
        ));
    }

    fn ensure_text_cache_for_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.body.is_empty() && !detail.links.is_empty() {
                return;
            }
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime().block_on(async move {
            if let Some(raw) = store_handle.get_raw_body(message_id).await? {
                let display = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)?;
                store_handle
                    .upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, &display.text)
                    .await?;
                Ok::<_, anyhow::Error>(Some(display))
            } else {
                Ok::<_, anyhow::Error>(None)
            }
        });

        if let Ok(Some(display)) = result {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.body = display.text;
                detail.links = display.links;
            } else if let Some(summary) = self.selected_message() {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        date: summary.date.clone(),
                        body: display.text,
                        links: display.links,
                        attachments: Vec::new(),
                    },
                );
            }
        } else if self.imap_enabled {
            if let (Some(uid), Some(folder_name)) = (message.imap_uid, folder_name) {
                let _ = self.engine.send(MailCommand::FetchMessageBody {
                    message_id,
                    folder_name,
                    uid,
                });
                self.status_message = Some("Fetching body...".to_string());
            }
        }
    }

    fn ensure_raw_body_for_render(&mut self, message_id: i64) -> bool {
        let store_handle = self.store_handle.clone();
        let has_raw = self.runtime().block_on(async move {
            store_handle
                .get_raw_body(message_id)
                .await
                .ok()
                .flatten()
                .is_some()
        });
        if has_raw {
            return true;
        }
        if self.imap_enabled && !self.pending_body_fetch.contains(&message_id) {
            let folder_name = self.selected_folder().map(|f| f.name.clone());
            if let (Some(uid), Some(folder_name)) = (
                self.selected_message().and_then(|m| m.imap_uid),
                folder_name,
            ) {
                self.pending_body_fetch.insert(message_id);
                let _ = self.engine.send(MailCommand::FetchMessageBody {
                    message_id,
                    folder_name,
                    uid,
                });
                self.status_message = Some("Fetching body...".to_string());
            }
        }
        false
    }

    fn prefetch_raw_bodies(&mut self, count: usize) {
        if !self.imap_enabled {
            return;
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());
        let Some(folder_name) = folder_name else {
            return;
        };
        let ids: Vec<(i64, Option<u32>)> = self
            .visible_messages()
            .into_iter()
            .take(count)
            .map(|m| (m.id, m.imap_uid))
            .collect();
        for (message_id, uid) in ids {
            let Some(uid) = uid else { continue };
            if self.pending_body_fetch.contains(&message_id) {
                continue;
            }
            let store_handle = self.store_handle.clone();
            let has_raw = self.runtime().block_on(async move {
                store_handle
                    .get_raw_body(message_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some()
            });
            if has_raw {
                continue;
            }
            self.pending_body_fetch.insert(message_id);
            let _ = self.engine.send(MailCommand::FetchMessageBody {
                message_id,
                folder_name: folder_name.clone(),
                uid,
            });
        }
    }

    fn ensure_attachments_for_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.attachments.is_empty() {
                return;
            }
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime().block_on(async move {
            if let Some(raw) = store_handle.get_raw_body(message_id).await? {
                let attachments = extract_attachments(&raw)?;
                Ok::<_, anyhow::Error>(Some(attachments))
            } else {
                Ok::<_, anyhow::Error>(None)
            }
        });

        if let Ok(Some(attachments)) = result {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.attachments = attachments;
            } else if let Some(summary) = self.selected_message() {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        date: summary.date.clone(),
                        body: String::new(),
                        links: Vec::new(),
                        attachments,
                    },
                );
            }
        } else if self.imap_enabled {
            if let (Some(uid), Some(folder_name)) = (message.imap_uid, folder_name) {
                let _ = self.engine.send(MailCommand::FetchMessageBody {
                    message_id,
                    folder_name,
                    uid,
                });
                self.status_message = Some("Fetching body...".to_string());
            }
        }
    }

    fn on_event(&mut self, event: MailEvent) {
        match event {
            MailEvent::SyncStarted(_) => self.sync_status = "syncing".to_string(),
            MailEvent::SyncCompleted(_) => self.sync_status = "idle".to_string(),
            MailEvent::SyncFailed { .. } => self.sync_status = "error".to_string(),
            MailEvent::SendStarted => {
                self.status_message = Some("Sending...".to_string());
            }
            MailEvent::SendCompleted => {
                self.status_message = Some("Sent".to_string());
                if self.mode == Mode::Compose {
                    self.mode = Mode::List;
                    self.focus = Focus::Messages;
                    self.compose_to.clear();
                    self.compose_cc.clear();
                    self.compose_bcc.clear();
                    self.compose_subject.clear();
                    self.compose_body.clear();
                    self.compose_quote.clear();
                    self.compose_attachments.clear();
                    self.compose_focus = ComposeFocus::Body;
                    self.compose_cursor_to = 0;
                    self.compose_cursor_cc = 0;
                    self.compose_cursor_bcc = 0;
                    self.compose_cursor_subject = 0;
                    self.compose_cursor_body = 0;
                    self.compose_body_desired_col = None;
                }
            }
            MailEvent::SendFailed { reason } => {
                self.status_message = Some(format!("Send failed: {}", reason));
            }
            MailEvent::ImapFolders(folders) => {
                self.imap_pending = self.imap_pending.saturating_sub(1);
                if self.imap_pending == 0 {
                    self.imap_status = None;
                }
                let account_id = self.store.account.id;
                self.imap_status = Some(format!("IMAP: {} folders", folders.len()));
                let models: Vec<Folder> = folders
                    .into_iter()
                    .map(|f| Folder {
                        id: 0,
                        account_id,
                        name: f.name,
                        unread: f.unread,
                    })
                    .collect();
                let _ = self.store_update_tx.send(StoreUpdate::Folders {
                    account_id,
                    folders: models,
                });
            }
            MailEvent::ImapMessages {
                folder_name,
                messages,
            } => {
                self.imap_pending = self.imap_pending.saturating_sub(1);
                if self.imap_pending == 0 {
                    self.imap_status = None;
                }
                let account_id = self.store.account.id;
                self.imap_status = Some(format!(
                    "IMAP: {} messages in {}",
                    messages.len(),
                    folder_name
                ));
                log_debug(&format!(
                    "imap_messages folder={} count={}",
                    folder_name,
                    messages.len()
                ));
                let items: Vec<MessageSummary> = messages
                    .into_iter()
                    .map(|m| MessageSummary {
                        id: 0,
                        folder_id: 0,
                        imap_uid: Some(m.uid),
                        date: m.date,
                        from: m.from,
                        subject: m.subject,
                        unread: m.unread,
                        preview: m.preview,
                    })
                    .collect();
                let sync_update = build_sync_update(&items);
                let _ = self.store_update_tx.send(StoreUpdate::AppendMessages {
                    account_id,
                    folder_name,
                    messages: items,
                    sync_update,
                });
                self.prefetch_raw_bodies(10);
            }
            MailEvent::ImapBody { message_id, raw } => {
                let account_id = self.store.account.id;
                self.pending_body_fetch.remove(&message_id);
                let cached_text =
                    if let Ok(display) = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize) {
                        if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                            detail.body = display.text.clone();
                            detail.links = display.links.clone();
                        } else if let Some(summary) =
                            self.store.messages.iter().find(|m| m.id == message_id)
                        {
                            self.store.message_details.insert(
                                message_id,
                                MessageDetail {
                                    id: message_id,
                                    subject: summary.subject.clone(),
                                    from: summary.from.clone(),
                                    date: summary.date.clone(),
                                    body: display.text.clone(),
                                    links: display.links.clone(),
                                    attachments: Vec::new(),
                                },
                            );
                        }
                        Some(display.text)
                    } else {
                        None
                    };
                let _ = self.store_update_tx.send(StoreUpdate::RawBody {
                    account_id,
                    message_id,
                    raw,
                    cached_text,
                });
                if self.view_mode == ViewMode::Rendered {
                    if self.selected_message().map(|m| m.id) == Some(message_id) {
                        self.schedule_render();
                        self.ensure_text_cache_for_selected();
                    }
                }
            }
            MailEvent::ImapError { reason } => {
                self.imap_pending = self.imap_pending.saturating_sub(1);
                self.imap_status = Some(format!("IMAP error: {}", reason));
            }
            _ => {}
        }
    }

    fn on_key_main(&mut self, key: KeyEvent) -> bool {
        if self.mode == Mode::ViewFocus {
            return self.on_key_focus(key);
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => match self.focus {
                Focus::Messages => {
                    let count = self.visible_messages().len();
                    if self.message_index + 1 < count {
                        self.message_index += 1;
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                            self.ensure_text_cache_for_selected();
                        } else {
                            self.ensure_text_cache_for_selected();
                        }
                        if self.message_index + 1 == count {
                            self.request_backfill_selected_folder();
                        }
                    }
                }
                Focus::Folders => {
                    if self.folder_index + 1 < self.store.folders.len() {
                        self.folder_index += 1;
                        self.message_index = 0;
                        self.clear_selected_messages();
                        self.request_sync_selected_folder();
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                        }
                    }
                }
            },
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => match self.focus {
                Focus::Messages => {
                    if self.message_index > 0 {
                        self.message_index -= 1;
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                            self.ensure_text_cache_for_selected();
                        } else {
                            self.ensure_text_cache_for_selected();
                        }
                    }
                }
                Focus::Folders => {
                    if self.folder_index > 0 {
                        self.folder_index -= 1;
                        self.message_index = 0;
                        self.clear_selected_messages();
                        self.request_sync_selected_folder();
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                        }
                    }
                }
            },
            (KeyCode::Char(' '), _) => {
                if self.focus == Focus::Messages {
                    self.toggle_select_current();
                    let count = self.visible_messages().len();
                    if self.message_index + 1 < count {
                        self.message_index += 1;
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                            self.ensure_text_cache_for_selected();
                        } else {
                            self.ensure_text_cache_for_selected();
                        }
                        if self.message_index + 1 == count {
                            self.request_backfill_selected_folder();
                        }
                    }
                }
            }
            (KeyCode::Enter, _) => {
                if self.focus == Focus::Messages {
                    if !self.selected_message_ids.is_empty() {
                        let ids = self.selected_message_ids.iter().copied().collect();
                        self.open_bulk_action_overlay(ids);
                    } else {
                        self.mode = Mode::ViewFocus;
                        self.view_scroll = 0;
                        self.render_tile_height_px = self.render_tile_height_px_focus;
                        self.render_tiles.clear();
                        self.render_tile_count = 0;
                        self.render_tiles_height_px = 0;
                        self.render_message_id = None;
                        self.image_protocol = None;
                        self.ensure_text_cache_for_selected();
                        if self.view_mode == ViewMode::Rendered {
                            self.schedule_render();
                            self.ensure_text_cache_for_selected();
                        }
                    }
                }
            }
            (KeyCode::Esc, _) => {
                if !self.selected_message_ids.is_empty() {
                    self.clear_selected_messages();
                } else if self.mode == Mode::ViewFocus {
                    self.mode = Mode::View;
                    self.render_tile_height_px = self.render_tile_height_px_side;
                    if self.view_mode == ViewMode::Rendered {
                        self.render_tiles.clear();
                        self.render_tile_count = 0;
                        self.render_tiles_height_px = 0;
                        self.render_message_id = None;
                        self.schedule_render();
                    }
                }
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Folders => Focus::Messages,
                    Focus::Messages => Focus::Folders,
                };
            }
            (KeyCode::Char('h'), _) | (KeyCode::Left, _) => {
                self.focus = Focus::Folders;
            }
            (KeyCode::Right, _) => {
                self.focus = Focus::Messages;
            }
            (KeyCode::Char('l'), _) if self.focus == Focus::Folders => {
                self.focus = Focus::Messages;
            }
            (KeyCode::Char('v'), _) => {
                self.view_mode = match self.view_mode {
                    ViewMode::Text => ViewMode::Rendered,
                    ViewMode::Rendered => ViewMode::Text,
                };
                if self.view_mode == ViewMode::Text {
                    self.ensure_text_cache_for_selected();
                } else {
                    self.render_tile_height_px = if self.mode == Mode::ViewFocus {
                        self.render_tile_height_px_focus
                    } else {
                        self.render_tile_height_px_side
                    };
                    self.schedule_render();
                    self.ensure_text_cache_for_selected();
                }
            }
            (KeyCode::Char('l'), _) => {
                self.ensure_text_cache_for_selected();
                self.link_index = 0;
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayLinks;
            }
            (KeyCode::Char('a'), _) => {
                self.ensure_attachments_for_selected();
                self.attach_index = 0;
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.start_compose_reply(false);
            }
            (KeyCode::Char('R'), _) => {
                self.start_compose_reply(true);
            }
            (KeyCode::Char('f'), _) => {
                self.start_compose_forward();
            }
            (KeyCode::Char('c'), _) => {
                self.start_compose_new();
            }
            (KeyCode::Char('m'), _) => {
                if self.focus == Focus::Messages {
                    let ids = self.active_message_ids();
                    self.open_move_overlay(ids, self.mode);
                }
            }
            (KeyCode::Char('d'), _) => {
                if self.focus == Focus::Messages {
                    let ids = self.active_message_ids();
                    self.open_confirm_delete(ids, self.mode);
                }
            }
            (KeyCode::Char('p'), _) => {
                self.show_preview = !self.show_preview;
            }
            (KeyCode::Char('?'), _) => {
                self.show_help = !self.show_help;
            }
            (KeyCode::Char('o'), _) => {
                self.request_backfill_selected_folder();
            }
            _ => {}
        }
        false
    }

    fn on_key_focus(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                self.view_scroll = self.view_scroll.saturating_add(1);
                log_debug(&format!(
                    "focus_scroll down now={} tiles={}",
                    self.view_scroll, self.render_tile_count
                ));
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(1);
                log_debug(&format!(
                    "focus_scroll up now={} tiles={}",
                    self.view_scroll, self.render_tile_count
                ));
            }
            (KeyCode::PageDown, _) => {
                self.view_scroll = self.view_scroll.saturating_add(10);
                log_debug(&format!(
                    "focus_scroll pgdn now={} tiles={}",
                    self.view_scroll, self.render_tile_count
                ));
            }
            (KeyCode::PageUp, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(10);
                log_debug(&format!(
                    "focus_scroll pgup now={} tiles={}",
                    self.view_scroll, self.render_tile_count
                ));
            }
            (KeyCode::Char('g'), _) => {
                self.view_scroll = 0;
            }
            (KeyCode::Char('G'), _) => {
                self.view_scroll = u16::MAX;
            }
            (KeyCode::Char('v'), _) => {
                self.view_mode = match self.view_mode {
                    ViewMode::Text => ViewMode::Rendered,
                    ViewMode::Rendered => ViewMode::Text,
                };
                if self.view_mode == ViewMode::Text {
                    self.ensure_text_cache_for_selected();
                } else {
                    self.render_tile_height_px = self.render_tile_height_px_focus;
                    self.schedule_render();
                }
            }
            (KeyCode::Char('l'), _) => {
                self.ensure_text_cache_for_selected();
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayLinks;
            }
            (KeyCode::Char('a'), _) => {
                self.ensure_attachments_for_selected();
                self.attach_index = 0;
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.start_compose_reply(false);
            }
            (KeyCode::Char('R'), _) => {
                self.start_compose_reply(true);
            }
            (KeyCode::Char('f'), _) => {
                self.start_compose_forward();
            }
            (KeyCode::Char('p'), _) => {
                self.show_preview = !self.show_preview;
            }
            (KeyCode::Char('?'), _) => {
                self.show_help = !self.show_help;
            }
            (KeyCode::Char('o'), _) => {
                self.request_backfill_selected_folder();
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
                self.render_tile_height_px = self.render_tile_height_px_side;
                if self.view_mode == ViewMode::Rendered {
                    self.render_tiles.clear();
                    self.render_tile_count = 0;
                    self.render_tiles_height_px = 0;
                    self.render_message_id = None;
                    self.schedule_render();
                }
            }
            _ => {}
        }

        if self.view_mode == ViewMode::Rendered && self.render_tile_count > 0 {
            let max = (self.render_tile_count - 1) as u16;
            if self.view_scroll > max {
                self.view_scroll = max;
            }
        }
        false
    }

    fn on_key_overlay(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::OverlayLinks => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('j') | KeyCode::Down => {
                    let count = self.selected_detail().map(|d| d.links.len()).unwrap_or(0);
                    if self.link_index + 1 < count {
                        self.link_index += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.link_index > 0 {
                        self.link_index -= 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(detail) = self.selected_detail() {
                        if let Some(link) = detail.links.get(self.link_index) {
                            if let Some((to, subject, body)) = parse_mailto(link) {
                                self.start_compose_to(to, subject, body);
                            } else if looks_like_email(link) {
                                self.start_compose_to(link.to_string(), None, None);
                            } else {
                                let _ = open::that(link);
                            }
                        }
                    }
                }
                _ => {}
            },
            Mode::OverlayAttach => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('j') | KeyCode::Down => {
                    let count = self
                        .selected_detail()
                        .map(|d| d.attachments.len())
                        .unwrap_or(0);
                    if self.attach_index + 1 < count {
                        self.attach_index += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.attach_index > 0 {
                        self.attach_index -= 1;
                    }
                }
                KeyCode::Enter => {
                    self.open_selected_attachment();
                }
                KeyCode::Char('s') => {
                    self.prompt_save_selected_attachment();
                }
                _ => {}
            },
            Mode::OverlayBulkAction => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('m') => {
                    let ids = self.bulk_action_ids.clone();
                    self.open_move_overlay(ids, Mode::OverlayBulkAction);
                }
                KeyCode::Char('d') => {
                    let ids = self.bulk_action_ids.clone();
                    self.open_confirm_delete(ids, Mode::OverlayBulkAction);
                }
                _ => {}
            },
            Mode::OverlayBulkMove => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.bulk_folder_index + 1 < self.store.folders.len() {
                        self.bulk_folder_index += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.bulk_folder_index > 0 {
                        self.bulk_folder_index -= 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(folder) = self.store.folders.get(self.bulk_folder_index) {
                        let ids = self.bulk_action_ids.clone();
                        self.queue_move_messages(ids, folder.id);
                        if self.overlay_return == Mode::OverlayBulkAction {
                            self.mode = self.bulk_done_return;
                        } else {
                            self.mode = self.overlay_return;
                        }
                    }
                }
                _ => {}
            },
            Mode::OverlayConfirmDelete => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('y') | KeyCode::Enter => {
                    let ids = self.confirm_delete_ids.clone();
                    self.queue_delete_messages(ids);
                    self.mode = self.confirm_delete_return;
                }
                KeyCode::Char('n') => {
                    self.mode = self.overlay_return;
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn on_key_compose(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A')) {
            self.start_picker(PickerMode::Attach);
            return false;
        }
        if ctrl && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
            if self.compose_attachments.is_empty() {
                self.status_message = Some("No attachments to remove".to_string());
            } else {
                let removed = self.compose_attachments.pop();
                if let Some(removed) = removed {
                    self.status_message = Some(format!("Removed attachment {}", removed.filename));
                }
            }
            return false;
        }
        if matches!(key.code, KeyCode::F(5))
            || (ctrl
                && matches!(
                    key.code,
                    KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('\u{13}')
                ))
        {
            let to = self.compose_to.clone();
            let cc = self.compose_cc.clone();
            let bcc = self.compose_bcc.clone();
            let subject = self.compose_subject.clone();
            let mut body = self.compose_body.clone();
            if !self.compose_quote.is_empty() {
                body.push_str(&self.compose_quote);
            }
            let attachments: Vec<OutgoingAttachment> = self
                .compose_attachments
                .iter()
                .map(|a| OutgoingAttachment {
                    filename: a.filename.clone(),
                    mime: a.mime.clone(),
                    data: a.data.clone(),
                })
                .collect();
            if to.trim().is_empty() && cc.trim().is_empty() && bcc.trim().is_empty() {
                self.status_message = Some("No recipient".to_string());
            } else {
                self.status_message = Some("Sending...".to_string());
                let _ = self.engine.send(MailCommand::SendMessage {
                    to,
                    cc,
                    bcc,
                    subject,
                    body,
                    attachments,
                });
            }
            return false;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.mode = Mode::View;
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
            }
            (KeyCode::BackTab, _) | (KeyCode::Tab, KeyModifiers::SHIFT) => {
                self.compose_focus = compose_focus_prev(self.compose_focus);
                self.compose_body_desired_col = None;
            }
            (KeyCode::Tab, _) | (KeyCode::Char('\t'), _) => {
                if self.compose_focus == ComposeFocus::Body {
                    insert_text_at_cursor(
                        &mut self.compose_body,
                        &mut self.compose_cursor_body,
                        "    ",
                    );
                    self.compose_body_desired_col = None;
                } else {
                    self.compose_focus = compose_focus_next(self.compose_focus);
                    self.compose_body_desired_col = None;
                }
            }
            (KeyCode::Left, _) => match self.compose_focus {
                ComposeFocus::To => {
                    move_cursor_left(&self.compose_to, &mut self.compose_cursor_to);
                }
                ComposeFocus::Cc => {
                    move_cursor_left(&self.compose_cc, &mut self.compose_cursor_cc);
                }
                ComposeFocus::Bcc => {
                    move_cursor_left(&self.compose_bcc, &mut self.compose_cursor_bcc);
                }
                ComposeFocus::Subject => {
                    move_cursor_left(&self.compose_subject, &mut self.compose_cursor_subject);
                }
                ComposeFocus::Body => {
                    move_cursor_left(&self.compose_body, &mut self.compose_cursor_body);
                    self.compose_body_desired_col = None;
                }
            },
            (KeyCode::Right, _) => match self.compose_focus {
                ComposeFocus::To => {
                    move_cursor_right(&self.compose_to, &mut self.compose_cursor_to);
                }
                ComposeFocus::Cc => {
                    move_cursor_right(&self.compose_cc, &mut self.compose_cursor_cc);
                }
                ComposeFocus::Bcc => {
                    move_cursor_right(&self.compose_bcc, &mut self.compose_cursor_bcc);
                }
                ComposeFocus::Subject => {
                    move_cursor_right(&self.compose_subject, &mut self.compose_cursor_subject);
                }
                ComposeFocus::Body => {
                    move_cursor_right(&self.compose_body, &mut self.compose_cursor_body);
                    self.compose_body_desired_col = None;
                }
            },
            (KeyCode::Up, _) => {
                if self.compose_focus == ComposeFocus::Body {
                    self.compose_cursor_body = move_cursor_vertical(
                        &self.compose_body,
                        self.compose_cursor_body,
                        DirectionKey::Up,
                        &mut self.compose_body_desired_col,
                    );
                }
            }
            (KeyCode::Down, _) => {
                if self.compose_focus == ComposeFocus::Body {
                    self.compose_cursor_body = move_cursor_vertical(
                        &self.compose_body,
                        self.compose_cursor_body,
                        DirectionKey::Down,
                        &mut self.compose_body_desired_col,
                    );
                }
            }
            (KeyCode::Enter, _) | (KeyCode::Char('\n'), _) | (KeyCode::Char('\r'), _) => {
                match self.compose_focus {
                    ComposeFocus::To
                    | ComposeFocus::Cc
                    | ComposeFocus::Bcc
                    | ComposeFocus::Subject => {
                        self.compose_focus = compose_focus_next(self.compose_focus);
                    }
                    ComposeFocus::Body => {
                        if apply_compose_key(
                            &mut self.compose_body,
                            &mut self.compose_cursor_body,
                            key,
                            true,
                        ) {
                            self.compose_body_desired_col = None;
                        }
                    }
                }
                self.compose_body_desired_col = None;
            }
            _ => match self.compose_focus {
                ComposeFocus::To => {
                    apply_compose_key(
                        &mut self.compose_to,
                        &mut self.compose_cursor_to,
                        key,
                        false,
                    );
                }
                ComposeFocus::Cc => {
                    apply_compose_key(
                        &mut self.compose_cc,
                        &mut self.compose_cursor_cc,
                        key,
                        false,
                    );
                }
                ComposeFocus::Bcc => {
                    apply_compose_key(
                        &mut self.compose_bcc,
                        &mut self.compose_cursor_bcc,
                        key,
                        false,
                    );
                }
                ComposeFocus::Subject => {
                    apply_compose_key(
                        &mut self.compose_subject,
                        &mut self.compose_cursor_subject,
                        key,
                        false,
                    );
                }
                ComposeFocus::Body => {
                    if apply_compose_key(
                        &mut self.compose_body,
                        &mut self.compose_cursor_body,
                        key,
                        true,
                    ) {
                        self.compose_body_desired_col = None;
                    }
                }
            },
        }
        false
    }

    fn on_key_picker(&mut self, key: KeyEvent) -> bool {
        let Some(mode) = self.picker_mode.clone() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {
                self.close_picker("Canceled");
                return false;
            }
            KeyCode::Tab => {
                if matches!(mode, PickerMode::Save { .. }) {
                    self.picker_focus = match self.picker_focus {
                        PickerFocus::Explorer => PickerFocus::Filename,
                        PickerFocus::Filename => PickerFocus::Explorer,
                    };
                }
                return false;
            }
            _ => {}
        }

        match self.picker_focus {
            PickerFocus::Explorer => {
                if key.code == KeyCode::Enter {
                    match mode {
                        PickerMode::Attach => {
                            self.confirm_attach_selection();
                        }
                        PickerMode::Save { .. } => {
                            if let Some(current) = self.picker.as_ref().map(|p| p.current()) {
                                if current.is_file() {
                                    self.picker_filename = current.name().to_string();
                                    self.picker_cursor = text_char_len(&self.picker_filename);
                                }
                            }
                            self.picker_focus = PickerFocus::Filename;
                        }
                    }
                    return false;
                }
                self.handle_picker_navigation(key);
            }
            PickerFocus::Filename => match key.code {
                KeyCode::Enter => {
                    if let PickerMode::Save {
                        message_id,
                        attachment_index,
                        filename,
                    } = mode
                    {
                        let mut name = self.picker_filename.trim().to_string();
                        if name.is_empty() {
                            name = filename;
                        }
                        if let Some(dir) = self.picker_selected_dir() {
                            let target = dir.join(name);
                            match self.save_attachment_to_path(
                                message_id,
                                attachment_index,
                                &target,
                            ) {
                                Ok(path) => {
                                    self.close_picker(&format!(
                                        "Saved attachment to {}",
                                        path.display()
                                    ));
                                }
                                Err(err) => {
                                    if err.to_string() != "message body not cached" {
                                        self.status_message = Some(format!("Save failed: {}", err));
                                    }
                                }
                            }
                        }
                    }
                }
                KeyCode::Left => move_cursor_left(&self.picker_filename, &mut self.picker_cursor),
                KeyCode::Right => move_cursor_right(&self.picker_filename, &mut self.picker_cursor),
                _ => {
                    apply_input_key(&mut self.picker_filename, &mut self.picker_cursor, key);
                }
            },
        }
        false
    }

    fn start_picker(&mut self, mode: PickerMode) {
        let theme = ExplorerTheme::default()
            .with_block(Block::default().borders(Borders::ALL))
            .add_default_title();
        let mut picker = match FileExplorer::with_theme(theme) {
            Ok(picker) => picker,
            Err(err) => {
                self.status_message = Some(format!("Picker error: {}", err));
                return;
            }
        };
        if let Ok(home) = std::env::var("HOME") {
            let _ = picker.set_cwd(home);
        }
        self.picker = Some(picker);
        self.picker_mode = Some(mode);
        self.picker_focus = PickerFocus::Explorer;
        self.picker_filename.clear();
        self.picker_cursor = 0;
        if let Some(PickerMode::Save { filename, .. }) = &self.picker_mode {
            self.picker_filename = filename.clone();
            self.picker_cursor = text_char_len(&self.picker_filename);
        }
    }

    fn close_picker(&mut self, status: &str) {
        self.picker_mode = None;
        self.picker = None;
        self.picker_filename.clear();
        self.picker_cursor = 0;
        self.picker_focus = PickerFocus::Explorer;
        self.status_message = Some(status.to_string());
    }

    fn handle_picker_navigation(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let input = match key.code {
            KeyCode::Char('j') | KeyCode::Down => ExplorerInput::Down,
            KeyCode::Char('k') | KeyCode::Up => ExplorerInput::Up,
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    ExplorerInput::ToggleShowHidden
                } else {
                    ExplorerInput::Left
                }
            }
            KeyCode::Char('l') | KeyCode::Right => ExplorerInput::Right,
            KeyCode::Home => ExplorerInput::Home,
            KeyCode::End => ExplorerInput::End,
            KeyCode::PageUp => ExplorerInput::PageUp,
            KeyCode::PageDown => ExplorerInput::PageDown,
            _ => ExplorerInput::None,
        };
        if input != ExplorerInput::None {
            let _ = picker.handle(input);
        }
    }

    fn picker_selected_dir(&self) -> Option<PathBuf> {
        let picker = self.picker.as_ref()?;
        let current = picker.current();
        if current.is_dir() {
            return Some(current.path().clone());
        }
        current
            .path()
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| Some(picker.cwd().clone()))
    }

    fn confirm_attach_selection(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let target = picker.current().path().clone();
        match self.add_compose_attachment_from_path(&target) {
            Ok(added) => {
                self.close_picker(&format!(
                    "Attached {} ({})",
                    added.filename,
                    format_size(added.size)
                ));
            }
            Err(err) => {
                self.status_message = Some(format!("Attach failed: {}", err));
            }
        }
    }

    fn open_selected_attachment(&mut self) {
        let Some(detail) = self.selected_detail() else {
            return;
        };
        if detail.attachments.is_empty() || self.attach_index >= detail.attachments.len() {
            return;
        }
        let message_id = detail.id;
        let filename = detail.attachments[self.attach_index].filename.clone();
        match self.save_attachment_to_temp(message_id, self.attach_index, &filename) {
            Ok(path) => {
                let _ = open::that(&path);
                self.status_message = Some(format!("Opened {}", path.display()));
            }
            Err(err) => {
                if err.to_string() != "message body not cached" {
                    self.status_message = Some(format!("Open failed: {}", err));
                }
            }
        }
    }

    fn prompt_save_selected_attachment(&mut self) {
        let Some(detail) = self.selected_detail() else {
            return;
        };
        if detail.attachments.is_empty() || self.attach_index >= detail.attachments.len() {
            return;
        }
        let filename = detail.attachments[self.attach_index].filename.clone();
        let message_id = detail.id;
        self.start_picker(PickerMode::Save {
            message_id,
            attachment_index: self.attach_index,
            filename,
        });
    }

    fn add_compose_attachment_from_path(&mut self, path: &Path) -> Result<ComposeAttachment> {
        if !path.exists() {
            return Err(anyhow::anyhow!("file not found"));
        }
        let attachment = if path.is_dir() {
            self.build_zip_attachment(path)?
        } else {
            self.build_file_attachment(path)?
        };
        self.compose_attachments.push(attachment.clone());
        Ok(attachment)
    }

    fn build_file_attachment(&self, path: &Path) -> Result<ComposeAttachment> {
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("attachment")
            .to_string();
        let data = std::fs::read(path)?;
        let size = data.len();
        let mime = MimeGuess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        Ok(ComposeAttachment {
            filename,
            mime,
            size,
            data,
        })
    }

    fn build_zip_attachment(&self, path: &Path) -> Result<ComposeAttachment> {
        let folder = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("attachment")
            .to_string();
        let filename = format!("{}.zip", folder);
        let data = zip_directory(path)?;
        let size = data.len();
        Ok(ComposeAttachment {
            filename,
            mime: "application/zip".to_string(),
            size,
            data,
        })
    }

    fn save_attachment_to_temp(
        &mut self,
        message_id: i64,
        attachment_index: usize,
        filename: &str,
    ) -> Result<PathBuf> {
        let safe = safe_filename(filename);
        let temp_name = format!("ratmail-{}-{}-{}", message_id, attachment_index + 1, safe);
        let path = std::env::temp_dir().join(temp_name);
        self.save_attachment_to_path(message_id, attachment_index, &path)
    }

    fn save_attachment_to_path(
        &mut self,
        message_id: i64,
        attachment_index: usize,
        path: &Path,
    ) -> Result<PathBuf> {
        let raw = self.get_raw_body_or_fetch(message_id)?;
        let data = extract_attachment_data(&raw, attachment_index)?
            .ok_or_else(|| anyhow::anyhow!("attachment not found"))?;

        let mut target = PathBuf::from(path);
        if target.is_dir() {
            target = target.join(safe_filename(&data.filename));
        }
        if let Some(parent) = target.parent() {
            if !parent.exists() {
                return Err(anyhow::anyhow!("parent directory does not exist"));
            }
        }
        std::fs::write(&target, &data.data)?;
        Ok(target)
    }

    fn get_raw_body_or_fetch(&mut self, message_id: i64) -> Result<Vec<u8>> {
        let raw = self.runtime().block_on(async {
            self.store_handle
                .get_raw_body(message_id)
                .await
                .ok()
                .flatten()
        });
        if let Some(raw) = raw {
            return Ok(raw);
        }
        if self.imap_enabled {
            let (uid, folder_name) = self.message_location(message_id);
            if let (Some(uid), Some(folder_name)) = (uid, folder_name) {
                let _ = self.engine.send(MailCommand::FetchMessageBody {
                    message_id,
                    folder_name,
                    uid,
                });
                self.status_message = Some("Fetching body...".to_string());
            }
        }
        Err(anyhow::anyhow!("message body not cached"))
    }

    fn message_location(&self, message_id: i64) -> (Option<u32>, Option<String>) {
        let message = self.store.messages.iter().find(|m| m.id == message_id);
        let uid = message.and_then(|m| m.imap_uid);
        let folder_name = message
            .and_then(|m| self.store.folders.iter().find(|f| f.id == m.folder_id))
            .map(|f| f.name.clone());
        (uid, folder_name)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectionKey {
    Up,
    Down,
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

fn insert_text_at_cursor(text: &mut String, cursor: &mut usize, insert: &str) {
    if insert.is_empty() {
        return;
    }
    let idx = char_to_byte_idx(text, *cursor);
    text.insert_str(idx, insert);
    *cursor += insert.chars().count();
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

fn line_col_to_cursor(text: &str, line: usize, col: usize) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() {
        return 0;
    }
    let target_line = line.min(lines.len().saturating_sub(1));
    let mut idx = 0usize;
    for i in 0..target_line {
        idx += lines[i].chars().count();
        idx += 1;
    }
    let line_len = lines[target_line].chars().count();
    idx + col.min(line_len)
}

fn move_cursor_vertical(
    text: &str,
    cursor: usize,
    direction: DirectionKey,
    desired_col: &mut Option<usize>,
) -> usize {
    let len = text_char_len(text);
    let cursor = cursor.min(len);
    let (line, col) = cursor_line_col(text, cursor);
    let lines = text.split('\n').count().max(1);
    let target_line = match direction {
        DirectionKey::Up => line.saturating_sub(1),
        DirectionKey::Down => (line + 1).min(lines.saturating_sub(1)),
    };
    let desired = desired_col.get_or_insert(col);
    line_col_to_cursor(text, target_line, *desired)
}

async fn render_worker(
    mut rx: tokio::sync::watch::Receiver<RenderRequest>,
    tx: tokio::sync::mpsc::UnboundedSender<RenderEvent>,
    store: SqliteMailStore,
    renderer: Arc<dyn Renderer>,
) {
    loop {
        if rx.changed().await.is_err() {
            break;
        }
        let request = rx.borrow().clone();
        let current_id = request.request_id;
        let allow_remote = request.remote_policy == "allowed";
        log_debug(&format!(
            "render_worker request id={} tile_h={} max_tiles={:?} msgs={:?}",
            current_id, request.tile_height_px, request.max_tiles, request.message_ids
        ));

        for message_id in request.message_ids {
            if rx.borrow().request_id != current_id {
                break;
            }

            log_debug(&format!(
                "render_worker start msg_id={} tile_h={}",
                message_id, request.tile_height_px
            ));
            let html = match store
                .get_cache_html(message_id, &request.remote_policy)
                .await
            {
                Ok(Some(html)) => Some(html),
                Ok(None) => {
                    if let Ok(Some(raw)) = store.get_raw_body(message_id).await {
                        if let Ok(Some(prepared)) = prepare_html(&raw, allow_remote) {
                            let _ = store
                                .upsert_cache_html(
                                    message_id,
                                    &request.remote_policy,
                                    &prepared.html,
                                )
                                .await;
                        }
                    }
                    store
                        .get_cache_html(message_id, &request.remote_policy)
                        .await
                        .ok()
                        .flatten()
                }
                Err(err) => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        tile_height_px: request.tile_height_px,
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                    log_debug(&format!(
                        "render_worker html error msg_id={} err={}",
                        message_id, err
                    ));
                    continue;
                }
            };

            let Some(html) = html else {
                let _ = tx.send(RenderEvent {
                    message_id,
                    tiles: Vec::new(),
                    tile_height_px: request.tile_height_px,
                    no_html: true,
                    error: None,
                });
                log_debug(&format!("render_worker no_html msg_id={}", message_id));
                continue;
            };

            match store
                .get_cache_tiles(
                    message_id,
                    request.width_px,
                    request.tile_height_px,
                    &request.theme,
                    &request.remote_policy,
                )
                .await
            {
                Ok(cached) if !cached.is_empty() => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: cached,
                        tile_height_px: request.tile_height_px,
                        no_html: false,
                        error: None,
                    });
                    log_debug(&format!(
                        "render_worker cache hit msg_id={} tiles={}",
                        message_id, request.tile_height_px
                    ));
                    continue;
                }
                Ok(_) => {}
                Err(err) => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        tile_height_px: request.tile_height_px,
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                    log_debug(&format!(
                        "render_worker cache error msg_id={} err={}",
                        message_id, err
                    ));
                    continue;
                }
            }

            let render_result = renderer
                .render(ratmail_render::RenderRequest {
                    message_id,
                    width_px: request.width_px,
                    tile_height_px: request.tile_height_px,
                    max_tiles: request.max_tiles,
                    theme: &request.theme,
                    remote_policy: if allow_remote {
                        RemotePolicy::Allowed
                    } else {
                        RemotePolicy::Blocked
                    },
                    prepared_html: &html,
                })
                .await;

            match render_result {
                Ok(result) => {
                    if result.tiles.is_empty() {
                        let _ = tx.send(RenderEvent {
                            message_id,
                            tiles: Vec::new(),
                            tile_height_px: request.tile_height_px,
                            no_html: false,
                            error: Some("Chromium produced no tiles. Try RATMAIL_CHROME_PATH=/usr/bin/chromium or RATMAIL_CHROME_NO_SANDBOX=1".to_string()),
                        });
                        log_debug(&format!("render_worker empty tiles msg_id={}", message_id));
                        continue;
                    }
                    let _ = store
                        .upsert_cache_tiles(
                            message_id,
                            request.width_px,
                            request.tile_height_px,
                            &request.theme,
                            &request.remote_policy,
                            &result.tiles,
                        )
                        .await;
                    let _ = store.prune_cache_tiles(TILE_CACHE_BUDGET_BYTES).await;
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: result.tiles,
                        tile_height_px: request.tile_height_px,
                        no_html: false,
                        error: None,
                    });
                    log_debug(&format!(
                        "render_worker rendered msg_id={} tile_h={}",
                        message_id, request.tile_height_px
                    ));
                }
                Err(err) => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        tile_height_px: request.tile_height_px,
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                    log_debug(&format!(
                        "render_worker render error msg_id={} err={}",
                        message_id, err
                    ));
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let rt = Arc::new(tokio::runtime::Runtime::new()?);
    let mut accounts = load_accounts_config();
    if accounts.is_empty() {
        accounts.push(AccountConfig {
            name: "demo".to_string(),
            db_path: "ratmail.db".to_string(),
            smtp: None,
            imap: None,
        });
    }
    let render_config = load_render_config();
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
                    store_handle.seed_demo_if_empty().await?;
                }
                let initial_folder_id = match store_handle.folder_id_by_name(1, "INBOX").await? {
                    Some(id) => id,
                    None => store_handle.first_folder_id(1).await?.unwrap_or(1),
                };
                let snapshot = store_handle.load_snapshot(1, initial_folder_id).await?;
                let (store_update_tx, mut store_update_rx) =
                    tokio::sync::mpsc::unbounded_channel::<StoreUpdate>();
                let (store_snapshot_tx, store_updates) =
                    tokio::sync::mpsc::unbounded_channel::<StoreSnapshot>();
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
                                let _ = store_snapshot_tx.send(snapshot);
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
        let (render_event_tx, render_event_rx) = tokio::sync::mpsc::unbounded_channel();
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
                    KeyCode::Char(c) if c.is_ascii_digit() => {
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
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };
            spans.push(Span::styled(format!(" {}:{} ", idx + 1, label), style));
        }
    }
    spans.push(Span::raw(format!(" acct: {} ", app.store.account.address)));
    spans.push(Span::raw(format!(" sync: {} ", app.sync_status)));
    spans.push(Span::styled(
        format!(" view: {} (v) ", view_label),
        Style::default().fg(Color::Yellow),
    ));
    spans.push(Span::raw(" [/] search "));
    if let Some(msg) = &app.status_message {
        spans.push(Span::raw(format!(" | {}", msg)));
    }
    if let Some(msg) = &app.imap_status {
        spans.push(Span::raw(format!(" | {}", msg)));
    }
    let line = Line::from(spans);
    let block = Block::default().borders(Borders::BOTTOM);
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_main(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.show_preview {
            vec![
                Constraint::Length(15),
                Constraint::Percentage(40),
                Constraint::Percentage(45),
            ]
        } else {
            vec![
                Constraint::Length(15),
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
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Yellow)
                }
            } else {
                Style::default()
            };
            Line::from(Span::styled(label, style))
        })
        .collect();

    let block = Block::default().borders(Borders::RIGHT).title("FOLDERS");
    let paragraph = Paragraph::new(items)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_message_list(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let header = Row::new(vec!["S", "Time", "From", "Subject"]).style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );

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
    let rows: Vec<Row> = visible[start..end]
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let global_idx = start + idx;
            let selected = app.selected_message_ids.contains(&message.id);
            let sel = if selected { "x" } else { " " };
            let unread = if message.unread { "*" } else { " " };
            let marker = format!("{}{}", sel, unread);
            let style = if global_idx == app.message_index {
                if app.focus == Focus::Messages {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Yellow)
                }
            } else {
                Style::default()
            };
            let from_display = format_from_display(&message.from);
            Row::new(vec![
                marker,
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
            Constraint::Length(16),
            Constraint::Length(18),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::RIGHT)
            .title("MESSAGE LIST"),
    )
    .column_spacing(1);

    frame.render_stateful_widget(table, area, &mut TableState::default());

    if visible.is_empty() {
        let msg = if let Some(status) = &app.imap_status {
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
        frame.render_widget(Paragraph::new(msg), hint_area);
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

fn link_style() -> Style {
    Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::UNDERLINED)
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

fn build_spans_with_ranges(line: &str, mut ranges: Vec<(usize, usize)>) -> Vec<Span<'_>> {
    if ranges.is_empty() {
        return vec![Span::raw(line)];
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
            spans.push(Span::raw(&line[last..start]));
        }
        spans.push(Span::styled(&line[start..end], link_style()));
        last = end;
    }
    if last < line.len() {
        spans.push(Span::raw(&line[last..]));
    }
    spans
}

fn text_with_link_style(text: &str) -> Text<'_> {
    let mut lines = Vec::new();
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url, LinkKind::Email]);

    for line in text.split('\n') {
        let mut ranges: Vec<(usize, usize)> = finder
            .links(line)
            .map(|link| (link.start(), link.end()))
            .collect();
        ranges.extend(find_reference_links(line));
        let spans = if line.is_empty() {
            vec![Span::raw("")]
        } else {
            build_spans_with_ranges(line, ranges)
        };
        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

fn render_message_view(frame: &mut ratatui::Frame, area: Rect, app: &mut App, scroll: u16) {
    let detail = app.selected_detail().cloned();
    let view_mode = app.view_mode;
    let render_supported = app.render_supported;
    let render_pending = app.render_pending;
    let render_tile_count = app.render_tile_count;
    let renderer_is_chromium = app.renderer_is_chromium;
    let render_error = app.render_error.clone();
    let render_no_html = app.render_no_html;
    let protocol_available = app.image_picker.is_some();

    if let Some(detail) = detail {
        let message_id = detail.id;
        let meta_block = Text::from(vec![
            Line::from(Span::styled(
                format!("Subject: {}", detail.subject),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("From: {}", detail.from)),
            Line::from(format!("Date: {}", detail.date)),
        ]);

        let content_text = match view_mode {
            ViewMode::Rendered => {
                if !render_supported {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode disabled."),
                        Line::from("Terminal image support not detected."),
                    ])
                } else if render_pending {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendering..."),
                        Line::from(""),
                        Line::from("Links: [l]  Attach: [a]"),
                    ])
                } else if let Some(err) = render_error {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode failed."),
                        Line::from(format!("Error: {}", err)),
                        Line::from("Try setting RATMAIL_CHROME_PATH=/usr/bin/chromium"),
                        Line::from("Or RATMAIL_CHROME_NO_SANDBOX=1"),
                    ])
                } else if render_no_html {
                    text_with_link_style(detail.body.as_str())
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
            ViewMode::Text => text_with_link_style(detail.body.as_str()),
        };

        let content_block = Paragraph::new(content_text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(8),
            ])
            .split(area);
        let separator = Block::default().borders(Borders::TOP);
        frame.render_widget(separator, chunks[1]);

        if view_mode == ViewMode::Rendered && render_supported && protocol_available {
            let content_area = chunks[2];
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
            frame.render_widget(content_block, chunks[2]);
        }
        frame.render_widget(Paragraph::new(meta_block), chunks[0]);
    } else {
        frame.render_widget(Paragraph::new("No message selected"), area);
    }
}

fn render_help_bar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut help = if app.show_help {
        String::from(
            "Tab/h/l focus  j/k move  Space select+next  Enter open/actions  v toggle view  p preview  o older  [ ] switch acct  ? help  Esc clear\n\
Ctrl+S/F5 send  r reply  R reply-all  f forward  m move  d delete  q quit",
        )
    } else {
        String::from(
            "Tab/h/l focus  j/k move  Space select+next  Enter open/actions  v view  p preview  o older  [ ] acct  ? help  Esc clear  q quit",
        )
    };
    if let Some(msg) = &app.status_message {
        help.push_str("  |  ");
        help.push_str(msg);
    }
    let block = Block::default().borders(Borders::TOP);
    frame.render_widget(Paragraph::new(help).block(block), area);
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

fn render_links_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    let title = app
        .selected_detail()
        .map(|detail| format!("Message: {}", detail.subject))
        .unwrap_or_else(|| "Message: (none)".to_string());
    lines.push(Line::from(title));
    lines.push(Line::from(""));

    if let Some(detail) = app.selected_detail() {
        if detail.links.is_empty() {
            lines.push(Line::from("  (no links found)"));
        } else {
            for (idx, link) in detail.links.iter().enumerate() {
                let style = if idx == app.link_index {
                    link_style().bg(Color::DarkGray)
                } else {
                    link_style()
                };
                lines.push(Line::from(Span::styled(
                    format!("  {:>2}  {}", idx + 1, link),
                    style,
                )));
            }
        }
    } else {
        lines.push(Line::from("  (no message selected)"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Enter open  y copy  o open ext  Esc close"));

    let block = Block::default().borders(Borders::ALL).title("LINKS");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_attach_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    let title = app
        .selected_detail()
        .map(|detail| format!("Message: {}", detail.subject))
        .unwrap_or_else(|| "Message: (none)".to_string());
    lines.push(Line::from(title));
    lines.push(Line::from(""));

    if let Some(detail) = app.selected_detail() {
        if detail.attachments.is_empty() {
            lines.push(Line::from("  (no attachments found)"));
        } else {
            for (idx, attachment) in detail.attachments.iter().enumerate() {
                let size = format_size(attachment.size);
                let marker = if idx == app.attach_index { ">" } else { " " };
                lines.push(Line::from(format!(
                    "{} {:>2}  {:<24} {:>7}  {}",
                    marker,
                    idx + 1,
                    attachment.filename,
                    size,
                    attachment.mime
                )));
            }
        }
    } else {
        lines.push(Line::from("  (no message selected)"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("j/k move  Enter open  s save  Esc close"));

    let block = Block::default().borders(Borders::ALL).title("ATTACHMENTS");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_bulk_action_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(50, 40, area);
    frame.render_widget(Clear, popup);

    let count = app.bulk_action_ids.len();
    let mut lines = Vec::new();
    lines.push(Line::from(format!(
        "Selected {} message{}",
        count,
        if count == 1 { "" } else { "s" }
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("m move"));
    lines.push(Line::from("d delete"));
    lines.push(Line::from("Esc close"));

    let block = Block::default().borders(Borders::ALL).title("BULK");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_bulk_move_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    lines.push(Line::from("Select folder to move to:"));
    lines.push(Line::from(""));

    for (idx, folder) in app.store.folders.iter().enumerate() {
        let style = if idx == app.bulk_folder_index {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };
        let label = App::display_folder_name(&folder.name);
        lines.push(Line::from(Span::styled(format!("  {}", label), style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Enter move  Esc back"));

    let block = Block::default().borders(Borders::ALL).title("MOVE");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_confirm_delete_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(50, 30, area);
    frame.render_widget(Clear, popup);

    let count = app.confirm_delete_ids.len();
    let mut lines = Vec::new();
    lines.push(Line::from(format!(
        "Delete {} message{}?",
        count,
        if count == 1 { "" } else { "s" }
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("y confirm"));
    lines.push(Line::from("n cancel"));
    lines.push(Line::from("Esc close"));

    let block = Block::default().borders(Borders::ALL).title("CONFIRM");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_picker_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let Some(mode) = &app.picker_mode else { return };
    let Some(picker) = &app.picker else { return };
    let popup = centered_rect(85, 85, area);
    frame.render_widget(Clear, popup);

    let title = match mode {
        PickerMode::Attach => "ATTACH FILE",
        PickerMode::Save { .. } => "SAVE ATTACHMENT",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(inner);

    let widget = picker.widget();
    frame.render_widget(widget, rows[0]);

    let mut lines = Vec::new();
    match mode {
        PickerMode::Attach => {
            lines.push(Line::from(
                "Enter attach  Right/L enter dir  Left/Back parent  Ctrl+H toggle hidden  Esc cancel",
            ));
        }
        PickerMode::Save { filename, .. } => {
            let dir = app
                .picker_selected_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "(unknown)".to_string());
            lines.push(Line::from(format!("Directory: {}", dir)));
            let label = if app.picker_focus == PickerFocus::Filename {
                "Filename*:"
            } else {
                "Filename :"
            };
            let display = if app.picker_filename.is_empty() {
                filename.clone()
            } else {
                app.picker_filename.clone()
            };
            lines.push(Line::from(format!("{} {}", label, display)));
            lines.push(Line::from(
                "Tab focus  Enter save  Right/L enter dir  Left/Back parent  Esc cancel",
            ));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), rows[1]);

    if matches!(mode, PickerMode::Save { .. }) && app.picker_focus == PickerFocus::Filename {
        let label_len = "Filename*: ".chars().count() as u16;
        let cursor_col = label_len.saturating_add(app.picker_cursor as u16);
        let x = rows[1].x + cursor_col.min(rows[1].width.saturating_sub(1));
        let y = rows[1].y + 1;
        frame.set_cursor_position((x, y));
    }
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", (bytes as f64 / (1024.0 * 1024.0)).round() as usize)
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.0).round() as usize)
    } else {
        format!("{} B", bytes)
    }
}

fn safe_filename(input: &str) -> String {
    Path::new(input)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_string()
}

fn zip_directory(dir: &Path) -> Result<Vec<u8>> {
    let base_parent = dir.parent().unwrap_or(dir);
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let root = dir
            .strip_prefix(base_parent)
            .unwrap_or(dir)
            .to_string_lossy()
            .replace('\\', "/");
        if !root.is_empty() {
            let name = if root.ends_with('/') {
                root
            } else {
                format!("{}/", root)
            };
            zip.add_directory(name, options)?;
        }
        add_dir_to_zip(&mut zip, base_parent, dir, options)?;
        zip.finish()?;
    }
    Ok(cursor.into_inner())
}

fn add_dir_to_zip(
    zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
    base_parent: &Path,
    dir: &Path,
    options: FileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base_parent)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            let name = if rel.ends_with('/') {
                rel
            } else {
                format!("{}/", rel)
            };
            if !name.is_empty() {
                zip.add_directory(name, options)?;
            }
            add_dir_to_zip(zip, base_parent, &path, options)?;
        } else {
            zip.start_file(rel, options)?;
            let data = std::fs::read(&path)?;
            zip.write_all(&data)?;
        }
    }
    Ok(())
}

fn render_compose_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);

    let outer = Block::default().borders(Borders::ALL).title("COMPOSE");
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(inner);

    let label_style = Style::default().fg(Color::Gray);
    let to_label = if app.compose_focus == ComposeFocus::To {
        Style::default().fg(Color::Yellow)
    } else {
        label_style
    };
    let subject_label = if app.compose_focus == ComposeFocus::Subject {
        Style::default().fg(Color::Yellow)
    } else {
        label_style
    };
    let cc_label = if app.compose_focus == ComposeFocus::Cc {
        Style::default().fg(Color::Yellow)
    } else {
        label_style
    };
    let bcc_label = if app.compose_focus == ComposeFocus::Bcc {
        Style::default().fg(Color::Yellow)
    } else {
        label_style
    };
    let to_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[0]);
    frame.render_widget(Paragraph::new("To:").style(to_label), to_layout[0]);
    frame.render_widget(Paragraph::new(app.compose_to.as_str()), to_layout[1]);

    let cc_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[1]);
    frame.render_widget(Paragraph::new("Cc:").style(cc_label), cc_layout[0]);
    frame.render_widget(Paragraph::new(app.compose_cc.as_str()), cc_layout[1]);

    let bcc_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[2]);
    frame.render_widget(Paragraph::new("Bcc:").style(bcc_label), bcc_layout[0]);
    frame.render_widget(Paragraph::new(app.compose_bcc.as_str()), bcc_layout[1]);

    let subject_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[3]);
    frame.render_widget(
        Paragraph::new("Subj:").style(subject_label),
        subject_layout[0],
    );
    frame.render_widget(
        Paragraph::new(app.compose_subject.as_str()),
        subject_layout[1],
    );

    let attachment_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[4]);
    let attachment_text = if app.compose_attachments.is_empty() {
        "(no attachments)".to_string()
    } else {
        app.compose_attachments
            .iter()
            .map(|a| format!("{} ({})", a.filename, format_size(a.size)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    frame.render_widget(
        Paragraph::new("Att:").style(label_style),
        attachment_layout[0],
    );
    frame.render_widget(Paragraph::new(attachment_text), attachment_layout[1]);

    let line = "─".repeat(rows[5].width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::DarkGray)),
        rows[5],
    );
    let body_display = if app.compose_quote.is_empty() {
        app.compose_body.clone()
    } else {
        let mut body = app.compose_body.clone();
        body.push_str(&app.compose_quote);
        body
    };
    frame.render_widget(Paragraph::new(body_display), rows[6]);

    let footer = if let Some(msg) = &app.status_message {
        format!(
            "Ctrl+S/F5 send   Ctrl+A attach   Ctrl+R remove last   Tab next   Shift+Tab prev   Ctrl+Q cancel   | {}",
            msg
        )
    } else {
        "Ctrl+S/F5 send   Ctrl+A attach   Ctrl+R remove last   Tab next   Shift+Tab prev   Ctrl+Q cancel"
            .to_string()
    };
    frame.render_widget(Paragraph::new(footer), rows[7]);

    match app.compose_focus {
        ComposeFocus::To => {
            set_cursor_at(frame, to_layout[1], &app.compose_to, app.compose_cursor_to)
        }
        ComposeFocus::Cc => {
            set_cursor_at(frame, cc_layout[1], &app.compose_cc, app.compose_cursor_cc)
        }
        ComposeFocus::Bcc => set_cursor_at(
            frame,
            bcc_layout[1],
            &app.compose_bcc,
            app.compose_cursor_bcc,
        ),
        ComposeFocus::Subject => set_cursor_at(
            frame,
            subject_layout[1],
            &app.compose_subject,
            app.compose_cursor_subject,
        ),
        ComposeFocus::Body => {
            set_cursor_at(frame, rows[6], &app.compose_body, app.compose_cursor_body)
        }
    }
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

fn render_view_focus(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);
    let outer = Block::default().borders(Borders::ALL).title("MESSAGE");
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

fn load_render_config() -> RenderConfig {
    let content = match load_config_text() {
        Some(content) => content,
        None => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
                render_scale: 1.0,
                tile_height_px_side: 5000,
                tile_height_px_focus: 120,
            };
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
                render_scale: 1.0,
                tile_height_px_side: 5000,
                tile_height_px_focus: 120,
            };
        }
    };
    let render = match value.get("render") {
        Some(render) => render,
        None => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
                render_scale: 1.0,
                tile_height_px_side: 5000,
                tile_height_px_focus: 120,
            };
        }
    };
    let allow_remote_images = match render.get("remote_images") {
        Some(v) => v
            .as_bool()
            .or_else(|| {
                v.as_str()
                    .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
            })
            .unwrap_or(false),
        None => false,
    };
    let width_px = match render.get("width_px") {
        Some(v) => v.as_integer().unwrap_or(1000) as i64,
        None => 1000,
    };
    let render_scale = render
        .get("render_scale")
        .and_then(|v| v.as_float())
        .unwrap_or(1.0)
        .clamp(0.25, 4.0);
    let tile_height_px_side = match render.get("tile_height_px_side") {
        Some(v) => v.as_integer().unwrap_or(5000) as i64,
        None => 5000,
    };
    let tile_height_px_focus = match render.get("tile_height_px_focus") {
        Some(v) => v.as_integer().unwrap_or(120) as i64,
        None => 120,
    };
    RenderConfig {
        allow_remote_images,
        width_px,
        render_scale,
        tile_height_px_side,
        tile_height_px_focus,
    }
}

fn build_reply(
    detail: Option<&MessageDetail>,
    raw: Option<&[u8]>,
    account_addr: &str,
    reply_all: bool,
) -> (String, String, String, String) {
    let Some(detail) = detail else {
        return (
            String::new(),
            String::new(),
            "Re:".to_string(),
            String::new(),
        );
    };
    let from_email = extract_email(&detail.from);
    let subject = if detail.subject.to_lowercase().starts_with("re:") {
        detail.subject.clone()
    } else {
        format!("Re: {}", detail.subject)
    };
    let mut body = String::new();
    body.push_str("\n\n");
    body.push_str(&format!("> On {}, {} wrote:\n", detail.date, detail.from));
    for line in detail.body.lines() {
        body.push_str("> ");
        body.push_str(line);
        body.push('\n');
    }

    let mut cc = String::new();
    if reply_all {
        if let Some(raw) = raw {
            if let Ok(parsed) = mailparse::parse_mail(raw) {
                let mut addrs = Vec::new();
                addrs.extend(extract_header_addresses(&parsed, "To"));
                addrs.extend(extract_header_addresses(&parsed, "Cc"));
                let mut seen = HashSet::new();
                let account = account_addr.trim().to_lowercase();
                let sender = from_email.trim().to_lowercase();
                let mut filtered = Vec::new();
                for addr in addrs {
                    let normalized = addr.to_lowercase();
                    if normalized.is_empty()
                        || normalized == account
                        || normalized == sender
                        || !seen.insert(normalized)
                    {
                        continue;
                    }
                    filtered.push(addr);
                }
                cc = filtered.join(", ");
            }
        }
    }

    (from_email, cc, subject, body)
}

fn build_forward(detail: Option<&MessageDetail>, raw: Option<&[u8]>) -> (String, String) {
    let Some(detail) = detail else {
        return ("Fwd:".to_string(), String::new());
    };
    let subject = if detail.subject.to_lowercase().starts_with("fwd:") {
        detail.subject.clone()
    } else {
        format!("Fwd: {}", detail.subject)
    };
    let mut original_to = String::new();
    if let Some(raw) = raw {
        if let Ok(parsed) = mailparse::parse_mail(raw) {
            let to_addrs = extract_header_addresses(&parsed, "To");
            if !to_addrs.is_empty() {
                original_to = to_addrs.join(", ");
            }
        }
    }
    let mut body = String::new();
    body.push_str("\n\n---------- Forwarded message ---------\n");
    body.push_str(&format!("From: {}\n", detail.from));
    if !original_to.is_empty() {
        body.push_str(&format!("To: {}\n", original_to));
    }
    body.push_str(&format!("Date: {}\n", detail.date));
    body.push_str(&format!("Subject: {}\n\n", detail.subject));
    body.push_str(&detail.body);
    (subject, body)
}

fn extract_header_addresses(parsed: &mailparse::ParsedMail, name: &str) -> Vec<String> {
    let Some(header) = parsed.headers.get_first_header(name) else {
        return Vec::new();
    };
    match addrparse_header(&header) {
        Ok(list) => mailaddrs_to_emails(&list),
        Err(_) => Vec::new(),
    }
}

fn mailaddrs_to_emails(addrs: &[MailAddr]) -> Vec<String> {
    let mut out = Vec::new();
    for addr in addrs {
        match addr {
            MailAddr::Single(info) => {
                let email = info.addr.trim();
                if !email.is_empty() {
                    out.push(email.to_string());
                }
            }
            MailAddr::Group(group) => {
                for info in &group.addrs {
                    let email = info.addr.trim();
                    if !email.is_empty() {
                        out.push(email.to_string());
                    }
                }
            }
        }
    }
    out
}

fn extract_email(input: &str) -> String {
    let trimmed = input.trim();
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        return trimmed[start + 1..end].trim().to_string();
    }
    trimmed.to_string()
}
