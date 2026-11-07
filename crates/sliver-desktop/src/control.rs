//! Telling the compositor what to do: switch workspace, focus or close a window. This is the
//! write side of the lists in [`crate::DesktopState`] (workspaces bar, taskbar), used only on an
//! explicit user action.
//!
//! Hyprland and sway take commands over their IPC sockets; niri takes JSON actions.

use crate::{Backend, WorkspaceInfo, hyprland, niri, sway};
use serde_json::json;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    FocusWorkspace(WorkspaceInfo),
    FocusWindow {
        address: String,
    },
    CloseWindow {
        address: String,
    },
    /// Cycle the keyboard layout (the `language` module's tap).
    NextLayout,
    /// Switch to the keyboard layout at this position in `DesktopState::keyboard_layouts`.
    SetLayout(usize),
}

/// A Lua string literal (Hyprland's Lua dispatchers take their arguments as Lua).
fn lua_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' | '"' => out.extend(['\\', c]),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Hyprland `socket1` commands for `a`: the Lua dispatcher (since 0.55, the only form a Lua
/// config accepts: `dispatch X` runs `hl.dispatch(X)`), then the classic one for hyprlang
/// configs. Switching the keyboard layout is a plain command, the same in both.
pub fn hyprland_commands(a: &Action) -> [String; 2] {
    let lua =
        |d: &str, k: &str, v: &str| format!("dispatch hl.dsp.{d}({{ {k} = {} }})", lua_str(v));
    match a {
        Action::FocusWorkspace(w) if w.name == w.id.to_string() => [
            lua("focus", "workspace", &w.id.to_string()),
            format!("dispatch workspace {}", w.id),
        ],
        Action::FocusWorkspace(w) => [
            lua("focus", "workspace", &format!("name:{}", w.name)),
            format!("dispatch workspace name:{}", w.name),
        ],
        Action::FocusWindow { address } => [
            lua("focus", "window", &format!("address:0x{address}")),
            format!("dispatch focuswindow address:0x{address}"),
        ],
        Action::CloseWindow { address } => [
            lua("window.close", "window", &format!("address:0x{address}")),
            format!("dispatch closewindow address:0x{address}"),
        ],
        Action::NextLayout => [
            "switchxkblayout all next".into(),
            "switchxkblayout all next".into(),
        ],
        Action::SetLayout(i) => [
            format!("switchxkblayout all {i}"),
            format!("switchxkblayout all {i}"),
        ],
    }
}

/// sway/i3 `RUN_COMMAND` text. Names are quoted; quotes and backslashes in them are escaped.
pub fn sway_command(a: &Action) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    match a {
        Action::FocusWorkspace(w) if w.name == w.id.to_string() => {
            format!("workspace number {}", w.id)
        }
        Action::FocusWorkspace(w) => format!("workspace {}", q(&w.name)),
        Action::FocusWindow { address } => format!("[con_id={address}] focus"),
        Action::CloseWindow { address } => format!("[con_id={address}] kill"),
        Action::NextLayout => "input type:keyboard xkb_switch_layout next".into(),
        Action::SetLayout(i) => format!("input type:keyboard xkb_switch_layout {i}"),
    }
}

/// niri IPC request (one JSON line). Unverified against a real niri: from the `niri-ipc` docs.
pub fn niri_request(a: &Action) -> Option<serde_json::Value> {
    Some(match a {
        Action::FocusWorkspace(w) => {
            json!({"Action": {"FocusWorkspace": {"reference": {"Id": w.id}}}})
        }
        Action::FocusWindow { address } => {
            json!({"Action": {"FocusWindow": {"id": address.parse::<u64>().ok()?}}})
        }
        Action::CloseWindow { address } => {
            json!({"Action": {"CloseWindow": {"id": address.parse::<u64>().ok()?}}})
        }
        Action::NextLayout => json!({"Action": {"SwitchLayout": {"layout": "Next"}}}),
        Action::SetLayout(i) => json!({"Action": {"SwitchLayout": {"layout": {"Index": i}}}}),
    })
}

/// Do `a` on the compositor of this session (`get` reads the environment).
pub fn run(get: impl Fn(&str) -> Option<String>, a: &Action) -> io::Result<()> {
    match Backend::detect(&get) {
        Backend::Hyprland => {
            let paths = hyprland::Paths::from_env(&get).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HYPRLAND_INSTANCE_SIGNATURE is not set",
                )
            })?;
            // The config's language decides which form Hyprland accepts, and it can change on a
            // reload, so the other is tried when the first is refused.
            let [lua, classic] = hyprland_commands(a);
            let first = hyprland::request(&paths.command, &lua)?;
            if first.trim() == "ok" {
                return Ok(());
            }
            if classic == lua {
                return Err(io::Error::other(format!("hyprland: {}", first.trim())));
            }
            let second = hyprland::request(&paths.command, &classic)?;
            if second.trim() == "ok" {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "hyprland: {} | {}",
                    first.trim(),
                    second.trim()
                )))
            }
        }
        Backend::Sway => {
            let sock = sway::socket_from_env(&get)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SWAYSOCK is not set"))?;
            sway_run(&sock, &sway_command(a))
        }
        Backend::Niri => {
            let sock = niri::socket_from_env(&get)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "NIRI_SOCKET is not set"))?;
            let req = niri_request(a).ok_or_else(|| io::Error::other("bad niri window id"))?;
            niri_run(&sock, &req)
        }
        Backend::Wlroots => crate::wlr::send(a),
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("no desktop control for {other:?}"),
        )),
    }
}

fn sway_run(sock: &Path, command: &str) -> io::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(&sway::encode(0, command.as_bytes()))?;
    let (_, payload) = sway::read_frame(&mut s)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no reply"))?;
    let v: serde_json::Value = serde_json::from_slice(&payload).map_err(io::Error::other)?;
    let failed = v.as_array().and_then(|a| {
        a.iter()
            .find(|r| r.get("success").and_then(|s| s.as_bool()) == Some(false))
    });
    match failed {
        None => Ok(()),
        Some(r) => Err(io::Error::other(
            r.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("sway refused")
                .to_string(),
        )),
    }
}

fn niri_run(sock: &Path, req: &serde_json::Value) -> io::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(s, "{req}")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    let v: serde_json::Value = serde_json::from_str(&line).map_err(io::Error::other)?;
    match v.get("Err") {
        None => Ok(()),
        Some(e) => Err(io::Error::other(format!("niri: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    #[test]
    fn set_layout_commands() {
        assert_eq!(
            hyprland_commands(&Action::SetLayout(2)),
            ["switchxkblayout all 2", "switchxkblayout all 2"]
        );
        assert_eq!(
            sway_command(&Action::SetLayout(1)),
            "input type:keyboard xkb_switch_layout 1"
        );
        assert_eq!(
            niri_request(&Action::SetLayout(1)).unwrap()["Action"]["SwitchLayout"]["layout"],
            json!({"Index": 1})
        );
    }

    fn ws(id: i64, name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn command_strings() {
        let num = Action::FocusWorkspace(ws(3, "3"));
        let named = Action::FocusWorkspace(ws(4, "we\"b"));
        let win = Action::FocusWindow {
            address: "aaba38".into(),
        };
        let close = Action::CloseWindow {
            address: "12".into(),
        };
        assert_eq!(
            hyprland_commands(&num),
            [
                "dispatch hl.dsp.focus({ workspace = \"3\" })",
                "dispatch workspace 3"
            ]
        );
        assert_eq!(
            hyprland_commands(&named),
            [
                r#"dispatch hl.dsp.focus({ workspace = "name:we\"b" })"#,
                "dispatch workspace name:we\"b"
            ]
        );
        assert_eq!(
            hyprland_commands(&win),
            [
                r#"dispatch hl.dsp.focus({ window = "address:0xaaba38" })"#,
                "dispatch focuswindow address:0xaaba38"
            ]
        );
        assert_eq!(
            hyprland_commands(&close),
            [
                r#"dispatch hl.dsp.window.close({ window = "address:0x12" })"#,
                "dispatch closewindow address:0x12"
            ]
        );
        assert_eq!(lua_str("a\nb\\"), r#""a\nb\\""#);
        assert_eq!(sway_command(&num), "workspace number 3");
        assert_eq!(sway_command(&named), "workspace \"we\\\"b\"");
        assert_eq!(sway_command(&close), "[con_id=12] kill");
        assert_eq!(
            hyprland_commands(&Action::NextLayout)[0],
            "switchxkblayout all next"
        );
        assert_eq!(
            sway_command(&Action::NextLayout),
            "input type:keyboard xkb_switch_layout next"
        );
        assert_eq!(
            niri_request(&Action::NextLayout).unwrap()["Action"]["SwitchLayout"]["layout"],
            "Next"
        );
        assert_eq!(niri_request(&win), None, "niri ids are numbers");
        assert_eq!(
            niri_request(&close).unwrap()["Action"]["CloseWindow"]["id"],
            12
        );
        assert_eq!(
            niri_request(&num).unwrap()["Action"]["FocusWorkspace"]["reference"]["Id"],
            3
        );
    }

    #[test]
    fn hyprland_roundtrip_and_errors() {
        let dir = std::env::temp_dir().join(format!("sliver-ctl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = hyprland::Paths::in_dir(&dir);
        let l = UnixListener::bind(&paths.command).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (i, c) in l.incoming().enumerate() {
                let mut c = c.unwrap();
                let mut buf = [0u8; 128];
                let n = c.read(&mut buf).unwrap();
                let q = String::from_utf8_lossy(&buf[..n]).into_owned();
                // A Lua config (the first two requests): dispatch arguments are Lua, and the
                // classic syntax is a Lua syntax error, as Hyprland 0.56 answers it. Then a
                // hyprlang config, which knows no Lua.
                let lua = i < 2;
                let reply: &[u8] = match (lua, q.starts_with("dispatch hl.dsp.")) {
                    (true, true) | (false, false) => b"ok",
                    (true, false) => b"error: [string \"return hl.dispatch(workspace 2)\"]:1: ')' expected near '2'",
                    (false, true) => b"Invalid dispatcher",
                };
                tx.send(q).unwrap();
                let _ = c.write_all(reply);
            }
        });
        // Paths::from_env looks under $XDG_RUNTIME_DIR/hypr/$SIG: build that layout.
        let rt = dir.join("rt");
        let sigdir = rt.join("hypr/sig");
        std::fs::create_dir_all(&sigdir).unwrap();
        std::fs::rename(&paths.command, sigdir.join(".socket.sock")).unwrap();
        let env = move |k: &str| match k {
            "HYPRLAND_INSTANCE_SIGNATURE" => Some("sig".to_string()),
            "XDG_RUNTIME_DIR" => Some(rt.display().to_string()),
            _ => None,
        };
        let focus = Action::FocusWindow {
            address: "ab".into(),
        };
        // Lua config: the Lua form, once.
        run(&env, &Action::FocusWorkspace(ws(2, "2"))).unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            r#"dispatch hl.dsp.focus({ workspace = "2" })"#
        );
        run(&env, &focus).unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            r#"dispatch hl.dsp.focus({ window = "address:0xab" })"#
        );
        // hyprlang config: the Lua form is refused, the classic one works.
        run(&env, &focus).unwrap();
        assert!(rx.recv().unwrap().starts_with("dispatch hl.dsp."));
        assert_eq!(rx.recv().unwrap(), "dispatch focuswindow address:0xab");
        assert!(rx.try_recv().is_err(), "nothing more was sent");
        assert_eq!(
            run(
                |_| None,
                &Action::CloseWindow {
                    address: "1".into()
                }
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
