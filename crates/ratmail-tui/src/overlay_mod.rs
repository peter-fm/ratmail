use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ratatui_image::{Resize, StatefulImage};

use super::{
    App, PickerFocus, PickerMode, PickerPreviewKind, SpellTarget, centered_rect, format_size,
    link_display_label, set_cursor_at, spell_issue_context_line, truncate_label,
};

pub(crate) fn render_search_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(80, 30, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("SEARCH")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new("Filter messages by From, Subject, Preview").style(app.ui_theme.base),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(
            "Fields: from:alice  subject:invoice  to:bob  date:2026-02-01  since:2026-01-01  before:2026-02-10",
        )
        .style(app.ui_theme.label),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new("Attach: att:invoice  file:report  type:pdf  mime:image/png")
            .style(app.ui_theme.label),
        rows[2],
    );
    let line = Line::from(vec![
        Span::styled("Query: ", app.ui_theme.label),
        Span::raw(app.search_query.as_str()),
    ]);
    frame.render_widget(Paragraph::new(line).style(app.ui_theme.base), rows[3]);
    let cursor_area = Rect {
        x: rows[3].x.saturating_add(7),
        y: rows[3].y,
        width: rows[3].width.saturating_sub(7),
        height: 1,
    };
    set_cursor_at(frame, cursor_area, &app.search_query, app.search_cursor);
}

pub(crate) fn render_links_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("LINKS")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);

    let mut lines = Vec::new();
    let title = app
        .selected_detail()
        .map(|detail| format!("Message: {}", detail.subject))
        .unwrap_or_else(|| "Message: (none)".to_string());
    lines.push(Line::from(title));
    lines.push(Line::from(""));

    if let Some(detail) = app.selected_detail() {
        if detail.links.is_empty() {
            lines.push(Line::from("  (no links found)"));
        } else {
            let selected = app.link_index.min(detail.links.len().saturating_sub(1));
            let max_lines = rows[0].height as usize;
            let header_lines = 2usize;
            let list_capacity = max_lines.saturating_sub(header_lines);
            let total = detail.links.len();
            let mut start = 0usize;
            if list_capacity > 0 && selected + 1 > list_capacity {
                start = selected + 1 - list_capacity;
                if start + list_capacity > total {
                    start = total.saturating_sub(list_capacity);
                }
            }
            let end = if list_capacity == 0 {
                0
            } else {
                (start + list_capacity).min(total)
            };

            let max_label = rows[0].width.saturating_sub(6).max(1) as usize;
            for (idx, link) in detail.links.iter().enumerate().take(end).skip(start) {
                let label = truncate_label(&link_display_label(link, Some(idx)), max_label);
                let marker = if idx == selected { ">" } else { " " };
                let idx_text = format!("{} {:>2}  ", marker, idx + 1);
                let label_style = if idx == selected {
                    app.ui_theme.link_selected
                } else {
                    app.ui_theme.link
                };
                let mut spans = Vec::new();
                spans.push(Span::raw(idx_text));
                spans.push(Span::styled(label, label_style));
                lines.push(Line::from(spans));
            }
        }
    } else {
        lines.push(Line::from("  (no message selected)"));
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, rows[0]);

    let footer = Paragraph::new(
        "j/k move  PgUp/PgDn jump  1-9 select  Enter open  y copy  o open ext  Esc close",
    )
    .style(app.ui_theme.base)
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, rows[1]);
}

pub(crate) fn render_attach_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    let title = app
        .selected_detail()
        .map(|detail| format!("Message: {}", detail.subject))
        .unwrap_or_else(|| "Message: (none)".to_string());
    lines.push(Line::from(title));
    lines.push(Line::from(""));

    if let Some(detail) = app.selected_detail() {
        if detail.attachments.is_empty() {
            lines.push(Line::from("  (no attachments found)"));
        } else {
            let selected = app
                .attach_index
                .min(detail.attachments.len().saturating_sub(1));
            let max_lines = popup.height.saturating_sub(4) as usize;
            let header_lines = 2usize;
            let list_capacity = max_lines.saturating_sub(header_lines);
            let total = detail.attachments.len();
            let mut start = 0usize;
            if list_capacity > 0 && selected + 1 > list_capacity {
                start = selected + 1 - list_capacity;
                if start + list_capacity > total {
                    start = total.saturating_sub(list_capacity);
                }
            }
            let end = if list_capacity == 0 {
                0
            } else {
                (start + list_capacity).min(total)
            };

            for (idx, attachment) in detail.attachments.iter().enumerate().take(end).skip(start) {
                let size = format_size(attachment.size);
                let marker = if idx == selected { ">" } else { " " };
                lines.push(Line::from(format!(
                    "{} {:>2}  {:<24} {:>7}  {}",
                    marker,
                    idx + 1,
                    attachment.filename,
                    size,
                    attachment.mime
                )));
            }
        }
    } else {
        lines.push(Line::from("  (no message selected)"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("j/k move  Enter open  s save  Esc close"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("ATTACHMENTS")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_bulk_action_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(50, 40, area);
    frame.render_widget(Clear, popup);

    let count = app.bulk_action_ids.len();
    let mut lines = Vec::new();
    lines.push(Line::from(format!(
        "Selected {} message{}",
        count,
        if count == 1 { "" } else { "s" }
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("r mark read"));
    lines.push(Line::from("m move"));
    lines.push(Line::from("d delete"));
    lines.push(Line::from("Esc close"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("BULK")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_bulk_move_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 70, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    lines.push(Line::from("Select folder to move to:"));
    lines.push(Line::from(""));

    for (idx, folder) in app.store.folders.iter().enumerate() {
        let style = if idx == app.bulk_folder_index {
            app.ui_theme.overlay_select
        } else {
            Style::default()
        };
        let label = App::display_folder_name(&folder.name);
        lines.push(Line::from(Span::styled(format!("  {}", label), style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Enter move  Esc back"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("MOVE")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_confirm_delete_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(50, 30, area);
    frame.render_widget(Clear, popup);

    let count = app.confirm_delete_ids.len();
    let mut lines = Vec::new();
    lines.push(Line::from(format!(
        "Delete {} message{}?",
        count,
        if count == 1 { "" } else { "s" }
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("y confirm"));
    lines.push(Line::from("n cancel"));
    lines.push(Line::from("Esc close"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("CONFIRM")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_confirm_link_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 40, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    lines.push(Line::from("Open link?"));
    lines.push(Line::from(""));
    if let Some(link) = &app.confirm_link {
        let display = link_display_label(link, None);
        if display != link.url {
            lines.push(Line::from(format!("Text: {}", display)));
        }
        lines.push(Line::from(format!("URL: {}", link.url)));
    } else {
        lines.push(Line::from("URL: (none)"));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("y confirm"));
    lines.push(Line::from("n cancel"));
    lines.push(Line::from("Esc close"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("CONFIRM")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_confirm_draft_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(55, 35, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    lines.push(Line::from("Save draft before closing?"));
    lines.push(Line::from(""));
    if !app.compose_attachments.is_empty() {
        lines.push(Line::from("Note: attachments won't be saved."));
        lines.push(Line::from(""));
    }
    lines.push(Line::from("y save to Drafts"));
    lines.push(Line::from("n discard"));
    lines.push(Line::from("Esc keep editing"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("DRAFT")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_spellcheck_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 60, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    if app.spell_issues.is_empty() {
        lines.push(Line::from("No spelling issues found."));
        lines.push(Line::from(""));
        lines.push(Line::from("Esc close"));
    } else {
        let idx = app
            .spell_issue_index
            .min(app.spell_issues.len().saturating_sub(1));
        let issue = &app.spell_issues[idx];
        let location = match issue.target {
            SpellTarget::Subject => "Subject",
            SpellTarget::Body => "Body",
        };
        lines.push(Line::from(format!(
            "Issue {}/{} ({})",
            idx + 1,
            app.spell_issues.len(),
            location
        )));
        lines.push(Line::from(""));
        let body_text;
        let source_text = match issue.target {
            SpellTarget::Subject => app.compose_subject.as_str(),
            SpellTarget::Body => {
                body_text = app.compose_body_text();
                body_text.as_str()
            }
        };
        let context = spell_issue_context_line(source_text, issue.start, issue.end, &app.ui_theme);
        lines.push(Line::from(context));
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Word: {}", issue.word)));
        lines.push(Line::from(""));
        if issue.suggestions.is_empty() {
            lines.push(Line::from("No suggestions available."));
        } else {
            lines.push(Line::from("Suggestions:"));
            for (sidx, suggestion) in issue.suggestions.iter().enumerate() {
                let style = if sidx == app.spell_suggestion_index {
                    app.ui_theme.overlay_select
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(format!("  {}", suggestion), style)));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(
            "Enter apply  n/p next/prev  Up/Down choose  s skip  i ignore  Esc close",
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title("SPELLCHECK")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

pub(crate) fn render_picker_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    if app.picker_mode.is_none() || app.picker.is_none() {
        return;
    }
    let mode = app.picker_mode.clone().unwrap_or(PickerMode::Attach);
    let popup = centered_rect(85, 85, area);
    frame.render_widget(Clear, popup);

    let title = match mode {
        PickerMode::Attach => "ATTACH FILE",
        PickerMode::Save { .. } => "SAVE ATTACHMENT",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    app.refresh_picker_preview();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(inner);

    if rows[0].width >= 60 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(3, 5), Constraint::Ratio(2, 5)])
            .split(rows[0]);
        {
            let widget = app.picker.as_ref().unwrap().widget();
            frame.render_widget(widget, cols[0]);
        }
        render_picker_preview(frame, cols[1], app);
    } else {
        let widget = app.picker.as_ref().unwrap().widget();
        frame.render_widget(widget, rows[0]);
    }

    let mut lines = Vec::new();
    let filter_label = if app.picker_focus == PickerFocus::Explorer {
        "Filter*:"
    } else {
        "Filter :"
    };
    let filter_text = if app.picker_filter.is_empty() {
        "(type to filter)".to_string()
    } else {
        app.picker_filter.clone()
    };
    lines.push(Line::from(format!("{} {}", filter_label, filter_text)));
    match mode {
        PickerMode::Attach => {
            lines.push(Line::from(
                "Enter attach  Right/L enter dir  Left/Back parent  Ctrl+H toggle hidden  Ctrl+U clear filter  Esc cancel",
            ));
        }
        PickerMode::Save { ref filename, .. } => {
            let dir = app
                .picker_selected_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "(unknown)".to_string());
            lines.push(Line::from(format!("Directory: {}", dir)));
            let label = if app.picker_focus == PickerFocus::Filename {
                "Filename*:"
            } else {
                "Filename :"
            };
            let display = if app.picker_filename.is_empty() {
                filename.clone()
            } else {
                app.picker_filename.clone()
            };
            lines.push(Line::from(format!("{} {}", label, display)));
            lines.push(Line::from(
                "Tab focus  Enter save  Right/L enter dir  Left/Back parent  Ctrl+U clear filter  Esc cancel",
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(app.ui_theme.base),
        rows[1],
    );

    if matches!(mode, PickerMode::Save { .. }) && app.picker_focus == PickerFocus::Filename {
        let label_len = "Filename*: ".chars().count() as u16;
        let cursor_col = label_len.saturating_add(app.picker_cursor as u16);
        let x = rows[1].x + cursor_col.min(rows[1].width.saturating_sub(1));
        let y = rows[1].y + 2;
        frame.set_cursor_position((x, y));
    }
}

fn render_picker_preview(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("PREVIEW")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.picker_preview_kind {
        PickerPreviewKind::Image | PickerPreviewKind::PdfImage => {
            let Some(img) = app.picker_preview_image.as_ref() else {
                return;
            };
            if app.picker_preview_protocol.is_none() {
                if let Some(picker) = app.image_picker.as_ref() {
                    app.picker_preview_protocol = Some(picker.new_resize_protocol(img.clone()));
                }
            }
            if let Some(protocol) = app.picker_preview_protocol.as_mut() {
                let widget = StatefulImage::default().resize(Resize::Fit(None));
                frame.render_stateful_widget(widget, inner, protocol);
            } else {
                let lines = vec![Line::from("Image preview unavailable.")];
                let paragraph = Paragraph::new(Text::from(lines))
                    .style(app.ui_theme.base)
                    .wrap(Wrap { trim: false });
                frame.render_widget(paragraph, inner);
            }
        }
        PickerPreviewKind::Text => {
            let paragraph = Paragraph::new(app.picker_preview_text.clone())
                .style(app.ui_theme.base)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
        }
        PickerPreviewKind::Meta | PickerPreviewKind::Error | PickerPreviewKind::Empty => {
            let mut lines: Vec<Line<'static>> = app
                .picker_preview_meta
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect();
            if let Some(err) = app.picker_preview_error.as_ref() {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(err.clone()));
            }
            if lines.is_empty() {
                lines.push(Line::from("(no preview available)"));
            }
            let paragraph = Paragraph::new(Text::from(lines))
                .style(app.ui_theme.base)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
        }
    }
}
