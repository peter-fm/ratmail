use std::collections::{HashMap, VecDeque};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

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
    MailStore, MessageDetail, MessageSummary, SqliteMailStore, StoreSnapshot, TileMeta,
    DEFAULT_TEXT_WIDTH,
};
use ratmail_content::{extract_attachments, extract_display, prepare_html};
use ratmail_mail::{MailCommand, MailEngine, MailEvent};
use ratmail_render::{detect_image_support, ChromiumRenderer, NullRenderer, RemotePolicy, Renderer};
use std::sync::Arc;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, StatefulImage};

const TICK_RATE: Duration = Duration::from_millis(200);
const RENDER_DEBOUNCE: Duration = Duration::from_millis(120);
const RENDER_PREFETCH_NEXT: usize = 2;
const RENDER_PREFETCH_PREV: usize = 1;
const TILE_CACHE_BUDGET_BYTES: i64 = 256 * 1024 * 1024;
const RENDER_MEM_CACHE_LIMIT: usize = 32;
const PROTOCOL_CACHE_LIMIT: usize = 32;

#[derive(Debug, Clone)]
struct RenderRequest {
    request_id: u64,
    message_ids: Vec<i64>,
    width_px: i64,
    theme: String,
    remote_policy: String,
}

#[derive(Debug, Clone)]
struct RenderEvent {
    message_id: i64,
    tiles: Vec<TileMeta>,
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
enum ViewMode {
    Text,
    Rendered,
}

struct App {
    mode: Mode,
    view_mode: ViewMode,
    store: StoreSnapshot,
    message_index: usize,
    last_tick: Instant,
    sync_status: String,
    engine: MailEngine,
    events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
    store_handle: SqliteMailStore,
    runtime: tokio::runtime::Runtime,
    overlay_return: Mode,
    view_scroll: u16,
    render_supported: bool,
    render_tile_count: usize,
    render_tiles: Vec<TileMeta>,
    render_tile_index: usize,
    image_picker: Option<Picker>,
    image_protocol: Option<StatefulProtocol>,
    renderer_is_chromium: bool,
    render_error: Option<String>,
    render_no_html: bool,
    render_pending: bool,
    render_request_tx: tokio::sync::watch::Sender<RenderRequest>,
    render_events: tokio::sync::mpsc::UnboundedReceiver<RenderEvent>,
    pending_render: Option<Instant>,
    render_request_id: u64,
    render_cache: HashMap<i64, Vec<TileMeta>>,
    render_cache_lru: VecDeque<i64>,
    protocol_cache: HashMap<(i64, usize), StatefulProtocol>,
    protocol_cache_lru: VecDeque<(i64, usize)>,
    last_render_area: Option<(u16, u16)>,
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
    ) -> Self {
        let mut app = Self {
            mode: Mode::List,
            view_mode: ViewMode::Rendered,
            store,
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
            image_picker,
            image_protocol: None,
            renderer_is_chromium,
            render_error: None,
            render_no_html: false,
            render_pending: false,
            render_request_tx,
            render_events,
            pending_render: None,
            render_request_id: 0,
            render_cache: HashMap::new(),
            render_cache_lru: VecDeque::new(),
            protocol_cache: HashMap::new(),
            protocol_cache_lru: VecDeque::new(),
            last_render_area: None,
        };
        if app.view_mode == ViewMode::Rendered {
            app.schedule_render();
        }
        app
    }

    fn selected_message(&self) -> Option<&MessageSummary> {
        self.store.messages.get(self.message_index)
    }

    fn selected_detail(&self) -> Option<&MessageDetail> {
        let message_id = self.selected_message()?.id;
        self.store.message_details.get(&message_id)
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::List | Mode::View | Mode::ViewFocus => self.on_key_main(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::OverlayLinks | Mode::OverlayAttach => self.on_key_overlay(key),
        }
    }

    fn on_tick(&mut self) {
        if let Some(started) = self.pending_render {
            if started.elapsed() >= RENDER_DEBOUNCE {
                self.pending_render = None;
                self.enqueue_render_plan();
            }
        }
    }

    fn schedule_render(&mut self) {
        if !self.render_supported {
            return;
        }
        self.render_error = None;
        self.render_no_html = false;

        if self.try_load_cached_tiles_memory() {
            self.render_pending = false;
            self.pending_render = None;
            return;
        }

        if self.try_load_cached_tiles_db() {
            self.render_pending = false;
            self.pending_render = None;
            return;
        }

        self.render_pending = true;
        self.pending_render = Some(Instant::now());
    }

    fn enqueue_render_plan(&mut self) {
        if self.view_mode != ViewMode::Rendered {
            return;
        }
        let Some(message) = self.selected_message() else { return };
        let mut ids = Vec::new();
        ids.push(message.id);

        for i in 1..=RENDER_PREFETCH_NEXT {
            if let Some(m) = self.store.messages.get(self.message_index + i) {
                ids.push(m.id);
            }
        }
        for i in 1..=RENDER_PREFETCH_PREV {
            if let Some(m) = self
                .message_index
                .checked_sub(i)
                .and_then(|idx| self.store.messages.get(idx))
            {
                ids.push(m.id);
            }
        }

        self.render_request_id += 1;
        let _ = self.render_request_tx.send(RenderRequest {
            request_id: self.render_request_id,
            message_ids: ids,
            width_px: 800,
            theme: "default".to_string(),
            remote_policy: "blocked".to_string(),
        });
    }

    fn try_load_cached_tiles_memory(&mut self) -> bool {
        let Some(message) = self.selected_message() else { return false };
        let message_id = message.id;
        let Some(tiles) = self.render_cache.get(&message_id) else { return false };
        let tiles = tiles.clone();
        self.apply_render_tiles(message_id, tiles);
        self.touch_render_cache(message_id);
        true
    }

    fn try_load_cached_tiles_db(&mut self) -> bool {
        let Some(message) = self.selected_message() else { return false };
        let store_handle = self.store_handle.clone();
        let message_id = message.id;
        let width_px = 800i64;
        let theme = "default";
        let remote_policy = "blocked";

        let cached = self.runtime.block_on(async move {
            store_handle
                .get_cache_tiles(message_id, width_px, theme, remote_policy)
                .await
        });

        let Ok(tiles) = cached else { return false };
        if tiles.is_empty() {
            return false;
        }
        self.insert_render_cache(message_id, tiles.clone());
        self.apply_render_tiles(message_id, tiles);
        self.prewarm_protocol_cache(message_id);
        true
    }

    fn apply_render_tiles(&mut self, message_id: i64, tiles: Vec<TileMeta>) {
        self.render_tiles = tiles;
        self.render_tile_count = self.render_tiles.len();
        self.render_tile_index = 0;
        self.render_error = None;
        self.render_no_html = false;

        self.image_protocol = self.take_cached_protocol(message_id, 0);

        self.touch_render_cache(message_id);
    }

    fn insert_render_cache(&mut self, message_id: i64, tiles: Vec<TileMeta>) {
        if !self.render_cache.contains_key(&message_id) {
            self.render_cache_lru.push_front(message_id);
        }
        self.render_cache.insert(message_id, tiles);
        self.prune_render_cache();
    }

    fn take_cached_protocol(&mut self, message_id: i64, tile_index: usize) -> Option<StatefulProtocol> {
        let key = (message_id, tile_index);
        if let Some(protocol) = self.protocol_cache.remove(&key) {
            self.touch_protocol_cache(key);
            return Some(protocol);
        }
        None
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

    fn touch_render_cache(&mut self, message_id: i64) {
        if let Some(pos) = self.render_cache_lru.iter().position(|id| *id == message_id) {
            self.render_cache_lru.remove(pos);
            self.render_cache_lru.push_front(message_id);
        }
    }

    fn prune_render_cache(&mut self) {
        while self.render_cache_lru.len() > RENDER_MEM_CACHE_LIMIT {
            if let Some(id) = self.render_cache_lru.pop_back() {
                self.render_cache.remove(&id);
            }
        }
    }

    fn on_render_event(&mut self, event: RenderEvent) {
        let Some(selected) = self.selected_message() else { return };
        if selected.id != event.message_id {
            return;
        }

        if let Some(err) = event.error {
            self.render_error = Some(err);
            self.render_no_html = false;
            self.render_tile_count = 0;
            self.render_tiles.clear();
            self.image_protocol = None;
            self.render_pending = false;
            return;
        }

        self.render_no_html = event.no_html;
        if event.no_html {
            self.render_tile_count = 0;
            self.render_tiles.clear();
            self.image_protocol = None;
            self.render_pending = false;
            return;
        }

        let tiles = event.tiles;
        self.insert_render_cache(event.message_id, tiles.clone());
        self.apply_render_tiles(event.message_id, tiles);
        self.prewarm_protocol_cache(event.message_id);
        self.render_pending = false;
    }

    fn prewarm_protocol_cache(&mut self, message_id: i64) {
        if self.image_picker.is_none() {
            return;
        }
        let start = self.render_tile_index;
        let end = (start + 2).min(self.render_tiles.len().saturating_sub(1));
        for idx in start..=end {
            let key = (message_id, idx);
            if self.protocol_cache.contains_key(&key) {
                continue;
            }
            if let Some(bytes) = self.render_tiles.get(idx).map(|t| t.bytes.clone()) {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    if let Some(picker) = self.image_picker.as_mut() {
                        let protocol = picker.new_resize_protocol(img);
                        self.store_protocol_cache(message_id, idx, protocol);
                    }
                }
            }
        }
    }

    fn ensure_text_cache_for_selected(&mut self) {
        let Some(message) = self.selected_message() else { return };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.body.is_empty() && !detail.links.is_empty() {
                return;
            }
        }

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime.block_on(async move {
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
        }
    }

    fn ensure_attachments_for_selected(&mut self) {
        let Some(message) = self.selected_message() else { return };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.attachments.is_empty() {
                return;
            }
        }

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime.block_on(async move {
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
        }
    }


    fn on_event(&mut self, event: MailEvent) {
        match event {
            MailEvent::SyncStarted(_) => self.sync_status = "syncing".to_string(),
            MailEvent::SyncCompleted(_) => self.sync_status = "idle".to_string(),
            MailEvent::SyncFailed { .. } => self.sync_status = "error".to_string(),
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
                if self.message_index + 1 < self.store.messages.len() {
                    self.message_index += 1;
                    if self.view_mode == ViewMode::Rendered {
                        self.schedule_render();
                    }
                }
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                if self.message_index > 0 {
                    self.message_index -= 1;
                    if self.view_mode == ViewMode::Rendered {
                        self.schedule_render();
                    }
                }
            }
            (KeyCode::Enter, _) => {
                self.mode = Mode::ViewFocus;
                self.view_scroll = 0;
                let _ = self.engine.send(MailCommand::SyncFolder(1));
                self.ensure_text_cache_for_selected();
                if self.view_mode == ViewMode::Rendered {
                    self.schedule_render();
                }
            }
            (KeyCode::Esc, _) => {
                if self.mode == Mode::ViewFocus {
                    self.mode = Mode::View;
                }
            }
            (KeyCode::Char('v'), _) => {
                self.view_mode = match self.view_mode {
                    ViewMode::Text => ViewMode::Rendered,
                    ViewMode::Rendered => ViewMode::Text,
                };
                if self.view_mode == ViewMode::Text {
                    self.ensure_text_cache_for_selected();
                } else {
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
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('R'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('f'), _) => {
                self.mode = Mode::Compose;
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
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(1);
            }
            (KeyCode::PageDown, _) => {
                self.view_scroll = self.view_scroll.saturating_add(10);
            }
            (KeyCode::PageUp, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(10);
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
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('R'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('f'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
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
        match key.code {
            KeyCode::Esc => {
                self.mode = self.overlay_return;
            }
            KeyCode::Char('q') => return true,
            _ => {}
        }
        false
    }

fn on_key_compose(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.mode = Mode::View;
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
            }
            _ => {}
        }
        false
    }
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

        for message_id in request.message_ids {
            if rx.borrow().request_id != current_id {
                break;
            }

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
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                    continue;
                }
            };

            let Some(html) = html else {
                let _ = tx.send(RenderEvent {
                    message_id,
                    tiles: Vec::new(),
                    no_html: true,
                    error: None,
                });
                continue;
            };

            match store
                .get_cache_tiles(
                    message_id,
                    request.width_px,
                    &request.theme,
                    &request.remote_policy,
                )
                .await
            {
                Ok(cached) if !cached.is_empty() => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: cached,
                        no_html: false,
                        error: None,
                    });
                    continue;
                }
                Ok(_) => {}
                Err(err) => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                    continue;
                }
            }

            let render_result = renderer
                .render(ratmail_render::RenderRequest {
                    message_id,
                    width_px: request.width_px,
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
                            no_html: false,
                            error: Some("Chromium produced no tiles. Try RATMAIL_CHROME_PATH=/usr/bin/chromium or RATMAIL_CHROME_NO_SANDBOX=1".to_string()),
                        });
                        continue;
                    }
                    let _ = store
                        .upsert_cache_tiles(
                            message_id,
                            request.width_px,
                            &request.theme,
                            &request.remote_policy,
                            &result.tiles,
                        )
                        .await;
                    let _ = store.prune_cache_tiles(TILE_CACHE_BUDGET_BYTES).await;
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: result.tiles,
                        no_html: false,
                        error: None,
                    });
                }
                Err(err) => {
                    let _ = tx.send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        no_html: false,
                        error: Some(err.to_string()),
                    });
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (engine, events, store_handle, store) = rt.block_on(async {
        let (engine, events) = MailEngine::start();
        let store_handle = SqliteMailStore::connect("ratmail.db").await?;
        store_handle.init().await?;
        store_handle.seed_demo_if_empty().await?;
        let snapshot = store_handle.load_snapshot(1, 1).await?;
        Ok::<_, anyhow::Error>((engine, events, store_handle, snapshot))
    })?;
    let render_supported = detect_image_support();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let picker = Picker::from_query_stdio().ok();
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
        while let Ok(event) = app.render_events.try_recv() {
            app.on_render_event(event);
        }

        let timeout = TICK_RATE.saturating_sub(app.last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if app.on_key(key) {
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
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_status_bar(frame, layout[0], app);
    render_main(frame, layout[1], app);
    render_help_bar(frame, layout[2]);

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

    let line = Line::from(vec![
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
    ]);

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
        .map(|folder| {
            let label = if folder.unread > 0 {
                format!("{}  {}", folder.name, folder.unread)
            } else {
                folder.name.clone()
            };
            Line::from(Span::raw(label))
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

    let rows: Vec<Row> = app
        .store
        .messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let unread = if message.unread { "*" } else { " " };
            let style = if idx == app.message_index {
                Style::default().bg(Color::DarkGray)
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

    let preview_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(2),
        width: area.width.saturating_sub(2),
        height: 1,
    };

    if let Some(message) = app.selected_message() {
        let preview = format!("Preview: {}", message.preview);
        frame.render_widget(Paragraph::new(preview), preview_area);
    }
}

fn render_message_view(frame: &mut ratatui::Frame, area: Rect, app: &mut App, scroll: u16) {
    let title = match app.view_mode {
        ViewMode::Text => "MESSAGE VIEW (text)",
        ViewMode::Rendered => "MESSAGE VIEW (rendered tiles)",
    };

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
        let outer = Block::default().borders(Borders::ALL).title(title);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

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
                    Text::from(vec![
                        Line::from(""),
                        Line::from("No HTML part found for this message."),
                        Line::from("Rendered mode requires HTML content."),
                        Line::from("Switch to Text mode (v)."),
                    ])
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
            .constraints([Constraint::Length(4), Constraint::Min(8)])
            .split(inner);

        let content_area = chunks[1];
        if view_mode == ViewMode::Rendered && render_supported && protocol_available {
            let desired_idx = app.view_scroll as usize;
            let area_key = (content_area.width, content_area.height);
            if app.last_render_area != Some(area_key) {
                app.protocol_cache.clear();
                app.protocol_cache_lru.clear();
                app.last_render_area = Some(area_key);
                app.image_protocol = None;
            }

            if app.render_tile_index != desired_idx || app.image_protocol.is_none() {
                if let Some(protocol) = app.take_cached_protocol(detail.id, desired_idx) {
                    app.image_protocol = Some(protocol);
                    app.render_tile_index = desired_idx;
                } else if let Some(bytes) = app.render_tiles.get(desired_idx).map(|t| t.bytes.clone()) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        if let Some(picker) = app.image_picker.as_mut() {
                            let protocol = picker.new_resize_protocol(img);
                            app.render_tile_index = desired_idx;
                            app.store_protocol_cache(detail.id, desired_idx, protocol);
                            app.image_protocol = app.take_cached_protocol(detail.id, desired_idx);
                        }
                    }
                }
            }

            if let Some(protocol) = app.image_protocol.as_mut() {
                frame.render_stateful_widget(StatefulImage::default(), content_area, protocol);
            } else {
                frame.render_widget(content_block, content_area);
            }
        } else {
            frame.render_widget(content_block, content_area);
        }
        frame.render_widget(Paragraph::new(meta_block), chunks[0]);
    } else {
        let block = Block::default().borders(Borders::ALL).title(title);
        frame.render_widget(Paragraph::new("No message selected").block(block), area);
    }
}

fn render_help_bar(frame: &mut ratatui::Frame, area: Rect) {
    let help = "j/k move  Enter open  v toggle view  r reply  R reply-all  f forward  m move  d delete  q quit";
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
                lines.push(Line::from(format!("  {:>2}  {}", idx + 1, link)));
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

fn render_compose_overlay(frame: &mut ratatui::Frame, area: Rect, _app: &mut App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from("To:   Alex Chen <alex@example.com>"),
        Line::from("Cc:"),
        Line::from("Subj: Re: Proposal"),
        Line::from(""),
        Line::from("Thanks — this looks good overall."),
        Line::from(""),
        Line::from("I've added comments to section 3 regarding timelines."),
        Line::from(""),
        Line::from("> On Feb 3, 2026, Alex Chen wrote:"),
        Line::from(">"),
        Line::from("> Hi,"),
        Line::from(">"),
        Line::from("> Attached is the updated proposal. Let me know if this"),
        Line::from("> works for you."),
        Line::from(">"),
        Line::from("> Best,"),
        Line::from("> Alex"),
        Line::from(""),
        Line::from("Attachments: proposal-v3.pdf"),
        Line::from(""),
        Line::from("Ctrl+S send   Ctrl+A attach   Ctrl+Q cancel   Ctrl+E edit headers"),
    ];

    let block = Block::default().borders(Borders::ALL).title("COMPOSE");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup);
}

fn render_view_focus(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(90, 85, area);
    frame.render_widget(Clear, popup);
    render_message_view(frame, popup, app, app.view_scroll);
}
