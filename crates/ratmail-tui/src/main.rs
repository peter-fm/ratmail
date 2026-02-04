use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap},
    Terminal,
};

use ratmail_core::{
    Folder, MailStore, MessageDetail, MessageSummary, SqliteMailStore, StoreSnapshot, TileMeta,
    DEFAULT_TEXT_WIDTH,
};
use ratmail_content::{extract_attachments, extract_display, prepare_html};
use ratmail_mail::{ImapConfig, MailCommand, MailEngine, MailEvent, SmtpConfig};
use ratmail_render::{detect_image_support, ChromiumRenderer, NullRenderer, RemotePolicy, Renderer};
use std::sync::Arc;
use ratatui_image::{picker::Picker, protocol::ImageSource, protocol::Protocol, protocol::StatefulProtocol, Image, Resize, StatefulImage};

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
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Some(base.join("ratmail").join("ratmail.log"))
}

fn log_debug(msg: &str) {
    let Some(path) = log_path() else { return };
    let lock = LOG_FILE.get_or_init(|| {
        let _ = std::fs::create_dir_all(
            path.parent().unwrap_or_else(|| std::path::Path::new("/tmp")),
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
    Folders { account_id: i64, folders: Vec<Folder> },
    Messages {
        account_id: i64,
        folder_name: String,
        messages: Vec<MessageSummary>,
    },
    RawBody { account_id: i64, message_id: i64, raw: Vec<u8>, cached_text: Option<String> },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Folders,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeFocus {
    To,
    Subject,
    Body,
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
    runtime: Option<tokio::runtime::Runtime>,
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
    link_index: usize,
    status_message: Option<String>,
    imap_enabled: bool,
    last_folder_sync: Option<(String, Instant)>,
    imap_pending: usize,
    imap_spinner: usize,
    imap_status: Option<String>,
    compose_to: String,
    compose_subject: String,
    compose_body: String,
    compose_quote: String,
    compose_focus: ComposeFocus,
    compose_cursor_to: usize,
    compose_cursor_subject: usize,
    compose_cursor_body: usize,
    compose_body_desired_col: Option<usize>,
}

impl App {
    fn new(
        store: StoreSnapshot,
        store_handle: SqliteMailStore,
        engine: MailEngine,
        events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
        runtime: tokio::runtime::Runtime,
        render_supported: bool,
        image_picker: Option<Picker>,
        renderer_is_chromium: bool,
        render_request_tx: tokio::sync::watch::Sender<RenderRequest>,
        render_events: tokio::sync::mpsc::UnboundedReceiver<RenderEvent>,
        store_update_tx: tokio::sync::mpsc::UnboundedSender<StoreUpdate>,
        store_updates: tokio::sync::mpsc::UnboundedReceiver<StoreSnapshot>,
        allow_remote_images: bool,
        render_width_px: i64,
        imap_enabled: bool,
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
            runtime: Some(runtime),
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
            render_tile_height_px: 1200,
            render_tile_height_px_focus: 120,
            render_tile_height_px_side: 1200,
            link_index: 0,
            status_message: None,
            imap_enabled,
            last_folder_sync: None,
            imap_pending: 0,
            imap_spinner: 0,
            imap_status: None,
            compose_to: String::new(),
            compose_subject: String::new(),
            compose_body: String::new(),
            compose_quote: String::new(),
            compose_focus: ComposeFocus::Body,
            compose_cursor_to: 0,
            compose_cursor_subject: 0,
            compose_cursor_body: 0,
            compose_body_desired_col: None,
        };
        app.select_inbox_if_available();
        if app.imap_enabled {
            let _ = app.engine.send(MailCommand::SyncAll);
            app.imap_pending = app.imap_pending.saturating_add(2);
            app.imap_status = Some("IMAP syncing...".to_string());
        }
        app
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("runtime unavailable during app lifetime")
    }

    fn shutdown(&mut self) {
        if let Some(rt) = self.runtime.take() {
            // Avoid hanging on long-running background tasks (e.g. IMAP sync).
            rt.shutdown_timeout(Duration::from_millis(200));
        }
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
        let mut messages: Vec<&MessageSummary> = self.store
            .messages
            .iter()
            .filter(|msg| Some(msg.folder_id) == folder_id)
            .collect();
        messages.sort_by_key(|m| m.imap_uid.unwrap_or(0));
        messages.reverse();
        messages
    }

    fn request_sync_selected_folder(&mut self) {
        if !self.imap_enabled {
            return;
        }
        let Some(folder) = self.selected_folder() else { return };
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
        let _ = self.engine.send(MailCommand::SyncFolderByName {
            name: folder_name,
        });
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::List | Mode::View => self.on_key_main(key),
            Mode::ViewFocus => self.on_key_focus(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::OverlayLinks | Mode::OverlayAttach => self.on_key_overlay(key),
        }
    }

    fn start_compose_new(&mut self) {
        self.compose_to.clear();
        self.compose_subject.clear();
        self.compose_body.clear();
        self.compose_quote.clear();
        self.compose_focus = ComposeFocus::To;
        self.compose_cursor_to = 0;
        self.compose_cursor_subject = 0;
        self.compose_cursor_body = 0;
        self.compose_body_desired_col = None;
        self.mode = Mode::Compose;
    }

    fn start_compose_reply(&mut self) {
        let (to, subject, quote) = build_reply(self.selected_detail());
        self.compose_to = to;
        self.compose_subject = subject;
        self.compose_body.clear();
        self.compose_quote = quote;
        self.compose_focus = ComposeFocus::Body;
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_cursor_body = 0;
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
                self.render_tile_height_px =
                    ((self.render_tile_height_px + fh - 1) / fh) * fh;
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
        let remote_policy = if self.allow_remote_images { "allowed" } else { "blocked" };

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
        let remote_policy = if self.allow_remote_images { "allowed" } else { "blocked" };
        self.render_request_id += 1;
        self.render_pending = true;
        log_debug(&format!(
            "render_enqueue id={} msg_id={} mode={:?} tile_h={} max_tiles={:?}",
            self.render_request_id,
            message_id,
            self.mode,
            self.render_tile_height_px,
            if self.mode == Mode::ViewFocus { None } else { Some(1) }
        ));
        let _ = self.render_request_tx.send(RenderRequest {
            request_id: self.render_request_id,
            message_ids: vec![message_id],
            width_px: self.render_width_px,
            tile_height_px: self.render_tile_height_px,
            max_tiles: if self.mode == Mode::ViewFocus { None } else { Some(1) },
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

    fn take_cached_protocol(&mut self, message_id: i64, tile_index: usize) -> Option<StatefulProtocol> {
        let key = (message_id, tile_index);
        let protocol = self.protocol_cache.remove(&key);
        if protocol.is_some() {
            self.touch_protocol_cache(key);
        }
        protocol
    }

    fn store_protocol_cache(&mut self, message_id: i64, tile_index: usize, protocol: StatefulProtocol) {
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
        let Some(message) = self.selected_message() else { return };
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
            store_handle.get_raw_body(message_id).await.ok().flatten().is_some()
        });
        if has_raw {
            return true;
        }
        if self.imap_enabled && !self.pending_body_fetch.contains(&message_id) {
            let folder_name = self.selected_folder().map(|f| f.name.clone());
            if let (Some(uid), Some(folder_name)) =
                (self.selected_message().and_then(|m| m.imap_uid), folder_name)
            {
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
        let Some(folder_name) = folder_name else { return };
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
                store_handle.get_raw_body(message_id).await.ok().flatten().is_some()
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
        let Some(message) = self.selected_message() else { return };
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
                    self.compose_subject.clear();
                    self.compose_body.clear();
                    self.compose_quote.clear();
                    self.compose_focus = ComposeFocus::Body;
                    self.compose_cursor_to = 0;
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
            MailEvent::ImapMessages { folder_name, messages } => {
                self.imap_pending = self.imap_pending.saturating_sub(1);
                if self.imap_pending == 0 {
                    self.imap_status = None;
                }
                let account_id = self.store.account.id;
                self.imap_status = Some(format!("IMAP: {} messages in {}", messages.len(), folder_name));
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
                let _ = self.store_update_tx.send(StoreUpdate::Messages {
                    account_id,
                    folder_name,
                    messages: items,
                });
                self.prefetch_raw_bodies(10);
            }
            MailEvent::ImapBody { message_id, raw } => {
                let account_id = self.store.account.id;
                self.pending_body_fetch.remove(&message_id);
                let cached_text = if let Ok(display) = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize) {
                    if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                        detail.body = display.text.clone();
                        detail.links = display.links.clone();
                    } else if let Some(summary) = self.store.messages.iter().find(|m| m.id == message_id) {
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
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                match self.focus {
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
                        }
                    }
                    Focus::Folders => {
                        if self.folder_index + 1 < self.store.folders.len() {
                            self.folder_index += 1;
                            self.message_index = 0;
                            self.request_sync_selected_folder();
                            if self.view_mode == ViewMode::Rendered {
                                self.schedule_render();
                            }
                        }
                    }
                }
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                match self.focus {
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
                            self.request_sync_selected_folder();
                            if self.view_mode == ViewMode::Rendered {
                                self.schedule_render();
                            }
                        }
                    }
                }
            }
            (KeyCode::Enter, _) => {
                if self.focus == Focus::Messages {
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
            (KeyCode::Esc, _) => {
                if self.mode == Mode::ViewFocus {
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
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.start_compose_reply();
            }
            (KeyCode::Char('R'), _) => {
                self.start_compose_reply();
            }
            (KeyCode::Char('f'), _) => {
                self.start_compose_reply();
            }
            (KeyCode::Char('c'), _) => {
                self.start_compose_new();
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
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.start_compose_reply();
            }
            (KeyCode::Char('R'), _) => {
                self.start_compose_reply();
            }
            (KeyCode::Char('f'), _) => {
                self.start_compose_reply();
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
                            let _ = open::that(link);
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
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn on_key_compose(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::F(5))
            || (ctrl
                && matches!(
                    key.code,
                    KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('\u{13}')
                ))
        {
                let to = self.compose_to.clone();
                let subject = self.compose_subject.clone();
                let mut body = self.compose_body.clone();
                if !self.compose_quote.is_empty() {
                    body.push_str(&self.compose_quote);
                }
                if to.trim().is_empty() {
                    self.status_message = Some("No recipient".to_string());
                } else {
                    self.status_message = Some("Sending...".to_string());
                    let _ = self.engine.send(MailCommand::SendMessage {
                        to,
                        subject,
                        body,
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
                self.compose_focus = match self.compose_focus {
                    ComposeFocus::To => ComposeFocus::Body,
                    ComposeFocus::Subject => ComposeFocus::To,
                    ComposeFocus::Body => ComposeFocus::Subject,
                };
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
                    self.compose_focus = match self.compose_focus {
                        ComposeFocus::To => ComposeFocus::Subject,
                        ComposeFocus::Subject => ComposeFocus::Body,
                        ComposeFocus::Body => ComposeFocus::To,
                    };
                    self.compose_body_desired_col = None;
                }
            }
            (KeyCode::Left, _) => match self.compose_focus {
                ComposeFocus::To => {
                    move_cursor_left(&self.compose_to, &mut self.compose_cursor_to);
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
                    ComposeFocus::To => self.compose_focus = ComposeFocus::Subject,
                    ComposeFocus::Subject => self.compose_focus = ComposeFocus::Body,
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
            _ => {
                match self.compose_focus {
                    ComposeFocus::To => {
                        apply_compose_key(
                            &mut self.compose_to,
                            &mut self.compose_cursor_to,
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
                }
            }
        }
        false
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
                                .upsert_cache_html(message_id, &request.remote_policy, &prepared.html)
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
                log_debug(&format!(
                    "render_worker no_html msg_id={}",
                    message_id
                ));
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
                        log_debug(&format!(
                            "render_worker empty tiles msg_id={}",
                            message_id
                        ));
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
    let rt = tokio::runtime::Runtime::new()?;
    let smtp = load_smtp_config();
    let imap = load_imap_config();
    let render_config = load_render_config();
    let allow_remote_images = render_config.allow_remote_images
        || std::env::var("RATMAIL_REMOTE_IMAGES")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let render_width_px = std::env::var("RATMAIL_RENDER_WIDTH_PX")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(render_config.width_px);
    let (engine, events, store_handle, store, store_update_tx, store_updates) = rt.block_on(async {
        let (engine, events) = MailEngine::start(smtp, imap.clone());
        let store_handle = SqliteMailStore::connect("ratmail.db").await?;
        store_handle.init().await?;
        if let Some(imap) = &imap {
            store_handle
                .upsert_account(1, &imap.username, &imap.username)
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
                    StoreUpdate::Folders { account_id, folders } => {
                            store_for_task.upsert_folders(account_id, &folders).await?;
                            let folder_id = match store_for_task
                                .folder_id_by_name(account_id, "INBOX")
                                .await?
                            {
                                Some(id) => id,
                                None => store_for_task.first_folder_id(account_id).await?.unwrap_or(1),
                            };
                            store_for_task.load_snapshot(account_id, folder_id).await
                        }
                    StoreUpdate::Messages {
                        account_id,
                        folder_name,
                        messages,
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
                            store_for_task.upsert_folders(account_id, &[fallback]).await?;
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
                                .replace_folder_messages(account_id, folder_id, &items)
                                .await?;
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
                                None => store_for_task.first_folder_id(account_id).await?.unwrap_or(1),
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
                        // Use cached_text if available, otherwise extract from raw
                        let text = cached_text.or_else(|| {
                            extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)
                                .ok()
                                .map(|d| d.text)
                        });
                        if let Some(text) = text {
                            let _ = store_for_task
                                .upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, &text)
                                .await;
                        }
                        let folder_id = match store_for_task
                            .folder_id_by_name(account_id, "INBOX")
                            .await?
                        {
                            Some(id) => id,
                            None => store_for_task.first_folder_id(account_id).await?.unwrap_or(1),
                        };
                        store_for_task.load_snapshot(account_id, folder_id).await
                    }
                }
                })()
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
            Ok(value) if value.to_lowercase() == "null" => (Arc::new(NullRenderer::default()), false),
            Ok(value) if value.to_lowercase() == "chromium" => {
                (Arc::new(ChromiumRenderer::default()), true)
            }
            Ok(_) => (Arc::new(ChromiumRenderer::default()), true),
            Err(_) => (Arc::new(ChromiumRenderer::default()), true),
        };

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

    let res = run_app(
        &mut terminal,
        App::new(
            store,
            store_handle,
            engine,
            events,
            rt,
            render_supported,
            picker,
            renderer_is_chromium,
            render_request_tx,
            render_event_rx,
            store_update_tx,
            store_updates,
            allow_remote_images,
            render_width_px,
            imap.is_some(),
        ),
    );

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        while let Ok(event) = app.events.try_recv() {
            app.on_event(event);
        }
        while let Ok(snapshot) = app.store_updates.try_recv() {
            app.store = snapshot;
            app.select_inbox_if_available();
        }
        while let Ok(event) = app.render_events.try_recv() {
            app.on_render_event(event);
        }

        let timeout = TICK_RATE.saturating_sub(app.last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if app.on_key(key) {
                    app.shutdown();
                    return Ok(());
                }
            }
        }

        if app.last_tick.elapsed() >= TICK_RATE {
            app.last_tick = Instant::now();
            app.on_tick();
        }
    }
}

fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);

    render_status_bar(frame, layout[0], app);
    render_main(frame, layout[1], app);
    render_help_bar(frame, layout[2], app);

    match app.mode {
        Mode::OverlayLinks => render_links_overlay(frame, area, app),
        Mode::OverlayAttach => render_attach_overlay(frame, area, app),
        Mode::Compose => render_compose_overlay(frame, area, app),
        Mode::ViewFocus => render_view_focus(frame, area, app),
        _ => {}
    }
}

fn render_status_bar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let view_label = match app.view_mode {
        ViewMode::Text => "TEXT",
        ViewMode::Rendered => "RENDERED",
    };

    let mut spans = vec![
        Span::styled(
            format!(" acct: {} ", app.store.account.address),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" INBOX (2,184) "),
        Span::raw(format!(" sync: {} ", app.sync_status)),
        Span::styled(
            format!(" view: {} (v) ", view_label),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" [/] search "),
    ];
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
        .constraints([
            Constraint::Length(15),
            Constraint::Percentage(40),
            Constraint::Percentage(45),
        ])
        .split(area);

    render_folders(frame, columns[0], app);
    render_message_list(frame, columns[1], app);
    render_message_view(frame, columns[2], app, 0);
}

fn render_folders(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let items: Vec<Line> = app
        .store
        .folders
        .iter()
        .enumerate()
        .map(|(idx, folder)| {
            let label = if folder.unread > 0 {
                format!("{}  {}", folder.name, folder.unread)
            } else {
                folder.name.clone()
            };
            let style = if idx == app.folder_index {
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
    let paragraph = Paragraph::new(items).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_message_list(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let header = Row::new(vec![" ", "Time", "From", "Subject"]).style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );

    let visible = app.visible_messages();
    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let unread = if message.unread { "*" } else { " " };
            let style = if idx == app.message_index {
                if app.focus == Focus::Messages {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Yellow)
                }
            } else {
                Style::default()
            };
            Row::new(vec![unread, message.date.as_str(), &message.from, &message.subject])
                .style(style)
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Length(2),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Min(10),
    ])
    .header(header)
    .block(Block::default().borders(Borders::RIGHT).title("MESSAGE LIST"))
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
                    Text::from(detail.body.as_str())
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
            ViewMode::Text => Text::from(detail.body.as_str()),
        };

        let content_block = Paragraph::new(content_text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Length(1), Constraint::Min(8)])
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
                let tile_rows = (app.render_tile_height_px / fh as i64)
                    .max(1) as u16;

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
                                    let source = ImageSource::new(
                                        img.clone(),
                                        picker.font_size(),
                                        image::Rgba([0, 0, 0, 0]),
                                    );
                                    app.tile_rows_cache.insert((message_id, idx), source.desired.height);
                                    if let Ok(protocol) =
                                        picker.new_protocol(img, rect, Resize::Crop(None))
                                    {
                                        app.protocol_cache_static.insert((message_id, idx), protocol);
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
                    } else if let Some(bytes) = app.render_tiles.get(desired_idx).map(|t| t.bytes.clone()) {
                        if let Ok(img) = image::load_from_memory(&bytes) {
                            if let Some(picker) = app.image_picker.as_mut() {
                                let protocol = picker.new_resize_protocol(img);
                                app.render_tile_index = desired_idx;
                                app.store_protocol_cache(message_id, desired_idx, protocol);
                                app.image_protocol = app.take_cached_protocol(message_id, desired_idx);
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
    let mut help = String::from(
        "Tab/h/l focus  j/k move  Enter open  v toggle view  Ctrl+S/F5 send  r reply  R reply-all  f forward  m move  d delete  q quit",
    );
    if let Some(msg) = &app.status_message {
        help.push_str("  |  ");
        help.push_str(msg);
    }
    if let Some(msg) = &app.imap_status {
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
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
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
    let paragraph = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
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
                lines.push(Line::from(format!(
                    "  {:>2}  {:<24} {:>7}  {}",
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
    lines.push(Line::from("Enter open  s save  y copy name  Esc close"));

    let block = Block::default().borders(Borders::ALL).title("ATTACHMENTS");
    let paragraph = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
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
    frame.render_widget(Paragraph::new("Cc:").style(label_style), cc_layout[0]);

    let subject_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[2]);
    frame.render_widget(Paragraph::new("Subj:").style(subject_label), subject_layout[0]);
    frame.render_widget(Paragraph::new(app.compose_subject.as_str()), subject_layout[1]);

    let line = "─".repeat(rows[3].width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
    let body_display = if app.compose_quote.is_empty() {
        app.compose_body.clone()
    } else {
        let mut body = app.compose_body.clone();
        body.push_str(&app.compose_quote);
        body
    };
    frame.render_widget(Paragraph::new(body_display), rows[4]);

    let footer = if let Some(msg) = &app.status_message {
        format!(
            "Ctrl+S/F5 send   Tab next field   Shift+Tab prev field   Ctrl+Q cancel   | {}",
            msg
        )
    } else {
        "Ctrl+S/F5 send   Tab next field   Shift+Tab prev field   Ctrl+Q cancel".to_string()
    };
    frame.render_widget(Paragraph::new(footer), rows[5]);

    match app.compose_focus {
        ComposeFocus::To => set_cursor_at(
            frame,
            to_layout[1],
            &app.compose_to,
            app.compose_cursor_to,
        ),
        ComposeFocus::Subject => set_cursor_at(
            frame,
            subject_layout[1],
            &app.compose_subject,
            app.compose_cursor_subject,
        ),
        ComposeFocus::Body => set_cursor_at(
            frame,
            rows[4],
            &app.compose_body,
            app.compose_cursor_body,
        ),
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

fn load_smtp_config() -> Option<SmtpConfig> {
    let path = "ratmail.toml";
    let content = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    let smtp = value.get("smtp")?;
    Some(SmtpConfig {
        host: smtp.get("host")?.as_str()?.to_string(),
        port: smtp.get("port").and_then(|v| v.as_integer()).unwrap_or(587) as u16,
        username: smtp.get("username")?.as_str()?.to_string(),
        password: smtp.get("password")?.as_str()?.to_string(),
        from: smtp.get("from")?.as_str()?.to_string(),
    })
}

fn load_imap_config() -> Option<ImapConfig> {
    let path = "ratmail.toml";
    let content = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    let imap = value.get("imap")?;
    Some(ImapConfig {
        host: imap.get("host")?.as_str()?.to_string(),
        port: imap.get("port").and_then(|v| v.as_integer()).unwrap_or(993) as u16,
        username: imap.get("username")?.as_str()?.to_string(),
        password: imap.get("password")?.as_str()?.to_string(),
    })
}

struct RenderConfig {
    allow_remote_images: bool,
    width_px: i64,
}

fn load_render_config() -> RenderConfig {
    let path = "ratmail.toml";
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
            }
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
            }
        }
    };
    let render = match value.get("render") {
        Some(render) => render,
        None => {
            return RenderConfig {
                allow_remote_images: false,
                width_px: 1000,
            }
        }
    };
    let allow_remote_images = match render.get("remote_images") {
        Some(v) => v
            .as_bool()
            .or_else(|| v.as_str().map(|s| s == "1" || s.eq_ignore_ascii_case("true")))
            .unwrap_or(false),
        None => false,
    };
    let width_px = match render.get("width_px") {
        Some(v) => v.as_integer().unwrap_or(1000) as i64,
        None => 1000,
    };
    RenderConfig {
        allow_remote_images,
        width_px,
    }
}

fn build_reply(detail: Option<&MessageDetail>) -> (String, String, String) {
    let Some(detail) = detail else {
        return (String::new(), "Re:".to_string(), String::new());
    };
    let to = extract_email(&detail.from);
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
    (to, subject, body)
}

fn extract_email(input: &str) -> String {
    let trimmed = input.trim();
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        return trimmed[start + 1..end].trim().to_string();
    }
    trimmed.to_string()
}
