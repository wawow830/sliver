//! Telling the compositor what to do: switch workspace, focus or close a window. This is the
//! write side of the lists in [`crate::DesktopState`] (workspaces bar, taskbar). It is the only
//! part of this crate that changes the desktop, and only on an explicit user action.
//!
//! Hyprland and sway speak commands over their IPC sockets; niri takes JSON actions. Command
//! strings are built by pure functions so they are tested without a compositor.

use crate::{Backend, WorkspaceInfo, hyprland, niri, sway};
use serde_json::json;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// What to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    FocusWorkspace(WorkspaceInfo),
    FocusWindow { address: String },
    CloseWindow { address: String },
}

/// Hyprland dispatcher command (`socket1`, without the `/dispatch` prefix hyprctl adds).
pub fn hyprland_command(a: &Action) -> String {
    match a {
        Action::FocusWorkspace(w) if w.name == w.id.to_string() => {
            format!("dispatch workspace {}", w.id)
        }
        Action::FocusWorkspace(w) => format!("dispatch workspace name:{}", w.name),
        Action::FocusWindow { address } => format!("dispatch focuswindow address:0x{address}"),
        Action::CloseWindow { address } => format!("dispatch closewindow address:0x{address}"),
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
            let reply = hyprland::request(&paths.command, &hyprland_command(a))?;
            if reply.trim() == "ok" {
                Ok(())
            } else {
                Err(io::Error::other(format!("hyprland: {}", reply.trim())))
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
        assert_eq!(hyprland_command(&num), "dispatch workspace 3");
        assert_eq!(hyprland_command(&named), "dispatch workspace name:we\"b");
        assert_eq!(
            hyprland_command(&win),
            "dispatch focuswindow address:0xaaba38"
        );
        assert_eq!(
            hyprland_command(&close),
            "dispatch closewindow address:0x12"
        );
        assert_eq!(sway_command(&num), "workspace number 3");
        assert_eq!(sway_command(&named), "workspace \"we\\\"b\"");
        assert_eq!(sway_command(&close), "[con_id=12] kill");
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
                tx.send(String::from_utf8_lossy(&buf[..n]).into_owned())
                    .unwrap();
                let _ = c.write_all(if i == 0 { b"ok" } else { b"Invalid dispatcher" });
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
        run(&env, &Action::FocusWorkspace(ws(2, "2"))).unwrap();
        assert_eq!(rx.recv().unwrap(), "dispatch workspace 2");
        let e = run(
            &env,
            &Action::FocusWindow {
                address: "ab".into(),
            },
        )
        .unwrap_err();
        assert!(e.to_string().contains("Invalid dispatcher"), "{e}");
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
