use crossterm::event::{KeyCode, KeyEvent};
use ratmail_core::{LinkInfo, log_debug};

use super::{
    App, Mode, ViewMode, apply_input_key, copy_with_command, copy_with_osc52, looks_like_email,
    move_cursor_left, move_cursor_right, next_index, parse_mailto, prev_index, text_char_len,
};

impl App {
    fn clamp_link_index(&mut self) -> usize {
        let count = self.selected_detail().map(|d| d.links.len()).unwrap_or(0);
        if count == 0 {
            self.link_index = 0;
            return 0;
        }
        if self.link_index >= count {
            self.link_index = count - 1;
        }
        count
    }

    pub(crate) fn on_key_focus(&mut self, key: KeyEvent) -> bool {
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
            (KeyCode::Esc, _) => {
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

    fn link_is_raw(link: &LinkInfo) -> bool {
        if let Some(text) = link.text.as_deref() {
            return text.trim() == link.url.trim();
        }
        !link.from_html
    }

    fn open_link_raw(&mut self, link: &str, _external: bool) {
        if let Some((to, subject, body)) = parse_mailto(link) {
            self.start_compose_to(to, subject, body);
        } else if looks_like_email(link) {
            self.start_compose_to(link.to_string(), None, None);
        } else {
            let _ = open::that(link);
        }
    }

    fn open_link_action(&mut self, link: &LinkInfo, external: bool) {
        if Self::link_is_raw(link) {
            self.open_link_raw(&link.url, external);
        } else {
            self.open_confirm_link(link.clone(), external, self.mode);
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        if copy_with_osc52(text) {
            self.status_message = Some("Copied link".to_string());
            return;
        }
        if copy_with_command(text) {
            self.status_message = Some("Copied link".to_string());
            return;
        }
        self.status_message = Some("Clipboard copy failed".to_string());
    }

    pub(crate) fn on_key_overlay(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::OverlaySearch => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Enter => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Left => move_cursor_left(&self.search_query, &mut self.search_cursor),
                KeyCode::Right => move_cursor_right(&self.search_query, &mut self.search_cursor),
                KeyCode::Home => self.search_cursor = 0,
                KeyCode::End => {
                    self.search_cursor = text_char_len(&self.search_query);
                }
                _ => {
                    if apply_input_key(&mut self.search_query, &mut self.search_cursor, key) {
                        self.on_search_updated();
                    }
                }
            },
            Mode::OverlaySpellcheck => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = self.spell_return;
                }
                KeyCode::Char('n') | KeyCode::Right => {
                    self.spell_issue_index =
                        next_index(self.spell_issue_index, self.spell_issues.len());
                    self.spell_suggestion_index = 0;
                }
                KeyCode::Char('p') | KeyCode::Left => {
                    self.spell_issue_index =
                        prev_index(self.spell_issue_index, self.spell_issues.len());
                    self.spell_suggestion_index = 0;
                }
                KeyCode::Up => {
                    if self.current_spell_suggestions_len() > 0 {
                        self.spell_suggestion_index = prev_index(
                            self.spell_suggestion_index,
                            self.current_spell_suggestions_len(),
                        );
                    }
                }
                KeyCode::Down => {
                    if self.current_spell_suggestions_len() > 0 {
                        self.spell_suggestion_index = next_index(
                            self.spell_suggestion_index,
                            self.current_spell_suggestions_len(),
                        );
                    }
                }
                KeyCode::Enter => {
                    if self.apply_spell_suggestion() {
                        self.refresh_spell_issues();
                    } else {
                        self.spell_issue_index =
                            next_index(self.spell_issue_index, self.spell_issues.len());
                        self.spell_suggestion_index = 0;
                    }
                }
                KeyCode::Char('i') => {
                    if self.add_spell_ignore_current() {
                        self.refresh_spell_issues();
                    }
                }
                KeyCode::Char('s') => {
                    self.spell_issue_index =
                        next_index(self.spell_issue_index, self.spell_issues.len());
                    self.spell_suggestion_index = 0;
                }
                _ => {}
            },
            Mode::OverlayLinks => match key.code {
                KeyCode::Esc => {
                    self.mode = self.overlay_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('j') | KeyCode::Down => {
                    let count = self.clamp_link_index();
                    if count > 0 && self.link_index + 1 < count {
                        self.link_index += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.clamp_link_index();
                    if self.link_index > 0 {
                        self.link_index -= 1;
                    }
                }
                KeyCode::PageDown => {
                    let count = self.clamp_link_index();
                    if count > 0 {
                        self.link_index = (self.link_index + 10).min(count - 1);
                    }
                }
                KeyCode::PageUp => {
                    self.clamp_link_index();
                    self.link_index = self.link_index.saturating_sub(10);
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    if self.clamp_link_index() > 0 {
                        self.link_index = 0;
                    }
                }
                KeyCode::End | KeyCode::Char('G') => {
                    let count = self.clamp_link_index();
                    if count > 0 {
                        self.link_index = count - 1;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let count = self.clamp_link_index();
                    if count > 0 {
                        let idx = c.to_digit(10).unwrap_or(0) as usize;
                        if idx > 0 && idx <= count {
                            self.link_index = idx - 1;
                        }
                    }
                }
                KeyCode::Enter => {
                    self.clamp_link_index();
                    if let Some(detail) = self.selected_detail() {
                        if let Some(link) = detail.links.get(self.link_index) {
                            let link = link.clone();
                            self.open_link_action(&link, false);
                        }
                    }
                }
                KeyCode::Char('o') => {
                    self.clamp_link_index();
                    if let Some(detail) = self.selected_detail() {
                        if let Some(link) = detail.links.get(self.link_index) {
                            let link = link.clone();
                            self.open_link_action(&link, true);
                        }
                    }
                }
                KeyCode::Char('y') => {
                    self.clamp_link_index();
                    if let Some(detail) = self.selected_detail() {
                        if let Some(link) = detail.links.get(self.link_index) {
                            let url = link.url.clone();
                            self.copy_to_clipboard(&url);
                        }
                    }
                }
                _ => {}
            },
            Mode::OverlayConfirmLink => match key.code {
                KeyCode::Esc => {
                    self.confirm_link = None;
                    self.mode = self.confirm_link_return;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('y') => {
                    if let Some(link) = self.confirm_link.clone() {
                        let external = self.confirm_link_external;
                        self.confirm_link = None;
                        self.mode = self.confirm_link_return;
                        self.open_link_raw(&link.url, external);
                    } else {
                        self.mode = self.confirm_link_return;
                    }
                }
                KeyCode::Char('n') => {
                    self.confirm_link = None;
                    self.mode = self.confirm_link_return;
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
                KeyCode::Char('r') => {
                    let ids = self.bulk_action_ids.clone();
                    self.queue_mark_read_messages(ids);
                    self.mode = self.bulk_done_return;
                }
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
            Mode::OverlayConfirmDraft => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Compose;
                }
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.save_compose_draft();
                }
                KeyCode::Char('n') => {
                    self.discard_compose();
                }
                _ => {}
            },
            _ => {}
        }
        false
    }
}
