//! Minimal XDG menu fragment loader.
//!
//! We only support what the launcher actually needs: a flat map from raw
//! category token (lowercased) -> display name. The full menu spec is
//! deeply recursive (nested menus, merge files, OnlyUnallocated, exclude
//! rules, ...), but for routing apps into a single-category-per-app launch
//! UI we just need: "if a .desktop has Category X, show it under menu Y".
//!
//! Source dirs:
//! - User: `$XDG_CONFIG_HOME/menus/applications-merged/*.menu` (defaults
//!   to `~/.config/menus/applications-merged`).
//! .directory lookups happen against `desktop-directories` dirs from
//! `XDG_DATA_HOME` and `XDG_DATA_DIRS`.
//!
//! System menu fragments under `/etc/xdg` are intentionally NOT read so
//! the hardcoded category buckets stay authoritative for stock apps. Users
//! who want to override stock buckets can drop a fragment in their config.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

/// Build a map of `lowercased category token -> display menu name` from
/// any `.menu` fragments under the user's XDG config dir. Returns an empty
/// map on any error.
pub fn load_category_overrides() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let menu_dirs = xdg_menu_dirs();
    let dir_entry_dirs = xdg_directory_entry_dirs();

    for menu_dir in &menu_dirs {
        let Ok(entries) = fs::read_dir(menu_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("menu") {
                continue;
            }
            // Best-effort: a malformed fragment shouldn't break the launcher.
            let _ = parse_menu_file(&path, &dir_entry_dirs, &mut out);
        }
    }

    out
}

fn xdg_menu_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(home).join("menus/applications-merged"));
    } else if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".config/menus/applications-merged"));
    }
    out
}

fn xdg_directory_entry_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = std::env::var("XDG_DATA_HOME").ok().filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(h).join("desktop-directories"));
    } else if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".local/share/desktop-directories"));
    }
    match std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()) {
        Some(dirs) => {
            for d in dirs.split(':') {
                if !d.is_empty() {
                    out.push(PathBuf::from(d).join("desktop-directories"));
                }
            }
        }
        None => {
            out.push(PathBuf::from("/usr/local/share/desktop-directories"));
            out.push(PathBuf::from("/usr/share/desktop-directories"));
        }
    }
    out
}

fn resolve_directory_name(dir_file: &str, dir_entry_dirs: &[PathBuf]) -> Option<String> {
    for d in dir_entry_dirs {
        let p = d.join(dir_file);
        let Ok(content) = fs::read_to_string(&p) else {
            continue;
        };
        let mut in_entry = false;
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            // Skip locale-suffixed Name[xx]= variants; take the bare key.
            if let Some(rest) = line.strip_prefix("Name=") {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

struct Frame {
    name: Option<String>,
    directory: Option<String>,
    in_include: bool,
    categories: Vec<String>,
}

impl Frame {
    fn new() -> Self {
        Self {
            name: None,
            directory: None,
            in_include: false,
            categories: Vec::new(),
        }
    }
}

fn parse_menu_file(
    path: &Path,
    dir_entry_dirs: &[PathBuf],
    out: &mut HashMap<String, String>,
) -> Result<(), quick_xml::Error> {
    let mut reader = Reader::from_file(path)?;
    reader.config_mut().trim_text(true);

    let mut stack: Vec<Frame> = Vec::new();
    let mut path_names: Vec<String> = Vec::new();
    let mut text_buf = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "Menu" {
                    stack.push(Frame::new());
                } else if name == "Include" {
                    if let Some(top) = stack.last_mut() {
                        top.in_include = true;
                    }
                }
                path_names.push(name);
                text_buf.clear();
            }
            Event::Text(t) => {
                if let Ok(s) = t.unescape() {
                    text_buf.push_str(&s);
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let text = std::mem::take(&mut text_buf);

                if let Some(top) = stack.last_mut() {
                    let parent_is_menu = path_names
                        .len()
                        .checked_sub(2)
                        .and_then(|i| path_names.get(i))
                        .map(|s| s == "Menu")
                        .unwrap_or(false);

                    match name.as_str() {
                        "Name" if parent_is_menu => top.name = Some(text.trim().to_string()),
                        "Directory" if parent_is_menu => {
                            top.directory = Some(text.trim().to_string())
                        }
                        "Category" if top.in_include => {
                            top.categories.push(text.trim().to_string())
                        }
                        _ => {}
                    }
                }

                if name == "Include" {
                    if let Some(top) = stack.last_mut() {
                        top.in_include = false;
                    }
                } else if name == "Menu" {
                    if let Some(frame) = stack.pop() {
                        if !frame.categories.is_empty() {
                            let display = frame
                                .directory
                                .as_deref()
                                .and_then(|d| resolve_directory_name(d, dir_entry_dirs))
                                .or(frame.name);
                            if let Some(disp) = display {
                                for cat in frame.categories {
                                    let key = cat.trim().to_lowercase();
                                    if !key.is_empty() {
                                        out.insert(key, disp.clone());
                                    }
                                }
                            }
                        }
                    }
                }

                path_names.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

/// Resolve an app's display category. Each token of `raw_categories` is
/// checked against the override map (case-insensitive) in order; the
/// first match wins. If nothing matches, returns None and the caller is
/// expected to fall back to the hardcoded bucket logic.
pub fn override_category(
    raw_categories: &str,
    overrides: &HashMap<String, String>,
) -> Option<String> {
    if overrides.is_empty() {
        return None;
    }
    for token in raw_categories.split(';') {
        let key = token.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        if let Some(name) = overrides.get(&key) {
            return Some(name.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_simple_menu_with_directory_lookup() {
        let tmp = tempdir();
        let dir_dirs = [tmp.join("desktop-directories")];
        fs::create_dir_all(&dir_dirs[0]).unwrap();
        write_file(
            &dir_dirs[0],
            "my-scripts.directory",
            "[Desktop Entry]\nType=Directory\nName=My Scripts\n",
        );

        let menu_dir = tmp.join("menus");
        fs::create_dir_all(&menu_dir).unwrap();
        let menu_file = write_file(
            &menu_dir,
            "my.menu",
            r#"<Menu>
                  <Name>Applications</Name>
                  <Menu>
                    <Name>My Scripts</Name>
                    <Directory>my-scripts.directory</Directory>
                    <Include><And><Category>X-MyScripts</Category></And></Include>
                  </Menu>
                </Menu>"#,
        );

        let mut out = HashMap::new();
        parse_menu_file(&menu_file, &dir_dirs, &mut out).unwrap();
        assert_eq!(out.get("x-myscripts").map(String::as_str), Some("My Scripts"));
    }

    #[test]
    fn falls_back_to_menu_name_when_directory_missing() {
        let tmp = tempdir();
        let dir_dirs: [PathBuf; 0] = [];
        let menu_file = write_file(
            &tmp,
            "no-directory.menu",
            r#"<Menu><Menu>
                <Name>Custom</Name>
                <Include><Category>X-Foo</Category></Include>
              </Menu></Menu>"#,
        );
        let mut out = HashMap::new();
        parse_menu_file(&menu_file, &dir_dirs, &mut out).unwrap();
        assert_eq!(out.get("x-foo").map(String::as_str), Some("Custom"));
    }

    #[test]
    fn override_category_picks_first_matching_token() {
        let mut m = HashMap::new();
        m.insert("x-myscripts".into(), "My Scripts".into());
        assert_eq!(
            override_category("Utility;X-MyScripts;", &m).as_deref(),
            Some("My Scripts")
        );
        assert_eq!(override_category("Game;Action;", &m), None);
        assert_eq!(override_category("", &m), None);
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!("bstl-menu-{}", rand_suffix()));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:x}", n)
    }
}
