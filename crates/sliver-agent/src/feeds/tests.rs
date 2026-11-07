use super::*;
use sliver_config::CtxValue;
use sliver_desktop::fake::ScriptedSource;
use sliver_desktop::{Backend, Window, Workspace};
use sliver_session::fake::ScriptedSession;
use sliver_session::{Media, Power};
use std::time::Duration;

fn wait_readable(f: &Feeds) -> bool {
    readable_within(f, 5)
}

fn readable_within(f: &Feeds, secs: i64) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let fd = f.as_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: secs,
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
    // The scripted session sends its one state; the scripted desktop its initial (empty) state,
    // the change, then "disconnected" when its script ends. Whenever it's drained, each kind is
    // only its latest state: fold drains until they come back empty.
    assert!(wait_readable(&f), "the wake fd becomes readable");
    let (mut desktop, mut session) = (None, None);
    let mut drains = 0;
    loop {
        assert!(drains < 100, "the sources went quiet");
        drains += 1;
        std::thread::sleep(Duration::from_millis(50));
        let got = f.drain();
        let kinds: Vec<_> = got.iter().filter_map(Feed::slot).collect();
        assert!(
            kinds
                .iter()
                .all(|k| kinds.iter().filter(|x| *x == k).count() == 1)
        );
        if got.is_empty() && session.is_some() {
            break;
        }
        for x in got {
            match x {
                Feed::Desktop(d) => desktop = Some(d),
                Feed::Session(s) => session = Some(s),
                _ => {}
            }
        }
    }
    assert_eq!(session.unwrap().power.battery_pct, Some(37));
    assert_eq!(
        desktop,
        Some(DesktopState::default()),
        "latest: disconnected"
    );
    assert!(!readable_within(&f, 0), "drain empties the fd");
    assert!(f.drain().is_empty(), "drain empties the mailbox and the fd");
}

#[test]
fn album_art_resolves_to_the_newest_url() {
    let d = std::env::temp_dir().join(format!("sliver-feed-art-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let jpg = include_bytes!("../../../sliver-session/tests/art-48x32.jpg");
    let states: Vec<_> = (0..5)
        .map(|i| {
            let file = d.join(format!("{i}.jpg"));
            std::fs::write(&file, jpg).unwrap();
            let mut s = session();
            s.media.as_mut().unwrap().art_url = Some(format!("file://{}", file.display()));
            s
        })
        .collect();
    let mut f = Feeds::new().unwrap();
    f.art_cache = Some(d.join("cache"));
    f.spawn_session(ScriptedSession::new(states));
    let mut a = Aggregate::new(None);
    let newest = d.join("4.jpg");
    for _ in 0..100 {
        if a.media_art() == Some(newest.as_path()) {
            break;
        }
        assert!(wait_readable(&f));
        for x in f.drain() {
            a.apply(x);
        }
    }
    assert_eq!(a.media_art(), Some(newest.as_path()));
    let _ = std::fs::remove_dir_all(&d);
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
    assert!(h.audio.is_none() && h.sysfs.is_none() && !h.notifications);
    let d = SessionOptions::default();
    assert!(d.audio.is_some(), "the default is the real desktop");
}

/// States replace the undrained state of their kind; notes are kept, the newest MAX_NOTES.
#[test]
fn the_mailbox_keeps_the_latest_state_and_the_newest_notes() {
    let mut m = Mailbox::default();
    let module = |k: &str, text: &str| {
        Feed::Module(Update {
            module: k.into(),
            output: Output {
                text: text.into(),
                ..Default::default()
            },
        })
    };
    for i in 0..1000 {
        m.put(Feed::Desktop(Default::default()));
        m.put(module("custom/a", &i.to_string()));
    }
    m.put(module("custom/b", "b"));
    for i in 0..(MAX_NOTES + 5) {
        m.put(Feed::Note(i.to_string()));
    }
    let got = m.take();
    assert_eq!(got.len(), 3 + MAX_NOTES);
    assert_eq!(got[0], Feed::Desktop(Default::default()));
    assert_eq!(got[1], module("custom/a", "999"));
    assert_eq!(got[2], module("custom/b", "b"));
    assert_eq!(got[3], Feed::Note("5".into()), "the oldest notes went");
    assert!(m.take().is_empty());
}
