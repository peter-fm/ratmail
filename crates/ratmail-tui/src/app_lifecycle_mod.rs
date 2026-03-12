use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use ratatui_image::picker::Picker;
use ratmail_core::{SqliteMailStore, StoreSnapshot, log_debug};
use ratmail_mail::{MailCommand, MailEngine, MailEvent};

use super::{
    App, ComposeFocus, ComposeVimMode, Focus, IMAP_SPINNER_FRAMES, Mode, PickerFocus,
    PickerPreviewKind, RAT_SPINNER_FRAMES, RenderEvent, RenderRequest, SearchSpec, SendConfig,
    StoreUpdate, UiTheme, ViewMode, canonical_folder_name, compose_buffer_from_body, extract_email,
    parse_from_addrs,
};

const MAX_MAIL_EVENTS_PER_FRAME: usize = 32;
const MAX_STORE_SNAPSHOTS_PER_FRAME: usize = 2;
const MAX_RENDER_EVENTS_PER_FRAME: usize = 8;

impl App {
    pub(crate) fn new(
        store: StoreSnapshot,
        store_handle: SqliteMailStore,
        engine: MailEngine,
        events: tokio::sync::mpsc::Receiver<MailEvent>,
        runtime: Arc<tokio::runtime::Runtime>,
        render_supported: bool,
        image_picker: Option<Picker>,
        renderer_is_chromium: bool,
        render_request_tx: tokio::sync::watch::Sender<RenderRequest>,
        render_events: tokio::sync::mpsc::Receiver<RenderEvent>,
        store_update_tx: tokio::sync::mpsc::Sender<StoreUpdate>,
        store_updates: tokio::sync::mpsc::Receiver<StoreSnapshot>,
        allow_remote_images: bool,
        render_width_px: i64,
        render_tile_height_px_side: i64,
        render_tile_height_px_focus: i64,
        imap_enabled: bool,
        initial_sync_days: i64,
        render_scale: f64,
        folder_pane_width: u16,
        ui_theme_name: String,
        ui_theme: Arc<UiTheme>,
        send_config: SendConfig,
        compose_vim_enabled: bool,
        sender_identities: Vec<String>,
    ) -> Self {
        let compose_from = sender_identities
            .first()
            .cloned()
            .or_else(|| {
                let account = store.account.address.trim();
                if account.is_empty() {
                    None
                } else {
                    Some(
                        parse_from_addrs(account)
                            .into_iter()
                            .next()
                            .unwrap_or_else(|| extract_email(account)),
                    )
                }
            })
            .unwrap_or_default();
        let mut app = Self {
            mode: Mode::List,
            focus: Focus::Messages,
            view_mode: ViewMode::Text,
            text_view_cache_key: None,
            store,
            folder_index: 0,
            message_index: 0,
            message_scroll_top: 0,
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
            render_tiles_width_px: 0,
            render_request_id: 0,
            render_message_id: None,
            render_pending: false,
            render_pending_message_id: None,
            render_pending_tile_height_px: 0,
            render_pending_width_px: 0,
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
            render_spinner: 0,
            render_scale,
            ui_theme_name,
            ui_theme: ui_theme.clone(),
            send_config,
            show_preview: false,
            folder_pane_width,
            show_help: false,
            link_index: 0,
            attach_index: 0,
            status_message: None,
            status_message_at: None,
            search_query: String::new(),
            search_cursor: 0,
            search_spec: SearchSpec::default(),
            search_attachment_queue: VecDeque::new(),
            attachment_checked: HashSet::new(),
            attachment_cache: HashMap::new(),
            selected_message_ids: HashSet::new(),
            bulk_action_ids: Vec::new(),
            bulk_folder_index: 0,
            bulk_done_return: Mode::View,
            confirm_delete_ids: Vec::new(),
            confirm_delete_return: Mode::View,
            confirm_link: None,
            confirm_link_external: false,
            confirm_link_return: Mode::View,
            confirm_compose_action: None,
            confirm_compose_attachment_count: 0,
            confirm_compose_return: Mode::View,
            picker_mode: None,
            picker_focus: PickerFocus::Explorer,
            picker: None,
            picker_filename: String::new(),
            picker_cursor: 0,
            picker_filter: String::new(),
            picker_preview_visible: false,
            picker_preview_path: None,
            picker_preview_kind: PickerPreviewKind::Empty,
            picker_preview_text: String::new(),
            picker_preview_meta: Vec::new(),
            picker_preview_image: None,
            picker_preview_protocol: None,
            picker_pdf_preview_available: None,
            picker_preview_error: None,
            image_resize_prompt: None,
            imap_enabled,
            last_folder_sync: None,
            last_backfill: None,
            imap_pending: 0,
            imap_spinner: 0,
            imap_status: None,
            initial_sync_days,
            compose_to: String::new(),
            compose_from,
            compose_cc: String::new(),
            compose_bcc: String::new(),
            compose_subject: String::new(),
            compose_body: compose_buffer_from_body(ui_theme.clone(), ""),
            compose_quote: String::new(),
            compose_attachments: Vec::new(),
            compose_vim_enabled,
            compose_vim_mode: if compose_vim_enabled {
                ComposeVimMode::Normal
            } else {
                ComposeVimMode::Insert
            },
            compose_vim_pending: None,
            compose_focus: ComposeFocus::Body,
            compose_cursor_to: 0,
            compose_cursor_from: 0,
            compose_cursor_cc: 0,
            compose_cursor_bcc: 0,
            compose_cursor_subject: 0,
            compose_body_desired_x: None,
            compose_body_area_width: 0,
            compose_body_area_height: 0,
            compose_address_book: HashSet::new(),
            compose_address_list: Vec::new(),
            compose_sender_book: HashSet::new(),
            compose_sender_list: sender_identities,
            spell_issues: Vec::new(),
            spell_issue_index: 0,
            spell_suggestion_index: 0,
            spell_return: Mode::Compose,
            inline_spell_suggest: None,
        };
        app.select_inbox_if_available();
        app.sort_folders();
        app.refresh_compose_address_book();
        app.refresh_compose_sender_book();
        if app.imap_enabled {
            let _ = app.engine.send(MailCommand::SyncAll);
            app.imap_pending = app.imap_pending.saturating_add(2);
            app.imap_status = Some("IMAP syncing...".to_string());
        }
        app
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    pub(crate) fn queue_store_update(&self, update: StoreUpdate) {
        if let Err(err) = self.store_update_tx.try_send(update) {
            log_debug(&format!("store_update queue drop: {}", err));
        }
    }

    pub(crate) fn queue_store_update_reliable(&self, update: StoreUpdate) {
        spawn_store_update_reliable(
            self.runtime().handle(),
            self.store_update_tx.clone(),
            update,
        );
    }

    pub(crate) fn sort_folders(&mut self) {
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

    pub(crate) fn display_folder_name(raw: &str) -> String {
        canonical_folder_name(raw)
    }

    pub(crate) fn restore_selection(
        &mut self,
        folder_name: Option<String>,
        message_uid: Option<u32>,
    ) {
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

    pub(crate) fn drain_channels(&mut self) {
        for _ in 0..MAX_MAIL_EVENTS_PER_FRAME {
            match self.events.try_recv() {
                Ok(event) => self.on_event(event),
                Err(_) => break,
            }
        }
        for _ in 0..MAX_STORE_SNAPSHOTS_PER_FRAME {
            match self.store_updates.try_recv() {
                Ok(snapshot) => {
                    let prev_folder = self.selected_folder().map(|f| f.name.clone());
                    let prev_uid = self.selected_message().and_then(|m| m.imap_uid);
                    self.store = snapshot;
                    self.text_view_cache_key = None;
                    self.sort_folders();
                    self.refresh_compose_address_book();
                    self.refresh_compose_sender_book();
                    self.reapply_attachment_cache();
                    self.restore_selection(prev_folder, prev_uid);
                    self.prune_selected_messages();
                    if self.store.folders.is_empty() {
                        self.select_inbox_if_available();
                    }
                }
                Err(_) => break,
            }
        }
        for _ in 0..MAX_RENDER_EVENTS_PER_FRAME {
            match self.render_events.try_recv() {
                Ok(event) => self.on_render_event(event),
                Err(_) => break,
            }
        }
    }

    pub(crate) fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_message_at = Some(Instant::now());
    }

    pub(crate) fn on_tick(&mut self) {
        if let Some(at) = self.status_message_at {
            if at.elapsed() >= std::time::Duration::from_secs(3) {
                self.status_message = None;
                self.status_message_at = None;
            }
        }
        if self.imap_enabled && self.last_folder_sync.is_none() {
            self.select_inbox_if_available();
            self.request_sync_selected_folder();
            self.prefetch_raw_bodies(10);
        }
        if self.imap_pending > 0 {
            self.imap_spinner = (self.imap_spinner + 1) % IMAP_SPINNER_FRAMES.len();
            self.imap_status = Some(format!(
                "IMAP syncing {}",
                IMAP_SPINNER_FRAMES[self.imap_spinner]
            ));
        }
        if self.search_spec.needs_raw() {
            self.prefetch_search_attachments_step(4);
        }
        if self.render_pending {
            self.render_spinner = (self.render_spinner + 1) % RAT_SPINNER_FRAMES.len();
        } else {
            self.render_spinner = 0;
        }
    }
}

fn spawn_store_update_reliable(
    handle: &tokio::runtime::Handle,
    tx: tokio::sync::mpsc::Sender<StoreUpdate>,
    update: StoreUpdate,
) {
    handle.spawn(async move {
        if let Err(err) = tx.send(update).await {
            log_debug(&format!("store_update reliable send failed: {}", err));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{StoreUpdate, spawn_store_update_reliable};

    #[tokio::test]
    async fn reliable_enqueue_delivers_after_queue_pressure() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StoreUpdate>(1);
        tx.try_send(StoreUpdate::SaveDraft {
            account_id: 1,
            from_addr: "from@example.com".to_string(),
            to: "to@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "first".to_string(),
            body: "first".to_string(),
        })
        .unwrap();

        spawn_store_update_reliable(
            &tokio::runtime::Handle::current(),
            tx.clone(),
            StoreUpdate::SaveDraft {
                account_id: 1,
                from_addr: "from@example.com".to_string(),
                to: "to@example.com".to_string(),
                cc: String::new(),
                bcc: String::new(),
                subject: "second".to_string(),
                body: "second".to_string(),
            },
        );

        let _ = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let next = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

        match next {
            StoreUpdate::SaveDraft { subject, .. } => assert_eq!(subject, "second"),
            other => panic!("unexpected update: {:?}", other),
        }
    }
}
