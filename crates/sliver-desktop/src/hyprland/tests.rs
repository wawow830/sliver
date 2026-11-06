use super::*;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU32, Ordering};

fn ev(line: &str) -> Event {
    parse_event(line)
}

#[test]
fn parses_events() {
    assert_eq!(
        ev("workspacev2>>3,web"),
        Event::Workspace {
            id: 3,
            name: "web".into()
        }
    );
    assert_eq!(
        ev("activewindow>>kitty,a, b: c"),
        Event::ActiveWindow {
            class: "kitty".into(),
            title: "a, b: c".into()
        }
    );
    assert_eq!(
        ev("activewindowv2>>aaab12"),
        Event::ActiveWindowV2 {
            address: Some("aaab12".into())
        }
    );
    assert_eq!(
        ev("activewindowv2>>"),
        Event::ActiveWindowV2 { address: None }
    );
    assert_eq!(
        ev("activewindowv2>>,"),
        Event::ActiveWindowV2 { address: None }
    );
    assert_eq!(
        ev("activelayout>>apple-mtp-keyboard,English (US, intl.)"),
        Event::ActiveLayout {
            keyboard: "apple-mtp-keyboard".into(),
            layout: "English (US, intl.)".into()
        }
    );
    assert_eq!(ev("submap>>resize"), Event::Submap(Some("resize".into())));
    assert_eq!(ev("submap>>"), Event::Submap(None));
    assert_eq!(
        ev("activespecialv2>>-98,special:term,eDP-1"),
        Event::ActiveSpecial {
            name: Some("special:term".into()),
            monitor: "eDP-1".into()
        }
    );
    assert_eq!(
        ev("activespecialv2>>0,,eDP-1"),
        Event::ActiveSpecial {
            name: None,
            monitor: "eDP-1".into()
        }
    );
    assert_eq!(ev("screencast>>1,2"), Event::Screencast(true));
    assert_eq!(ev("screencast>>0,2"), Event::Screencast(false));
    assert_eq!(
        ev("monitoraddedv2>>1,HDMI-A-1,desc"),
        Event::MonitorsChanged
    );
    assert_eq!(
        ev("monitorremovedv2>>1,HDMI-A-1,desc"),
        Event::MonitorsChanged
    );
    assert_eq!(ev("createworkspacev2>>3,3"), Event::ListsChanged);
    assert_eq!(ev("openwindow>>a2,1,firefox,Doc"), Event::ListsChanged);
    assert_eq!(
        ev("urgent>>0xb2"),
        Event::Urgent {
            address: "b2".into()
        }
    );
    assert_eq!(ev("somethingnew>>x"), Event::Other);
    assert_eq!(ev("garbage"), Event::Other);
    assert_eq!(ev("workspacev2>>x,y"), Event::Other);
}

#[test]
fn folds_focus_sequence() {
    let mut s = DesktopState {
        connected: true,
        ..Default::default()
    };
    assert!(apply(&mut s, &ev("activewindow>>kitty,shell")));
    assert!(apply(&mut s, &ev("activewindowv2>>a1")));
    assert_eq!(s.window.as_ref().unwrap().address, "a1");
    // title change for another window is ignored, for the focused one applied
    assert!(!apply(&mut s, &ev("windowtitlev2>>zz,other")));
    assert!(apply(&mut s, &ev("windowtitlev2>>a1,new, title")));
    assert_eq!(s.window.as_ref().unwrap().title, "new, title");
    assert!(apply(&mut s, &ev("fullscreen>>1")));
    assert!(s.window.as_ref().unwrap().fullscreen);
    assert!(
        !apply(&mut s, &ev("fullscreen>>1")),
        "no-op is not a change"
    );
    assert!(apply(&mut s, &ev("closewindow>>a1")));
    assert_eq!(s.window, None);
    // fullscreen with no window is harmless
    assert!(!apply(&mut s, &ev("fullscreen>>0")));
}

#[test]
fn layouts_from_devices() {
    let devs = r#"{"keyboards":[
        {"name":"hl-virtual-keyboard","layout":"us","variant":"","main":false},
        {"name":"apple-mtp-keyboard","layout":"us,de,gr","variant":"intl,,","active_keymap":"English (US, intl.)","main":false}]}"#;
    assert_eq!(layouts_from_devices_json(devs), ["us(intl)", "de", "gr"]);
    assert!(layouts_from_devices_json("nope").is_empty());
}

#[test]
fn folds_workspace_layout_special() {
    let mut s = DesktopState::default();
    assert!(apply(&mut s, &ev("workspacev2>>2,2")));
    assert!(!apply(&mut s, &ev("workspacev2>>2,2")));
    assert!(
        !apply(&mut s, &ev("workspacev2>>-98,special:x")),
        "special ids are not workspaces"
    );
    assert!(apply(&mut s, &ev("focusedmonv2>>eDP-1,2")));
    assert_eq!(
        s.workspace.as_ref().unwrap().name,
        "2",
        "same id keeps its name"
    );
    assert!(apply(&mut s, &ev("activespecialv2>>-98,special:x,eDP-1")));
    assert_eq!(s.special_workspace.as_deref(), Some("special:x"));
    assert!(
        !apply(&mut s, &ev("activespecialv2>>-99,special:y,HDMI-A-1")),
        "other monitor"
    );
    assert!(apply(&mut s, &ev("activelayout>>kb,German")));
    assert_eq!(s.keyboard_layout.as_deref(), Some("German"));
}

#[test]
fn parses_queries() {
    let w = window_from_json(
        r#"{"address":"0xaaaba38dfa60","pid":42,"xwayland":true,"floating":true,
            "fullscreen":2,"class":"kitty","title":"t"}"#,
    )
    .unwrap();
    assert_eq!(
        w,
        Window {
            address: "aaaba38dfa60".into(),
            class: "kitty".into(),
            title: "t".into(),
            pid: Some(42),
            floating: true,
            fullscreen: true,
            xwayland: true
        }
    );
    assert_eq!(
        monitors_from_json(
            r#"[{"name":"HDMI-A-1"},{"name":"eDP-1"},{"name":"X","disabled":true}]"#
        ),
        Some(vec!["HDMI-A-1".to_string(), "eDP-1".to_string()])
    );
    assert_eq!(window_from_json("{}"), None);
    assert_eq!(window_from_json("Invalid"), None);
    let (ws, mon) = workspace_from_json(r#"{"id":1,"name":"1","monitor":"eDP-1"}"#).unwrap();
    assert_eq!(
        (ws.id, ws.name.as_str(), mon.as_deref()),
        (1, "1", Some("eDP-1"))
    );
    let devs = r#"{"keyboards":[
        {"name":"apple-smc-power/lid-events","active_keymap":"X","main":false},
        {"name":"hl-virtual-keyboard-fcitx5","active_keymap":"Y","main":false},
        {"name":"apple-mtp-keyboard","active_keymap":"English (US)","main":false}]}"#;
    assert_eq!(
        layout_from_devices_json(devs).as_deref(),
        Some("English (US)")
    );
}

#[test]
fn paths_from_env() {
    assert_eq!(Paths::from_env(|_| None), None);
    let p =
        Paths::from_env(|k| (k == "HYPRLAND_INSTANCE_SIGNATURE").then(|| "sig".into())).unwrap();
    assert_eq!(p.events, Path::new("/tmp/hypr/sig/.socket2.sock"));
}

/// A tiny fake compositor: serves canned replies on the command socket and pushes events on
/// the event socket. `!focus b2` in the event list moves the focus (what `j/activewindow`
/// answers) to that window instead of sending a line.
struct FakeHypr {
    dir: PathBuf,
}

impl FakeHypr {
    fn start(events: Vec<&'static str>) -> (Self, Paths) {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sliver-hypr-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::in_dir(&dir);
        let cmd = UnixListener::bind(&paths.command).unwrap();
        let evs = UnixListener::bind(&paths.events).unwrap();
        let focus = std::sync::Arc::new(std::sync::Mutex::new("a1"));
        let focus2 = focus.clone();
        std::thread::spawn(move || {
            for conn in cmd.incoming() {
                let Ok(mut c) = conn else { return };
                let mut buf = [0u8; 64];
                let n = c.read(&mut buf).unwrap_or(0);
                let reply = match std::str::from_utf8(&buf[..n]).unwrap_or("") {
                    "j/activewindow" if *focus.lock().unwrap() == "b2" => {
                        r#"{"address":"0xb2","pid":8,"class":"firefox","title":"Docs","floating":false,"fullscreen":0,"xwayland":false}"#
                    }
                    "j/activewindow" => {
                        r#"{"address":"0xa1","pid":7,"class":"kitty","title":"sh","floating":false,"fullscreen":0,"xwayland":false}"#
                    }
                    "j/activeworkspace" => r#"{"id":1,"name":"1","monitor":"eDP-1"}"#,
                    "j/monitors" => r#"[{"name":"eDP-1"},{"name":"HDMI-A-1","disabled":true}]"#,
                    "j/workspaces" => {
                        r#"[{"id":1,"name":"1","monitor":"eDP-1","windows":2},{"id":2,"name":"web","windows":0},{"id":-98,"name":"special:x","windows":1}]"#
                    }
                    "j/clients" => {
                        r#"[{"address":"0xa1","mapped":true,"workspace":{"id":1,"name":"1"},"class":"kitty","title":"sh","pid":7},{"address":"0xb2","mapped":true,"workspace":{"id":1,"name":"1"},"class":"firefox","title":"Docs","pid":8},{"address":"0xc3","mapped":false,"workspace":{"id":2,"name":"2"},"class":"x","title":"y"}]"#
                    }
                    "j/devices" => {
                        r#"{"keyboards":[{"name":"kb","active_keymap":"English (US)","main":true}]}"#
                    }
                    "submap" => "default\n",
                    _ => "unknown request",
                };
                let _ = c.write_all(reply.as_bytes());
            }
        });
        std::thread::spawn(move || {
            let Ok((mut c, _)) = evs.accept() else { return };
            // Give the client time to finish its snapshot, then speak.
            std::thread::sleep(Duration::from_millis(100));
            for e in events {
                match e.strip_prefix("!focus ") {
                    Some(a) => *focus2.lock().unwrap() = a,
                    None => {
                        let _ = c.write_all(format!("{e}\n").as_bytes());
                    }
                }
            }
            // dropping closes the stream: compositor "died"
        });
        (Self { dir }, paths)
    }
}

impl Drop for FakeHypr {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn watcher_end_to_end() {
    let (_fake, paths) = FakeHypr::start(vec![
        "openwindow>>a2,1,firefox,Mozilla Firefox", // untracked: no change emitted
        "activewindow>>kitty,sh",                   // legacy event, ignored
        "activewindowv2>>a1",                       // query result identical to snapshot: swallowed
        "workspacev2>>2,web",
        "activelayout>>kb,German",
        "submap>>resize",
    ]);
    let mut w = Watcher::connect(&paths).unwrap();
    let s0 = w.state().clone();
    assert!(s0.connected);
    assert_eq!(s0.backend, Backend::Hyprland);
    assert_eq!(s0.window.as_ref().unwrap().pid, Some(7));
    assert_eq!(s0.workspace.as_ref().unwrap().id, 1);
    assert_eq!(s0.monitor.as_deref(), Some("eDP-1"));
    assert_eq!(s0.keyboard_layout.as_deref(), Some("English (US)"));
    assert_eq!(s0.submap, None);
    assert_eq!(
        s0.workspaces
            .iter()
            .map(|w| (w.id, w.windows, w.active))
            .collect::<Vec<_>>(),
        [(1, 2, true), (2, 0, false)],
        "special workspaces are not listed"
    );
    assert_eq!(
        s0.windows
            .iter()
            .map(|w| (w.address.as_str(), w.focused))
            .collect::<Vec<_>>(),
        [("a1", true), ("b2", false)],
        "unmapped windows are not listed; the focused one is flagged"
    );
    assert_eq!(
        (s0.monitors, s0.external_display),
        (1, false),
        "disabled output ignored"
    );

    let s2 = w.next_change().unwrap().unwrap();
    assert_eq!(
        s2.window.as_ref().unwrap().pid,
        Some(7),
        "no flicker through a half window"
    );
    assert_eq!(s2.workspace.as_ref().unwrap().name, "web");
    let s3 = w.next_change().unwrap().unwrap();
    assert_eq!(s3.keyboard_layout.as_deref(), Some("German"));
    let s4 = w.next_change().unwrap().unwrap();
    assert_eq!(s4.submap.as_deref(), Some("resize"));
    assert_eq!(w.next_change().unwrap(), None, "EOF ends the stream");
    assert!(!w.state().connected);
}

#[test]
fn the_taskbar_follows_focus() {
    // As Hyprland sends it: the workspace first, then the window.
    let (_fake, paths) =
        FakeHypr::start(vec!["!focus b2", "workspacev2>>1,1", "activewindowv2>>b2"]);
    let mut w = Watcher::connect(&paths).unwrap();
    let focused = |s: &DesktopState| -> Vec<String> {
        s.windows
            .iter()
            .filter(|w| w.focused)
            .map(|w| w.address.clone())
            .collect()
    };
    assert_eq!(focused(w.state()), ["a1"]);
    let s = w.next_change().unwrap().unwrap();
    assert_eq!(s.window.as_ref().unwrap().address, "b2");
    assert_eq!(
        focused(&s),
        ["b2"],
        "the list's focus moved with the window"
    );
}

#[test]
fn watch_reports_disconnect_and_stops() {
    let (_fake, paths) = FakeHypr::start(vec!["submap>>x"]);
    let mut seen = Vec::new();
    watch(&paths, |s| {
        seen.push(s.clone());
        // initial, submap change, disconnected -> stop
        if seen.len() == 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    assert!(seen[0].connected && seen[0].submap.is_none());
    assert_eq!(seen[1].submap.as_deref(), Some("x"));
    assert_eq!(seen[2], DesktopState::default());
}

#[test]
fn watch_without_compositor_reports_disconnected_once() {
    let dir = std::env::temp_dir().join("sliver-hypr-nonexistent");
    let paths = Paths::in_dir(&dir);
    let mut n = 0;
    watch(&paths, |s| {
        assert!(!s.connected);
        n += 1;
        ControlFlow::Break(())
    });
    assert_eq!(n, 1);
}

#[test]
fn bar_keymap_from_hyprland_devices() {
    // As `hyprctl devices -j` reported it on the Mac14,7, plus a second layout.
    let devs = r#"{"keyboards":[
        {"name":"apple-mtp-keyboard","layout":"us","variant":"","options":"","active_layout_index":0,"main":false},
        {"name":"sliver-touch-bar-keys","rules":"","model":"","layout":"fr,us","variant":"azerty,","options":"caps:escape","active_layout_index":1,"active_keymap":"English (US)","capsLock":false,"numLock":true,"main":true}]}"#;
    assert_eq!(
        bar_keymap_from_devices_json(devs),
        Some(crate::XkbNames {
            rules: String::new(),
            model: String::new(),
            layout: "fr,us".into(),
            variant: "azerty,".into(),
            options: "caps:escape".into(),
            index: 1,
        })
    );
    let without = r#"{"keyboards":[{"name":"apple-mtp-keyboard","layout":"us"}]}"#;
    assert_eq!(
        bar_keymap_from_devices_json(without),
        None,
        "sliverd not running"
    );
}
