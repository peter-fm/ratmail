use std::collections::HashSet;

use ratmail_content::extract_attachments;
use ratmail_core::{AttachmentMeta, Folder, MailStore, MessageDetail, MessageSummary};
use ratmail_mail::MailCommand;

use super::{
    App, Mode, SearchSpec, ViewMode, canonical_folder_name, clamp_cursor, from_matches_filter,
    parse_search_spec, text_char_len,
};

impl App {
    pub(crate) fn allow_account_switch_shortcut(&self) -> bool {
        matches!(self.mode, Mode::List | Mode::View | Mode::ViewFocus)
    }

    pub(crate) fn search_active(&self) -> bool {
        !self.search_query.trim().is_empty()
    }

    pub(crate) fn parse_search_spec(raw: &str) -> SearchSpec {
        parse_search_spec(raw)
    }

    pub(crate) fn search_spec_matches_attachments(&self, attachments: &[AttachmentMeta]) -> bool {
        for name in &self.search_spec.attachment_name {
            let name = name.as_str();
            let matches = attachments
                .iter()
                .any(|att| att.filename.to_ascii_lowercase().contains(name));
            if !matches {
                return false;
            }
        }
        for ty in &self.search_spec.attachment_type {
            let ty = ty.as_str();
            let matches = attachments.iter().any(|att| {
                let mime = att.mime.to_ascii_lowercase();
                let filename = att.filename.to_ascii_lowercase();
                if mime.contains(ty) {
                    return true;
                }
                if !ty.contains('/') {
                    return filename.ends_with(&format!(".{}", ty));
                }
                false
            });
            if !matches {
                return false;
            }
        }
        true
    }

    pub(crate) fn search_spec_matches_text_fields(
        &self,
        message: &MessageSummary,
        detail: Option<&MessageDetail>,
    ) -> bool {
        let needle = self.search_spec.text.trim();
        if !needle.is_empty()
            && !message.from.to_ascii_lowercase().contains(needle)
            && !message.subject.to_ascii_lowercase().contains(needle)
            && !message.preview.to_ascii_lowercase().contains(needle)
        {
            return false;
        }
        for from in &self.search_spec.from {
            if !from_matches_filter(&message.from, from) {
                return false;
            }
        }
        for subject in &self.search_spec.subject {
            if !message.subject.to_ascii_lowercase().contains(subject) {
                return false;
            }
        }
        if !self.search_spec.to.is_empty() {
            let Some(detail) = detail else {
                return false;
            };
            for to in &self.search_spec.to {
                let needle = to.as_str();
                if !detail.to.to_ascii_lowercase().contains(needle)
                    && !detail.cc.to_ascii_lowercase().contains(needle)
                {
                    return false;
                }
            }
        }
        for date in &self.search_spec.date {
            if !message.date.to_ascii_lowercase().contains(date) {
                return false;
            }
        }
        if self.search_spec.since_ts.is_some() || self.search_spec.before_ts.is_some() {
            let ts = mailparse::dateparse(&message.date).ok();
            let Some(ts) = ts else { return false };
            if let Some(since) = self.search_spec.since_ts {
                if ts < since {
                    return false;
                }
            }
            if let Some(before) = self.search_spec.before_ts {
                if ts > before {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn attachments_match_search(&self, message_id: i64) -> Option<bool> {
        if !self.search_spec.needs_attachments() {
            return Some(true);
        }
        if !self.attachment_checked.contains(&message_id) {
            return None;
        }
        let attachments = self
            .store
            .message_details
            .get(&message_id)
            .map(|d| d.attachments.as_slice())
            .unwrap_or(&[]);
        Some(self.search_spec_matches_attachments(attachments))
    }

    pub(crate) fn selected_message(&self) -> Option<&MessageSummary> {
        let messages = self.visible_messages();
        messages.get(self.message_index).copied()
    }

    pub(crate) fn selected_detail(&self) -> Option<&MessageDetail> {
        let message_id = self.selected_message()?.id;
        self.store.message_details.get(&message_id)
    }

    pub(crate) fn selected_folder(&self) -> Option<&Folder> {
        self.store.folders.get(self.folder_index)
    }

    pub(crate) fn selected_folder_is_drafts(&self) -> bool {
        self.selected_folder()
            .map(|f| canonical_folder_name(&f.name) == "Drafts")
            .unwrap_or(false)
    }

    pub(crate) fn select_inbox_if_available(&mut self) {
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

    pub(crate) fn on_folder_changed(&mut self) {
        if self.search_spec.needs_attachments() {
            self.refresh_search_attachment_queue();
            self.prefetch_search_attachments_step(8);
        }
    }

    pub(crate) fn visible_messages(&self) -> Vec<&MessageSummary> {
        let folder_id = self.selected_folder().map(|f| f.id);
        let messages: Vec<&MessageSummary> = self
            .store
            .messages
            .iter()
            .filter(|msg| Some(msg.folder_id) == folder_id)
            .filter(|msg| {
                let detail = self.store.message_details.get(&msg.id);
                if !self.search_spec_matches_text_fields(msg, detail) {
                    return false;
                }
                match self.attachments_match_search(msg.id) {
                    Some(true) => true,
                    Some(false) => false,
                    None => false,
                }
            })
            .collect();
        // Preserve backend ordering (already sorted by date).
        messages
    }

    pub(crate) fn prune_selected_messages(&mut self) {
        if self.selected_message_ids.is_empty() {
            return;
        }
        let visible: HashSet<i64> = self.visible_messages().iter().map(|m| m.id).collect();
        self.selected_message_ids.retain(|id| visible.contains(id));
        if self.selected_message_ids.is_empty() {
            self.status_message = None;
        }
    }

    pub(crate) fn clear_selected_messages(&mut self) {
        self.selected_message_ids.clear();
        self.bulk_action_ids.clear();
        self.confirm_delete_ids.clear();
        self.status_message = None;
    }

    pub(crate) fn open_search_overlay(&mut self) {
        self.overlay_return = self.mode;
        self.mode = Mode::OverlaySearch;
        self.search_cursor = text_char_len(&self.search_query);
    }

    pub(crate) fn clear_search(&mut self) {
        if self.search_query.trim().is_empty() {
            return;
        }
        self.search_query.clear();
        self.search_cursor = 0;
        self.on_search_updated();
    }

    pub(crate) fn on_search_updated(&mut self) {
        self.search_cursor = clamp_cursor(self.search_cursor, &self.search_query);
        self.search_spec = Self::parse_search_spec(&self.search_query);
        if self.search_spec.needs_raw() {
            self.refresh_search_attachment_queue();
            self.prefetch_search_attachments_step(8);
        } else {
            self.search_attachment_queue.clear();
        }
        self.message_index = 0;
        self.view_scroll = 0;
        self.prune_selected_messages();
        if self.view_mode == ViewMode::Rendered {
            self.schedule_render();
        }
        self.ensure_text_cache_for_selected();
    }

    pub(crate) fn refresh_search_attachment_queue(&mut self) {
        if !self.search_spec.needs_raw() {
            self.search_attachment_queue.clear();
            return;
        }
        let folder_id = self.selected_folder().map(|f| f.id);
        self.search_attachment_queue = self
            .store
            .messages
            .iter()
            .filter(|msg| Some(msg.folder_id) == folder_id)
            .map(|msg| msg.id)
            .filter(|id| !self.attachment_checked.contains(id))
            .collect();
    }

    pub(crate) fn set_message_attachments(
        &mut self,
        message_id: i64,
        attachments: Vec<AttachmentMeta>,
    ) {
        self.attachment_cache
            .insert(message_id, attachments.clone());
        if let Some(detail) = self.store.message_details.get_mut(&message_id) {
            detail.attachments = attachments;
        } else if let Some(summary) = self.store.messages.iter().find(|m| m.id == message_id) {
            self.store.message_details.insert(
                message_id,
                MessageDetail {
                    id: message_id,
                    subject: summary.subject.clone(),
                    from: summary.from.clone(),
                    to: String::new(),
                    cc: String::new(),
                    date: summary.date.clone(),
                    body: String::new(),
                    links: Vec::new(),
                    attachments,
                },
            );
        }
        self.attachment_checked.insert(message_id);
    }

    pub(crate) fn reapply_attachment_cache(&mut self) {
        let valid_ids: HashSet<i64> = self.store.messages.iter().map(|m| m.id).collect();
        self.attachment_cache
            .retain(|message_id, _| valid_ids.contains(message_id));
        for (message_id, attachments) in self.attachment_cache.clone() {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.attachments = attachments.clone();
                continue;
            }
            if attachments.is_empty() {
                continue;
            }
            if let Some(summary) = self.store.messages.iter().find(|m| m.id == message_id) {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        to: String::new(),
                        cc: String::new(),
                        date: summary.date.clone(),
                        body: String::new(),
                        links: Vec::new(),
                        attachments,
                    },
                );
            }
        }
    }

    pub(crate) fn prefetch_search_attachments_step(&mut self, limit: usize) {
        if !self.search_spec.needs_raw() || self.search_attachment_queue.is_empty() {
            return;
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());
        let mut remaining = limit;
        while remaining > 0 {
            let Some(message_id) = self.search_attachment_queue.pop_front() else {
                break;
            };
            if self.attachment_checked.contains(&message_id) {
                continue;
            }
            let has_raw = self.runtime().block_on(async {
                self.store_handle
                    .get_raw_body(message_id)
                    .await
                    .ok()
                    .flatten()
            });
            if let Some(raw) = has_raw {
                let attachments = match extract_attachments(&raw) {
                    Ok(a) => a,
                    Err(_) => Vec::new(),
                };
                self.set_message_attachments(message_id, attachments);
                remaining = remaining.saturating_sub(1);
                continue;
            }
            if !self.imap_enabled {
                self.attachment_checked.insert(message_id);
                remaining = remaining.saturating_sub(1);
                continue;
            }
            if self.pending_body_fetch.contains(&message_id) {
                continue;
            }
            let summary = self.store.messages.iter().find(|m| m.id == message_id);
            if let (Some(summary), Some(folder_name)) = (summary, folder_name.clone()) {
                if let Some(uid) = summary.imap_uid {
                    self.pending_body_fetch.insert(message_id);
                    let _ = self.engine.send(MailCommand::FetchMessageBody {
                        message_id,
                        folder_name,
                        uid,
                    });
                }
            }
            remaining = remaining.saturating_sub(1);
        }
    }

    pub(crate) fn prefetch_visible_attachments(&mut self, message_ids: &[i64], limit: usize) {
        if message_ids.is_empty() {
            return;
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());
        let mut remaining = limit;
        for &message_id in message_ids {
            if remaining == 0 {
                break;
            }
            if self.attachment_checked.contains(&message_id) {
                continue;
            }
            let raw = self.runtime().block_on(async {
                self.store_handle
                    .get_raw_body(message_id)
                    .await
                    .ok()
                    .flatten()
            });
            if let Some(raw) = raw {
                let attachments = match extract_attachments(&raw) {
                    Ok(a) => a,
                    Err(_) => Vec::new(),
                };
                self.set_message_attachments(message_id, attachments);
                remaining = remaining.saturating_sub(1);
                continue;
            }
            if !self.imap_enabled {
                self.attachment_checked.insert(message_id);
                remaining = remaining.saturating_sub(1);
                continue;
            }
            if self.pending_body_fetch.contains(&message_id) {
                continue;
            }
            let summary = self.store.messages.iter().find(|m| m.id == message_id);
            if let (Some(summary), Some(folder_name)) = (summary, folder_name.clone()) {
                if let Some(uid) = summary.imap_uid {
                    self.pending_body_fetch.insert(message_id);
                    let _ = self.engine.send(MailCommand::FetchMessageBody {
                        message_id,
                        folder_name,
                        uid,
                    });
                    remaining = remaining.saturating_sub(1);
                }
            }
        }
    }

    pub(crate) fn toggle_select_current(&mut self) {
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

    pub(crate) fn active_message_ids(&self) -> Vec<i64> {
        if !self.selected_message_ids.is_empty() {
            return self.selected_message_ids.iter().copied().collect();
        }
        self.selected_message()
            .map(|m| vec![m.id])
            .unwrap_or_default()
    }

    pub(crate) fn default_move_folder_index(&self) -> usize {
        let current_id = self.selected_folder().map(|f| f.id);
        self.store
            .folders
            .iter()
            .position(|f| Some(f.id) != current_id)
            .unwrap_or(self.folder_index)
    }
}
