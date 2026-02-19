use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratmail_mail::{MailCommand, OutgoingAttachment};

use super::{
    App, ComposeFocus, ComposeVimMode, InlineSpellSuggest, Mode, PickerFocus, PickerMode,
    SpellTarget, VisualMove, add_spell_ignore_word, apply_compose_key, apply_input_key,
    build_html_body, char_index_from_row_col, char_to_byte_idx, collect_spell_issues,
    compose_buffer_from_body, compose_focus_next, compose_focus_prev, compose_move_visual,
    compose_token_at_cursor, cursor_from_char_index, move_cursor_left, move_cursor_right,
    next_index, parse_from_addrs, prev_index, replace_range_chars, spell_dictionary, text_char_len,
    word_at_col,
};

impl App {
    pub(crate) fn on_key_compose(&mut self, key: KeyEvent) -> bool {
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
            let body = self.compose_body_for_save();
            let body_html = build_html_body(&body, &self.send_config);
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
                    body_html,
                    attachments,
                });
            }
            return false;
        }
        if matches!(key.code, KeyCode::F(7)) {
            self.open_spellcheck_overlay();
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q')) {
            if self.compose_has_content() {
                self.open_confirm_draft();
            } else {
                self.mode = Mode::View;
            }
            return false;
        }
        if self.compose_focus == ComposeFocus::Body && self.compose_vim_enabled {
            if key.code == KeyCode::Esc {
                self.compose_vim_pending = None;
                if self.compose_vim_mode == ComposeVimMode::Insert {
                    self.compose_vim_mode = ComposeVimMode::Normal;
                    self.inline_spell_suggest = None;
                    return false;
                }
                if self.compose_has_content() {
                    self.open_confirm_draft();
                } else {
                    self.mode = Mode::View;
                }
                return false;
            }
            if self.compose_vim_mode == ComposeVimMode::Normal {
                return self.on_key_compose_vim_normal(key);
            }
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                if self.compose_has_content() {
                    self.open_confirm_draft();
                } else {
                    self.mode = Mode::View;
                }
                return false;
            }
            (KeyCode::BackTab, _) | (KeyCode::Tab, KeyModifiers::SHIFT) => {
                self.compose_focus = compose_focus_prev(self.compose_focus);
                return false;
            }
            _ => {}
        }

        if self.compose_focus == ComposeFocus::Body {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char(' '))
            {
                self.inline_spell_suggest = self.build_inline_spell_suggest();
                if self.inline_spell_suggest.is_none() {
                    self.status_message = Some("No spelling suggestions".to_string());
                }
                return false;
            }
            if let Some(suggest) = &mut self.inline_spell_suggest {
                match key.code {
                    KeyCode::Esc => {
                        self.inline_spell_suggest = None;
                        return false;
                    }
                    KeyCode::Up => {
                        suggest.index = prev_index(suggest.index, suggest.suggestions.len());
                        return false;
                    }
                    KeyCode::Down => {
                        suggest.index = next_index(suggest.index, suggest.suggestions.len());
                        return false;
                    }
                    KeyCode::Tab | KeyCode::Enter => {
                        if self.apply_inline_spell_suggest() {
                            self.inline_spell_suggest = None;
                        }
                        return false;
                    }
                    _ => {}
                }
            }
            match key.code {
                KeyCode::Up => {
                    if !compose_move_visual(
                        &mut self.compose_body,
                        VisualMove::Up,
                        &mut self.compose_body_desired_x,
                        self.compose_body_area_width,
                    ) {
                        self.compose_body.input(key);
                    }
                }
                KeyCode::Down => {
                    if !compose_move_visual(
                        &mut self.compose_body,
                        VisualMove::Down,
                        &mut self.compose_body_desired_x,
                        self.compose_body_area_width,
                    ) {
                        self.compose_body.input(key);
                    }
                }
                _ => {
                    self.compose_body_desired_x = None;
                    self.compose_body.input(key);
                }
            }
            self.inline_spell_suggest = None;
            return false;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Tab, _) | (KeyCode::Char('\t'), _) => {
                self.compose_focus = compose_focus_next(self.compose_focus);
            }
            (KeyCode::Enter, _) | (KeyCode::Char('\n'), _) | (KeyCode::Char('\r'), _) => {
                self.compose_focus = compose_focus_next(self.compose_focus);
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
                ComposeFocus::Body => {}
            },
            (KeyCode::Right, _) => match self.compose_focus {
                ComposeFocus::To | ComposeFocus::Cc | ComposeFocus::Bcc => {
                    if self.accept_compose_autocomplete() {
                        return false;
                    }
                    match self.compose_focus {
                        ComposeFocus::To => {
                            move_cursor_right(&self.compose_to, &mut self.compose_cursor_to);
                        }
                        ComposeFocus::Cc => {
                            move_cursor_right(&self.compose_cc, &mut self.compose_cursor_cc);
                        }
                        ComposeFocus::Bcc => {
                            move_cursor_right(&self.compose_bcc, &mut self.compose_cursor_bcc);
                        }
                        ComposeFocus::Subject | ComposeFocus::Body => {}
                    }
                }
                ComposeFocus::Subject => {
                    move_cursor_right(&self.compose_subject, &mut self.compose_cursor_subject);
                }
                ComposeFocus::Body => {}
            },
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
                ComposeFocus::Body => {}
            },
        }
        false
    }

    pub(crate) fn on_key_compose_vim_normal(&mut self, key: KeyEvent) -> bool {
        if let Some(pending) = self.compose_vim_pending.take() {
            match (pending, key.code) {
                ('d', KeyCode::Char('d')) => {
                    self.compose_body_desired_x = None;
                    self.compose_body.move_cursor_head();
                    self.compose_body.delete_line_by_end();
                    return false;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('i') => {
                self.compose_body_desired_x = None;
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('a') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_forward();
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('A') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_end();
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('I') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_head();
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('o') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_end();
                self.compose_body.insert_newline();
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('O') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_head();
                self.compose_body.insert_newline();
                self.compose_body.move_cursor_up();
                self.compose_vim_mode = ComposeVimMode::Insert;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_back();
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_forward();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let _ = compose_move_visual(
                    &mut self.compose_body,
                    VisualMove::Down,
                    &mut self.compose_body_desired_x,
                    self.compose_body_area_width,
                );
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let _ = compose_move_visual(
                    &mut self.compose_body,
                    VisualMove::Up,
                    &mut self.compose_body_desired_x,
                    self.compose_body_area_width,
                );
            }
            KeyCode::Char('w') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_word_forward();
            }
            KeyCode::Char('b') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_word_back();
            }
            KeyCode::Char('0') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_head();
            }
            KeyCode::Char('$') => {
                self.compose_body_desired_x = None;
                self.compose_body.move_cursor_end();
            }
            KeyCode::Char('x') => {
                self.compose_body_desired_x = None;
                self.compose_body.delete_next_char();
            }
            KeyCode::Char('d') => {
                self.compose_body_desired_x = None;
                self.compose_vim_pending = Some('d');
            }
            _ => {}
        }
        false
    }

    pub(crate) fn refresh_compose_address_book(&mut self) {
        let mut out = HashSet::new();
        let account = self.store.account.address.trim();
        if !account.is_empty() {
            out.insert(account.to_ascii_lowercase());
        }
        for message in &self.store.messages {
            for addr in parse_from_addrs(&message.from) {
                let normalized = addr.trim().to_ascii_lowercase();
                if !normalized.is_empty() {
                    out.insert(normalized);
                }
            }
        }
        for detail in self.store.message_details.values() {
            for addr in parse_from_addrs(&detail.from) {
                let normalized = addr.trim().to_ascii_lowercase();
                if !normalized.is_empty() {
                    out.insert(normalized);
                }
            }
        }
        self.compose_address_list = out.iter().cloned().collect();
        self.compose_address_list.sort();
        self.compose_address_book = out;
    }

    fn accept_compose_autocomplete(&mut self) -> bool {
        let (target, cursor) = match self.compose_focus {
            ComposeFocus::To => (&mut self.compose_to, &mut self.compose_cursor_to),
            ComposeFocus::Cc => (&mut self.compose_cc, &mut self.compose_cursor_cc),
            ComposeFocus::Bcc => (&mut self.compose_bcc, &mut self.compose_cursor_bcc),
            _ => return false,
        };
        let Some((_, _, prefix)) = compose_token_at_cursor(target, *cursor) else {
            return false;
        };
        let prefix_lower = prefix.to_ascii_lowercase();
        let Some(suggestion) = self
            .compose_address_list
            .iter()
            .find(|addr| addr.starts_with(&prefix_lower) && *addr != &prefix_lower)
            .cloned()
        else {
            return false;
        };
        let suffix = suggestion
            .chars()
            .skip(prefix_lower.chars().count())
            .collect::<String>();
        if suffix.is_empty() {
            return false;
        }
        let idx = char_to_byte_idx(target, *cursor);
        target.insert_str(idx, &suffix);
        *cursor += suffix.chars().count();
        true
    }

    fn open_spellcheck_overlay(&mut self) {
        let Some(dict) = spell_dictionary() else {
            self.status_message = Some("Spellcheck unavailable (dictionary not found)".to_string());
            return;
        };
        self.spell_issues =
            collect_spell_issues(&self.compose_subject, &self.compose_body_text(), dict);
        if self.spell_issues.is_empty() {
            self.status_message = Some("Spellcheck: no issues found".to_string());
            return;
        }
        self.spell_issue_index = 0;
        self.spell_suggestion_index = 0;
        self.spell_return = Mode::Compose;
        self.mode = Mode::OverlaySpellcheck;
    }

    pub(crate) fn refresh_spell_issues(&mut self) {
        let Some(dict) = spell_dictionary() else {
            self.spell_issues.clear();
            self.mode = self.spell_return;
            self.status_message = Some("Spellcheck unavailable (dictionary not found)".to_string());
            return;
        };
        let prev = self.spell_issue_index;
        self.spell_issues =
            collect_spell_issues(&self.compose_subject, &self.compose_body_text(), dict);
        if self.spell_issues.is_empty() {
            self.mode = self.spell_return;
            self.status_message = Some("Spellcheck: no issues found".to_string());
            return;
        }
        self.spell_issue_index = prev.min(self.spell_issues.len().saturating_sub(1));
        self.spell_suggestion_index = 0;
    }

    pub(crate) fn current_spell_suggestions_len(&self) -> usize {
        self.spell_issues
            .get(self.spell_issue_index)
            .map(|i| i.suggestions.len())
            .unwrap_or(0)
    }

    pub(crate) fn add_spell_ignore_current(&mut self) -> bool {
        let Some(issue) = self.spell_issues.get(self.spell_issue_index) else {
            return false;
        };
        if let Err(err) = add_spell_ignore_word(&issue.word) {
            self.status_message = Some(format!("Spell ignore failed: {}", err));
            return false;
        }
        self.status_message = Some(format!("Ignored word: {}", issue.word));
        true
    }

    pub(crate) fn apply_spell_suggestion(&mut self) -> bool {
        let Some(issue) = self.spell_issues.get(self.spell_issue_index).cloned() else {
            return false;
        };
        if issue.suggestions.is_empty() {
            return false;
        }
        let suggestion = match issue.suggestions.get(self.spell_suggestion_index) {
            Some(s) => s.clone(),
            None => return false,
        };
        match issue.target {
            SpellTarget::Subject => {
                replace_range_chars(
                    &mut self.compose_subject,
                    issue.start,
                    issue.end,
                    &suggestion,
                );
                self.compose_cursor_subject = text_char_len(&self.compose_subject);
            }
            SpellTarget::Body => {
                let mut body = self.compose_body_text();
                replace_range_chars(&mut body, issue.start, issue.end, &suggestion);
                self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &body);
                self.compose_body_desired_x = None;
                let (row, col) =
                    cursor_from_char_index(&body, issue.start + suggestion.chars().count());
                self.compose_body.set_cursor(row, col);
            }
        }
        true
    }

    fn build_inline_spell_suggest(&self) -> Option<InlineSpellSuggest> {
        let dict = spell_dictionary()?;
        let (row, col) = self.compose_body.cursor();
        let lines = self.compose_body.lines();
        let line = lines.get(row)?;
        let (start_col, end_col, word) = word_at_col(line, col)?;
        let global_start = char_index_from_row_col(&self.compose_body_text(), row, start_col);
        let global_end = char_index_from_row_col(&self.compose_body_text(), row, end_col);
        if dict.check(&word) || dict.check(&word.to_ascii_lowercase()) {
            return None;
        }
        let mut suggestions = Vec::new();
        dict.suggest(&word, &mut suggestions);
        let suggestions = suggestions
            .into_iter()
            .take(5)
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        if suggestions.is_empty() {
            return None;
        }
        Some(InlineSpellSuggest {
            start: global_start,
            end: global_end,
            suggestions,
            index: 0,
        })
    }

    fn apply_inline_spell_suggest(&mut self) -> bool {
        let Some(suggest) = self.inline_spell_suggest.clone() else {
            return false;
        };
        let Some(replacement) = suggest.suggestions.get(suggest.index) else {
            return false;
        };
        let mut body = self.compose_body_text();
        replace_range_chars(&mut body, suggest.start, suggest.end, replacement);
        self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &body);
        self.compose_body_desired_x = None;
        let (row, col) = cursor_from_char_index(&body, suggest.start + replacement.chars().count());
        self.compose_body.set_cursor(row, col);
        true
    }

    pub(crate) fn on_key_picker(&mut self, key: KeyEvent) -> bool {
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
                if self.handle_picker_filter_input(key) {
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
}
