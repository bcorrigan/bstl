use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
use crate::config::BstlConfig;
use crate::storage::Storage;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use tui_input::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Search,
    Categories,
    Apps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    SinglePane,
    DualPane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinglePaneMode {
    Dmenu,       // load apps from PATH (dmenu style)
    DesktopApps, // load .desktop apps
}

pub struct App {
    pub mode: Mode,
    pub single_pane_mode: SinglePaneMode,
    pub should_quit: bool,
    pub input: Input,
    pub cursor_visible: bool,
    pub cursor_last_toggle: std::time::Instant,
    pub categories: Vec<String>,
    pub apps: Vec<AppEntry>,
    pub recent_apps: Vec<String>,
    pub selected_category: usize,
    pub selected_app: usize,
    pub focus: Focus,
    pub app_to_launch: Option<String>,
    pub config: BstlConfig,
    pub popularity: HashMap<String, u32>,
    pub storage: Rc<Storage>,
    pub category_overrides: HashMap<String, String>,
    pub apps_list_state: ListState,
    pub categories_list_state: ListState,
    pub apps_rect: Option<Rect>,
    pub categories_rect: Option<Rect>,
    pub search_rect: Option<Rect>,
    fuzzy_matcher: SkimMatcherV2,
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self {
            mode: self.mode,
            single_pane_mode: self.single_pane_mode,
            should_quit: self.should_quit,
            input: self.input.clone(),
            cursor_visible: true,
            cursor_last_toggle: Instant::now(),
            categories: self.categories.clone(),
            apps: self.apps.clone(),
            recent_apps: self.recent_apps.clone(),
            selected_category: self.selected_category,
            selected_app: self.selected_app,
            focus: self.focus,
            app_to_launch: self.app_to_launch.clone(),
            config: self.config.clone(),
            popularity: self.popularity.clone(),
            storage: Rc::clone(&self.storage),
            category_overrides: self.category_overrides.clone(),
            apps_list_state: self.apps_list_state.clone(),
            categories_list_state: self.categories_list_state.clone(),
            apps_rect: self.apps_rect,
            categories_rect: self.categories_rect,
            search_rect: self.search_rect,
            fuzzy_matcher: SkimMatcherV2::default(),
        }
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("mode", &self.mode)
            .field("single_pane_mode", &self.single_pane_mode)
            .field("should_quit", &self.should_quit)
            .field("input", &self.input)
            .field("cursor_visible", &self.cursor_visible)
            .field("cursor_last_toggle", &self.cursor_last_toggle)
            .field("categories", &self.categories)
            .field("apps", &self.apps)
            .field("recent_apps", &self.recent_apps)
            .field("selected_category", &self.selected_category)
            .field("selected_app", &self.selected_app)
            .field("focus", &self.focus)
            .field("app_to_launch", &self.app_to_launch)
            .field("config", &self.config)
            .field("popularity", &self.popularity)
            .field("storage", &"Storage")
            .field("fuzzy_matcher", &"SkimMatcherV2")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub category: String,
    pub exec: String,
    pub terminal: bool,
    pub comment: String,
}

impl AppEntry {
    pub fn needs_terminal(&self) -> bool {
        self.category == "CLI"
            || self.exec.contains("bash")
            || self.exec.contains("sh ")
            || self.exec.contains("python")
            || self.exec.contains("cargo")
            || self.exec.contains("make")
            || self.exec.contains("npm")
    }
}

impl App {
    /// Initialize the app with specified single pane mode and start mode.
    pub fn new(
        single_pane_mode: SinglePaneMode,
        start_mode: Mode,
        config: &BstlConfig,
        storage: Rc<Storage>,
        category_overrides: HashMap<String, String>,
    ) -> Self {
        let (categories, apps, mode, focus) = match start_mode {
            Mode::SinglePane => {
                let (cats, apps) = Self::load_for_mode(single_pane_mode, &storage, &category_overrides);
                (cats, apps, Mode::SinglePane, Focus::Apps)
            }
            Mode::DualPane => {
                let (cats, apps) = Self::load_desktop_apps(&storage, &category_overrides);
                (cats, apps, Mode::DualPane, Focus::Categories)
            }
        };

        let popularity = storage
            .popularity_map(config.history_window_days)
            .unwrap_or_default();
        let recent_apps = storage
            .recent_names(config.max_recent_apps.max(1))
            .unwrap_or_default();

        Self {
            mode,
            single_pane_mode,
            should_quit: false,
            input: Input::default(),
            cursor_visible: true,
            cursor_last_toggle: Instant::now(),
            categories,
            apps,
            recent_apps,
            selected_category: 0,
            selected_app: 0,
            focus,
            app_to_launch: None,
            config: config.clone(),
            popularity,
            storage,
            category_overrides,
            apps_list_state: ListState::default(),
            categories_list_state: ListState::default(),
            apps_rect: None,
            categories_rect: None,
            search_rect: None,
            fuzzy_matcher: SkimMatcherV2::default(),
        }
    }

    /// Helper to get the current search query
    pub fn query(&self) -> String {
        self.input.value().to_string()
    }

    /// Record a launch: persist to storage and update in-memory recency &
    /// popularity caches so the same instance reflects the change immediately.
    pub fn add_to_recent(&mut self, app_name: String) {
        let _ = self.storage.record_launch(&app_name);

        // MRU bookkeeping (used by the "Recent" category in DualPane mode).
        self.recent_apps.retain(|a| a != &app_name);
        self.recent_apps.insert(0, app_name.clone());
        let max_recent = self.config.max_recent_apps.max(1);
        if self.recent_apps.len() > max_recent {
            self.recent_apps.truncate(max_recent);
        }

        // Popularity bump.
        *self.popularity.entry(app_name).or_insert(0) += 1;
    }

    pub fn visible_apps(&self) -> Vec<&AppEntry> {
        let query_string = self.query();
        let query = &query_string;

        // Start with all apps
        let mut apps: Vec<&AppEntry> = if query.is_empty() {
            self.apps.iter().collect()
        } else {
            // Fuzzy match when searching
            let mut matched: Vec<(&AppEntry, i64)> = self.apps.iter()
                .filter_map(|a| self.matches_search(&a.name, query).map(|score| (a, score)))
                .collect();
            matched.sort_by(|a, b| b.1.cmp(&a.1));
            matched.into_iter().map(|(a, _)| a).collect()
        };

        // Empty-query default view: top N by popularity in the configured window,
        // followed by everything else in the underlying order.
        if query.is_empty() && self.config.recent_first && !self.popularity.is_empty() {
            let top_n = self.config.top_recent_count.max(1);

            // Rank apps that have any recorded launches by count desc, name asc.
            let mut ranked: Vec<&AppEntry> = apps
                .iter()
                .copied()
                .filter(|a| self.popularity.contains_key(&a.name))
                .collect();
            ranked.sort_by(|a, b| {
                let ca = self.popularity.get(&a.name).copied().unwrap_or(0);
                let cb = self.popularity.get(&b.name).copied().unwrap_or(0);
                cb.cmp(&ca)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            ranked.truncate(top_n);

            let top_names: HashSet<String> =
                ranked.iter().map(|a| a.name.clone()).collect();

            let mut out = ranked;
            for app in apps {
                if !top_names.contains(&app.name) {
                    out.push(app);
                }
            }
            apps = out;
        }

        apps
    }

    pub fn update_cursor_blink(&mut self) {
        use std::time::Duration;

        // Get the blink interval from config
        let blink_interval = self.config.colors.cursor_blink_interval;
        
        // If interval is 0, cursor should always be visible (no blinking)
        if blink_interval == 0 {
            self.cursor_visible = true;
            return;
        }

        // Check if it's time to toggle
        if self.cursor_last_toggle.elapsed() >= Duration::from_millis(blink_interval) {
            self.cursor_visible = !self.cursor_visible;
            self.cursor_last_toggle = std::time::Instant::now();
        }
    }

    pub fn reset_cursor_blink(&mut self) {
        self.cursor_visible = true;
        self.cursor_last_toggle = std::time::Instant::now();
    }

    /// Toggle between SinglePane and DualPane
    pub fn toggle_mode(&mut self) {
        match self.mode {
            Mode::SinglePane => {
                let (categories, apps) = Self::load_desktop_apps(&self.storage, &self.category_overrides);
                self.categories = categories;
                self.apps = apps;
                self.mode = Mode::DualPane;

                // Keep leftmost pane focused when switching to DualPane
                self.focus = Focus::Categories;
            }
            Mode::DualPane => {
                let (categories, apps) = Self::load_for_mode(
                    self.single_pane_mode,
                    &self.storage,
                    &self.category_overrides,
                );
                self.categories = categories;
                self.apps = apps;
                self.mode = Mode::SinglePane;

                // Leftmost pane in SinglePane is Apps
                self.focus = Focus::Apps;
            }
        }

        // Reset selection indexes
        self.selected_category = 0;
        self.selected_app = 0;
    }

    /// Toggle dmenu mode (PATH executables) vs Desktop Apps (SinglePane)
    pub fn toggle_dmenu_mode(&mut self) {
        self.single_pane_mode = match self.single_pane_mode {
            SinglePaneMode::Dmenu => SinglePaneMode::DesktopApps,
            SinglePaneMode::DesktopApps => SinglePaneMode::Dmenu,
        };

        // Always switch to SinglePane to show the new list
        self.mode = Mode::SinglePane;
        let (categories, apps) = Self::load_for_mode(
            self.single_pane_mode,
            &self.storage,
            &self.category_overrides,
        );
        self.categories = categories;
        self.apps = apps;
        self.selected_app = 0;
        self.selected_category = 0;
        self.focus = Focus::Apps;
    }

    /// Check if an app matches the search query using fuzzy matching
    /// (case-insensitive). Tiered: prefix matches always rank above fuzzy
    /// matches; within each tier, popularity in the configured window breaks
    /// ties so frequently-launched apps surface first.
    pub fn matches_search(&self, app_name: &str, query: &str) -> Option<i64> {
        if query.is_empty() {
            return Some(0); // Empty query matches everything
        }

        let app_name_lower = app_name.to_lowercase();
        let query_lower = query.to_lowercase();
        let bonus = self.popularity_bonus(app_name);

        // Prefix tier: large base score puts these above fuzzy matches.
        // Capped popularity bonus keeps a low-frequency new app discoverable.
        if app_name_lower.starts_with(&query_lower) {
            return Some(1_000_000 + bonus);
        }

        // Fuzzy tier: combine score with popularity.
        self.fuzzy_matcher
            .fuzzy_match(&app_name_lower, &query_lower)
            .map(|s| s + bonus)
    }

    fn popularity_bonus(&self, app_name: &str) -> i64 {
        let count = self.popularity.get(app_name).copied().unwrap_or(0) as i64;
        // Cap so a habit can't permanently shadow new apps.
        count.min(100) * self.config.popularity_weight
    }

    /// Load apps based on the single pane mode
    fn load_for_mode(
        mode: SinglePaneMode,
        storage: &Storage,
        overrides: &HashMap<String, String>,
    ) -> (Vec<String>, Vec<AppEntry>) {
        let (categories, mut apps) = match mode {
            SinglePaneMode::DesktopApps => Self::load_desktop_apps(storage, overrides),
            SinglePaneMode::Dmenu => Self::load_from_path("/usr/bin", storage, overrides),
        };

        // Sort apps alphabetically for single pane mode
        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        (categories, apps)
    }

    /// Load .desktop apps from the storage cache and derive the category list.
    /// Hardcoded buckets keep their fixed order at the top of the list (so
    /// stock apps land where users expect them); any extra categories
    /// introduced by XDG menu fragments are appended after, sorted.
    fn load_desktop_apps(
        storage: &Storage,
        overrides: &HashMap<String, String>,
    ) -> (Vec<String>, Vec<AppEntry>) {
        let apps = storage.load_apps(overrides).unwrap_or_default();

        let mut category_set: HashSet<String> = HashSet::new();
        for a in &apps {
            category_set.insert(a.category.clone());
        }

        let mut categories = vec!["Recent".to_string()];
        let category_order = [
            "Utilities", "Development", "Network", "Audio/Video", "Graphics",
            "System", "Office", "Games", "Education", "Settings",
        ];
        let known: HashSet<&str> = category_order.iter().copied().collect();
        categories.extend(
            category_order
                .iter()
                .filter(|c| category_set.contains(**c))
                .map(|s| s.to_string()),
        );

        let mut extras: Vec<String> = category_set
            .iter()
            .filter(|c| !known.contains(c.as_str()))
            .cloned()
            .collect();
        extras.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        categories.extend(extras);

        (categories, apps)
    }

    /// Load executables from a directory (dmenu style). Determines GUI status
    /// from the cached desktop apps so we don't re-scan .desktop files.
    fn load_from_path<P: AsRef<Path>>(
        path: P,
        storage: &Storage,
        overrides: &HashMap<String, String>,
    ) -> (Vec<String>, Vec<AppEntry>) {
        let mut apps = Vec::new();
        let gui_bins = Self::gui_binaries_from_cache(storage, overrides);

        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                        let is_gui = gui_bins.contains(name);
                        apps.push(AppEntry {
                            name: name.to_string(),
                            category: "CLI".to_string(),
                            exec: name.to_string(),
                            terminal: !is_gui,
                            comment: String::new(),
                        });
                    }
                }
            }
        }

        (vec!["CLI".to_string()], apps)
    }

    #[cfg(test)]
    fn for_test(config: BstlConfig, popularity: HashMap<String, u32>) -> Self {
        Self {
            mode: Mode::SinglePane,
            single_pane_mode: SinglePaneMode::DesktopApps,
            should_quit: false,
            input: Input::default(),
            cursor_visible: true,
            cursor_last_toggle: Instant::now(),
            categories: Vec::new(),
            apps: Vec::new(),
            recent_apps: Vec::new(),
            selected_category: 0,
            selected_app: 0,
            focus: Focus::Apps,
            app_to_launch: None,
            config,
            popularity,
            storage: Rc::new(Storage::in_memory()),
            category_overrides: HashMap::new(),
            apps_list_state: ListState::default(),
            categories_list_state: ListState::default(),
            apps_rect: None,
            categories_rect: None,
            search_rect: None,
            fuzzy_matcher: SkimMatcherV2::default(),
        }
    }

    /// Derive the set of known GUI binary names from the cached app list:
    /// any entry where Terminal=false contributes its first exec token.
    fn gui_binaries_from_cache(
        storage: &Storage,
        overrides: &HashMap<String, String>,
    ) -> HashSet<String> {
        let mut gui_bins: HashSet<String> = HashSet::new();
        let cached = storage.load_apps(overrides).unwrap_or_default();
        for app in cached {
            if app.terminal {
                continue;
            }
            if let Some(bin) = app.exec.split_whitespace().next() {
                if let Some(name) = Path::new(bin).file_name().and_then(|s| s.to_str()) {
                    gui_bins.insert(name.to_string());
                }
            }
        }
        gui_bins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CursorShape, BstlConfig, LauncherTheme, SearchPosition, StartMode};

    fn test_config() -> BstlConfig {
        BstlConfig {
            dmenu: false,
            search_position: SearchPosition::Top,
            start_mode: StartMode::Single,
            focus_search_on_switch: true,
            colors: LauncherTheme {
                border: String::new(),
                focus: String::new(),
                unfocused: String::new(),
                highlight: String::new(),
                border_style: String::new(),
                highlight_type: String::new(),
                cursor_color: String::new(),
                cursor_shape: CursorShape::Block,
                cursor_blink_interval: 0,
            },
            terminal: "foot".into(),
            timeout: 0,
            max_recent_apps: 5,
            recent_first: true,
            print_selection: false,
            sway: false,
            history_window_days: 90,
            top_recent_count: 5,
            popularity_weight: 10,
        }
    }

    #[test]
    fn prefix_match_with_higher_popularity_wins_tie() {
        // Both "Firefox" and "Final Fantasy" are prefix matches for "f", but
        // Firefox has been launched many times. It should rank above.
        let mut pop = HashMap::new();
        pop.insert("Firefox".to_string(), 50);
        pop.insert("Final Fantasy".to_string(), 0);
        let app = App::for_test(test_config(), pop);

        let firefox = app.matches_search("Firefox", "f").unwrap();
        let final_fantasy = app.matches_search("Final Fantasy", "f").unwrap();
        assert!(
            firefox > final_fantasy,
            "Firefox ({}) should outrank Final Fantasy ({})",
            firefox,
            final_fantasy
        );
    }

    #[test]
    fn prefix_match_always_beats_fuzzy_match_regardless_of_popularity() {
        // Even if a fuzzy-matched app has been launched a lot, a prefix match
        // for the query must rank higher.
        let mut pop = HashMap::new();
        pop.insert("Firefox".to_string(), 0);
        pop.insert("Inkscape".to_string(), 1000); // wildly popular but only fuzzy-matches "f"
        let app = App::for_test(test_config(), pop);

        let firefox = app.matches_search("Firefox", "f").unwrap();
        let inkscape = app.matches_search("Inkscape", "f").unwrap_or(0);
        assert!(
            firefox > inkscape,
            "Prefix match Firefox ({}) must beat fuzzy match Inkscape ({})",
            firefox,
            inkscape
        );
    }

    #[test]
    fn empty_query_default_view_puts_top_n_first() {
        let mut pop = HashMap::new();
        pop.insert("Firefox".to_string(), 50);
        pop.insert("Vim".to_string(), 30);
        let mut app = App::for_test(test_config(), pop);
        app.apps = vec![
            AppEntry { name: "Aaa".into(), category: "Util".into(), exec: "a".into(), terminal: false, comment: String::new() },
            AppEntry { name: "Vim".into(), category: "Util".into(), exec: "vim".into(), terminal: false, comment: String::new() },
            AppEntry { name: "Firefox".into(), category: "Net".into(), exec: "firefox".into(), terminal: false, comment: String::new() },
            AppEntry { name: "Zzz".into(), category: "Util".into(), exec: "z".into(), terminal: false, comment: String::new() },
        ];

        let visible = app.visible_apps();
        let names: Vec<&str> = visible.iter().map(|a| a.name.as_str()).collect();
        // Top 2 by popularity first, then the rest in original order.
        assert_eq!(names[0], "Firefox");
        assert_eq!(names[1], "Vim");
        assert!(names[2..].contains(&"Aaa"));
        assert!(names[2..].contains(&"Zzz"));
    }
}
