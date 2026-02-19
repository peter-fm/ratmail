use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mime_guess::MimeGuess;
use ratatui::widgets::{Block, Borders};
use ratatui_explorer::{FileExplorer, Input as ExplorerInput, Theme as ExplorerTheme};

use ratmail_content::extract_attachment_data;
use ratmail_core::MailStore;
use ratmail_mail::MailCommand;

use super::{
    App, ComposeAttachment, PICKER_IMAGE_PREVIEW_MAX_BYTES, PICKER_PDF_PREVIEW_MAX_BYTES,
    PICKER_PREVIEW_MAX_BYTES, PickerFocus, PickerMode, PickerPreviewKind, format_size,
    picker_meta_lines, render_pdf_first_page, safe_filename, text_char_len,
    text_preview_from_bytes, zip_directory,
};

impl App {
    pub(crate) fn start_picker(&mut self, mode: PickerMode) {
        let theme = ExplorerTheme::default()
            .with_block(
                Block::default()
                    .borders(Borders::ALL)
                    .style(self.ui_theme.base)
                    .border_style(self.ui_theme.border),
            )
            .add_default_title();
        let mut picker = match FileExplorer::with_theme(theme) {
            Ok(picker) => picker,
            Err(err) => {
                self.status_message = Some(format!("Picker error: {}", err));
                return;
            }
        };
        if let Ok(home) = std::env::var("HOME") {
            let _ = picker.set_cwd(home);
        }
        self.picker = Some(picker);
        self.picker_mode = Some(mode);
        self.picker_focus = PickerFocus::Explorer;
        self.picker_filename.clear();
        self.picker_cursor = 0;
        self.picker_filter.clear();
        if let Some(picker) = self.picker.as_mut() {
            picker.clear_filter();
        }
        self.reset_picker_preview();
        if let Some(PickerMode::Save { filename, .. }) = &self.picker_mode {
            self.picker_filename = filename.clone();
            self.picker_cursor = text_char_len(&self.picker_filename);
        }
        self.refresh_picker_preview();
    }

    pub(crate) fn close_picker(&mut self, status: &str) {
        self.picker_mode = None;
        self.picker = None;
        self.picker_filename.clear();
        self.picker_cursor = 0;
        self.picker_filter.clear();
        self.picker_focus = PickerFocus::Explorer;
        self.reset_picker_preview();
        self.status_message = Some(status.to_string());
    }

    pub(crate) fn handle_picker_navigation(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let input = match key.code {
            KeyCode::Char('j') | KeyCode::Down => ExplorerInput::Down,
            KeyCode::Char('k') | KeyCode::Up => ExplorerInput::Up,
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    ExplorerInput::ToggleShowHidden
                } else {
                    ExplorerInput::Left
                }
            }
            KeyCode::Char('l') | KeyCode::Right => ExplorerInput::Right,
            KeyCode::Home => ExplorerInput::Home,
            KeyCode::End => ExplorerInput::End,
            KeyCode::PageUp => ExplorerInput::PageUp,
            KeyCode::PageDown => ExplorerInput::PageDown,
            _ => ExplorerInput::None,
        };
        if input != ExplorerInput::None {
            let _ = picker.handle(input);
            self.refresh_picker_preview();
        }
    }

    pub(crate) fn handle_picker_filter_input(&mut self, key: KeyEvent) -> bool {
        let Some(picker) = self.picker.as_mut() else {
            return false;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(key.code, KeyCode::Char('u')) && !self.picker_filter.is_empty() {
                self.picker_filter.clear();
                picker.clear_filter();
                self.refresh_picker_preview();
                return true;
            }
            return false;
        }
        match key.code {
            KeyCode::Backspace | KeyCode::Delete => {
                if !self.picker_filter.is_empty() {
                    self.picker_filter.pop();
                    picker.set_filter(self.picker_filter.clone());
                    self.refresh_picker_preview();
                    return true;
                }
            }
            KeyCode::Char(c) => {
                if !c.is_control() {
                    if self.picker_filter.is_empty() && matches!(c, 'h' | 'j' | 'k' | 'l') {
                        return false;
                    }
                    self.picker_filter.push(c);
                    picker.set_filter(self.picker_filter.clone());
                    self.refresh_picker_preview();
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    pub(crate) fn refresh_picker_preview(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let current = picker.current();
        if current.is_placeholder() {
            self.picker_preview_path = None;
            self.picker_preview_kind = PickerPreviewKind::Meta;
            self.picker_preview_text.clear();
            self.picker_preview_meta.clear();
            self.picker_preview_image = None;
            self.picker_preview_protocol = None;
            self.picker_preview_error = Some("No matching files.".to_string());
            return;
        }
        let path = current.path().clone();
        if self.picker_preview_path.as_ref() == Some(&path) {
            return;
        }

        self.picker_preview_path = Some(path.clone());
        self.picker_preview_kind = PickerPreviewKind::Meta;
        self.picker_preview_text.clear();
        self.picker_preview_meta.clear();
        self.picker_preview_image = None;
        self.picker_preview_protocol = None;
        self.picker_preview_error = None;

        let mime = MimeGuess::from_path(&path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        self.picker_preview_meta = picker_meta_lines(&path, Some(mime.as_str()));

        if current.is_dir() {
            return;
        }

        let meta = std::fs::metadata(&path).ok();
        let file_size = meta.as_ref().map(|m| m.len());

        let is_pdf = mime.eq_ignore_ascii_case("application/pdf")
            || path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false);
        if is_pdf {
            if self.image_picker.is_none() {
                self.picker_preview_kind = PickerPreviewKind::Meta;
                self.picker_preview_error =
                    Some("PDF preview requires terminal image support.".to_string());
                return;
            }
            if file_size.is_some_and(|size| size > PICKER_PDF_PREVIEW_MAX_BYTES) {
                self.picker_preview_kind = PickerPreviewKind::Meta;
                self.picker_preview_error = Some("PDF too large for preview.".to_string());
                return;
            }
            if !self.pdf_preview_available() {
                self.picker_preview_kind = PickerPreviewKind::Meta;
                self.picker_preview_error =
                    Some("PDF preview requires pdftoppm (poppler-utils) in PATH.".to_string());
                return;
            }
            match render_pdf_first_page(&path) {
                Ok(img) => {
                    self.picker_preview_image = Some(img);
                    self.picker_preview_kind = PickerPreviewKind::PdfImage;
                }
                Err(err) => {
                    self.picker_preview_kind = PickerPreviewKind::Meta;
                    self.picker_preview_error = Some(format!("PDF preview failed: {}", err));
                }
            }
            return;
        }

        if mime.starts_with("image/") {
            if self.image_picker.is_none() {
                self.picker_preview_kind = PickerPreviewKind::Meta;
                self.picker_preview_error =
                    Some("Image preview requires terminal image support.".to_string());
                return;
            }
            if file_size.is_some_and(|size| size > PICKER_IMAGE_PREVIEW_MAX_BYTES) {
                self.picker_preview_kind = PickerPreviewKind::Meta;
                self.picker_preview_error = Some("Image too large for preview.".to_string());
                return;
            }
            match std::fs::read(&path)
                .ok()
                .and_then(|bytes| image::load_from_memory(&bytes).ok())
            {
                Some(img) => {
                    self.picker_preview_image = Some(img);
                    self.picker_preview_kind = PickerPreviewKind::Image;
                }
                None => {
                    self.picker_preview_kind = PickerPreviewKind::Meta;
                    self.picker_preview_error = Some("Unable to decode image.".to_string());
                }
            }
            return;
        }

        let Ok(file) = std::fs::File::open(&path) else {
            self.picker_preview_kind = PickerPreviewKind::Error;
            self.picker_preview_error = Some("Unable to read file.".to_string());
            return;
        };
        let mut bytes = Vec::new();
        let read_limit = (PICKER_PREVIEW_MAX_BYTES as u64).saturating_add(1);
        let read_result = file.take(read_limit).read_to_end(&mut bytes).is_ok();
        if !read_result {
            self.picker_preview_kind = PickerPreviewKind::Error;
            self.picker_preview_error = Some("Unable to read file.".to_string());
            return;
        }
        let truncated_bytes = if bytes.len() > PICKER_PREVIEW_MAX_BYTES {
            bytes.truncate(PICKER_PREVIEW_MAX_BYTES);
            true
        } else {
            false
        };
        if let Some(text) = text_preview_from_bytes(&bytes, truncated_bytes) {
            self.picker_preview_text = text;
            self.picker_preview_kind = PickerPreviewKind::Text;
        } else {
            self.picker_preview_kind = PickerPreviewKind::Meta;
        }
    }

    pub(crate) fn reset_picker_preview(&mut self) {
        self.picker_preview_path = None;
        self.picker_preview_kind = PickerPreviewKind::Empty;
        self.picker_preview_text.clear();
        self.picker_preview_meta.clear();
        self.picker_preview_image = None;
        self.picker_preview_protocol = None;
        self.picker_preview_error = None;
    }

    pub(crate) fn pdf_preview_available(&mut self) -> bool {
        if let Some(available) = self.picker_pdf_preview_available {
            return available;
        }
        let available = Command::new("pdftoppm")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.picker_pdf_preview_available = Some(available);
        available
    }

    pub(crate) fn picker_selected_dir(&self) -> Option<PathBuf> {
        let picker = self.picker.as_ref()?;
        let current = picker.current();
        if current.is_dir() {
            return Some(current.path().clone());
        }
        current
            .path()
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| Some(picker.cwd().clone()))
    }

    pub(crate) fn confirm_attach_selection(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let current = picker.current();
        if current.is_placeholder() {
            self.status_message = Some("No matching files to attach.".to_string());
            return;
        }
        let target = current.path().clone();
        match self.add_compose_attachment_from_path(&target) {
            Ok(added) => {
                self.close_picker(&format!(
                    "Attached {} ({})",
                    added.filename,
                    format_size(added.size)
                ));
            }
            Err(err) => {
                self.status_message = Some(format!("Attach failed: {}", err));
            }
        }
    }

    pub(crate) fn open_selected_attachment(&mut self) {
        let Some(detail) = self.selected_detail() else {
            return;
        };
        if detail.attachments.is_empty() || self.attach_index >= detail.attachments.len() {
            return;
        }
        let message_id = detail.id;
        let filename = detail.attachments[self.attach_index].filename.clone();
        match self.save_attachment_to_temp(message_id, self.attach_index, &filename) {
            Ok(path) => {
                let _ = open::that(&path);
                self.status_message = Some(format!("Opened {}", path.display()));
            }
            Err(err) => {
                if err.to_string() != "message body not cached" {
                    self.status_message = Some(format!("Open failed: {}", err));
                }
            }
        }
    }

    pub(crate) fn prompt_save_selected_attachment(&mut self) {
        let Some(detail) = self.selected_detail() else {
            return;
        };
        if detail.attachments.is_empty() || self.attach_index >= detail.attachments.len() {
            return;
        }
        let filename = detail.attachments[self.attach_index].filename.clone();
        let message_id = detail.id;
        self.start_picker(PickerMode::Save {
            message_id,
            attachment_index: self.attach_index,
            filename,
        });
    }

    pub(crate) fn add_compose_attachment_from_path(
        &mut self,
        path: &Path,
    ) -> Result<ComposeAttachment> {
        if !path.exists() {
            return Err(anyhow::anyhow!("file not found"));
        }
        let attachment = if path.is_dir() {
            self.build_zip_attachment(path)?
        } else {
            self.build_file_attachment(path)?
        };
        self.compose_attachments.push(attachment.clone());
        Ok(attachment)
    }

    pub(crate) fn build_file_attachment(&self, path: &Path) -> Result<ComposeAttachment> {
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("attachment")
            .to_string();
        let data = std::fs::read(path)?;
        let size = data.len();
        let mime = MimeGuess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        Ok(ComposeAttachment {
            filename,
            mime,
            size,
            data,
        })
    }

    pub(crate) fn build_zip_attachment(&self, path: &Path) -> Result<ComposeAttachment> {
        let folder = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("attachment")
            .to_string();
        let filename = format!("{}.zip", folder);
        let data = zip_directory(path)?;
        let size = data.len();
        Ok(ComposeAttachment {
            filename,
            mime: "application/zip".to_string(),
            size,
            data,
        })
    }

    pub(crate) fn save_attachment_to_temp(
        &mut self,
        message_id: i64,
        attachment_index: usize,
        filename: &str,
    ) -> Result<PathBuf> {
        let safe = safe_filename(filename);
        let temp_name = format!("ratmail-{}-{}-{}", message_id, attachment_index + 1, safe);
        let path = std::env::temp_dir().join(temp_name);
        self.save_attachment_to_path(message_id, attachment_index, &path)
    }

    pub(crate) fn save_attachment_to_path(
        &mut self,
        message_id: i64,
        attachment_index: usize,
        path: &Path,
    ) -> Result<PathBuf> {
        let raw = self.get_raw_body_or_fetch(message_id)?;
        let data = extract_attachment_data(&raw, attachment_index)?
            .ok_or_else(|| anyhow::anyhow!("attachment not found"))?;

        let mut target = PathBuf::from(path);
        if target.is_dir() {
            target = target.join(safe_filename(&data.filename));
        }
        if let Some(parent) = target.parent() {
            if !parent.exists() {
                return Err(anyhow::anyhow!("parent directory does not exist"));
            }
        }
        std::fs::write(&target, &data.data)?;
        Ok(target)
    }

    pub(crate) fn get_raw_body_or_fetch(&mut self, message_id: i64) -> Result<Vec<u8>> {
        let raw = self.runtime().block_on(async {
            self.store_handle
                .get_raw_body(message_id)
                .await
                .ok()
                .flatten()
        });
        if let Some(raw) = raw {
            return Ok(raw);
        }
        if self.imap_enabled {
            let (uid, folder_name) = self.message_location(message_id);
            if let (Some(uid), Some(folder_name)) = (uid, folder_name) {
                let _ = self.engine.send(MailCommand::FetchMessageBody {
                    message_id,
                    folder_name,
                    uid,
                });
                self.status_message = Some("Fetching body...".to_string());
            }
        }
        Err(anyhow::anyhow!("message body not cached"))
    }

    pub(crate) fn message_location(&self, message_id: i64) -> (Option<u32>, Option<String>) {
        let message = self.store.messages.iter().find(|m| m.id == message_id);
        let uid = message.and_then(|m| m.imap_uid);
        let folder_name = message
            .and_then(|m| self.store.folders.iter().find(|f| f.id == m.folder_id))
            .map(|f| f.name.clone());
        (uid, folder_name)
    }
}
