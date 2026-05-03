use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use eyre::Result;
use ratatui::style::Color;
use ratatui::widgets::BorderType;
use rune_cfg::{RuneConfig, Value, RuneError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchPosition {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StartMode {
    Single,
    Dual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CursorShape {
    Block,      // █
    Underline,  // _
    Pipe,       // |
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherTheme {
    pub border: String,
    pub focus: String,
    pub unfocused: String,
    pub highlight: String,
    pub border_style: String,
    pub highlight_type: String,
    pub cursor_color: String,
    pub cursor_shape: CursorShape,
    pub cursor_blink_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BstlConfig {
    pub dmenu: bool,
    pub search_position: SearchPosition,
    pub start_mode: StartMode,
    pub focus_search_on_switch: bool,
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

impl LauncherTheme {
    /// Convert hex string to ratatui::Color
    pub fn parse_color(color: &str) -> Color {
        let color = color.trim();
        
        // Handle hex colors (#RGB, #RRGGBB, #RRGGBBAA)
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
                        // Expand single digit to double (e.g., F -> FF)
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
                // #RRGGBBAA format (ignore alpha for now)
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
        
        // Fallback to reset if parsing fails
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

/// Helper: tries key as-is, then _ → -, then - → _
fn get_config_or<T>(
    config: &RuneConfig,
    key: &str,
    default: T,
) -> T
where
    T: Clone + TryFrom<Value, Error = RuneError>,
{
    let variants = [
        key.to_string(),
        key.replace('_', "-"),
        key.replace('-', "_"),
    ];

    for k in variants {
        if let Ok(val) = config.get::<T>(&k) {
            return val;
        }
    }

    default
}

/// Extract BstlConfig from a loaded RuneConfig
fn extract_bstl_config(config: RuneConfig) -> Result<BstlConfig> {
    // --- Fetch values with validation ---
    let dmenu = get_config_or(&config, "bstl.dmenu", false);
    let terminal = get_config_or(&config, "bstl.terminal", "foot".to_string());
    let timeout = get_config_or(&config, "bstl.timeout", 0u64);
    let max_recent_apps: usize = get_config_or(&config, "bstl.max_recent_apps", 15u64) as usize;
    let recent_first = get_config_or(&config, "bstl.recent_first", false);
    let print_selection = get_config_or(&config, "bstl.print_selection", false);
    let sway = get_config_or(&config, "bstl.sway", false);
    let history_window_days = get_config_or(&config, "bstl.history_window_days", 90u64) as u32;
    let top_recent_count = get_config_or(&config, "bstl.top_recent_count", 5u64) as usize;
    let popularity_weight = get_config_or(&config, "bstl.popularity_weight", 10u64) as i64;

    // Validate search_position
    let search_position_str: String = get_config_or(&config, "bstl.search_position", "top".to_string());
    let search_position = match search_position_str.to_lowercase().as_str() {
        "top" => SearchPosition::Top,
        "bottom" => SearchPosition::Bottom,
        _ => SearchPosition::Top,
    };

    // Validate startup_mode
    let start_mode_str: String = get_config_or(&config, "bstl.startup_mode", "single".to_string());
    let start_mode = match start_mode_str.to_lowercase().as_str() {
        "single" => StartMode::Single,
        "dual" => StartMode::Dual,
        _ => StartMode::Single,
    };

    // Load colors with theme priority system
    let (border_color, focus_color, unfocused_color, highlight_color, cursor_color) = load_theme_colors(&config)?;

    let cursor_shape_str: String = get_config_or(&config, "bstl.theme.cursor_shape", "block".to_string());
    let cursor_shape = match cursor_shape_str.to_lowercase().as_str() {
        "block" => CursorShape::Block,
        "underline" => CursorShape::Underline,
        "pipe" => CursorShape::Pipe,
        _ => CursorShape::Block,
    };

    let cursor_blink_interval: u64 = get_config_or(&config, "bstl.theme.cursor_blink_interval", 0u64);
    let border_style: String = get_config_or(&config, "bstl.theme.border_style", "plain".to_string());
    let highlight_type: String = get_config_or(&config, "bstl.theme.highlight_type", "background".to_string());
    let focus_search: bool = get_config_or(&config, "bstl.focus_search_on_switch", true);

    let colors = LauncherTheme {
        border: border_color,
        focus: focus_color,
        unfocused: unfocused_color,
        highlight: highlight_color,
        border_style,
        highlight_type,
        cursor_color,
        cursor_shape,
        cursor_blink_interval,
    };

    Ok(BstlConfig {
        dmenu,
        search_position,
        start_mode,
        focus_search_on_switch: focus_search,
        colors,
        terminal,
        timeout,
        max_recent_apps,
        recent_first,
        print_selection,
        sway,
        history_window_days,
        top_recent_count,
        popularity_weight,
    })
}

/// Load theme colors with priority system similar to claw
fn load_theme_colors(config: &RuneConfig) -> Result<(String, String, String, String, String)> {
    let mut border = None;
    let mut focus = None;
    let mut unfocused = None;
    let mut highlight = None;
    let mut cursor = None;

    // PRIORITY 1: Check for aliased gather imports
    let aliases = config.import_aliases();
    for alias in aliases {
        if config.has_document(&alias) {
            // Test if this import has theme data
            let test_path = format!("{}.bstl.theme.border", alias);
            if let Ok(val) = config.get::<String>(&test_path) {
                border = Some(val);
                focus = config.get::<String>(&format!("{}.bstl.theme.focus", alias)).ok();
                unfocused = config.get::<String>(&format!("{}.bstl.theme.unfocused", alias)).ok();
                highlight = config.get::<String>(&format!("{}.bstl.theme.highlight", alias)).ok();
                cursor = config.get::<String>(&format!("{}.bstl.theme.cursor_color", alias)).ok();
                break;
            }
        }
    }

    // PRIORITY 2: Check for top-level theme (from non-aliased gather or main config)
    if border.is_none() {
        border = config.get::<String>("bstl.theme.border").ok();
        focus = config.get::<String>("bstl.theme.focus").ok();
        unfocused = config.get::<String>("bstl.theme.unfocused").ok();
        highlight = config.get::<String>("bstl.theme.highlight").ok();
        cursor = config.get::<String>("bstl.theme.cursor_color").ok();
    }

    // PRIORITY 3: Check for "theme" document
    if border.is_none() && config.has_document("theme") {
        border = config.get::<String>("theme.bstl.theme.border").ok();
        focus = config.get::<String>("theme.bstl.theme.focus").ok();
        unfocused = config.get::<String>("theme.bstl.theme.unfocused").ok();
        highlight = config.get::<String>("theme.bstl.theme.highlight").ok();
        cursor = config.get::<String>("theme.bstl.theme.cursor_color").ok();
    }

    // Defaults
    let border = border.unwrap_or_else(|| "#ffffff".to_string());
    let focus = focus.unwrap_or_else(|| "#00ff00".to_string());
    let unfocused = unfocused.unwrap_or_else(|| "#808080".to_string());
    let highlight = highlight.unwrap_or_else(|| "#0000ff".to_string());
    let cursor = cursor.unwrap_or_else(|| focus.clone());

    Ok((border, focus, unfocused, highlight, cursor))
}

/// One-shot migration from a pre-rename `~/.config/dstl/dstl.rune`. Copies
/// the file to the new location with the top-level `dstl:` document key
/// rewritten to `bstl:`. The legacy file is left in place so the user can
/// verify the result before deleting it themselves.
fn migrate_legacy_config(new_path: &Path) {
    if new_path.exists() {
        return;
    }
    let Some(legacy) = dirs::config_dir().map(|c| c.join("dstl/dstl.rune")) else {
        return;
    };
    if !legacy.exists() {
        return;
    }
    let Ok(content) = fs::read_to_string(&legacy) else {
        return;
    };

    // Only replace the document-name line (`dstl:` at column 0, possibly with
    // trailing whitespace). Do not touch anything indented or inside strings.
    let migrated: String = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_end();
            if trimmed == "dstl:" {
                "bstl:".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if let Some(parent) = new_path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = fs::write(new_path, migrated);
    eprintln!(
        "bstl: migrated config {} -> {} (legacy file left in place)",
        legacy.display(),
        new_path.display()
    );
}

/// Top-level config loader that exits gracefully on failure.
pub fn load_launcher_config() -> BstlConfig {
    let user_config = dirs::config_dir()
        .map(|c| c.join("bstl/bstl.rune"))
        .unwrap_or_else(|| PathBuf::from("~/.config/bstl/bstl.rune"));

    migrate_legacy_config(&user_config);

    let system_config = PathBuf::from("/usr/share/doc/bstl/bstl.rune");
    
    // Load config with automatic import resolution and fallback support
    let config = RuneConfig::from_file_with_fallback(&user_config, &system_config)
        .unwrap_or_else(|e| {
            eprintln!("❌ Configuration error:\n{}", e);
            process::exit(1);
        });

    // Extract BstlConfig from the loaded RuneConfig
    extract_bstl_config(config).unwrap_or_else(|e| {
        eprintln!("❌ Configuration parsing error:\n{}", e);
        process::exit(1);
    })
}
