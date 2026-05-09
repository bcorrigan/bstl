mod app;
mod config;
mod events;
mod icons;
mod launch;
mod menu;
mod storage;
mod sway;
mod ui;

use crossterm::{
    ExecutableCommand,
    cursor::SetCursorStyle,
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eyre::Result;
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    rc::Rc,
    sync::mpsc::{channel, Receiver},
    thread,
    time::{Duration, Instant},
};

use app::{App, Focus, Mode, SinglePaneMode};
use config::{CursorShape, load_launcher_config};
use storage::Storage;

fn main() -> Result<()> {
    color_eyre::install()?;

    let socket_path = "/tmp/bstl.sock";

    // Try to connect to existing instance
    match UnixStream::connect(socket_path) {
        Ok(mut stream) => {
            stream.write_all(b"quit")?;
            return Ok(());
        }
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            // Socket exists but no listener - stale socket
            let _ = fs::remove_file(socket_path);
        }
        Err(_) => {
            // Assume doesn't exist or other error we can try to recover from by binding
        }
    }

    // Setup listener
    let (tx, rx) = channel();
    let listener = UnixListener::bind(socket_path)?;
    
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let mut buf = String::new();
                    if let Ok(_) = stream.read_to_string(&mut buf) {
                        if buf == "quit" {
                            let _ = tx.send(());
                            break;
                        }
                    }
                }
                Err(_) => continue,
            }
        }
    });

    let cfg = load_launcher_config();

    // Open storage and refresh the .desktop cache. mtime checks make this
    // close to free when nothing has been installed since last launch.
    let mut storage = Storage::open()?;
    let dirs = storage::xdg_application_dirs();
    let desktops = storage::current_desktops();
    storage.refresh_app_cache(&dirs, &desktops)?;
    let storage = Rc::new(storage);

    // User-defined XDG menu fragments override the hardcoded category
    // buckets, so a `Categories=X-MyScripts;` entry can be routed under a
    // "My Scripts" menu instead of falling through to "Utilities".
    let category_overrides = menu::load_category_overrides();

    let single_pane_mode = if cfg.dmenu {
        SinglePaneMode::Dmenu
    } else {
        SinglePaneMode::DesktopApps
    };

    let start_mode = match cfg.start_mode {
        config::StartMode::Dual => Mode::DualPane,
        config::StartMode::Single => Mode::SinglePane,
    };

    let mut app = App::new(
        single_pane_mode,
        start_mode,
        &cfg,
        Rc::clone(&storage),
        category_overrides,
    );

    let print_only = cfg.print_selection || std::env::args().any(|arg| arg == "--print-selection");
    let sway_mode = cfg.sway || std::env::args().any(|arg| arg == "--sway");
    let sway_app_id = arg_value("--app-id");
    let sway_size = arg_value("--size");

    let mut sway_client = if sway_mode {
        sway::Client::connect().ok()
    } else {
        None
    };

    let mut fullscreen_window_id = None;
    if let Some(client) = &mut sway_client {
        if let Ok(Some(id)) = client.get_focused_fullscreen_node_id() {
            fullscreen_window_id = Some(id);
            let _ = client.set_fullscreen(false, Some(id));
            // The for_window rule sized us against a workspace that still had the
            // fullscreen container; re-apply now that bars/usable area are restored.
            // Caller passes --app-id / --size so we don't bake the sway config in.
            if let (Some(app_id), Some(size)) = (&sway_app_id, &sway_size) {
                let _ = client.run_command(&format!(
                    r#"[app_id="{}"] resize set {} ppt {} ppt, move position center, focus"#,
                    app_id, size, size,
                ));
            }
        }
    }

    enable_raw_mode()?;

    let res = if print_only {
        run_with_writer(io::stderr(), &mut app, &cfg, &rx)
    } else {
        run_with_writer(io::stdout(), &mut app, &cfg, &rx)
    };

    disable_raw_mode()?;

    // Cleanup socket
    let _ = fs::remove_file(socket_path);

    if let Err(err) = res {
        eprintln!("Error: {err:?}");
    }

    if let Some(ref cmd) = app.app_to_launch {
        if print_only {
            // Just print the command to stdout - useful for those who wish to pipe to swayexec or similar
            // Check if app needs terminal
            if let Some(entry) = app.apps.iter().find(|a| &a.exec == cmd) {
                if entry.terminal || entry.needs_terminal() {
                    println!("{} {}", app.config.terminal, cmd);
                } else {
                    println!("{}", cmd);
                }
            } else {
                println!("{}", cmd);
            }
        } else {
            // directly launch
            if sway_mode {
                let full_cmd = if let Some(entry) = app.apps.iter().find(|a| &a.exec == cmd).cloned() {
                    app.add_to_recent(entry.name.clone());
                    let command = crate::launch::build_command(&entry, &app.config);
                    crate::launch::build_sway_exec_string(&command)
                } else {
                    cmd.clone()
                };

                if let Some(client) = &mut sway_client {
                    let _ = client.exec(&full_cmd);
                }
            } else {
                if let Some(entry) = app.apps.iter().find(|a| &a.exec == cmd).cloned() {
                    app.add_to_recent(entry.name.clone());
                    crate::launch::launch_app(&entry, &app.config);
                } else {
                    let _ = std::process::Command::new("sh").arg("-c").arg(cmd).spawn();
                }
            }
        }
    } else {
        // User cancelled
        if let Some(id) = fullscreen_window_id {
            if let Some(client) = &mut sway_client {
                let _ = client.set_fullscreen(true, Some(id));
            }
        }
    }

    Ok(())
}

fn run_with_writer<W: Write + ExecutableCommand>(
    mut writer: W,
    app: &mut App,
    cfg: &config::BstlConfig,
    rx: &Receiver<()>,
) -> Result<()> {
    set_cursor_color(&mut writer, &cfg.colors.cursor_color)?;

    execute!(writer, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(writer);
    let mut terminal = Terminal::new(backend)?;

    warmup_icons(&mut terminal, app, cfg)?;

    if app.mode == Mode::DualPane && !app.categories.is_empty() {
        let old_focus = app.focus;
        app.focus = Focus::Categories;
        terminal.draw(|f| ui::draw(f, app, cfg.search_position.clone(), cfg))?;
        app.focus = old_focus;
    }

    let res = run_app(&mut terminal, app, cfg, rx);

    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Reset cursor color to default
    reset_cursor_color(terminal.backend_mut())?;

    res
}

/// Set the cursor color using ANSI escape codes
fn set_cursor_color<W: Write>(writer: &mut W, color_hex: &str) -> Result<()> {
    if let Some((r, g, b)) = parse_hex_color(color_hex) {
        // OSC 12 ; color ST - Set cursor color
        write!(writer, "\x1b]12;rgb:{:02x}/{:02x}/{:02x}\x07", r, g, b)?;
        writer.flush()?;
    }
    Ok(())
}

/// Reset cursor color to terminal default
fn reset_cursor_color<W: Write>(writer: &mut W) -> Result<()> {
    // OSC 112 ST - Reset cursor color
    write!(writer, "\x1b]112\x07")?;
    writer.flush()?;
    Ok(())
}

/// Parse hex color string to RGB values
fn parse_hex_color(color: &str) -> Option<(u8, u8, u8)> {
    let color = color.trim();

    if !color.starts_with('#') {
        return None;
    }

    let hex = &color[1..];

    match hex.len() {
        // #RGB format
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            Some((r * 17, g * 17, b * 17))
        }
        // #RRGGBB format
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        // #RRGGBBAA format (ignore alpha)
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

fn run_app<B: Backend + ExecutableCommand>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    cfg: &config::BstlConfig,
    rx: &Receiver<()>,
) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    let mut last_input = Instant::now();

    loop {
        // Check for quit signal from socket
        if rx.try_recv().is_ok() {
            return Ok(());
        }

        app.update_cursor_blink();

        terminal.draw(|f| ui::draw(f, app, cfg.search_position.clone(), cfg))?;

        // Always show cursor (input always active)
        // Set shape based on blink interval
        let style = if cfg.colors.cursor_blink_interval > 0 {
            // Use steady cursor - we'll handle blinking manually
            match cfg.colors.cursor_shape {
                CursorShape::Block => SetCursorStyle::SteadyBlock,
                CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
                CursorShape::Pipe => SetCursorStyle::SteadyBar,
            }
        } else {
            // Use terminal's built-in blinking
            match cfg.colors.cursor_shape {
                CursorShape::Block => SetCursorStyle::BlinkingBlock,
                CursorShape::Underline => SetCursorStyle::BlinkingUnderScore,
                CursorShape::Pipe => SetCursorStyle::BlinkingBar,
            }
        };
        
        terminal.backend_mut().execute(style)?;

        // Handle manual cursor blinking if interval is set
        if cfg.colors.cursor_blink_interval > 0 {
            if app.cursor_visible {
                terminal.show_cursor()?;
            } else {
                terminal.hide_cursor()?;
            }
        } else {
            terminal.show_cursor()?;
        }

        let tick = Duration::from_millis(50);

        if cfg.timeout > 0 && last_input.elapsed().as_secs() >= cfg.timeout {
            break;
        }

        if event::poll(tick)? {
            match event::read()? {
                Event::Key(key) => {
                    last_input = Instant::now();
                    if events::handle_key(app, key)? {
                        break;
                    }
                }
                Event::Mouse(mouse) => {
                    last_input = Instant::now();
                    if events::handle_mouse(app, mouse)? {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Read `--name value` or `--name=value` from CLI args.
fn arg_value(name: &str) -> Option<String> {
    let prefix = format!("{}=", name);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

fn warmup_icons<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &App,
    cfg: &config::BstlConfig,
) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    if app.categories.is_empty() {
        return Ok(());
    }

    let mut tmp = app.clone();
    tmp.focus = Focus::Apps;
    terminal.draw(|f| ui::draw(f, &mut tmp, cfg.search_position.clone(), cfg))?;

    if app.mode == Mode::DualPane {
        tmp.focus = Focus::Categories;
        terminal.draw(|f| ui::draw(f, &mut tmp, cfg.search_position.clone(), cfg))?;
    }

    Ok(())
}
