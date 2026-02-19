use std::time::{Duration, Instant};

use ratmail_content::{extract_attachments, extract_display};
use ratmail_core::{DEFAULT_TEXT_WIDTH, Folder, MessageDetail, MessageSummary, log_debug};
use ratmail_mail::{ImapErrorContext, MailCommand, MailEvent};

use super::{
    App, ComposeFocus, Focus, Mode, StoreUpdate, ViewMode, build_sync_update, cc_from_raw,
    compose_buffer_from_body, to_from_raw,
};

impl App {
    pub(crate) fn request_sync_selected_folder(&mut self) {
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

    pub(crate) fn request_backfill_selected_folder(&mut self) {
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

    pub(crate) fn on_event(&mut self, event: MailEvent) {
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
                    self.compose_body = compose_buffer_from_body(self.ui_theme.clone(), "");
                    self.compose_quote.clear();
                    self.compose_attachments.clear();
                    self.compose_focus = ComposeFocus::Body;
                    self.compose_cursor_to = 0;
                    self.compose_cursor_cc = 0;
                    self.compose_cursor_bcc = 0;
                    self.compose_cursor_subject = 0;
                    self.compose_body_desired_x = None;
                    self.reset_compose_vim_state();
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
                self.queue_store_update(StoreUpdate::Folders {
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
                self.queue_store_update(StoreUpdate::AppendMessages {
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
                let parsed_to = to_from_raw(&raw);
                let parsed_cc = cc_from_raw(&raw);
                let cached_text =
                    if let Ok(display) = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize) {
                        if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                            detail.body = display.text.clone();
                            detail.links = display.links.clone();
                            if let Some(to) = &parsed_to {
                                detail.to = to.clone();
                            }
                            if let Some(cc) = &parsed_cc {
                                detail.cc = cc.clone();
                            }
                        } else if let Some(summary) =
                            self.store.messages.iter().find(|m| m.id == message_id)
                        {
                            self.store.message_details.insert(
                                message_id,
                                MessageDetail {
                                    id: message_id,
                                    subject: summary.subject.clone(),
                                    from: summary.from.clone(),
                                    to: parsed_to.clone().unwrap_or_default(),
                                    cc: parsed_cc.clone().unwrap_or_default(),
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
                if let Ok(attachments) = extract_attachments(&raw) {
                    self.set_message_attachments(message_id, attachments);
                }
                if let Some(to) = parsed_to.as_deref() {
                    let _ = self
                        .runtime()
                        .block_on(self.store_handle.update_message_to(message_id, to));
                }
                if let Some(cc) = parsed_cc.as_deref() {
                    let _ = self
                        .runtime()
                        .block_on(self.store_handle.update_message_cc(message_id, cc));
                }
                self.queue_store_update(StoreUpdate::RawBody {
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
            MailEvent::ImapError { context, reason } => {
                match context {
                    ImapErrorContext::SyncAll | ImapErrorContext::SyncFolder { .. } => {
                        self.imap_pending = self.imap_pending.saturating_sub(1);
                    }
                    ImapErrorContext::FetchBody { message_id, .. } => {
                        self.pending_body_fetch.remove(&message_id);
                    }
                    ImapErrorContext::MoveMessages { .. }
                    | ImapErrorContext::DeleteMessages { .. } => {}
                }
                let context_label = imap_error_context_label(&context);
                self.imap_status = Some(format!("IMAP error ({}): {}", context_label, reason));
            }
            _ => {}
        }
    }
}

fn imap_error_context_label(context: &ImapErrorContext) -> String {
    match context {
        ImapErrorContext::SyncAll => "sync-all".to_string(),
        ImapErrorContext::SyncFolder { folder_name } => {
            format!("sync-folder {}", folder_name)
        }
        ImapErrorContext::FetchBody {
            message_id,
            folder_name,
            uid,
        } => format!(
            "fetch-body id={} folder={} uid={}",
            message_id, folder_name, uid
        ),
        ImapErrorContext::MoveMessages {
            folder_name,
            target_folder,
            count,
        } => format!(
            "move-messages {} -> {} ({})",
            folder_name, target_folder, count
        ),
        ImapErrorContext::DeleteMessages { folder_name, count } => {
            format!("delete-messages {} ({})", folder_name, count)
        }
    }
}

#[cfg(test)]
mod tests {
    use ratmail_mail::ImapErrorContext;

    use crate::sync_mod::imap_error_context_label;

    #[test]
    fn imap_error_context_label_formats_fetch_body() {
        let label = imap_error_context_label(&ImapErrorContext::FetchBody {
            message_id: 42,
            folder_name: "INBOX".to_string(),
            uid: 99,
        });
        assert_eq!(label, "fetch-body id=42 folder=INBOX uid=99");
    }

    #[test]
    fn imap_error_context_label_formats_move_messages() {
        let label = imap_error_context_label(&ImapErrorContext::MoveMessages {
            folder_name: "INBOX".to_string(),
            target_folder: "Archive".to_string(),
            count: 3,
        });
        assert_eq!(label, "move-messages INBOX -> Archive (3)");
    }

    #[test]
    fn imap_error_context_label_formats_delete_messages() {
        let label = imap_error_context_label(&ImapErrorContext::DeleteMessages {
            folder_name: "Spam".to_string(),
            count: 2,
        });
        assert_eq!(label, "delete-messages Spam (2)");
    }
}
