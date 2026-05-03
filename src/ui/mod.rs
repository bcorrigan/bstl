use crate::app::{App, Mode};
use crate::config::{BstlConfig, SearchPosition};
use ratatui::Frame;

mod layout;
mod dual_pane;
mod single_pane;

pub fn draw(f: &mut Frame, app: &mut App, search_position: SearchPosition, config: &BstlConfig) {
    match app.mode {
        Mode::SinglePane => {
            single_pane::draw(
                f,
                app,  // Pass app reference
                app.selected_app,
                app.focus,
                search_position,
                config,
            )
        }
        Mode::DualPane => dual_pane::draw(f, app, search_position, config),
    }
}
