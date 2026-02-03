use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap},
    Terminal,
};

use ratmail_core::{
    MailStore, MessageDetail, MessageSummary, SqliteMailStore, StoreSnapshot, DEFAULT_TEXT_WIDTH,
};
use ratmail_content::{extract_attachments, extract_display, prepare_html};
use ratmail_mail::{MailCommand, MailEngine, MailEvent};
use ratmail_render::{NullRenderer, RemotePolicy, Renderer};

const TICK_RATE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    View,
    ViewFocus,
    Compose,
    OverlayLinks,
    OverlayAttach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Text,
    Rendered,
}

struct App {
    mode: Mode,
    view_mode: ViewMode,
    store: StoreSnapshot,
    message_index: usize,
    last_tick: Instant,
    sync_status: String,
    engine: MailEngine,
    events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
    store_handle: SqliteMailStore,
    runtime: tokio::runtime::Runtime,
    overlay_return: Mode,
    view_scroll: u16,
    render_supported: bool,
    render_pending: bool,
    renderer: NullRenderer,
}

impl App {
    fn new(
        store: StoreSnapshot,
        store_handle: SqliteMailStore,
        engine: MailEngine,
        events: tokio::sync::mpsc::UnboundedReceiver<MailEvent>,
        runtime: tokio::runtime::Runtime,
        render_supported: bool,
    ) -> Self {
        Self {
            mode: Mode::List,
            view_mode: ViewMode::Rendered,
            store,
            message_index: 0,
            last_tick: Instant::now(),
            sync_status: "idle".to_string(),
            engine,
            events,
            store_handle,
            runtime,
            overlay_return: Mode::View,
            view_scroll: 0,
            render_supported,
            render_pending: false,
            renderer: NullRenderer::default(),
        }
    }

    fn selected_message(&self) -> Option<&MessageSummary> {
        self.store.messages.get(self.message_index)
    }

    fn selected_detail(&self) -> Option<&MessageDetail> {
        let message_id = self.selected_message()?.id;
        self.store.message_details.get(&message_id)
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::List | Mode::View | Mode::ViewFocus => self.on_key_main(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::OverlayLinks | Mode::OverlayAttach => self.on_key_overlay(key),
        }
    }

    fn ensure_text_cache_for_selected(&mut self) {
        let Some(message) = self.selected_message() else { return };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.body.is_empty() && !detail.links.is_empty() {
                return;
            }
        }

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime.block_on(async move {
            if let Some(raw) = store_handle.get_raw_body(message_id).await? {
                let display = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize)?;
                store_handle
                    .upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, &display.text)
                    .await?;
                Ok::<_, anyhow::Error>(Some(display))
            } else {
                Ok::<_, anyhow::Error>(None)
            }
        });

        if let Ok(Some(display)) = result {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.body = display.text;
                detail.links = display.links;
            } else if let Some(summary) = self.selected_message() {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        date: summary.date.clone(),
                        body: display.text,
                        links: display.links,
                        attachments: Vec::new(),
                    },
                );
            }
        }
    }

    fn ensure_attachments_for_selected(&mut self) {
        let Some(message) = self.selected_message() else { return };
        if let Some(detail) = self.store.message_details.get(&message.id) {
            if !detail.attachments.is_empty() {
                return;
            }
        }

        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime.block_on(async move {
            if let Some(raw) = store_handle.get_raw_body(message_id).await? {
                let attachments = extract_attachments(&raw)?;
                Ok::<_, anyhow::Error>(Some(attachments))
            } else {
                Ok::<_, anyhow::Error>(None)
            }
        });

        if let Ok(Some(attachments)) = result {
            if let Some(detail) = self.store.message_details.get_mut(&message_id) {
                detail.attachments = attachments;
            } else if let Some(summary) = self.selected_message() {
                self.store.message_details.insert(
                    message_id,
                    MessageDetail {
                        id: message_id,
                        subject: summary.subject.clone(),
                        from: summary.from.clone(),
                        date: summary.date.clone(),
                        body: String::new(),
                        links: Vec::new(),
                        attachments,
                    },
                );
            }
        }
    }

    fn ensure_prepared_html_for_selected(&mut self) {
        let Some(message) = self.selected_message() else { return };
        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let result = self.runtime.block_on(async move {
            if store_handle
                .get_cache_html(message_id, "blocked")
                .await?
                .is_some()
            {
                return Ok::<_, anyhow::Error>(None::<()>);
            }

            if let Some(raw) = store_handle.get_raw_body(message_id).await? {
                if let Some(prepared) = prepare_html(&raw, false)? {
                    store_handle
                        .upsert_cache_html(message_id, "blocked", &prepared.html)
                        .await?;
                }
            }
            Ok::<_, anyhow::Error>(None::<()>)
        });

        let _ = result;
    }

    fn ensure_tiles_for_selected(&mut self) {
        if !self.render_supported {
            return;
        }
        let Some(message) = self.selected_message() else { return };
        let message_id = message.id;
        let store_handle = self.store_handle.clone();
        let renderer = self.renderer.clone();
        let width_px = 800i64;
        let theme = "default";
        let remote_policy = "blocked";

        self.render_pending = true;
        let result = self.runtime.block_on(async move {
            let cached = store_handle
                .get_cache_tiles(message_id, width_px, theme, remote_policy)
                .await?;
            if !cached.is_empty() {
                return Ok::<_, anyhow::Error>(());
            }

            let html = store_handle
                .get_cache_html(message_id, remote_policy)
                .await?;
            let Some(html) = html else { return Ok::<_, anyhow::Error>(()) };

            let render_result = renderer
                .render(ratmail_render::RenderRequest {
                    message_id,
                    width_px,
                    theme,
                    remote_policy: RemotePolicy::Blocked,
                    prepared_html: &html,
                })
                .await?;

            if !render_result.tiles.is_empty() {
                store_handle
                    .upsert_cache_tiles(message_id, width_px, theme, remote_policy, &render_result.tiles)
                    .await?;
            }

            Ok::<_, anyhow::Error>(())
        });

        let _ = result;
        self.render_pending = false;
    }

    fn on_event(&mut self, event: MailEvent) {
        match event {
            MailEvent::SyncStarted(_) => self.sync_status = "syncing".to_string(),
            MailEvent::SyncCompleted(_) => self.sync_status = "idle".to_string(),
            MailEvent::SyncFailed { .. } => self.sync_status = "error".to_string(),
            _ => {}
        }
    }

    fn on_key_main(&mut self, key: KeyEvent) -> bool {
        if self.mode == Mode::ViewFocus {
            return self.on_key_focus(key);
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                if self.message_index + 1 < self.store.messages.len() {
                    self.message_index += 1;
                }
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                if self.message_index > 0 {
                    self.message_index -= 1;
                }
            }
            (KeyCode::Enter, _) => {
                self.mode = Mode::ViewFocus;
                self.view_scroll = 0;
                let _ = self.engine.send(MailCommand::SyncFolder(1));
                self.ensure_text_cache_for_selected();
            }
            (KeyCode::Esc, _) => {
                if self.mode == Mode::ViewFocus {
                    self.mode = Mode::View;
                }
            }
            (KeyCode::Char('v'), _) => {
                self.view_mode = match self.view_mode {
                    ViewMode::Text => ViewMode::Rendered,
                    ViewMode::Rendered => ViewMode::Text,
                };
                if self.view_mode == ViewMode::Text {
                    self.ensure_text_cache_for_selected();
                } else {
                    self.ensure_prepared_html_for_selected();
                    self.ensure_tiles_for_selected();
                }
            }
            (KeyCode::Char('l'), _) => {
                self.ensure_text_cache_for_selected();
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayLinks;
            }
            (KeyCode::Char('a'), _) => {
                self.ensure_attachments_for_selected();
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('R'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('f'), _) => {
                self.mode = Mode::Compose;
            }
            _ => {}
        }
        false
    }

    fn on_key_focus(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                self.view_scroll = self.view_scroll.saturating_add(1);
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(1);
            }
            (KeyCode::PageDown, _) => {
                self.view_scroll = self.view_scroll.saturating_add(10);
            }
            (KeyCode::PageUp, _) => {
                self.view_scroll = self.view_scroll.saturating_sub(10);
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
                    self.ensure_prepared_html_for_selected();
                }
            }
            (KeyCode::Char('l'), _) => {
                self.ensure_text_cache_for_selected();
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayLinks;
            }
            (KeyCode::Char('a'), _) => {
                self.ensure_attachments_for_selected();
                self.overlay_return = self.mode;
                self.mode = Mode::OverlayAttach;
            }
            (KeyCode::Char('r'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('R'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Char('f'), _) => {
                self.mode = Mode::Compose;
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
            }
            _ => {}
        }
        false
    }

    fn on_key_overlay(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = self.overlay_return;
            }
            KeyCode::Char('q') => return true,
            _ => {}
        }
        false
    }

    fn on_key_compose(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.mode = Mode::View;
            }
            (KeyCode::Esc, _) => {
                self.mode = Mode::View;
            }
            _ => {}
        }
        false
    }
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (engine, events, store_handle, store) = rt.block_on(async {
        let (engine, events) = MailEngine::start();
        let store_handle = SqliteMailStore::connect("ratmail.db").await?;
        store_handle.init().await?;
        store_handle.seed_demo_if_empty().await?;
        let snapshot = store_handle.load_snapshot(1, 1).await?;
        Ok::<_, anyhow::Error>((engine, events, store_handle, snapshot))
    })?;
    let render_supported = rt.block_on(async {
        let renderer = NullRenderer::default();
        renderer.supports_images().await
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(
        &mut terminal,
        App::new(store, store_handle, engine, events, rt, render_supported),
    );

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, &app))?;

        while let Ok(event) = app.events.try_recv() {
            app.on_event(event);
        }

        let timeout = TICK_RATE.saturating_sub(app.last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if app.on_key(key) {
                    return Ok(());
                }
            }
        }

        if app.last_tick.elapsed() >= TICK_RATE {
            app.last_tick = Instant::now();
        }
    }
}

fn ui(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_status_bar(frame, layout[0], app);
    render_main(frame, layout[1], app);
    render_help_bar(frame, layout[2]);

    match app.mode {
        Mode::OverlayLinks => render_links_overlay(frame, area, app),
        Mode::OverlayAttach => render_attach_overlay(frame, area, app),
        Mode::Compose => render_compose_overlay(frame, area, app),
        Mode::ViewFocus => render_view_focus(frame, area, app),
        _ => {}
    }
}

fn render_status_bar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let view_label = match app.view_mode {
        ViewMode::Text => "TEXT",
        ViewMode::Rendered => "RENDERED",
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" acct: {} ", app.store.account.address),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" INBOX (2,184) "),
        Span::raw(format!(" sync: {} ", app.sync_status)),
        Span::styled(
            format!(" view: {} (v) ", view_label),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" [/] search "),
    ]);

    let block = Block::default().borders(Borders::BOTTOM);
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_main(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(15),
            Constraint::Percentage(40),
            Constraint::Percentage(45),
        ])
        .split(area);

    render_folders(frame, columns[0], app);
    render_message_list(frame, columns[1], app);
    render_message_view(frame, columns[2], app, 0);
}

fn render_folders(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let items: Vec<Line> = app
        .store
        .folders
        .iter()
        .map(|folder| {
            let label = if folder.unread > 0 {
                format!("{}  {}", folder.name, folder.unread)
            } else {
                folder.name.clone()
            };
            Line::from(Span::raw(label))
        })
        .collect();

    let block = Block::default().borders(Borders::RIGHT).title("FOLDERS");
    let paragraph = Paragraph::new(items).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_message_list(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let header = Row::new(vec![" ", "Time", "From", "Subject"]).style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = app
        .store
        .messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let unread = if message.unread { "*" } else { " " };
            let style = if idx == app.message_index {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(vec![unread, message.date.as_str(), &message.from, &message.subject])
                .style(style)
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Length(2),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Min(10),
    ])
    .header(header)
    .block(Block::default().borders(Borders::RIGHT).title("MESSAGE LIST"))
    .column_spacing(1);

    frame.render_stateful_widget(table, area, &mut TableState::default());

    let preview_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(2),
        width: area.width.saturating_sub(2),
        height: 1,
    };

    if let Some(message) = app.selected_message() {
        let preview = format!("Preview: {}", message.preview);
        frame.render_widget(Paragraph::new(preview), preview_area);
    }
}

fn render_message_view(frame: &mut ratatui::Frame, area: Rect, app: &App, scroll: u16) {
    let title = match app.view_mode {
        ViewMode::Text => "MESSAGE VIEW (text)",
        ViewMode::Rendered => "MESSAGE VIEW (rendered tiles)",
    };

    if let Some(detail) = app.selected_detail() {
        let meta_block = Text::from(vec![
            Line::from(Span::styled(
                format!("Subject: {}", detail.subject),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("From: {}", detail.from)),
            Line::from(format!("Date: {}", detail.date)),
        ]);

        let content_text = match app.view_mode {
            ViewMode::Rendered => {
                if !app.render_supported {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendered mode disabled."),
                        Line::from("Terminal image support not detected."),
                    ])
                } else if app.render_pending {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("Rendering..."),
                        Line::from(""),
                        Line::from("Links: [l]  Attach: [a]"),
                    ])
                } else {
                    Text::from(vec![
                        Line::from(""),
                        Line::from("(HTML email rendered as image tiles via kitty/sixel)"),
                        Line::from(""),
                        Line::from("Scroll:  0%  (PgDn/PgUp)"),
                        Line::from("Links: [l]  Attach: [a]"),
                    ])
                }
            }
            ViewMode::Text => Text::from(detail.body.as_str()),
        };

        let content_block = Paragraph::new(content_text)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(4)])
            .split(area);

        frame.render_widget(content_block, chunks[0]);
        frame.render_widget(Paragraph::new(meta_block), chunks[1]);
    } else {
        let block = Block::default().borders(Borders::ALL).title(title);
        frame.render_widget(Paragraph::new("No message selected").block(block), area);
    }
}

fn render_help_bar(frame: &mut ratatui::Frame, area: Rect) {
    let help = "j/k move  Enter open  v toggle view  r reply  R reply-all  f forward  m move  d delete  q quit";
    let block = Block::default().borders(Borders::TOP);
    frame.render_widget(Paragraph::new(help).block(block), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_links_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

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
            for (idx, link) in detail.links.iter().enumerate() {
                lines.push(Line::from(format!("  {:>2}  {}", idx + 1, link)));
            }
        }
    } else {
        lines.push(Line::from("  (no message selected)"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Enter open  y copy  o open ext  Esc close"));

    let block = Block::default().borders(Borders::ALL).title("LINKS");
    let paragraph = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn render_attach_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
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
            for (idx, attachment) in detail.attachments.iter().enumerate() {
                let size = format_size(attachment.size);
                lines.push(Line::from(format!(
                    "  {:>2}  {:<24} {:>7}  {}",
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
    lines.push(Line::from("Enter open  s save  y copy name  Esc close"));

    let block = Block::default().borders(Borders::ALL).title("ATTACHMENTS");
    let paragraph = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", (bytes as f64 / (1024.0 * 1024.0)).round() as usize)
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.0).round() as usize)
    } else {
        format!("{} B", bytes)
    }
}

fn render_compose_overlay(frame: &mut ratatui::Frame, area: Rect, _app: &App) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from("To:   Alex Chen <alex@example.com>"),
        Line::from("Cc:"),
        Line::from("Subj: Re: Proposal"),
        Line::from(""),
        Line::from("Thanks — this looks good overall."),
        Line::from(""),
        Line::from("I've added comments to section 3 regarding timelines."),
        Line::from(""),
        Line::from("> On Feb 3, 2026, Alex Chen wrote:"),
        Line::from(">"),
        Line::from("> Hi,"),
        Line::from(">"),
        Line::from("> Attached is the updated proposal. Let me know if this"),
        Line::from("> works for you."),
        Line::from(">"),
        Line::from("> Best,"),
        Line::from("> Alex"),
        Line::from(""),
        Line::from("Attachments: proposal-v3.pdf"),
        Line::from(""),
        Line::from("Ctrl+S send   Ctrl+A attach   Ctrl+Q cancel   Ctrl+E edit headers"),
    ];

    let block = Block::default().borders(Borders::ALL).title("COMPOSE");
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup);
}

fn render_view_focus(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(90, 85, area);
    frame.render_widget(Clear, popup);
    render_message_view(frame, popup, app, app.view_scroll);
}
