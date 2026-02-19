use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use serde_json::json;

use super::{AccountConfig, output_ok};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnboardingProvider {
    Gmail,
    ProtonBridge,
    Yahoo,
    OtherImap,
}

impl OnboardingProvider {
    fn label(self) -> &'static str {
        match self {
            OnboardingProvider::Gmail => "Gmail",
            OnboardingProvider::ProtonBridge => "Proton Bridge",
            OnboardingProvider::Yahoo => "Yahoo Mail",
            OnboardingProvider::OtherImap => "Other IMAP/SMTP",
        }
    }

    fn all() -> [OnboardingProvider; 4] {
        [
            OnboardingProvider::Gmail,
            OnboardingProvider::ProtonBridge,
            OnboardingProvider::Yahoo,
            OnboardingProvider::OtherImap,
        ]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OnboardingField {
    label: &'static str,
    value: String,
    secret: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OnboardingAccountDraft {
    name: String,
    email: String,
    password: String,
    display_name: String,
    imap_host: String,
    imap_port: u16,
    smtp_host: String,
    smtp_port: u16,
    skip_tls_verify: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OnboardingResult {
    accounts: Vec<OnboardingAccountDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnboardingStep {
    Welcome,
    Provider,
    AccountForm,
    AddAnotherAccount,
    Complete,
}

pub(crate) fn provider_presets(provider: OnboardingProvider) -> (String, u16, String, u16, bool) {
    match provider {
        OnboardingProvider::Gmail => (
            "imap.gmail.com".to_string(),
            993,
            "smtp.gmail.com".to_string(),
            587,
            false,
        ),
        OnboardingProvider::ProtonBridge => (
            "127.0.0.1".to_string(),
            1143,
            "127.0.0.1".to_string(),
            1025,
            true,
        ),
        OnboardingProvider::Yahoo => (
            "imap.mail.yahoo.com".to_string(),
            993,
            "smtp.mail.yahoo.com".to_string(),
            587,
            false,
        ),
        OnboardingProvider::OtherImap => (
            "imap.example.com".to_string(),
            993,
            "smtp.example.com".to_string(),
            587,
            false,
        ),
    }
}

pub(crate) fn build_onboarding_fields(provider: OnboardingProvider) -> Vec<OnboardingField> {
    let (imap_host, imap_port, smtp_host, smtp_port, _) = provider_presets(provider);
    let mut out = vec![
        OnboardingField {
            label: "Account name",
            value: "Personal".to_string(),
            secret: false,
        },
        OnboardingField {
            label: "Email / username",
            value: String::new(),
            secret: false,
        },
        OnboardingField {
            label: "Password / app password",
            value: String::new(),
            secret: true,
        },
        OnboardingField {
            label: "Display name (for sent From)",
            value: String::new(),
            secret: false,
        },
    ];
    if provider == OnboardingProvider::OtherImap {
        out.push(OnboardingField {
            label: "IMAP host",
            value: imap_host,
            secret: false,
        });
        out.push(OnboardingField {
            label: "IMAP port",
            value: imap_port.to_string(),
            secret: false,
        });
        out.push(OnboardingField {
            label: "SMTP host",
            value: smtp_host,
            secret: false,
        });
        out.push(OnboardingField {
            label: "SMTP port",
            value: smtp_port.to_string(),
            secret: false,
        });
    }
    out
}

pub(crate) fn parse_onboarding_account(
    provider: OnboardingProvider,
    fields: &[OnboardingField],
) -> Result<OnboardingAccountDraft> {
    let get = |idx: usize| fields.get(idx).map(|f| f.value.trim()).unwrap_or("");
    let name = get(0).to_string();
    let email = get(1).to_string();
    let password = get(2).to_string();
    let display_name = get(3).to_string();
    if name.is_empty() {
        return Err(anyhow::anyhow!("Account name is required"));
    }
    if email.is_empty() {
        return Err(anyhow::anyhow!("Email / username is required"));
    }
    if password.is_empty() {
        return Err(anyhow::anyhow!("Password is required"));
    }
    let (mut imap_host, mut imap_port, mut smtp_host, mut smtp_port, skip_tls_verify) =
        provider_presets(provider);
    if provider == OnboardingProvider::OtherImap {
        imap_host = get(4).to_string();
        imap_port = get(5)
            .parse::<u16>()
            .map_err(|_| anyhow::anyhow!("IMAP port must be a number"))?;
        smtp_host = get(6).to_string();
        smtp_port = get(7)
            .parse::<u16>()
            .map_err(|_| anyhow::anyhow!("SMTP port must be a number"))?;
    }
    if imap_host.is_empty() || smtp_host.is_empty() {
        return Err(anyhow::anyhow!("IMAP/SMTP host is required"));
    }
    Ok(OnboardingAccountDraft {
        name,
        email,
        password,
        display_name,
        imap_host,
        imap_port,
        smtp_host,
        smtp_port,
        skip_tls_verify,
    })
}

pub(crate) fn render_onboarding_ui(
    frame: &mut ratatui::Frame,
    step: OnboardingStep,
    provider: OnboardingProvider,
    provider_idx: usize,
    fields: &[OnboardingField],
    field_idx: usize,
    add_another_account: bool,
    error: Option<&str>,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let popup = centered_rect(84, 82, area);
    let block = Block::default()
        .title("Ratmail Setup")
        .borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = Vec::new();
    match step {
        OnboardingStep::Welcome => {
            lines.push(Line::from("Welcome to Ratmail setup."));
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Enter: start account setup   S: skip   Esc: cancel",
            ));
        }
        OnboardingStep::Provider => {
            lines.push(Line::from("Choose your email provider:"));
            lines.push(Line::from(""));
            for (idx, provider) in OnboardingProvider::all().iter().enumerate() {
                let prefix = if idx == provider_idx { ">" } else { " " };
                lines.push(Line::from(format!("{} {}", prefix, provider.label())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Up/Down: select   Enter: continue   Esc: cancel",
            ));
        }
        OnboardingStep::AccountForm => {
            lines.push(Line::from(format!(
                "{} - Enter account details:",
                provider.label()
            )));
            lines.push(Line::from(""));
            for (idx, field) in fields.iter().enumerate() {
                let prefix = if idx == field_idx { ">" } else { " " };
                let value = if field.secret {
                    "*".repeat(field.value.chars().count())
                } else {
                    field.value.clone()
                };
                lines.push(Line::from(format!("{} {}: {}", prefix, field.label, value)));
            }
            lines.push(Line::from(""));
            let typed_email = fields
                .get(1)
                .map(|f| f.value.trim())
                .filter(|v| !v.is_empty())
                .unwrap_or("you@example.com");
            let typed_name = fields
                .get(3)
                .map(|f| f.value.trim())
                .filter(|v| !v.is_empty())
                .unwrap_or("Name");
            lines.push(Line::from(format!(
                "Shown in sent emails as: {} <{}>",
                typed_name, typed_email
            )));
            lines.push(Line::from(
                "Type to edit   Up/Down/Tab: move   Enter: next/save",
            ));
        }
        OnboardingStep::AddAnotherAccount => {
            lines.push(Line::from("Add another email account?"));
            lines.push(Line::from(""));
            lines.push(Line::from(if add_another_account {
                "[X] Yes, add another account now"
            } else {
                "[ ] Yes, add another account now"
            }));
            lines.push(Line::from(""));
            lines.push(Line::from("Space: toggle   Enter: continue"));
        }
        OnboardingStep::Complete => {
            lines.push(Line::from("Setup complete."));
            lines.push(Line::from(""));
            lines.push(Line::from("Press Enter to exit setup."));
        }
    }
    if let Some(err) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);

    if step == OnboardingStep::AccountForm {
        let label_len = fields
            .get(field_idx)
            .map(|f| f.label.chars().count())
            .unwrap_or(0) as u16;
        let value_len = fields
            .get(field_idx)
            .map(|f| f.value.chars().count())
            .unwrap_or(0) as u16;
        let x = inner.x + 1 + 1 + label_len + 1 + 1 + value_len;
        let y = inner.y + 2 + field_idx as u16;
        frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
    }
}

pub(crate) fn run_onboarding_splash() -> Result<Option<OnboardingResult>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "`ratmail setup` requires an interactive terminal"
        ));
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut step = OnboardingStep::Welcome;
    let mut provider_idx = 0usize;
    let mut fields = Vec::<OnboardingField>::new();
    let mut field_idx = 0usize;
    let mut add_another_account = false;
    let mut accounts: Vec<OnboardingAccountDraft> = Vec::new();
    let mut error: Option<String> = None;

    let result: Result<Option<OnboardingResult>> = loop {
        terminal.draw(|frame| {
            let provider = OnboardingProvider::all()[provider_idx];
            render_onboarding_ui(
                frame,
                step,
                provider,
                provider_idx,
                &fields,
                field_idx,
                add_another_account,
                error.as_deref(),
            )
        })?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        error = None;
        match step {
            OnboardingStep::Welcome => match key.code {
                KeyCode::Esc => break Ok(None),
                KeyCode::Char('s') | KeyCode::Char('S') => step = OnboardingStep::Complete,
                KeyCode::Enter => step = OnboardingStep::Provider,
                _ => {}
            },
            OnboardingStep::Provider => match key.code {
                KeyCode::Esc => break Ok(None),
                KeyCode::Up => {
                    provider_idx = provider_idx.saturating_sub(1);
                }
                KeyCode::Down => {
                    provider_idx = (provider_idx + 1).min(OnboardingProvider::all().len() - 1);
                }
                KeyCode::Enter => {
                    let provider = OnboardingProvider::all()[provider_idx];
                    fields = build_onboarding_fields(provider);
                    field_idx = 0;
                    step = OnboardingStep::AccountForm;
                }
                _ => {}
            },
            OnboardingStep::AccountForm => match key.code {
                KeyCode::Esc => break Ok(None),
                KeyCode::Up => field_idx = field_idx.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    if !fields.is_empty() {
                        field_idx = (field_idx + 1).min(fields.len() - 1);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(field) = fields.get_mut(field_idx) {
                        field.value.pop();
                    }
                }
                KeyCode::Enter => {
                    if field_idx + 1 < fields.len() {
                        field_idx += 1;
                    } else {
                        let provider = OnboardingProvider::all()[provider_idx];
                        match parse_onboarding_account(provider, &fields) {
                            Ok(parsed) => {
                                accounts.push(parsed);
                                add_another_account = false;
                                step = OnboardingStep::AddAnotherAccount;
                            }
                            Err(err) => error = Some(err.to_string()),
                        }
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(field) = fields.get_mut(field_idx) {
                        field.value.push(c);
                    }
                }
                _ => {}
            },
            OnboardingStep::AddAnotherAccount => match key.code {
                KeyCode::Esc => break Ok(None),
                KeyCode::Char(' ') => add_another_account = !add_another_account,
                KeyCode::Left | KeyCode::Char('n') | KeyCode::Char('N') => {
                    add_another_account = false
                }
                KeyCode::Right | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    add_another_account = true
                }
                KeyCode::Enter => {
                    if add_another_account {
                        step = OnboardingStep::Provider;
                    } else {
                        step = OnboardingStep::Complete;
                    }
                }
                _ => {}
            },
            OnboardingStep::Complete => match key.code {
                KeyCode::Enter | KeyCode::Esc => break Ok(Some(OnboardingResult { accounts })),
                _ => {}
            },
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub(crate) fn upsert_account_in_config(account: &OnboardingAccountDraft) -> Result<()> {
    let cfg_path = preferred_config_path_for_write();
    let mut root = if cfg_path.exists() {
        toml::from_str::<toml::Value>(&std::fs::read_to_string(&cfg_path)?)
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("Config root is not a table"))?;
    let cli = table
        .entry("cli")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[cli] is not a table"))?;
    cli.entry("enabled")
        .or_insert_with(|| toml::Value::Boolean(true));
    cli.entry("default_account")
        .or_insert_with(|| toml::Value::String(account.name.clone()));

    let accounts = table
        .entry("accounts")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("[accounts] is not an array"))?;

    let mut account_table = toml::map::Map::new();
    account_table.insert(
        "name".to_string(),
        toml::Value::String(account.name.clone()),
    );
    account_table.insert(
        "db_path".to_string(),
        toml::Value::String(format!("ratmail-{}.db", slugify_name(&account.name))),
    );
    let mut imap = toml::map::Map::new();
    imap.insert(
        "host".to_string(),
        toml::Value::String(account.imap_host.clone()),
    );
    imap.insert(
        "port".to_string(),
        toml::Value::Integer(account.imap_port as i64),
    );
    imap.insert(
        "username".to_string(),
        toml::Value::String(account.email.clone()),
    );
    imap.insert(
        "password".to_string(),
        toml::Value::String(account.password.clone()),
    );
    imap.insert(
        "skip_tls_verify".to_string(),
        toml::Value::Boolean(account.skip_tls_verify),
    );
    imap.insert("initial_sync_days".to_string(), toml::Value::Integer(90));
    imap.insert("fetch_chunk_size".to_string(), toml::Value::Integer(10));

    let from_value = if account.display_name.trim().is_empty() {
        account.email.clone()
    } else {
        format!("{} <{}>", account.display_name.trim(), account.email.trim())
    };
    let mut smtp = toml::map::Map::new();
    smtp.insert(
        "host".to_string(),
        toml::Value::String(account.smtp_host.clone()),
    );
    smtp.insert(
        "port".to_string(),
        toml::Value::Integer(account.smtp_port as i64),
    );
    smtp.insert(
        "username".to_string(),
        toml::Value::String(account.email.clone()),
    );
    smtp.insert(
        "password".to_string(),
        toml::Value::String(account.password.clone()),
    );
    smtp.insert("from".to_string(), toml::Value::String(from_value));
    smtp.insert(
        "skip_tls_verify".to_string(),
        toml::Value::Boolean(account.skip_tls_verify),
    );

    account_table.insert("imap".to_string(), toml::Value::Table(imap));
    account_table.insert("smtp".to_string(), toml::Value::Table(smtp));
    accounts.push(toml::Value::Table(account_table));

    let serialized = toml::to_string_pretty(&root)?;
    write_text_atomic(&cfg_path, &serialized)
}

pub(crate) fn run_setup_wizard(accounts: &[AccountConfig], emit_json: bool) -> Result<()> {
    let onboarding = run_onboarding_splash()?;
    let Some(onboarding) = onboarding else {
        if emit_json {
            return output_ok(json!({
                "setup_complete": false,
                "cancelled": true
            }));
        }
        return Ok(());
    };
    for account in &onboarding.accounts {
        upsert_account_in_config(account)?;
    }

    if emit_json {
        return output_ok(json!({
            "setup_complete": true,
            "accounts_added": onboarding.accounts.iter().map(|a| a.name.clone()).collect::<Vec<String>>(),
            "accounts_before": accounts.iter().map(|a| a.name.clone()).collect::<Vec<String>>()
        }));
    }
    println!("Setup complete.");
    Ok(())
}

fn preferred_config_path_for_write() -> PathBuf {
    for p in config_path_candidates() {
        if p.exists() {
            return p;
        }
    }
    xdg_config_dir().join("ratmail").join("ratmail.toml")
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

fn config_path_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("ratmail.toml"),
        xdg_config_dir().join("ratmail").join("ratmail.toml"),
    ]
}

fn xdg_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn write_text_atomic(path: &std::path::Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn slugify_name(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed
    }
}
