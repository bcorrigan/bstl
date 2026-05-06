use std::fs;
use std::path::PathBuf;
use std::process;
use ratatui::style::Color;
use ratatui::widgets::BorderType;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchPosition {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StartMode {
    Single,
    Dual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    Block,
    Underline,
    Pipe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherTheme {
    pub border: String,
    pub focus: String,
    pub unfocused: String,
    pub highlight: String,
    pub border_style: String,
    pub highlight_type: String,
    /// Empty means "follow `focus`"; resolved at load time.
    pub cursor_color: String,
    pub cursor_shape: CursorShape,
    pub cursor_blink_interval: u64,
}

impl Default for LauncherTheme {
    fn default() -> Self {
        Self {
            border: "#ffffff".into(),
            focus: "#00ff00".into(),
            unfocused: "#808080".into(),
            highlight: "#0000ff".into(),
            border_style: "plain".into(),
            highlight_type: "background".into(),
            cursor_color: String::new(),
            cursor_shape: CursorShape::Block,
            cursor_blink_interval: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BstlConfig {
    pub dmenu: bool,
    pub search_position: SearchPosition,
    #[serde(alias = "startup_mode")]
    pub start_mode: StartMode,
    pub focus_search_on_switch: bool,
    #[serde(rename = "theme")]
    pub colors: LauncherTheme,
    pub terminal: String,
    pub timeout: u64,
    pub max_recent_apps: usize,
    pub recent_first: bool,
    pub print_selection: bool,
    pub sway: bool,
    pub history_window_days: u32,
    pub top_recent_count: usize,
    pub popularity_weight: i64,
}

impl Default for BstlConfig {
    fn default() -> Self {
        Self {
            dmenu: false,
            search_position: SearchPosition::Top,
            start_mode: StartMode::Single,
            focus_search_on_switch: true,
            colors: LauncherTheme::default(),
            terminal: "foot".into(),
            timeout: 0,
            max_recent_apps: 15,
            recent_first: false,
            print_selection: false,
            sway: false,
            history_window_days: 90,
            top_recent_count: 5,
            popularity_weight: 10,
        }
    }
}

impl LauncherTheme {
    /// Convert hex string to ratatui::Color
    pub fn parse_color(color: &str) -> Color {
        let color = color.trim();

        if color.starts_with('#') {
            let hex = &color[1..];

            match hex.len() {
                // #RGB format
                3 => {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..1], 16),
                        u8::from_str_radix(&hex[1..2], 16),
                        u8::from_str_radix(&hex[2..3], 16),
                    ) {
                        return Color::Rgb(r * 17, g * 17, b * 17);
                    }
                }
                // #RRGGBB format
                6 => {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Color::Rgb(r, g, b);
                    }
                }
                // #RRGGBBAA format (ignore alpha)
                8 => {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Color::Rgb(r, g, b);
                    }
                }
                _ => {}
            }
        }

        Color::Reset
    }

    pub fn parse_border_type(style: &str) -> BorderType {
        match style.to_lowercase().as_str() {
            "plain" => BorderType::Plain,
            "rounded" => BorderType::Rounded,
            "thick" => BorderType::Thick,
            "double" => BorderType::Double,
            _ => BorderType::Plain,
        }
    }
}

/// Parse a TOML string into a BstlConfig, applying post-load fallbacks
/// (currently: cursor_color follows focus when unset).
fn parse_toml_config(content: &str) -> Result<BstlConfig, toml::de::Error> {
    let mut cfg: BstlConfig = toml::from_str(content)?;
    if cfg.colors.cursor_color.trim().is_empty() {
        cfg.colors.cursor_color = cfg.colors.focus.clone();
    }
    Ok(cfg)
}

/// Warn (once, on startup) if a legacy rune-format config is sitting at the
/// new path. The format change is breaking — we don't try to translate.
fn warn_on_legacy_rune_files() {
    let candidates = [
        dirs::config_dir().map(|c| c.join("bstl/bstl.rune")),
        dirs::config_dir().map(|c| c.join("dstl/dstl.rune")),
    ];
    for c in candidates.into_iter().flatten() {
        if c.exists() {
            eprintln!(
                "bstl: legacy rune-format config found at {} — bstl now uses TOML. \
                 See examples/bstl.toml and migrate by hand; the legacy file is otherwise ignored.",
                c.display()
            );
        }
    }
}

/// Top-level config loader. Falls back to defaults if no config file exists,
/// and exits with a friendly diagnostic on parse error.
pub fn load_launcher_config() -> BstlConfig {
    warn_on_legacy_rune_files();

    let user_path = dirs::config_dir()
        .map(|c| c.join("bstl/bstl.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.config/bstl/bstl.toml"));
    let system_path = PathBuf::from("/usr/share/doc/bstl/bstl.toml");

    let path = if user_path.exists() {
        Some(user_path)
    } else if system_path.exists() {
        Some(system_path)
    } else {
        None
    };

    let Some(path) = path else {
        return BstlConfig::default();
    };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bstl: failed to read {}: {}", path.display(), e);
            process::exit(1);
        }
    };

    parse_toml_config(&content).unwrap_or_else(|e| {
        eprintln!(
            "bstl: failed to parse {}:\n{}",
            path.display(),
            e
        );
        process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_yields_defaults() {
        let cfg = parse_toml_config("").unwrap();
        let def = BstlConfig::default();
        assert_eq!(cfg.dmenu, def.dmenu);
        assert_eq!(cfg.terminal, def.terminal);
        assert_eq!(cfg.search_position, def.search_position);
        assert_eq!(cfg.start_mode, def.start_mode);
    }

    #[test]
    fn partial_config_inherits_defaults() {
        let cfg = parse_toml_config(
            r##"
            terminal = "alacritty"
            timeout = 30
            "##,
        )
        .unwrap();
        assert_eq!(cfg.terminal, "alacritty");
        assert_eq!(cfg.timeout, 30);
        // Untouched fields keep defaults
        assert_eq!(cfg.max_recent_apps, 15);
        assert_eq!(cfg.search_position, SearchPosition::Top);
    }

    #[test]
    fn theme_section_parses() {
        let cfg = parse_toml_config(
            r##"
            [theme]
            border = "#abcdef"
            cursor_shape = "underline"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.colors.border, "#abcdef");
        assert_eq!(cfg.colors.cursor_shape, CursorShape::Underline);
        // Untouched theme fields still default
        assert_eq!(cfg.colors.unfocused, "#808080");
    }

    #[test]
    fn cursor_color_falls_back_to_focus_when_empty() {
        let cfg = parse_toml_config(
            r##"
            [theme]
            focus = "#112233"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.colors.cursor_color, "#112233");
    }

    #[test]
    fn cursor_color_explicit_overrides_focus() {
        let cfg = parse_toml_config(
            r##"
            [theme]
            focus = "#112233"
            cursor_color = "#aabbcc"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.colors.cursor_color, "#aabbcc");
    }

    #[test]
    fn enums_are_lowercase() {
        let cfg = parse_toml_config(
            r##"
            search_position = "bottom"
            start_mode = "dual"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.search_position, SearchPosition::Bottom);
        assert_eq!(cfg.start_mode, StartMode::Dual);
    }

    #[test]
    fn startup_mode_alias_still_works() {
        // Old example used `startup_mode`. Accept it as an alias for `start_mode`.
        let cfg = parse_toml_config(r##"startup_mode = "dual""##).unwrap();
        assert_eq!(cfg.start_mode, StartMode::Dual);
    }

    #[test]
    fn shipped_example_parses() {
        // Guards against the example file drifting away from the schema.
        let example = include_str!("../examples/bstl.toml");
        let cfg = parse_toml_config(example).expect("examples/bstl.toml must parse");
        // Sanity-check a couple of values from the example.
        assert_eq!(cfg.terminal, "alacritty");
        assert_eq!(cfg.colors.border, "#ffffff");
    }
}
