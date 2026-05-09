use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use crate::app::{App, Focus, Mode};
use eyre::Result;
use tui_input::backend::crossterm::EventHandler;
use tui_input::InputRequest;

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    // 1. Global / Exit keys
    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => return Ok(true),
        KeyCode::Char('g') if key.modifiers == KeyModifiers::CONTROL => return Ok(true),
        
        // Toggle mode (Global)
        KeyCode::Char('m') if key.modifiers == KeyModifiers::CONTROL => {
            app.toggle_mode();
            return Ok(false);
        }
        KeyCode::Char('t') if key.modifiers == KeyModifiers::CONTROL => {
            app.toggle_mode();
            return Ok(false);
        }
        KeyCode::Char('x') if key.modifiers == KeyModifiers::CONTROL => {
            app.toggle_dmenu_mode();
            return Ok(false);
        }
        KeyCode::Tab => {
            app.toggle_mode();
            return Ok(false);
        }
        
        // Launch
        KeyCode::Enter => {
            if let Some(app_entry) = get_selected_app(app) {
                app.app_to_launch = Some(app_entry.exec.clone());
                app.should_quit = true;
                return Ok(true);
            }
        }
        _ => {}
    }

    // 2. Navigation (Arrows) - Independent of input focus
    match key.code {
        KeyCode::Up => {
            navigate_up(app);
            return Ok(false);
        }
        KeyCode::Down => {
            navigate_down(app);
            return Ok(false);
        }
        KeyCode::Left => {
            navigate_left(app);
            return Ok(false);
        }
        KeyCode::Right => {
            navigate_right(app);
            return Ok(false);
        }
        _ => {}
    }

    // 3. Input Handling - Everything else goes to search
    // Helper for Emacs bindings
    let req = match key {
        KeyEvent { code: KeyCode::Char('a'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::GoToStart),
        KeyEvent { code: KeyCode::Char('e'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::GoToEnd),
        KeyEvent { code: KeyCode::Char('b'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::GoToPrevChar),
        KeyEvent { code: KeyCode::Char('f'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::GoToNextChar),
        KeyEvent { code: KeyCode::Char('w'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::DeletePrevWord),
        // Ctrl+D -> DeleteNextChar (Delete)
        KeyEvent { code: KeyCode::Char('d'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::DeleteNextChar),
        // Ctrl+H -> DeletePrevChar (Backspace)
        KeyEvent { code: KeyCode::Char('h'), modifiers: KeyModifiers::CONTROL, .. } => Some(InputRequest::DeletePrevChar),
        _ => None,
    };

    // Manual handling for missing InputRequest variants (Ctrl+U, Ctrl+K)
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
                KeyCode::Char('u') => {
                    let cursor = app.input.cursor();
                    let val = app.input.value();
                    if cursor > 0 && cursor <= val.len() {
                        let suffix = &val[cursor..];
                        let mut new_input = tui_input::Input::new(suffix.to_string());
                        new_input.handle(InputRequest::GoToStart);
                        app.input = new_input;
                        update_selection_after_search(app);
                    }
                    return Ok(false);
                }
                KeyCode::Char('k') => {
                    let cursor = app.input.cursor();
                    let val = app.input.value();
                    if cursor < val.len() {
                        let prefix = &val[..cursor];
                        app.input = tui_input::Input::new(prefix.to_string());
                        update_selection_after_search(app);
                    }
                    return Ok(false);
                }
                _ => {}
        }
    }

    if let Some(req) = req {
        app.input.handle(req);
        app.reset_cursor_blink();
        update_selection_after_search(app);
    } else {
        // Only pass to input if it's not a reserved key we missed or modifier
        // tui_input handles most things well.
        app.input.handle_event(&Event::Key(key));
        app.reset_cursor_blink();
        update_selection_after_search(app);
    }

    Ok(false)
}

fn navigate_up(app: &mut App) {
    match app.mode {
        Mode::SinglePane => {
            if app.selected_app > 0 {
                app.selected_app -= 1;
            }
        }
        Mode::DualPane => {
            match app.focus {
                Focus::Categories => {
                    let matching_categories = get_matching_category_indices(app);
                    if let Some(current_pos) = matching_categories.iter().position(|&idx| idx == app.selected_category) {
                        let new_pos = if current_pos > 0 {
                            current_pos - 1
                        } else {
                            matching_categories.len() - 1
                        };
                        app.selected_category = matching_categories[new_pos];
                        app.selected_app = 0;
                    }
                }
                _ => { // Focus::Apps or Search (effectively Apps)
                    if app.selected_app > 0 {
                        app.selected_app -= 1;
                    }
                }
            }
        }
    }
}

fn navigate_down(app: &mut App) {
    match app.mode {
        Mode::SinglePane => {
            let count = count_filtered_apps_in_current_category(app);
            if count > 0 && app.selected_app + 1 < count {
                app.selected_app += 1;
            }
        }
        Mode::DualPane => {
            match app.focus {
                Focus::Categories => {
                    let matching_categories = get_matching_category_indices(app);
                    if let Some(current_pos) = matching_categories.iter().position(|&idx| idx == app.selected_category) {
                        let new_pos = if current_pos + 1 < matching_categories.len() {
                            current_pos + 1
                        } else {
                            0
                        };
                        app.selected_category = matching_categories[new_pos];
                        app.selected_app = 0;
                    }
                }
                _ => { // Focus::Apps
                    let count = count_filtered_apps_in_current_category(app);
                    if count > 0 && app.selected_app + 1 < count {
                        app.selected_app += 1;
                    }
                }
            }
        }
    }
}

fn navigate_left(app: &mut App) {
    if app.mode == Mode::DualPane {
        // If focusing apps, go to categories
        if app.focus == Focus::Apps {
             app.focus = Focus::Categories;
        }
    }
}

fn navigate_right(app: &mut App) {
    if app.mode == Mode::DualPane {
        // If focusing categories, go to apps
        if app.focus == Focus::Categories {
             app.focus = Focus::Apps;
        }
    }
}

fn get_matching_category_indices(app: &App) -> Vec<usize> {
    let query = app.query();
    if query.is_empty() {
        (0..app.categories.len()).collect()
    } else {
        let query_lower = query.to_lowercase();
        app.categories
            .iter()
            .enumerate()
            .filter(|(_, cat_name)| {
                if *cat_name == "Recent" {
                    app.recent_apps.iter().any(|recent_name| {
                        app.apps.iter()
                            .find(|a| &a.name == recent_name)
                            .and_then(|a| app.matches_search(a, &query_lower))
                            .is_some()
                    })
                } else {
                    app.apps.iter().any(|a| {
                        &a.category == *cat_name && app.matches_search(a, &query_lower).is_some()
                    })
                }
            })
            .map(|(idx, _)| idx)
            .collect()
    }
}

fn update_selection_after_search(app: &mut App) {
    if app.query().is_empty() {
        app.selected_category = 0;
        app.selected_app = 0;
        return;
    }

    match app.mode {
        Mode::DualPane => {
            let matching_indices = get_matching_category_indices(app);
            if let Some(&first_match) = matching_indices.first() {
                app.selected_category = first_match;
                app.selected_app = 0;
            }
        }
        Mode::SinglePane => { app.selected_app = 0; }
    }
}

fn get_selected_app(app: &App) -> Option<&crate::app::AppEntry> {
    match app.mode {
        Mode::SinglePane => {
            app.visible_apps().get(app.selected_app).map(|v| &**v)
        }
        Mode::DualPane => {
            let cat_name = app.categories.get(app.selected_category)?;
            let query = app.query();
            
            if cat_name == "Recent" {
                let apps_in_order: Vec<&crate::app::AppEntry> = app.recent_apps.iter()
                    .filter_map(|recent_name| {
                        app.apps.iter().find(|a| &a.name == recent_name)
                    })
                    .collect();
                
                if !query.is_empty() {
                    let mut apps_with_scores: Vec<(&crate::app::AppEntry, i64)> = apps_in_order
                        .into_iter()
                        .filter_map(|a| app.matches_search(a, &query).map(|score| (a, score)))
                        .collect();
                    apps_with_scores.sort_by(|a, b| b.1.cmp(&a.1));
                    return apps_with_scores.get(app.selected_app).map(|(entry, _)| *entry);
                }
                
                apps_in_order.get(app.selected_app).copied()
            } else {
                let mut apps_with_scores: Vec<(&crate::app::AppEntry, i64)> = app.apps.iter()
                    .filter(|a| &a.category == cat_name)
                    .filter_map(|a| app.matches_search(a, &query).map(|score| (a, score)))
                    .collect();

                if !query.is_empty() {
                    apps_with_scores.sort_by(|a, b| b.1.cmp(&a.1));
                }

                apps_with_scores.get(app.selected_app).map(|(entry, _)| *entry)
            }
        }
    }
}

fn count_filtered_apps_in_current_category(app: &App) -> usize {
    match app.mode {
        Mode::SinglePane => {
            app.visible_apps().len()
        }
        Mode::DualPane => {
            let cat_name = match app.categories.get(app.selected_category) {
                Some(c) => c,
                None => return 0,
            };
            let query = app.query();

            if cat_name == "Recent" {
                app.recent_apps.iter()
                    .filter_map(|recent_name| {
                        app.apps.iter().find(|a| &a.name == recent_name)
                    })
                    .filter(|a| app.matches_search(a, &query).is_some())
                    .count()
            } else {
                app.apps.iter()
                    .filter(|a| &a.category == cat_name)
                    .filter(|a| app.matches_search(a, &query).is_some())
                    .count()
            }
        }
    }
}

/// Map a screen click coordinate inside `rect` to a list-content row index
/// (0-based, excluding the top/bottom borders). Returns None if the click
/// landed on the border or outside the rect.
fn click_row_in_list(rect: Rect, col: u16, row: u16) -> Option<usize> {
    if col < rect.x || col >= rect.x + rect.width {
        return None;
    }
    if row <= rect.y || row + 1 >= rect.y + rect.height {
        return None;
    }
    Some((row - rect.y - 1) as usize)
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// Translate a click on the apps list into selected_app + focus. Launches if
/// the user clicked on the already-selected row. Returns Ok(true) on launch.
fn click_apps_list(app: &mut App, rect: Rect, col: u16, row: u16) -> Result<bool> {
    let Some(visible_row) = click_row_in_list(rect, col, row) else {
        return Ok(false);
    };
    let count = count_filtered_apps_in_current_category(app);
    if count == 0 {
        return Ok(false);
    }
    let offset = app.apps_list_state.offset();
    let target = offset + visible_row;
    if target >= count {
        return Ok(false);
    }
    let already_selected = app.focus == Focus::Apps && app.selected_app == target;
    app.focus = Focus::Apps;
    app.selected_app = target;

    if already_selected {
        if let Some(entry) = get_selected_app(app) {
            app.app_to_launch = Some(entry.exec.clone());
            app.should_quit = true;
            return Ok(true);
        }
    }
    Ok(false)
}

fn click_categories_list(app: &mut App, rect: Rect, col: u16, row: u16) {
    let Some(visible_row) = click_row_in_list(rect, col, row) else {
        return;
    };
    let matching = get_matching_category_indices(app);
    if matching.is_empty() {
        return;
    }
    let offset = app.categories_list_state.offset();
    let display_idx = offset + visible_row;
    if display_idx >= matching.len() {
        return;
    }
    let new_category = matching[display_idx];
    app.focus = Focus::Categories;
    if app.selected_category != new_category {
        app.selected_category = new_category;
        app.selected_app = 0;
    }
}

fn scroll_in_apps(app: &mut App, delta: i32) {
    app.focus = Focus::Apps;
    if delta < 0 {
        for _ in 0..(-delta) {
            navigate_up_apps(app);
        }
    } else {
        for _ in 0..delta {
            navigate_down_apps(app);
        }
    }
}

fn scroll_in_categories(app: &mut App, delta: i32) {
    if app.mode != Mode::DualPane {
        return;
    }
    app.focus = Focus::Categories;
    if delta < 0 {
        for _ in 0..(-delta) {
            navigate_up_categories(app);
        }
    } else {
        for _ in 0..delta {
            navigate_down_categories(app);
        }
    }
}

fn navigate_up_apps(app: &mut App) {
    if app.selected_app > 0 {
        app.selected_app -= 1;
    }
}

fn navigate_down_apps(app: &mut App) {
    let count = count_filtered_apps_in_current_category(app);
    if count > 0 && app.selected_app + 1 < count {
        app.selected_app += 1;
    }
}

fn navigate_up_categories(app: &mut App) {
    let matching = get_matching_category_indices(app);
    if let Some(pos) = matching.iter().position(|&i| i == app.selected_category) {
        if pos > 0 {
            app.selected_category = matching[pos - 1];
            app.selected_app = 0;
        }
    }
}

fn navigate_down_categories(app: &mut App) {
    let matching = get_matching_category_indices(app);
    if let Some(pos) = matching.iter().position(|&i| i == app.selected_category) {
        if pos + 1 < matching.len() {
            app.selected_category = matching[pos + 1];
            app.selected_app = 0;
        }
    }
}

pub fn handle_mouse(app: &mut App, ev: MouseEvent) -> Result<bool> {
    let (col, row) = (ev.column, ev.row);

    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(rect) = app.apps_rect {
                if rect_contains(rect, col, row) {
                    return click_apps_list(app, rect, col, row);
                }
            }
            if let Some(rect) = app.categories_rect {
                if rect_contains(rect, col, row) {
                    click_categories_list(app, rect, col, row);
                    return Ok(false);
                }
            }
            if let Some(rect) = app.search_rect {
                if rect_contains(rect, col, row) {
                    app.focus = Focus::Search;
                    return Ok(false);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(rect) = app.categories_rect {
                if rect_contains(rect, col, row) {
                    scroll_in_categories(app, -3);
                    return Ok(false);
                }
            }
            if let Some(rect) = app.apps_rect {
                if rect_contains(rect, col, row) {
                    scroll_in_apps(app, -3);
                    return Ok(false);
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(rect) = app.categories_rect {
                if rect_contains(rect, col, row) {
                    scroll_in_categories(app, 3);
                    return Ok(false);
                }
            }
            if let Some(rect) = app.apps_rect {
                if rect_contains(rect, col, row) {
                    scroll_in_apps(app, 3);
                    return Ok(false);
                }
            }
        }
        _ => {}
    }
    Ok(false)
}
