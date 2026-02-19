use ratatui::layout::Rect;
use ratatui_image::protocol::StatefulProtocol;
use ratmail_core::{MailStore, TileMeta, log_debug};

use super::{App, Mode, PROTOCOL_CACHE_LIMIT, RenderEvent, RenderRequest, ViewMode};

impl App {
    pub(crate) fn schedule_render(&mut self) {
        if self.view_mode != ViewMode::Rendered {
            return;
        }
        if !self.render_supported {
            return;
        }

        let Some(message_id) = self.selected_message().map(|m| m.id) else {
            return;
        };

        // Selection changed: clear visible rendered tiles immediately so stale
        // preview/focus images from the previous message do not linger.
        if self.render_message_id != Some(message_id) {
            self.render_tile_count = 0;
            self.render_tiles.clear();
            self.render_tile_index = 0;
            self.render_tiles_width_px = 0;
            self.image_protocol = None;
            self.render_error = None;
            self.render_no_html = false;
        }

        self.render_tile_height_px = self.current_tile_height();
        if let Some(picker) = self.image_picker.as_ref() {
            let fh = picker.font_size().1 as i64;
            if fh > 0 {
                self.render_tile_height_px = ((self.render_tile_height_px + fh - 1) / fh) * fh;
            }
        }

        if self.render_message_id == Some(message_id)
            && self.render_tile_count > 0
            && self.render_tiles_height_px == self.render_tile_height_px
            && self.render_tiles_width_px == self.render_width_px
        {
            return;
        }

        if self.render_pending
            && self.render_pending_message_id == Some(message_id)
            && self.render_pending_tile_height_px == self.render_tile_height_px
            && self.render_pending_width_px == self.render_width_px
        {
            return;
        }

        log_debug(&format!(
            "schedule_render msg_id={} tile_h={} width_px={}",
            message_id, self.render_tile_height_px, self.render_width_px
        ));

        if !self.ensure_raw_body_for_render(message_id) {
            log_debug(&format!("render_skip no_raw msg_id={}", message_id));
            return;
        }

        if self.try_load_cached_tiles_db(message_id) {
            log_debug(&format!("render_cache db_hit msg_id={}", message_id));
            return;
        }

        log_debug(&format!("render_cache miss msg_id={}", message_id));
        self.send_render_request(message_id);
    }

    pub(crate) fn try_load_cached_tiles_db(&mut self, message_id: i64) -> bool {
        let store_handle = self.store_handle.clone();
        let width_px = self.render_width_px;
        let tile_height_px = self.render_tile_height_px;
        let theme = format!("{}:bgv2", self.ui_theme_name);
        let remote_policy = if self.allow_remote_images {
            "allowed"
        } else {
            "blocked"
        };

        let cached = self.runtime().block_on(async move {
            store_handle
                .get_cache_tiles(message_id, width_px, tile_height_px, &theme, remote_policy)
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

    pub(crate) fn send_render_request(&mut self, message_id: i64) {
        let remote_policy = if self.allow_remote_images {
            "allowed"
        } else {
            "blocked"
        };
        self.render_request_id += 1;
        self.render_pending = true;
        self.render_pending_message_id = Some(message_id);
        self.render_pending_tile_height_px = self.render_tile_height_px;
        self.render_pending_width_px = self.render_width_px;
        self.render_spinner = 0;
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
            theme: self.ui_theme_name.clone(),
            remote_policy: remote_policy.to_string(),
        });
    }

    pub(crate) fn apply_render_tiles(&mut self, message_id: i64, tiles: Vec<TileMeta>) {
        self.render_message_id = Some(message_id);
        self.render_tiles = tiles;
        self.render_tile_count = self.render_tiles.len();
        self.render_tile_index = 0;
        self.render_tiles_height_px = self.render_tile_height_px;
        self.render_tiles_width_px = self.render_width_px;
        self.render_error = None;
        self.render_no_html = false;
        self.image_protocol = None;
        self.render_pending = false;
        self.render_pending_message_id = None;
        self.render_pending_tile_height_px = 0;
        self.render_pending_width_px = 0;
    }

    pub(crate) fn take_cached_protocol(
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

    pub(crate) fn store_protocol_cache(
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

    pub(crate) fn touch_protocol_cache(&mut self, key: (i64, usize)) {
        if let Some(pos) = self.protocol_cache_lru.iter().position(|k| *k == key) {
            self.protocol_cache_lru.remove(pos);
        }
        self.protocol_cache_lru.push_front(key);
    }

    pub(crate) fn prune_protocol_cache(&mut self) {
        while self.protocol_cache_lru.len() > PROTOCOL_CACHE_LIMIT {
            if let Some(key) = self.protocol_cache_lru.pop_back() {
                self.protocol_cache.remove(&key);
            }
        }
    }

    pub(crate) fn current_tile_height(&self) -> i64 {
        if self.mode == Mode::ViewFocus {
            self.render_tile_height_px_focus
        } else {
            self.render_tile_height_px_side
        }
    }

    pub(crate) fn update_render_geometry(&mut self, content_area: Rect) -> bool {
        if content_area.width == 0 || content_area.height == 0 {
            return false;
        }
        let Some(picker) = self.image_picker.as_ref() else {
            return false;
        };
        let (fw, fh) = picker.font_size();
        if fw == 0 || fh == 0 {
            return false;
        }

        let width_px = ((content_area.width as f64 * fw as f64) / self.render_scale)
            .round()
            .max(1.0) as i64;
        let height_px = ((content_area.height as f64 * fh as f64) / self.render_scale)
            .round()
            .max(1.0) as i64;

        let mut changed = false;
        if self.render_width_px != width_px {
            self.render_width_px = width_px;
            changed = true;
        }
        if self.mode != Mode::ViewFocus && self.render_tile_height_px_side != height_px {
            self.render_tile_height_px_side = height_px;
            changed = true;
        }

        changed
    }

    pub(crate) fn on_render_event(&mut self, event: RenderEvent) {
        let selected_id = self.selected_message().map(|m| m.id);
        let is_current = selected_id == Some(event.message_id)
            && event.tile_height_px == self.render_tile_height_px
            && event.width_px == self.render_width_px;

        if let Some(err) = event.error {
            if is_current {
                self.render_error = Some(err.clone());
                self.render_no_html = false;
                self.render_tile_count = 0;
                self.render_tiles.clear();
                self.render_message_id = None;
                self.render_tiles_width_px = 0;
                self.image_protocol = None;
                self.render_pending = false;
                self.render_pending_message_id = None;
                self.render_pending_tile_height_px = 0;
                self.render_pending_width_px = 0;
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
                self.render_tiles_width_px = 0;
                self.image_protocol = None;
                self.render_pending = false;
                self.render_pending_message_id = None;
                self.render_pending_tile_height_px = 0;
                self.render_pending_width_px = 0;
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
}
