use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use chrono::Utc;
use eyre::{Context, Result};
use rusqlite::{Connection, params};

use crate::app::AppEntry;

const SCHEMA_VERSION: i32 = 1;

pub struct Storage {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct CachedApp {
    pub name: String,
    pub category: String,
    pub exec: String,
    pub terminal: bool,
}

impl Storage {
    /// In-memory storage for tests.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        let mut s = Self { conn };
        s.migrate().unwrap();
        s
    }

    /// Open (or create) the bstl sqlite database under XDG_DATA_HOME.
    pub fn open() -> Result<Self> {
        let data_root = dirs::data_dir()
            .ok_or_else(|| eyre::eyre!("could not resolve XDG_DATA_HOME"))?;
        let dir = data_root.join("bstl");
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        let path = dir.join("bstl.sqlite");

        // One-shot rename from the pre-rename location, so users carrying state
        // forward from the dstl-named build don't lose their launch history.
        if !path.exists() {
            let legacy = data_root.join("dstl/dstl.sqlite");
            if legacy.exists() {
                let _ = fs::rename(&legacy, &path);
            }
        }

        let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;

        // WAL gives us crash safety and a fast launch path.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();

        let mut storage = Self { conn };
        storage.migrate()?;
        storage.maybe_import_legacy_recent()?;
        Ok(storage)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i32 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        if current < 1 {
            tx.execute_batch(
                r#"
                CREATE TABLE apps (
                    name        TEXT PRIMARY KEY,
                    exec        TEXT NOT NULL,
                    category    TEXT NOT NULL,
                    terminal    INTEGER NOT NULL,
                    source_path TEXT NOT NULL,
                    file_mtime  INTEGER NOT NULL
                );
                CREATE INDEX apps_source ON apps(source_path);

                CREATE TABLE scan_meta (
                    directory  TEXT PRIMARY KEY,
                    dir_mtime  INTEGER NOT NULL,
                    scanned_at INTEGER NOT NULL
                );

                CREATE TABLE launches (
                    ts   TEXT NOT NULL,
                    name TEXT NOT NULL
                );
                CREATE INDEX launches_name_ts ON launches(name, ts);
                CREATE INDEX launches_ts      ON launches(ts);
                "#,
            )?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    /// One-shot migration of the old recent.json MRU list into the launches
    /// table, so we don't start with an empty popularity model.
    fn maybe_import_legacy_recent(&self) -> Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM launches", [], |r| r.get(0))?;
        if count > 0 {
            return Ok(());
        }

        let Some(cache_dir) = dirs::cache_dir() else {
            return Ok(());
        };
        let recent_file = cache_dir.join("dstl/recent.json");
        if !recent_file.exists() {
            return Ok(());
        }

        let json = match fs::read_to_string(&recent_file) {
            Ok(j) => j,
            Err(_) => return Ok(()),
        };
        let names: Vec<String> = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };

        // Synthesize timestamps so the most-recently-used entry looks newest.
        // Spread across the last `len` minutes — arbitrary but preserves order.
        let now = Utc::now();
        let stmt = "INSERT INTO launches(ts, name) VALUES (?, ?)";
        let mut prep = self.conn.prepare(stmt)?;
        for (i, name) in names.iter().enumerate() {
            let ts = now - chrono::Duration::minutes(i as i64);
            prep.execute(params![ts.format("%Y-%m-%dT%H:%M:%SZ").to_string(), name])?;
        }
        drop(prep);

        // Migration done — remove the legacy file so we don't keep checking it.
        let _ = fs::remove_file(&recent_file);
        Ok(())
    }

    /// Refresh the cached `apps` table by walking the given directories.
    /// Uses directory mtime to skip unchanged dirs entirely; within a changed
    /// dir, only re-parses .desktop files whose own mtime is newer than the
    /// cached row's mtime.
    pub fn refresh_app_cache(
        &mut self,
        dirs: &[String],
        current_desktops: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Track which (source_path) values we've seen so we can prune deleted files.
        let mut seen_paths: Vec<String> = Vec::new();

        for dir in dirs {
            let dir_path = Path::new(dir);
            let dir_mtime = match mtime_secs(dir_path) {
                Some(m) => m,
                None => continue, // dir doesn't exist
            };

            let cached_dir_mtime: Option<i64> = tx
                .query_row(
                    "SELECT dir_mtime FROM scan_meta WHERE directory = ?",
                    params![dir],
                    |r| r.get(0),
                )
                .ok();

            // Even if dir mtime hasn't changed we still need to know what files
            // *should* still exist, so we collect paths as we scan. To keep that
            // cheap on the hot path: when dir mtime is unchanged, we trust the
            // cache and read all source_paths in this dir directly from the DB.
            if cached_dir_mtime == Some(dir_mtime) {
                let mut stmt = tx.prepare(
                    "SELECT source_path FROM apps WHERE source_path LIKE ?",
                )?;
                let pat = format!("{}/%", dir.trim_end_matches('/'));
                let rows = stmt.query_map(params![pat], |r| r.get::<_, String>(0))?;
                for r in rows {
                    seen_paths.push(r?);
                }
                continue;
            }

            // Dir changed: walk it.
            let entries = match fs::read_dir(dir_path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                    continue;
                }
                let path_str = path.to_string_lossy().into_owned();
                seen_paths.push(path_str.clone());

                let file_mtime = mtime_secs(&path).unwrap_or(0);
                let cached_file_mtime: Option<i64> = tx
                    .query_row(
                        "SELECT file_mtime FROM apps WHERE source_path = ?",
                        params![path_str],
                        |r| r.get(0),
                    )
                    .ok();

                if cached_file_mtime == Some(file_mtime) {
                    continue;
                }

                // Re-parse this file.
                let parsed = parse_desktop_file(&path, current_desktops);
                // Drop any previous row from this source_path (handles renames within the file).
                tx.execute("DELETE FROM apps WHERE source_path = ?", params![path_str])?;
                if let Some(app) = parsed {
                    // Use INSERT OR REPLACE to handle the rare case where two .desktop
                    // files declare the same Name=. Last-writer wins, matching the old
                    // behavior loosely (the old code skipped duplicates).
                    tx.execute(
                        "INSERT OR REPLACE INTO apps(name, exec, category, terminal, source_path, file_mtime)
                         VALUES (?, ?, ?, ?, ?, ?)",
                        params![
                            app.name,
                            app.exec,
                            app.category,
                            app.terminal as i32,
                            path_str,
                            file_mtime,
                        ],
                    )?;
                }
            }

            tx.execute(
                "INSERT INTO scan_meta(directory, dir_mtime, scanned_at)
                 VALUES (?, ?, strftime('%s','now'))
                 ON CONFLICT(directory) DO UPDATE SET
                    dir_mtime = excluded.dir_mtime,
                    scanned_at = excluded.scanned_at",
                params![dir, dir_mtime],
            )?;
        }

        // Prune rows whose source_path no longer exists on disk.
        // We only consider source_paths inside the dirs we were asked about,
        // so we don't accidentally evict cache entries from a dir the caller
        // chose not to scan this time.
        if !seen_paths.is_empty() {
            // Build a temp table of seen paths and delete the complement, scoped
            // to the dirs we walked.
            tx.execute("CREATE TEMP TABLE seen(path TEXT PRIMARY KEY)", [])?;
            {
                let mut ins = tx.prepare("INSERT OR IGNORE INTO seen(path) VALUES (?)")?;
                for p in &seen_paths {
                    ins.execute(params![p])?;
                }
            }
            for dir in dirs {
                let pat = format!("{}/%", dir.trim_end_matches('/'));
                tx.execute(
                    "DELETE FROM apps
                     WHERE source_path LIKE ?
                       AND source_path NOT IN (SELECT path FROM seen)",
                    params![pat],
                )?;
            }
            tx.execute("DROP TABLE seen", [])?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_apps(&self) -> Result<Vec<AppEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, exec, category, terminal FROM apps ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(AppEntry {
                name: r.get(0)?,
                exec: r.get(1)?,
                category: r.get(2)?,
                terminal: r.get::<_, i32>(3)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Distinct app names ordered by most recent launch (descending). Used to
    /// populate the "Recent" view.
    pub fn recent_names(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, MAX(ts) AS last_ts
             FROM launches
             GROUP BY name
             ORDER BY last_ts DESC
             LIMIT ?",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Map of name -> launch count within the trailing `window_days` window.
    pub fn popularity_map(&self, window_days: u32) -> Result<HashMap<String, u32>> {
        let cutoff = Utc::now() - chrono::Duration::days(window_days as i64);
        let cutoff_str = cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut stmt = self.conn.prepare(
            "SELECT name, COUNT(*) FROM launches WHERE ts >= ? GROUP BY name",
        )?;
        let rows = stmt.query_map(params![cutoff_str], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
        })?;
        let mut map = HashMap::new();
        for r in rows {
            let (name, count) = r?;
            map.insert(name, count);
        }
        Ok(map)
    }

    pub fn record_launch(&self, name: &str) -> Result<()> {
        let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        self.conn
            .execute("INSERT INTO launches(ts, name) VALUES (?, ?)", params![ts, name])?;
        Ok(())
    }
}

fn mtime_secs(p: &Path) -> Option<i64> {
    let md = fs::metadata(p).ok()?;
    let mt = md.modified().ok()?;
    let dur = mt.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

/// Parse a single .desktop file. Returns None if it should be skipped (Hidden,
/// NoDisplay, blocked by OnlyShowIn/NotShowIn, or missing required fields).
fn parse_desktop_file(path: &Path, current_desktops: &[String]) -> Option<CachedApp> {
    let content = fs::read_to_string(path).ok()?;

    let mut name: Option<String> = None;
    let mut generic_name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut categories: Option<String> = None;
    let mut no_display = false;
    let mut terminal = false;
    let mut only_show_in: Option<Vec<String>> = None;
    let mut not_show_in: Option<Vec<String>> = None;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.contains('[') {
            continue;
        }
        let key = key.trim();
        let value = value.trim();
        match key {
            "Name" => name = Some(value.to_string()),
            "GenericName" => generic_name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "Categories" => categories = Some(value.to_string()),
            "NoDisplay" => no_display = value == "true",
            "Hidden" => no_display = no_display || value == "true",
            "Terminal" => terminal = value == "true",
            "OnlyShowIn" => {
                only_show_in = Some(
                    value
                        .split(';')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect(),
                );
            }
            "NotShowIn" => {
                not_show_in = Some(
                    value
                        .split(';')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect(),
                );
            }
            _ => {}
        }
    }

    if no_display {
        return None;
    }

    let name = name.or(generic_name)?;
    let exec_raw = exec?;

    if let Some(allow) = &only_show_in {
        let allowed = allow
            .iter()
            .any(|d| current_desktops.contains(&d.to_lowercase()));
        if !allowed {
            return None;
        }
    }
    if let Some(block) = &not_show_in {
        let blocked = block
            .iter()
            .any(|d| current_desktops.contains(&d.to_lowercase()));
        if blocked {
            return None;
        }
    }

    let category = group_category(categories.as_deref().unwrap_or(""), &name);
    let exec_clean = clean_exec(&exec_raw);

    Some(CachedApp {
        name,
        category,
        exec: exec_clean,
        terminal,
    })
}

fn clean_exec(exec: &str) -> String {
    let mut result = exec.to_string();
    let field_codes = [
        "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v", "%m",
    ];
    for code in &field_codes {
        result = result.replace(code, "");
    }
    result.trim().to_string()
}

fn group_category(raw: &str, app_name: &str) -> String {
    let raw = raw.to_lowercase();

    if app_name.eq_ignore_ascii_case("claw") || app_name.eq_ignore_ascii_case("rofi") {
        return "Utilities".to_string();
    }

    if raw.contains("game") {
        "Games".to_string()
    } else if raw.contains("utility") {
        "Utilities".to_string()
    } else if raw.contains("development") {
        "Development".to_string()
    } else if raw.contains("network") {
        "Network".to_string()
    } else if raw.contains("audio") || raw.contains("video") {
        "Audio/Video".to_string()
    } else if raw.contains("graphics")
        || raw.contains("2dgraphics")
        || raw.contains("3dgraphics")
    {
        "Graphics".to_string()
    } else if raw.contains("system") {
        "System".to_string()
    } else if raw.contains("office") {
        "Office".to_string()
    } else if raw.contains("education") {
        "Education".to_string()
    } else if raw.contains("settings") {
        "Settings".to_string()
    } else {
        "Utilities".to_string()
    }
}

/// Resolve the ordered list of XDG application directories.
pub fn xdg_application_dirs() -> Vec<String> {
    let mut paths = Vec::new();

    let data_home = std::env::var("XDG_DATA_HOME").ok().filter(|s| !s.is_empty());
    if let Some(home) = data_home {
        paths.push(format!("{}/applications", home));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(format!("{}/.local/share/applications", home));
    }

    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty());
    match data_dirs {
        Some(dirs) => {
            for dir in dirs.split(':') {
                if !dir.is_empty() {
                    paths.push(format!("{}/applications", dir));
                }
            }
        }
        None => {
            paths.push("/usr/local/share/applications".to_string());
            paths.push("/usr/share/applications".to_string());
        }
    }

    paths
}

pub fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default()
        .split(':')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_count_launches() {
        let s = Storage::in_memory();
        s.record_launch("Firefox").unwrap();
        s.record_launch("Firefox").unwrap();
        s.record_launch("Vim").unwrap();
        let map = s.popularity_map(30).unwrap();
        assert_eq!(map.get("Firefox"), Some(&2));
        assert_eq!(map.get("Vim"), Some(&1));
    }

    #[test]
    fn recent_names_orders_by_last_launch() {
        let s = Storage::in_memory();
        s.conn
            .execute(
                "INSERT INTO launches(ts, name) VALUES ('2025-01-01T00:00:00Z','Old')",
                [],
            )
            .unwrap();
        s.conn
            .execute(
                "INSERT INTO launches(ts, name) VALUES ('2026-01-01T00:00:00Z','New')",
                [],
            )
            .unwrap();
        let names = s.recent_names(10).unwrap();
        assert_eq!(names, vec!["New".to_string(), "Old".to_string()]);
    }

    #[test]
    fn group_category_falls_back_to_utilities() {
        assert_eq!(group_category("", "anything"), "Utilities");
        assert_eq!(group_category("Game;Action;", "Foo"), "Games");
        assert_eq!(group_category("Network;", "Bar"), "Network");
    }

    #[test]
    fn clean_exec_strips_field_codes() {
        assert_eq!(clean_exec("firefox %u"), "firefox");
        assert_eq!(clean_exec("vlc --foo %F"), "vlc --foo");
    }

    // The xdg_application_dirs tests mutate process-wide env vars and must run
    // serially. We chain them into one test to avoid cross-thread interference.
    #[test]
    fn xdg_application_dirs_resolves() {
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/tmp/xdg_home");
            std::env::set_var("XDG_DATA_DIRS", "/tmp/xdg_dir1:/tmp/xdg_dir2");
        }
        let paths = xdg_application_dirs();
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], "/tmp/xdg_home/applications");
        assert_eq!(paths[1], "/tmp/xdg_dir1/applications");
        assert_eq!(paths[2], "/tmp/xdg_dir2/applications");

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_DATA_DIRS");
            std::env::set_var("HOME", "/home/test");
        }
        let paths = xdg_application_dirs();
        assert_eq!(paths[0], "/home/test/.local/share/applications");
        assert_eq!(paths[1], "/usr/local/share/applications");
        assert_eq!(paths[2], "/usr/share/applications");

        unsafe {
            std::env::set_var("XDG_DATA_HOME", "");
            std::env::set_var("XDG_DATA_DIRS", "");
        }
        let paths = xdg_application_dirs();
        assert_eq!(paths[0], "/home/test/.local/share/applications");
        assert_eq!(paths[1], "/usr/local/share/applications");
        assert_eq!(paths[2], "/usr/share/applications");
    }
}
