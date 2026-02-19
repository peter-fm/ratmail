use super::{
    CliConfig, RenderConfig, SendConfig, SpellConfig, UiConfig, load_config_text, normalize_ui_theme,
    parse_ui_palette,
};

pub(crate) fn load_render_config() -> RenderConfig {
    let content = match load_config_text() {
        Some(content) => content,
        None => {
            return RenderConfig {
                allow_remote_images: true,
                width_px: 800,
                render_scale: 1.5,
                tile_height_px_side: 1000,
                tile_height_px_focus: 60,
            };
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return RenderConfig {
                allow_remote_images: true,
                width_px: 800,
                render_scale: 1.5,
                tile_height_px_side: 1000,
                tile_height_px_focus: 60,
            };
        }
    };
    let render = match value.get("render") {
        Some(render) => render,
        None => {
            return RenderConfig {
                allow_remote_images: true,
                width_px: 800,
                render_scale: 1.5,
                tile_height_px_side: 1000,
                tile_height_px_focus: 60,
            };
        }
    };
    let allow_remote_images = match render.get("remote_images") {
        Some(v) => v
            .as_bool()
            .or_else(|| {
                v.as_str()
                    .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
            })
            .unwrap_or(true),
        None => true,
    };
    let width_px = match render.get("width_px") {
        Some(v) => v.as_integer().unwrap_or(800) as i64,
        None => 800,
    };
    let render_scale = render
        .get("render_scale")
        .and_then(|v| v.as_float())
        .unwrap_or(1.5)
        .clamp(0.25, 4.0);
    let tile_height_px_side = match render.get("tile_height_px_side") {
        Some(v) => v.as_integer().unwrap_or(1000) as i64,
        None => 1000,
    };
    let tile_height_px_focus = match render.get("tile_height_px_focus") {
        Some(v) => v.as_integer().unwrap_or(60) as i64,
        None => 60,
    };
    RenderConfig {
        allow_remote_images,
        width_px,
        render_scale,
        tile_height_px_side,
        tile_height_px_focus,
    }
}

pub(crate) fn load_ui_config() -> UiConfig {
    let content = match load_config_text() {
        Some(content) => content,
        None => {
            return UiConfig {
                folder_width_cols: 25,
                theme: "default".to_string(),
                palette: None,
                compose_vim: false,
            };
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return UiConfig {
                folder_width_cols: 25,
                theme: "default".to_string(),
                palette: None,
                compose_vim: false,
            };
        }
    };
    let ui = match value.get("ui") {
        Some(ui) => ui,
        None => {
            return UiConfig {
                folder_width_cols: 25,
                theme: "default".to_string(),
                palette: None,
                compose_vim: false,
            };
        }
    };
    let folder_width_cols = match ui.get("folder_width_cols") {
        Some(v) => v.as_integer().unwrap_or(25) as i64,
        None => 25,
    };
    let theme = ui
        .get("theme")
        .and_then(|v| v.as_str())
        .map(normalize_ui_theme)
        .unwrap_or_else(|| "default".to_string());
    let palette = ui.get("palette").and_then(parse_ui_palette);
    let compose_vim = ui
        .get("compose_vim")
        .and_then(|v| {
            v.as_bool().or_else(|| {
                v.as_str()
                    .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
            })
        })
        .unwrap_or(false);
    UiConfig {
        folder_width_cols: folder_width_cols.clamp(8, 40) as u16,
        theme,
        palette,
        compose_vim,
    }
}

pub(crate) fn load_send_config() -> SendConfig {
    let default = SendConfig {
        html: true,
        html_font_family: "Arial, sans-serif".to_string(),
        html_font_size_px: 14,
    };
    let content = match load_config_text() {
        Some(content) => content,
        None => return default,
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => return default,
    };
    let send = match value.get("send") {
        Some(send) => send,
        None => return default,
    };
    let html = send
        .get("html")
        .and_then(parse_bool)
        .unwrap_or(default.html);
    let html_font_family = send
        .get("font_family")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(default.html_font_family);
    let html_font_size_px = send
        .get("font_size_px")
        .and_then(|v| v.as_integer())
        .map(|v| v.clamp(8, 72) as u16)
        .unwrap_or(default.html_font_size_px);
    SendConfig {
        html,
        html_font_family,
        html_font_size_px,
    }
}

pub(crate) fn load_spell_config() -> SpellConfig {
    let content = match load_config_text() {
        Some(content) => content,
        None => {
            return SpellConfig {
                lang: "en_US".to_string(),
                dir: None,
                ignore: Vec::new(),
            };
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return SpellConfig {
                lang: "en_US".to_string(),
                dir: None,
                ignore: Vec::new(),
            };
        }
    };
    let spell = match value.get("spell") {
        Some(spell) => spell,
        None => {
            return SpellConfig {
                lang: "en_US".to_string(),
                dir: None,
                ignore: Vec::new(),
            };
        }
    };
    let lang = spell
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("en_US")
        .to_string();
    let dir = spell
        .get("dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ignore = parse_string_list(spell, "ignore")
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    SpellConfig { lang, dir, ignore }
}

pub(crate) fn load_cli_config() -> CliConfig {
    let content = match load_config_text() {
        Some(content) => content,
        None => {
            return CliConfig {
                enabled: true,
                default_account: None,
                load_error: None,
            };
        }
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(_) => {
            return CliConfig {
                enabled: true,
                default_account: None,
                load_error: Some("Invalid ratmail.toml".to_string()),
            };
        }
    };
    let cli = match value.get("cli") {
        Some(cli) => cli,
        None => {
            return CliConfig {
                enabled: true,
                default_account: None,
                load_error: None,
            };
        }
    };
    let enabled = cli.get("enabled").and_then(parse_bool).unwrap_or(true);
    let default_account = cli
        .get("default_account")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    CliConfig {
        enabled,
        default_account,
        load_error: None,
    }
}

fn parse_string_list(value: &toml::Value, key: &str) -> Option<Vec<String>> {
    let list = value.get(key)?.as_array()?;
    let mut out = Vec::new();
    for item in list {
        if let Some(s) = item.as_str() {
            out.push(s.to_string());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_bool(value: &toml::Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
    })
}
