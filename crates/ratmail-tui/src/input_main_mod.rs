use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Focus, Mode, ViewMode};

impl App {
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> bool {
        if self.picker_mode.is_some() {
            return self.on_key_picker(key);
        }
        match self.mode {
            Mode::List | Mode::View => self.on_key_main(key),
            Mode::ViewFocus => self.on_key_focus(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::OverlaySearch => self.on_key_overlay(key),
            Mode::OverlaySpellcheck => self.on_key_overlay(key),
            Mode::OverlayLinks
            | Mode::OverlayAttach
            | Mode::OverlayBulkAction
            | Mode::OverlayBulkMove
            | Mode::OverlayConfirmDelete
            | Mode::OverlayConfirmLink
            | Mode::OverlayConfirmDraft => self.on_key_overlay(key),
        }
    }

    pub(crate) fn on_key_main(&mut self, key: KeyEvent) -> bool {
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
                        self.on_folder_changed();
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
                        self.on_folder_changed();
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
                    } else if self.selected_folder_is_drafts() {
                        self.start_compose_draft();
                    } else {
                        self.mode = Mode::ViewFocus;
                        self.view_scroll = 0;
                        self.render_tile_height_px = self.render_tile_height_px_focus;
                        self.render_tiles.clear();
                        self.render_tile_count = 0;
                        self.render_tiles_height_px = 0;
                        self.render_tiles_width_px = 0;
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
                } else if self.search_active() {
                    self.clear_search();
                } else if self.mode == Mode::ViewFocus {
                    self.mode = Mode::View;
                    self.render_tile_height_px = self.render_tile_height_px_side;
                    if self.view_mode == ViewMode::Rendered {
                        self.render_tiles.clear();
                        self.render_tile_count = 0;
                        self.render_tiles_height_px = 0;
                        self.render_tiles_width_px = 0;
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
            (KeyCode::Char('s'), _) => {
                self.request_sync_selected_folder();
            }
            (KeyCode::Char('/'), _) => {
                self.open_search_overlay();
            }
            _ => {}
        }
        false
    }
}
