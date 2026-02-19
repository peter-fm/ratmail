use ratmail_content::{extract_attachments, extract_display};
use ratmail_core::{DEFAULT_TEXT_WIDTH, LinkInfo, MailStore, MessageDetail};
use ratmail_mail::MailCommand;

use super::{App, Mode, StoreUpdate, cc_from_raw, to_from_raw};

impl App {
    pub(crate) fn open_bulk_action_overlay(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        self.bulk_action_ids = ids;
        self.bulk_done_return = self.mode;
        self.overlay_return = self.mode;
        self.mode = Mode::OverlayBulkAction;
    }

    pub(crate) fn open_move_overlay(&mut self, ids: Vec<i64>, return_mode: Mode) {
        if ids.is_empty() || self.store.folders.is_empty() {
            return;
        }
        self.bulk_action_ids = ids;
        self.bulk_folder_index = self.default_move_folder_index();
        if return_mode != Mode::OverlayBulkAction {
            self.bulk_done_return = return_mode;
        }
        self.overlay_return = return_mode;
        self.mode = Mode::OverlayBulkMove;
    }

    pub(crate) fn open_confirm_delete(&mut self, ids: Vec<i64>, return_mode: Mode) {
        if ids.is_empty() {
            return;
        }
        self.confirm_delete_ids = ids;
        self.confirm_delete_return = return_mode;
        self.overlay_return = return_mode;
        self.mode = Mode::OverlayConfirmDelete;
    }

    pub(crate) fn open_confirm_link(&mut self, link: LinkInfo, external: bool, return_mode: Mode) {
        self.confirm_link = Some(link);
        self.confirm_link_external = external;
        self.confirm_link_return = return_mode;
        self.mode = Mode::OverlayConfirmLink;
    }

    pub(crate) fn collect_imap_uids(&self, ids: &[i64]) -> Vec<u32> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(uid) = self
                .store
                .messages
                .iter()
                .find(|m| m.id == *id)
                .and_then(|m| m.imap_uid)
                .and_then(|uid| u32::try_from(uid).ok())
            {
                out.push(uid);
            }
        }
        out
    }

    pub(crate) fn queue_move_messages(&mut self, ids: Vec<i64>, target_folder_id: i64) {
        if ids.is_empty() {
            return;
        }
        if Some(target_folder_id) == self.selected_folder().map(|f| f.id) {
            self.status_message = Some("Already in that folder".to_string());
            return;
        }
        let account_id = self.store.account.id;
        let refresh_folder_id = self.selected_folder().map(|f| f.id).unwrap_or(1);
        self.queue_store_update_reliable(StoreUpdate::MoveMessages {
            account_id,
            message_ids: ids.clone(),
            target_folder_id,
            refresh_folder_id,
        });

        if self.imap_enabled {
            let uids = self.collect_imap_uids(&ids);
            if !uids.is_empty() {
                if let (Some(src_folder), Some(dst_folder)) = (
                    self.selected_folder().map(|f| f.name.clone()),
                    self.store
                        .folders
                        .iter()
                        .find(|f| f.id == target_folder_id)
                        .map(|f| f.name.clone()),
                ) {
                    let _ = self.engine.send(MailCommand::MoveMessages {
                        folder_name: src_folder,
                        target_folder: dst_folder,
                        uids,
                    });
                }
            }
        }

        self.clear_selected_messages();
        self.status_message = Some(format!(
            "Moved {} message{}",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        ));
    }

    pub(crate) fn queue_delete_messages(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        let account_id = self.store.account.id;
        let refresh_folder_id = self.selected_folder().map(|f| f.id).unwrap_or(1);
        self.queue_store_update_reliable(StoreUpdate::DeleteMessages {
            account_id,
            message_ids: ids.clone(),
            refresh_folder_id,
        });

        if self.imap_enabled {
            let uids = self.collect_imap_uids(&ids);
            if !uids.is_empty() {
                if let Some(folder_name) = self.selected_folder().map(|f| f.name.clone()) {
                    let _ = self
                        .engine
                        .send(MailCommand::DeleteMessages { folder_name, uids });
                }
            }
        }

        self.clear_selected_messages();
        self.status_message = Some(format!(
            "Deleted {} message{}",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        ));
    }

    pub(crate) fn queue_mark_read_messages(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        let account_id = self.store.account.id;
        let refresh_folder_id = self.selected_folder().map(|f| f.id).unwrap_or(1);
        self.queue_store_update_reliable(StoreUpdate::SetMessagesUnread {
            account_id,
            message_ids: ids,
            unread: false,
            refresh_folder_id,
        });

        self.clear_selected_messages();
        self.status_message = Some("Marked selected messages as read".to_string());
    }

    pub(crate) fn ensure_text_cache_for_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
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
                let to = to_from_raw(&raw);
                let cc = cc_from_raw(&raw);
                store_handle
                    .upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, &display.text)
                    .await?;
                Ok::<_, anyhow::Error>(Some((display, to, cc)))
            } else {
                Ok::<_, anyhow::Error>(None)
            }
        });

        if let Ok(Some((display, to, cc))) = result {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.body = display.text;
                detail.links = display.links;
                if let Some(to) = &to {
                    detail.to = to.clone();
                }
                if let Some(cc) = &cc {
                    detail.cc = cc.clone();
                }
            } else if let Some(summary) = self.selected_message() {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        to: to.clone().unwrap_or_default(),
                        cc: cc.clone().unwrap_or_default(),
                        date: summary.date.clone(),
                        body: display.text,
                        links: display.links,
                        attachments: Vec::new(),
                    },
                );
            }
            if let Some(to) = to.as_deref() {
                let _ = self
                    .runtime()
                    .block_on(self.store_handle.update_message_to(message_id, to));
            }
            if let Some(cc) = cc.as_deref() {
                let _ = self
                    .runtime()
                    .block_on(self.store_handle.update_message_cc(message_id, cc));
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

    pub(crate) fn ensure_raw_body_for_render(&mut self, message_id: i64) -> bool {
        let store_handle = self.store_handle.clone();
        let has_raw = self.runtime().block_on(async move {
            store_handle
                .get_raw_body(message_id)
                .await
                .ok()
                .flatten()
                .is_some()
        });
        if has_raw {
            return true;
        }
        if self.imap_enabled && !self.pending_body_fetch.contains(&message_id) {
            let folder_name = self.selected_folder().map(|f| f.name.clone());
            if let (Some(uid), Some(folder_name)) = (
                self.selected_message().and_then(|m| m.imap_uid),
                folder_name,
            ) {
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

    pub(crate) fn prefetch_raw_bodies(&mut self, count: usize) {
        if !self.imap_enabled {
            return;
        }
        let folder_name = self.selected_folder().map(|f| f.name.clone());
        let Some(folder_name) = folder_name else {
            return;
        };
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
                store_handle
                    .get_raw_body(message_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some()
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

    pub(crate) fn ensure_attachments_for_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
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
            self.set_message_attachments(message_id, attachments);
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
}
