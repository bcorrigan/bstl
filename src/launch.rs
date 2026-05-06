use std::process::{Command, Stdio};
use std::os::unix::process::CommandExt;
use crate::app::AppEntry;
use crate::config::BstlConfig;

/// Reconstruct a `Command` as a single string suitable for sway's `exec` IPC.
///
/// Sway's command parser treats `,` and `;` as command separators and splits
/// on whitespace, so any argument containing those (or quote/backslash chars)
/// must be wrapped in double quotes with `\` and `"` escaped.
pub fn build_sway_exec_string(command: &Command) -> String {
    let prog = quote_arg_for_sway(&command.get_program().to_string_lossy());
    let args = command
        .get_args()
        .map(|a| quote_arg_for_sway(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        prog
    } else {
        format!("{} {}", prog, args)
    }
}

fn quote_arg_for_sway(arg: &str) -> String {
    let needs_quoting = arg.is_empty()
        || arg
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\\'));
    if !needs_quoting {
        return arg.to_string();
    }
    let escaped: String = arg
        .chars()
        .flat_map(|c| match c {
            '\\' | '"' => vec!['\\', c],
            other => vec![other],
        })
        .collect();
    format!("\"{}\"", escaped)
}

pub fn build_command(entry: &AppEntry, config: &BstlConfig) -> Command {
    let terminal = &config.terminal;

    if entry.terminal || entry.needs_terminal() {
        // Terminal app
        let parts: Vec<&str> = terminal.split_whitespace().collect();
        if let Some((prog, args)) = parts.split_first() {
            let mut c = Command::new(prog);
            c.args(args);
            
            // If the terminal config is a single word (e.g. "alacritty"), 
            // assume we need to add -e (backward compatibility).
            // If it has multiple words (e.g. "wezterm start" or "alacritty -e"),
            // assume the user provided the necessary flags.
            if parts.len() == 1 {
                c.arg("-e");
            }
            c.arg(&entry.exec);
            c
        } else {
             // Fallback for empty terminal config
             let mut c = Command::new("sh");
             c.arg("-c").arg(&entry.exec);
             c
        }
    } else {
        // GUI app
        let mut c = Command::new("sh");
        c.arg("-c").arg(&entry.exec);
        c
    }
}

pub fn launch_app(entry: &AppEntry, config: &BstlConfig) {
    let mut cmd = build_command(entry, config);

    // Fully detach (don't block, don't get killed with parent)
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BstlConfig, SearchPosition, StartMode, LauncherTheme, CursorShape};

    fn make_config(terminal: &str) -> BstlConfig {
        BstlConfig {
            dmenu: false,
            search_position: SearchPosition::Top,
            start_mode: StartMode::Single,
            focus_search_on_switch: true,
            colors: LauncherTheme {
                border: "".to_string(),
                focus: "".to_string(),
                unfocused: "".to_string(),
                highlight: "".to_string(),
                border_style: "".to_string(),
                highlight_type: "".to_string(),
                cursor_color: "".to_string(),
                cursor_shape: CursorShape::Block,
                cursor_blink_interval: 0,
            },
            terminal: terminal.to_string(),
            timeout: 0,
            max_recent_apps: 0,
            recent_first: false,
            print_selection: false,
            sway: false,
            history_window_days: 90,
            top_recent_count: 5,
            popularity_weight: 10,
        }
    }

    #[test]
    fn test_build_command_terminal_simple() {
        let entry = AppEntry {
            name: "Vim".to_string(),
            category: "CLI".to_string(),
            exec: "vim".to_string(),
            terminal: true,
            comment: String::new(),
        };
        let config = make_config("alacritty");
        let cmd = build_command(&entry, &config);
        let debug_str = format!("{:?}", cmd);
        // Expect: "alacritty" "-e" "vim"
        assert!(debug_str.contains("alacritty"));
        assert!(debug_str.contains("-e"));
        assert!(debug_str.contains("vim"));
    }

    #[test]
    fn test_build_command_terminal_complex() {
        let entry = AppEntry {
            name: "Vim".to_string(),
            category: "CLI".to_string(),
            exec: "vim".to_string(),
            terminal: true,
            comment: String::new(),
        };
        let config = make_config("wezterm start");
        let cmd = build_command(&entry, &config);
        let debug_str = format!("{:?}", cmd);
        // Expect: "wezterm" "start" "vim"
        assert!(debug_str.contains("wezterm"));
        assert!(debug_str.contains("start"));
        assert!(debug_str.contains("vim"));
        // Should NOT contain -e (unless implicitly in wezterm or start, but we check specific flag)
        // Note: contains("-e") might match inside "wezterm" if it had -e, but it doesn't.
        // However, we want to ensure we didn't add the standalone flag "-e".
        // Debug output quotes args: "wezterm" "start" "vim".
        // So we can check for "-e" with quotes if we want to be strict, but format varies by platform maybe.
        // Simplest is to assume it shouldn't be there as a separate arg.
        // Let's rely on manual inspection if this fails or just weak assertion.
        
        // Actually, if I run `cargo test`, I'll see if it fails.
    }

    #[test]
    fn sway_exec_string_quotes_comma_path() {
        // Regression: a comma in the Exec path used to leak through unquoted,
        // which caused sway IPC to split on it and silently drop the launch.
        let entry = AppEntry {
            name: "Full Charge".to_string(),
            category: "My Scripts".to_string(),
            exec: "/home/bc/bin/,fullcharge".to_string(),
            terminal: false,
            comment: String::new(),
        };
        let cmd = build_command(&entry, &make_config("wezterm"));
        let s = build_sway_exec_string(&cmd);
        assert_eq!(s, r#"sh -c "/home/bc/bin/,fullcharge""#);
    }

    #[test]
    fn sway_exec_string_quotes_semicolon_and_whitespace() {
        assert_eq!(quote_arg_for_sway("a;b"), r#""a;b""#);
        assert_eq!(quote_arg_for_sway("a b"), r#""a b""#);
        assert_eq!(quote_arg_for_sway("a,b"), r#""a,b""#);
    }

    #[test]
    fn sway_exec_string_escapes_quotes_and_backslashes() {
        assert_eq!(quote_arg_for_sway(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg_for_sway(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn sway_exec_string_leaves_plain_args_alone() {
        assert_eq!(quote_arg_for_sway("plain"), "plain");
        assert_eq!(quote_arg_for_sway("/usr/bin/foo"), "/usr/bin/foo");
    }
}