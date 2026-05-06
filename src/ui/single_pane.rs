use crate::ui::layout;
use crate::app::{App, Focus};
use crate::config::{BstlConfig, SearchPosition};
use ratatui::Frame;

pub fn draw(
    f: &mut Frame,
    app: &mut App,
    selected: usize,
    focus: Focus,
    search_position: SearchPosition,
    config: &BstlConfig,
) {
    let chunks = layout::vertical_split(f, 3, search_position);

    let visible = app.visible_apps();
    let filtered_apps: Vec<String> = visible.iter().map(|a| a.name.clone()).collect();
    let description = visible
        .get(selected)
        .map(|a| a.comment.clone())
        .unwrap_or_default();

    let (apps_area, desc_area) = layout::apps_with_description_split(chunks.1);

    layout::render_list(
        f,
        apps_area,
        " Apps ",
        &filtered_apps,
        selected,
        focus == Focus::Apps,
        config,
        &mut app.apps_list_state,
    );
    app.apps_rect = Some(apps_area);
    app.categories_rect = None;

    layout::render_description(f, desc_area, &description, config);

    // Pass input to render_search_bar
    layout::render_search_bar(
        f,
        chunks.0,
        &app.input,
        focus,
        config,
    );
    app.search_rect = Some(chunks.0);
}
