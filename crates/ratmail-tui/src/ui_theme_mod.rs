use ratatui::style::{Color, Modifier, Style};

use super::{UiPalette, UiTheme, log_debug, style_with_colors};

impl UiTheme {
    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "ratmail" => Self::ratmail(),
            "nord" => Self::nord(),
            "gruvbox" => Self::gruvbox(),
            "solarized-dark" => Self::solarized_dark(),
            "solarized-light" => Self::solarized_light(),
            "dracula" => Self::dracula(),
            "catppuccin-mocha" => Self::catppuccin_mocha(),
            "catppuccin-latte" => Self::catppuccin_latte(),
            _ => Self::default_theme(),
        }
    }

    pub(crate) fn from_palette(palette: Option<&UiPalette>) -> Self {
        let Some(palette) = palette else {
            log_debug("config warn ui.theme=custom but ui.palette missing; using default");
            return Self::default_theme();
        };
        let base_fg = palette.base_fg;
        let base_bg = palette.base_bg;
        let border = palette.border;
        let bar_fg = palette.bar_fg.or(base_fg);
        let bar_bg = palette.bar_bg.or(base_bg);
        let accent = palette.accent.or(palette.link).or(base_fg);
        let warn = palette.warn.or(accent);
        let error = palette.error;
        let selection_bg = palette.selection_bg.or(bar_bg).or(base_bg);
        let selection_fg = palette.selection_fg.or(accent).or(base_fg);
        let link = palette.link.or(accent);
        let muted = palette.muted.or(border).or(base_fg);

        let base = style_with_colors(base_fg, base_bg);
        let border_style = style_with_colors(border, None);
        let bar = style_with_colors(bar_fg, bar_bg);

        let status_tab_active = style_with_colors(base_bg, accent);
        let status_tab_inactive = style_with_colors(accent, None);
        let status_view = style_with_colors(warn, None);
        let focus_bg = style_with_colors(None, selection_bg);
        let focus_fg = style_with_colors(selection_fg, None);
        let table_header = style_with_colors(base_fg, None).add_modifier(Modifier::BOLD);
        let link_style = style_with_colors(link, None).add_modifier(Modifier::UNDERLINED);
        let link_selected =
            style_with_colors(link, selection_bg).add_modifier(Modifier::UNDERLINED);
        let label = style_with_colors(muted, None);
        let label_focus = style_with_colors(accent, None);
        let suffix = style_with_colors(muted, None);
        let separator = style_with_colors(muted, None);
        let overlay_select = style_with_colors(None, selection_bg);
        let spell_error = style_with_colors(error, None).add_modifier(Modifier::UNDERLINED);

        Self {
            base,
            border: border_style,
            bar,
            show_bars: false,
            status_tab_active,
            status_tab_inactive,
            status_view,
            focus_bg,
            focus_fg,
            table_header,
            link: link_style,
            link_selected,
            label,
            label_focus,
            suffix,
            separator,
            overlay_select,
            spell_error,
        }
    }

    pub(crate) fn default_theme() -> Self {
        Self {
            base: Style::default(),
            border: Style::default(),
            bar: Style::default(),
            show_bars: true,
            status_tab_active: Style::default().fg(Color::Black).bg(Color::Cyan),
            status_tab_inactive: Style::default().fg(Color::Cyan),
            status_view: Style::default().fg(Color::Yellow),
            focus_bg: Style::default().bg(Color::DarkGray),
            focus_fg: Style::default().fg(Color::Yellow),
            table_header: Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(Color::LightBlue)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(Color::Gray),
            label_focus: Style::default().fg(Color::Yellow),
            suffix: Style::default().fg(Color::DarkGray),
            separator: Style::default().fg(Color::DarkGray),
            overlay_select: Style::default().bg(Color::DarkGray),
            spell_error: Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn ratmail() -> Self {
        let neon_pink = Color::Rgb(255, 45, 149); // #ff2d95
        let neon_purple = Color::Rgb(176, 38, 255); // #b026ff
        let neon_blue = Color::Rgb(0, 212, 255); // #00d4ff
        let neon_cyan = Color::Rgb(0, 255, 247); // #00fff7
        let dark_purple = Color::Rgb(26, 10, 46); // #1a0a2e
        let focus_purple = Color::Rgb(44, 18, 74); // #2c124a
        let darker_purple = Color::Rgb(13, 5, 21); // #0d0515
        let chrome_light = Color::Rgb(232, 232, 255); // #e8e8ff
        let chrome_mid = Color::Rgb(168, 168, 216); // #a8a8d8
        let chrome_dark = Color::Rgb(88, 88, 168); // #5858a8
        Self {
            base: Style::default().fg(chrome_light).bg(darker_purple),
            border: Style::default().fg(neon_purple),
            bar: Style::default().fg(chrome_light).bg(dark_purple),
            show_bars: false,
            status_tab_active: Style::default().fg(darker_purple).bg(neon_pink),
            status_tab_inactive: Style::default().fg(neon_pink),
            status_view: Style::default().fg(neon_cyan),
            focus_bg: Style::default().bg(focus_purple),
            focus_fg: Style::default().fg(neon_cyan),
            table_header: Style::default()
                .fg(chrome_light)
                .add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(neon_blue)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(neon_blue)
                .bg(dark_purple)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(chrome_mid),
            label_focus: Style::default().fg(neon_pink),
            suffix: Style::default().fg(chrome_dark),
            separator: Style::default().fg(chrome_dark),
            overlay_select: Style::default().bg(dark_purple),
            spell_error: Style::default()
                .fg(neon_pink)
                .add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn nord() -> Self {
        let nord0 = Color::Rgb(46, 52, 64);
        let nord2 = Color::Rgb(67, 76, 94);
        let _nord3 = Color::Rgb(76, 86, 106);
        let nord4 = Color::Rgb(216, 222, 233);
        let nord6 = Color::Rgb(236, 239, 244);
        let nord8 = Color::Rgb(136, 192, 208);
        let nord11 = Color::Rgb(191, 97, 106);
        let nord13 = Color::Rgb(235, 203, 139);
        Self {
            base: Style::default().fg(nord6).bg(nord0),
            border: Style::default().fg(nord4),
            bar: Style::default().fg(nord6).bg(nord2),
            show_bars: false,
            status_tab_active: Style::default().fg(nord0).bg(nord8),
            status_tab_inactive: Style::default().fg(nord8),
            status_view: Style::default().fg(nord13),
            focus_bg: Style::default().bg(nord2),
            focus_fg: Style::default().fg(nord8),
            table_header: Style::default().fg(nord6).add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(nord8)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(nord8)
                .bg(nord2)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(nord4),
            label_focus: Style::default().fg(nord8),
            suffix: Style::default().fg(nord4),
            separator: Style::default().fg(nord4),
            overlay_select: Style::default().bg(nord2),
            spell_error: Style::default()
                .fg(nord11)
                .add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn gruvbox() -> Self {
        let bg = Color::Rgb(40, 40, 40);
        let bg_alt = Color::Rgb(60, 56, 54);
        let fg = Color::Rgb(235, 219, 178);
        let border = Color::Rgb(146, 131, 116);
        let accent = Color::Rgb(131, 165, 152);
        let warn = Color::Rgb(250, 189, 47);
        let err = Color::Rgb(204, 36, 29);
        Self {
            base: Style::default().fg(fg).bg(bg),
            border: Style::default().fg(border),
            bar: Style::default().fg(fg).bg(bg_alt),
            show_bars: false,
            status_tab_active: Style::default().fg(bg).bg(accent),
            status_tab_inactive: Style::default().fg(accent),
            status_view: Style::default().fg(warn),
            focus_bg: Style::default().bg(bg_alt),
            focus_fg: Style::default().fg(accent),
            table_header: Style::default().fg(fg).add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(accent)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(accent)
                .bg(bg_alt)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(border),
            label_focus: Style::default().fg(accent),
            suffix: Style::default().fg(border),
            separator: Style::default().fg(border),
            overlay_select: Style::default().bg(bg_alt),
            spell_error: Style::default().fg(err).add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn solarized_dark() -> Self {
        let base03 = Color::Rgb(0, 43, 54);
        let base02 = Color::Rgb(7, 54, 66);
        let base01 = Color::Rgb(88, 110, 117);
        let base0 = Color::Rgb(131, 148, 150);
        let cyan = Color::Rgb(42, 161, 152);
        let yellow = Color::Rgb(181, 137, 0);
        let red = Color::Rgb(220, 50, 47);
        Self {
            base: Style::default().fg(base0).bg(base03),
            border: Style::default().fg(base01),
            bar: Style::default().fg(base0).bg(base02),
            show_bars: false,
            status_tab_active: Style::default().fg(base03).bg(cyan),
            status_tab_inactive: Style::default().fg(cyan),
            status_view: Style::default().fg(yellow),
            focus_bg: Style::default().bg(base02),
            focus_fg: Style::default().fg(cyan),
            table_header: Style::default().fg(base0).add_modifier(Modifier::BOLD),
            link: Style::default().fg(cyan).add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(cyan)
                .bg(base02)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(base01),
            label_focus: Style::default().fg(cyan),
            suffix: Style::default().fg(base01),
            separator: Style::default().fg(base01),
            overlay_select: Style::default().bg(base02),
            spell_error: Style::default().fg(red).add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn solarized_light() -> Self {
        let base3 = Color::Rgb(253, 246, 227);
        let base2 = Color::Rgb(238, 232, 213);
        let base1 = Color::Rgb(147, 161, 161);
        let base00 = Color::Rgb(101, 123, 131);
        let cyan = Color::Rgb(42, 161, 152);
        let yellow = Color::Rgb(181, 137, 0);
        let red = Color::Rgb(220, 50, 47);
        Self {
            base: Style::default().fg(base00).bg(base3),
            border: Style::default().fg(base1),
            bar: Style::default().fg(base00).bg(base2),
            show_bars: false,
            status_tab_active: Style::default().fg(base3).bg(cyan),
            status_tab_inactive: Style::default().fg(cyan),
            status_view: Style::default().fg(yellow),
            focus_bg: Style::default().bg(base2),
            focus_fg: Style::default().fg(cyan),
            table_header: Style::default().fg(base00).add_modifier(Modifier::BOLD),
            link: Style::default().fg(cyan).add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(cyan)
                .bg(base2)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(base1),
            label_focus: Style::default().fg(cyan),
            suffix: Style::default().fg(base1),
            separator: Style::default().fg(base1),
            overlay_select: Style::default().bg(base2),
            spell_error: Style::default().fg(red).add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn dracula() -> Self {
        let bg = Color::Rgb(40, 42, 54);
        let bg_alt = Color::Rgb(54, 57, 72);
        let fg = Color::Rgb(248, 248, 242);
        let border = Color::Rgb(98, 114, 164);
        let accent = Color::Rgb(139, 233, 253);
        let warn = Color::Rgb(241, 250, 140);
        let err = Color::Rgb(255, 85, 85);
        Self {
            base: Style::default().fg(fg).bg(bg),
            border: Style::default().fg(border),
            bar: Style::default().fg(fg).bg(bg_alt),
            show_bars: false,
            status_tab_active: Style::default().fg(bg).bg(accent),
            status_tab_inactive: Style::default().fg(accent),
            status_view: Style::default().fg(warn),
            focus_bg: Style::default().bg(bg_alt),
            focus_fg: Style::default().fg(accent),
            table_header: Style::default().fg(fg).add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(accent)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(accent)
                .bg(bg_alt)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(border),
            label_focus: Style::default().fg(accent),
            suffix: Style::default().fg(border),
            separator: Style::default().fg(border),
            overlay_select: Style::default().bg(bg_alt),
            spell_error: Style::default().fg(err).add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn catppuccin_mocha() -> Self {
        let base = Color::Rgb(30, 30, 46);
        let mantle = Color::Rgb(24, 24, 37);
        let text = Color::Rgb(205, 214, 244);
        let subtext = Color::Rgb(166, 173, 200);
        let sapphire = Color::Rgb(116, 199, 236);
        let yellow = Color::Rgb(249, 226, 175);
        let red = Color::Rgb(243, 139, 168);
        Self {
            base: Style::default().fg(text).bg(base),
            border: Style::default().fg(subtext),
            bar: Style::default().fg(text).bg(mantle),
            show_bars: false,
            status_tab_active: Style::default().fg(base).bg(sapphire),
            status_tab_inactive: Style::default().fg(sapphire),
            status_view: Style::default().fg(yellow),
            focus_bg: Style::default().bg(mantle),
            focus_fg: Style::default().fg(sapphire),
            table_header: Style::default().fg(text).add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(sapphire)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(sapphire)
                .bg(mantle)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(subtext),
            label_focus: Style::default().fg(sapphire),
            suffix: Style::default().fg(subtext),
            separator: Style::default().fg(subtext),
            overlay_select: Style::default().bg(mantle),
            spell_error: Style::default().fg(red).add_modifier(Modifier::UNDERLINED),
        }
    }

    pub(crate) fn catppuccin_latte() -> Self {
        let base = Color::Rgb(239, 241, 245);
        let mantle = Color::Rgb(230, 233, 239);
        let text = Color::Rgb(76, 79, 105);
        let subtext = Color::Rgb(140, 143, 161);
        let sapphire = Color::Rgb(32, 159, 181);
        let yellow = Color::Rgb(223, 142, 29);
        let red = Color::Rgb(210, 15, 57);
        Self {
            base: Style::default().fg(text).bg(base),
            border: Style::default().fg(subtext),
            bar: Style::default().fg(text).bg(mantle),
            show_bars: false,
            status_tab_active: Style::default().fg(base).bg(sapphire),
            status_tab_inactive: Style::default().fg(sapphire),
            status_view: Style::default().fg(yellow),
            focus_bg: Style::default().bg(mantle),
            focus_fg: Style::default().fg(sapphire),
            table_header: Style::default().fg(text).add_modifier(Modifier::BOLD),
            link: Style::default()
                .fg(sapphire)
                .add_modifier(Modifier::UNDERLINED),
            link_selected: Style::default()
                .fg(sapphire)
                .bg(mantle)
                .add_modifier(Modifier::UNDERLINED),
            label: Style::default().fg(subtext),
            label_focus: Style::default().fg(sapphire),
            suffix: Style::default().fg(subtext),
            separator: Style::default().fg(subtext),
            overlay_select: Style::default().bg(mantle),
            spell_error: Style::default().fg(red).add_modifier(Modifier::UNDERLINED),
        }
    }
}
