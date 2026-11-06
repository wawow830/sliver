use super::*;
use sliver_config::CtxValue;
use sliver_desktop::fake::ScriptedSource;
use sliver_desktop::{Backend, Window, Workspace};
use sliver_session::fake::ScriptedSession;
use sliver_session::{Media, Power};
use std::time::Duration;

fn wait_readable(f: &Feeds) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let fd = f.as_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: 5,
        tv_nsec: 0,
    };
    matches!(poll(&mut fds, Some(&timeout)), Ok(n) if n > 0)
}

fn desktop() -> DesktopState {
    DesktopState {
        connected: true,
        backend: Backend::Hyprland,
        window: Some(Window {
            address: "a".into(),
            class: "firefox".into(),
            title: "Docs".into(),
            pid: Some(42),
            floating: false,
            fullscreen: true,
            xwayland: false,
        }),
        workspace: Some(Workspace {
            id: 2,
            name: "web".into(),
        }),
        monitor: Some("eDP-1".into()),
        keyboard_layout: Some("English (US)".into()),
        monitors: 2,
        external_display: true,
        ..Default::default()
    }
}

fn session() -> SessionState {
    SessionState {
        locked: false,
        power: Power {
            on_battery: Some(true),
            battery_pct: Some(37),
            battery_state: sliver_session::BatteryState::Discharging,
            ..Default::default()
        },
        media: Some(Media {
            identity: "Spotify".into(),
            title: "Song".into(),
            status: sliver_session::PlaybackStatus::Playing,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn context_has_the_rule_keys() {
    let mut a = Aggregate::new(None);
    assert_eq!(a.apply(Feed::Desktop(desktop())), Changed::Context);
    assert_eq!(a.apply(Feed::Session(session())), Changed::Context);
    let c = a.context();
    let s = |k: &str| match c.get(k) {
        Some(CtxValue::Str(s)) => s.clone(),
        other => panic!("{k}: {other:?}"),
    };
    let n = |k: &str| match c.get(k) {
        Some(CtxValue::Num(n)) => *n,
        other => panic!("{k}: {other:?}"),
    };
    let b = |k: &str| match c.get(k) {
        Some(CtxValue::Bool(b)) => *b,
        other => panic!("{k}: {other:?}"),
    };
    assert_eq!(
        (s("app_id").as_str(), s("title").as_str(), n("pid")),
        ("firefox", "Docs", 42.0)
    );
    assert_eq!(
        (
            n("workspace"),
            s("workspace_name").as_str(),
            s("monitor").as_str()
        ),
        (2.0, "web", "eDP-1")
    );
    assert_eq!(
        (n("monitors"), b("external_display"), b("fullscreen")),
        (2.0, true, true)
    );
    assert_eq!(s("keyboard_layout"), "English (US)");
    assert_eq!(
        (
            b("on_battery"),
            n("battery_pct"),
            s("battery_state").as_str()
        ),
        (true, 37.0, "discharging")
    );
    assert_eq!(
        (s("media_status").as_str(), s("media_player").as_str()),
        ("playing", "Spotify")
    );
    assert!(!b("locked"));
}

#[test]
fn unknown_stays_unknown_and_disconnect_clears_the_window() {
    let mut a = Aggregate::new(None);
    a.apply(Feed::Session(SessionState::default()));
    let c = a.context();
    assert!(
        c.get("on_battery").is_none(),
        "no UPower: on_battery is unknown, not false"
    );
    assert!(
        c.get("battery_pct").is_none() && c.get("volume").is_none() && c.get("app_id").is_none()
    );
    assert!(matches!(c.get("media_status"), Some(CtxValue::Str(s)) if s == "none"));
    a.apply(Feed::Desktop(desktop()));
    assert!(a.context().get("app_id").is_some());
    assert_eq!(
        a.apply(Feed::Desktop(DesktopState::default())),
        Changed::Context
    );
    assert!(
        a.context().get("app_id").is_none(),
        "compositor gone: focus keys are unknown again"
    );
}

#[test]
fn unchanged_state_is_not_a_change() {
    let mut a = Aggregate::new(None);
    a.apply(Feed::Desktop(desktop()));
    assert_eq!(a.apply(Feed::Desktop(desktop())), Changed::Nothing);
    let mut d = desktop();
    d.window.as_mut().unwrap().address = "other".into();
    assert_eq!(
        a.apply(Feed::Desktop(d)),
        Changed::Nothing,
        "fields no rule can see"
    );
    let u = Update {
        module: "custom/x".into(),
        output: Output {
            text: "1".into(),
            ..Default::default()
        },
    };
    assert_eq!(
        a.apply(Feed::Module(u.clone())),
        Changed::Module("custom/x".into())
    );
    assert_eq!(a.apply(Feed::Module(u)), Changed::Nothing);
    assert_eq!(a.module_output("custom/x").unwrap().text, "1");
}

#[test]
fn feeds_deliver_and_wake() {
    let mut f = Feeds::new().unwrap();
    assert!(f.drain().is_empty());
    f.spawn_desktop(ScriptedSource::new([desktop()]));
    f.spawn_session(ScriptedSession::new([session()]));
    let mut got = Vec::new();
    let have = |g: &[Feed]| {
        g.iter().any(|x| matches!(x, Feed::Session(_)))
            && g.iter()
                .any(|x| matches!(x, Feed::Desktop(d) if d.connected))
    };
    while !have(&got) {
        assert!(wait_readable(&f), "the wake fd became readable");
        got.extend(f.drain());
    }
    assert!(
        got.iter()
            .any(|x| matches!(x, Feed::Session(s) if s.power.battery_pct == Some(37)))
    );
    // a scripted desktop source ends after its script: initial (empty) state, change, disconnect
    assert!(
        got.iter()
            .any(|x| matches!(x, Feed::Desktop(d) if d.connected && d.window.is_some()))
    );
    // initial (empty) state, the change, then "disconnected": three desktop feeds in all
    while got.iter().filter(|x| matches!(x, Feed::Desktop(_))).count() < 3 {
        assert!(wait_readable(&f));
        got.extend(f.drain());
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(f.drain().is_empty(), "drain empties the channel and the fd");
}

#[test]
fn modules_flow_through_the_same_channel() {
    let mut f = Feeds::new().unwrap();
    f.start_modules(vec![Spec::new("custom/hi", "echo hello")]);
    assert!(wait_readable(&f));
    let feeds = f.drain();
    assert!(feeds.iter().any(
        |x| matches!(x, Feed::Module(u) if u.module == "custom/hi" && u.output.text == "hello")
    ));
    assert!(f.modules.as_ref().unwrap().trigger("custom/hi"));
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn desktop_backend_unavailable_leaves_a_note() {
    let mut f = Feeds::new().unwrap();
    f.start_desktop(|_| None);
    assert!(wait_readable(&f));
    let feeds = f.drain();
    assert!(
        feeds
            .iter()
            .any(|x| matches!(x, Feed::Note(n) if n.contains("desktop backend"))),
        "{feeds:?}"
    );
}

#[test]
fn hermetic_session_options_start_nothing_real() {
    let h = SessionOptions::hermetic();
    assert!(h.audio.is_none() && h.sysfs.is_none() && !h.notifications && !h.tray);
    let d = SessionOptions::default();
    assert!(d.audio.is_some(), "the default is the real desktop");
}

#[test]
fn a_spectrum_frame_is_levels_from_0_to_1() {
    assert_eq!(spectrum::parse("0 500 1000 \n"), [0.0, 0.5, 1.0]);
    assert_eq!(spectrum::parse("1200 x 250"), [1.0, 0.25]);
    assert!(spectrum::parse("").is_empty());
}

#[test]
fn cava_runs_while_asked_and_repeats_are_dropped() {
    // A stand-in that checks it got the config on stdin, then sends two equal frames and one
    // new one, and stays up.
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/test-tmp/cava-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("cava");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1 $2\" = '-p /dev/stdin' ] || exit 1\ngrep -q '^bars = 3$' || exit 1\n\
         echo '0 0 0'; echo '0 0 0'; echo '100 900 0'; exec sleep 30\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut f = Feeds::new().unwrap();
    f.cava = fake.to_string_lossy().into_owned();
    f.run_spectrum(Some("[general]\nbars = 3\n"));
    let mut frames = Vec::new();
    while frames.len() < 2 {
        assert!(wait_readable(&f), "cava's frames arrive");
        frames.extend(f.drain().into_iter().filter_map(|x| match x {
            Feed::Spectrum(l) => Some(l),
            _ => None,
        }));
    }
    assert_eq!(frames, [vec![0.0; 3], vec![0.1, 0.9, 0.0]]);
    // The same config again leaves it running; none stops it, and an empty spectrum follows.
    f.run_spectrum(Some("[general]\nbars = 3\n"));
    f.run_spectrum(None);
    let mut got = Vec::new();
    while !got
        .iter()
        .any(|x| matches!(x, Feed::Spectrum(l) if l.is_empty()))
    {
        assert!(wait_readable(&f));
        got.extend(f.drain());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_cava_is_a_note_not_a_retry() {
    let mut f = Feeds::new().unwrap();
    f.cava = "/nonexistent/cava".into();
    f.run_spectrum(Some("x"));
    f.run_spectrum(Some("x"));
    assert!(wait_readable(&f));
    let notes = f
        .drain()
        .into_iter()
        .filter(|x| matches!(x, Feed::Note(n) if n.starts_with("cava:")))
        .count();
    assert_eq!(notes, 1);
}

#[test]
fn a_new_spectrum_frame_redraws_the_cava_module() {
    let mut a = Aggregate::new(None);
    assert_eq!(
        a.apply(Feed::Spectrum(vec![0.5])),
        Changed::Module("cava".into())
    );
    assert_eq!(a.apply(Feed::Spectrum(vec![0.5])), Changed::Nothing);
    assert_eq!(a.spectrum, [0.5]);
}
