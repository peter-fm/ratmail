use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::Text,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::{
    App, ComposeFocus, ComposeVimMode, centered_rect, compose_autocomplete_suffix, cursor_line_col,
    format_size, spans_for_spell_line_range, word_wrap_spans,
};

pub(crate) fn render_compose_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);

    let title = if app.compose_vim_enabled {
        let mode = match app.compose_vim_mode {
            ComposeVimMode::Normal => "NORMAL",
            ComposeVimMode::Insert => "INSERT",
        };
        format!("COMPOSE  [MODE: {}]", mode)
    } else {
        "COMPOSE".to_string()
    };
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(inner);

    let label_style = app.ui_theme.label;
    let to_label = if app.compose_focus == ComposeFocus::To {
        app.ui_theme.label_focus
    } else {
        label_style
    };
    let subject_label = if app.compose_focus == ComposeFocus::Subject {
        app.ui_theme.label_focus
    } else {
        label_style
    };
    let cc_label = if app.compose_focus == ComposeFocus::Cc {
        app.ui_theme.label_focus
    } else {
        label_style
    };
    let bcc_label = if app.compose_focus == ComposeFocus::Bcc {
        app.ui_theme.label_focus
    } else {
        label_style
    };
    let to_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[0]);
    frame.render_widget(Paragraph::new("To:").style(to_label), to_layout[0]);
    let to_suffix = if app.compose_focus == ComposeFocus::To {
        compose_autocomplete_suffix(
            &app.compose_address_list,
            &app.compose_to,
            app.compose_cursor_to,
        )
    } else {
        None
    };
    let to_line = if let Some(suffix) = to_suffix {
        Line::from(vec![
            Span::raw(app.compose_to.as_str()),
            Span::styled(suffix, app.ui_theme.suffix),
        ])
    } else {
        Line::from(app.compose_to.as_str())
    };
    frame.render_widget(
        Paragraph::new(to_line).style(app.ui_theme.base),
        to_layout[1],
    );

    let cc_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[1]);
    frame.render_widget(Paragraph::new("Cc:").style(cc_label), cc_layout[0]);
    let cc_suffix = if app.compose_focus == ComposeFocus::Cc {
        compose_autocomplete_suffix(
            &app.compose_address_list,
            &app.compose_cc,
            app.compose_cursor_cc,
        )
    } else {
        None
    };
    let cc_line = if let Some(suffix) = cc_suffix {
        Line::from(vec![
            Span::raw(app.compose_cc.as_str()),
            Span::styled(suffix, app.ui_theme.suffix),
        ])
    } else {
        Line::from(app.compose_cc.as_str())
    };
    frame.render_widget(
        Paragraph::new(cc_line).style(app.ui_theme.base),
        cc_layout[1],
    );

    let bcc_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[2]);
    frame.render_widget(Paragraph::new("Bcc:").style(bcc_label), bcc_layout[0]);
    let bcc_suffix = if app.compose_focus == ComposeFocus::Bcc {
        compose_autocomplete_suffix(
            &app.compose_address_list,
            &app.compose_bcc,
            app.compose_cursor_bcc,
        )
    } else {
        None
    };
    let bcc_line = if let Some(suffix) = bcc_suffix {
        Line::from(vec![
            Span::raw(app.compose_bcc.as_str()),
            Span::styled(suffix, app.ui_theme.suffix),
        ])
    } else {
        Line::from(app.compose_bcc.as_str())
    };
    frame.render_widget(
        Paragraph::new(bcc_line).style(app.ui_theme.base),
        bcc_layout[1],
    );

    let subject_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[3]);
    frame.render_widget(
        Paragraph::new("Subj:").style(subject_label),
        subject_layout[0],
    );
    frame.render_widget(
        Paragraph::new(app.compose_subject.as_str()).style(app.ui_theme.base),
        subject_layout[1],
    );

    let attachment_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(rows[4]);
    let attachment_text = if app.compose_attachments.is_empty() {
        "(no attachments)".to_string()
    } else {
        app.compose_attachments
            .iter()
            .map(|a| format!("{} ({})", a.filename, format_size(a.size)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    frame.render_widget(
        Paragraph::new("Att:").style(label_style),
        attachment_layout[0],
    );
    frame.render_widget(
        Paragraph::new(attachment_text).style(app.ui_theme.base),
        attachment_layout[1],
    );

    let line = "─".repeat(rows[5].width as usize);
    frame.render_widget(Paragraph::new(line).style(app.ui_theme.separator), rows[5]);
    render_compose_body(frame, rows[6], app);

    let close_hint = if app.compose_vim_enabled {
        "Ctrl+Q close"
    } else {
        "Ctrl+Q/Esc close"
    };
    let base_footer = format!(
        "Ctrl+S/F5 send   F7 spell   Ctrl+Space suggest   Ctrl+A attach   Ctrl+R remove last   Tab next   Shift+Tab prev   Right accept   {}",
        close_hint
    );
    let footer = if let Some(msg) = &app.status_message {
        format!("{}   | {}", base_footer, msg)
    } else {
        base_footer
    };
    frame.render_widget(Paragraph::new(footer).style(app.ui_theme.base), rows[7]);

    match app.compose_focus {
        ComposeFocus::To => {
            set_cursor_at(frame, to_layout[1], &app.compose_to, app.compose_cursor_to)
        }
        ComposeFocus::Cc => {
            set_cursor_at(frame, cc_layout[1], &app.compose_cc, app.compose_cursor_cc)
        }
        ComposeFocus::Bcc => set_cursor_at(
            frame,
            bcc_layout[1],
            &app.compose_bcc,
            app.compose_cursor_bcc,
        ),
        ComposeFocus::Subject => set_cursor_at(
            frame,
            subject_layout[1],
            &app.compose_subject,
            app.compose_cursor_subject,
        ),
        ComposeFocus::Body => {
            if let Some(pos) = app.compose_body.cursor_screen_position(rows[6]) {
                frame.set_cursor_position(pos);
            }
        }
    }

    render_inline_spell_suggest(frame, rows[6], app);
}

fn render_compose_body(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    app.compose_body_area_width = area.width;
    app.compose_body_area_height = area.height;
    app.compose_body.update_scroll(area.height as usize);

    let top = app.compose_body.scroll_top();
    let height = area.height as usize;
    let lines = app.compose_body.lines();
    let bottom = (top + height).min(lines.len());
    let width = area.width as usize;
    let mut out = Vec::with_capacity(bottom.saturating_sub(top));
    for line in lines.iter().take(bottom).skip(top) {
        let wrapped = word_wrap_spans(line, width, app.compose_body.tab_len);
        for (start, end) in wrapped {
            let spans = spans_for_spell_line_range(line, start, end, &app.ui_theme);
            out.push(Line::from(spans));
        }
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    let paragraph = Paragraph::new(Text::from(out)).style(app.ui_theme.base);
    frame.render_widget(paragraph, area);
}

fn render_inline_spell_suggest(frame: &mut ratatui::Frame, body_area: Rect, app: &App) {
    if app.compose_focus != ComposeFocus::Body {
        return;
    }
    let Some(suggest) = &app.inline_spell_suggest else {
        return;
    };
    let Some(cursor_pos) = app.compose_body.cursor_screen_position(body_area) else {
        return;
    };
    if suggest.suggestions.is_empty() {
        return;
    }

    let max_len = suggest
        .suggestions
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    let width = (max_len + 4).min(body_area.width as usize).max(12) as u16;
    let height = (suggest.suggestions.len() + 2).min(8) as u16;

    let mut x = cursor_pos.0;
    let mut y = cursor_pos.1.saturating_add(1);
    if x + width > body_area.x + body_area.width {
        x = body_area
            .x
            .saturating_add(body_area.width.saturating_sub(width));
    }
    if y + height > body_area.y + body_area.height {
        y = cursor_pos.1.saturating_sub(height).max(body_area.y);
    }
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    for (idx, suggestion) in suggest
        .suggestions
        .iter()
        .take(height.saturating_sub(2) as usize)
        .enumerate()
    {
        let style = if idx == suggest.index {
            app.ui_theme.overlay_select
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!("  {}", suggestion), style)));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Suggest")
        .style(app.ui_theme.base)
        .border_style(app.ui_theme.border);
    let paragraph = Paragraph::new(Text::from(lines))
        .style(app.ui_theme.base)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn set_cursor_at(frame: &mut ratatui::Frame, area: Rect, text: &str, cursor: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (line, col) = cursor_line_col(text, cursor);
    let max_x = area.width.saturating_sub(1);
    let max_y = area.height.saturating_sub(1);
    let x = area.x + (col as u16).min(max_x);
    let y = area.y + (line as u16).min(max_y);
    frame.set_cursor_position((x, y));
}
