use ratmail_content::extract_display;
use ratmail_core::{DEFAULT_TEXT_WIDTH, MailStore};

use super::{
    App, ComposeFocus, ComposeVimMode, Mode, StoreUpdate, build_forward, build_reply,
    compose_buffer_from_body, draft_headers_from_raw, extract_email, parse_from_addrs,
    text_char_len,
};

impl App {
    pub(crate) fn start_compose_new(&mut self) {
        self.compose_to.clear();
        if self.compose_from.trim().is_empty() {
            self.compose_from = parse_from_addrs(&self.store.account.address)
                .into_iter()
                .next()
                .unwrap_or_else(|| extract_email(&self.store.account.address));
        }
        self.compose_cc.clear();
        self.compose_bcc.clear();
        self.compose_subject.clear();
        self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), "");
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::From;
        self.compose_cursor_to = 0;
        self.compose_cursor_from = text_char_len(&self.compose_from);
        self.compose_cursor_cc = 0;
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = 0;
        self.compose_body_desired_x = None;
        self.reset_compose_vim_state();
        self.mode = Mode::Compose;
    }

    pub(crate) fn start_compose_to(
        &mut self,
        to: String,
        subject: Option<String>,
        body: Option<String>,
    ) {
        self.start_compose_new();
        self.compose_to = to;
        if let Some(subject) = subject {
            self.compose_subject = subject;
        }
        if let Some(body) = body {
            self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &body);
        }
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_from = text_char_len(&self.compose_from);
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_body_desired_x = None;
        self.compose_focus = if !self.compose_body.is_empty() {
            ComposeFocus::Body
        } else if !self.compose_subject.is_empty() {
            ComposeFocus::Subject
        } else if !self.compose_to.is_empty() {
            ComposeFocus::To
        } else {
            ComposeFocus::From
        };
        self.reset_compose_vim_state();
    }

    pub(crate) fn start_compose_reply(&mut self, reply_all: bool) {
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
        if self.compose_from.trim().is_empty() {
            self.compose_from = parse_from_addrs(&self.store.account.address)
                .into_iter()
                .next()
                .unwrap_or_else(|| extract_email(&self.store.account.address));
        }
        self.compose_cc = cc;
        self.compose_bcc.clear();
        self.compose_subject = subject;
        self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &quote);
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::Body;
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_from = text_char_len(&self.compose_from);
        self.compose_cursor_cc = text_char_len(&self.compose_cc);
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_body_desired_x = None;
        self.reset_compose_vim_state();
        self.mode = Mode::Compose;
    }

    pub(crate) fn start_compose_forward(&mut self) {
        let raw = self.selected_message().and_then(|msg| {
            self.runtime()
                .block_on(async { self.store_handle.get_raw_body(msg.id).await.ok().flatten() })
        });
        let (subject, body) = build_forward(self.selected_detail(), raw.as_deref());
        self.compose_to.clear();
        if self.compose_from.trim().is_empty() {
            self.compose_from = parse_from_addrs(&self.store.account.address)
                .into_iter()
                .next()
                .unwrap_or_else(|| extract_email(&self.store.account.address));
        }
        self.compose_cc.clear();
        self.compose_bcc.clear();
        self.compose_subject = subject;
        self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &body);
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_focus = ComposeFocus::From;
        self.compose_cursor_to = 0;
        self.compose_cursor_from = text_char_len(&self.compose_from);
        self.compose_cursor_cc = 0;
        self.compose_cursor_bcc = 0;
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_body_desired_x = None;
        self.reset_compose_vim_state();
        self.mode = Mode::Compose;
    }

    pub(crate) fn start_compose_draft(&mut self) {
        let Some(summary) = self.selected_message() else {
            return;
        };
        let raw = self.runtime().block_on(async {
            self.store_handle
                .get_raw_body(summary.id)
                .await
                .ok()
                .flatten()
        });
        let (from, to, cc, bcc, subject) = match raw.as_deref() {
            Some(raw) => draft_headers_from_raw(raw),
            None => (
                self.compose_from.clone(),
                String::new(),
                String::new(),
                String::new(),
                summary.subject.clone(),
            ),
        };
        let body = if let Some(detail) = self.selected_detail() {
            detail.body.clone()
        } else if let Some(raw) = raw.as_deref() {
            extract_display(raw, DEFAULT_TEXT_WIDTH as usize)
                .ok()
                .map(|d| d.text)
                .unwrap_or_default()
        } else {
            String::new()
        };

        self.compose_from = parse_from_addrs(&from)
            .into_iter()
            .next()
            .unwrap_or_else(|| extract_email(&from));
        self.compose_to = to;
        self.compose_cc = cc;
        self.compose_bcc = bcc;
        self.compose_subject = subject;
        self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), &body);
        self.compose_quote.clear();
        self.compose_attachments.clear();
        self.compose_cursor_from = text_char_len(&self.compose_from);
        self.compose_cursor_to = text_char_len(&self.compose_to);
        self.compose_cursor_cc = text_char_len(&self.compose_cc);
        self.compose_cursor_bcc = text_char_len(&self.compose_bcc);
        self.compose_cursor_subject = text_char_len(&self.compose_subject);
        self.compose_body_desired_x = None;
        self.compose_focus = if !self.compose_body.is_empty() {
            ComposeFocus::Body
        } else if !self.compose_subject.is_empty() {
            ComposeFocus::Subject
        } else if !self.compose_to.is_empty()
            || !self.compose_cc.is_empty()
            || !self.compose_bcc.is_empty()
        {
            ComposeFocus::To
        } else if !self.compose_from.is_empty() {
            ComposeFocus::From
        } else {
            ComposeFocus::Body
        };
        self.reset_compose_vim_state();
        self.mode = Mode::Compose;
    }

    pub(crate) fn compose_body_text(&self) -> String {
        self.compose_body.text().to_string()
    }

    pub(crate) fn compose_has_content(&self) -> bool {
        !self.compose_to.trim().is_empty()
            || !self.compose_cc.trim().is_empty()
            || !self.compose_bcc.trim().is_empty()
            || !self.compose_subject.trim().is_empty()
            || !self.compose_body_text().trim().is_empty()
            || !self.compose_quote.trim().is_empty()
            || !self.compose_attachments.is_empty()
    }

    pub(crate) fn compose_body_for_save(&self) -> String {
        let mut body = self.compose_body_text();
        if self.compose_quote.is_empty() {
            return body;
        }
        body.push_str(&self.compose_quote);
        body
    }

    pub(crate) fn reset_compose_vim_state(&mut self) {
        self.compose_vim_pending = None;
        self.compose_vim_mode = if self.compose_vim_enabled {
            ComposeVimMode::Normal
        } else {
            ComposeVimMode::Insert
        };
    }

    pub(crate) fn open_confirm_draft(&mut self) {
        self.mode = Mode::OverlayConfirmDraft;
    }

    pub(crate) fn discard_compose(&mut self) {
        self.start_compose_new();
        self.mode = Mode::View;
    }

    pub(crate) fn save_compose_draft(&mut self) {
        let account_id = self.store.account.id;
        let body = self.compose_body_for_save();
        self.queue_store_update_reliable(StoreUpdate::SaveDraft {
            account_id,
            from_addr: self.compose_from.clone(),
            to: self.compose_to.clone(),
            cc: self.compose_cc.clone(),
            bcc: self.compose_bcc.clone(),
            subject: self.compose_subject.clone(),
            body,
        });
        if self.compose_attachments.is_empty() {
            self.set_status("Draft saved");
        } else {
            self.set_status("Draft saved (attachments not saved)");
        }
        self.start_compose_new();
        self.mode = Mode::View;
    }
}
