//! Hot reload against real files and real inotify, in a temp directory.

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sliver_reload::{Outcome, Reloader};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("sliver-reload-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn path(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
    fn write(&self, n: &str, text: &str) {
        std::fs::write(self.path(n), text).unwrap();
    }
    /// How editors save: a temp file renamed over the target (new inode).
    fn save_atomic(&self, n: &str, text: &str) {
        let tmp = self.path(&format!(".{n}.swp"));
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, self.path(n)).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Wait (up to `secs`) for the next reload outcome the way the agent's loop would: poll the
/// fd, feed it, use the deadline as the timeout.
fn next_outcome(r: &mut Reloader, secs: u64) -> Option<Outcome> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let now = Instant::now();
        if now >= end {
            return None;
        }
        let until = r.deadline().map_or(end, |d| d.min(end));
        let timeout = until.saturating_duration_since(now);
        let fd = r.as_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let ts = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: timeout.subsec_nanos() as i64,
        };
        let n = poll(&mut fds, Some(&ts)).unwrap_or(0);
        let now = Instant::now();
        if n > 0 {
            r.on_readable(now);
        }
        if let Some(o) = r.poll_due(now) {
            return Some(o);
        }
    }
}

fn quiet(r: &mut Reloader, ms: u64) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        let fd = r.as_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let ts = Timespec {
            tv_sec: 0,
            tv_nsec: 20_000_000,
        };
        if poll(&mut fds, Some(&ts)).unwrap_or(0) > 0 {
            r.on_readable(Instant::now());
        }
        if r.poll_due(Instant::now()).is_some() {
            return false;
        }
    }
    true
}

fn bar_name(r: &Reloader) -> Option<String> {
    r.current().config.bar.name.clone()
}

#[test]
fn atomic_saves_reload_repeatedly_and_invalid_is_rejected() {
    let d = Dir::new("main");
    d.write("config.toml", "[bar]\nname = \"one\"\n");
    let (mut r, first) = Reloader::start(Some(&d.path("config.toml"))).unwrap();
    assert!(first.errors.is_empty());
    assert_eq!(bar_name(&r).as_deref(), Some("one"));

    // two atomic-rename saves in a row: a watch on the inode would miss the second
    for name in ["two", "three"] {
        d.save_atomic("config.toml", &format!("[bar]\nname = \"{name}\"\n"));
        match next_outcome(&mut r, 5) {
            Some(Outcome::Applied(a)) => assert_eq!(a.config.bar.name.as_deref(), Some(name)),
            other => panic!("expected Applied for {name}, got {}", describe(&other)),
        }
        assert_eq!(bar_name(&r).as_deref(), Some(name));
    }

    // a broken save is rejected, the last good config stays
    d.save_atomic("config.toml", "[bar\nname = oops\n");
    match next_outcome(&mut r, 5) {
        Some(Outcome::Rejected(e)) => assert!(!e.is_empty()),
        other => panic!("expected Rejected, got {}", describe(&other)),
    }
    assert_eq!(
        bar_name(&r).as_deref(),
        Some("three"),
        "last good stays live"
    );

    // fixing it applies again
    d.save_atomic("config.toml", "[bar]\nname = \"fixed\"\n");
    assert!(matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))));
    assert_eq!(bar_name(&r).as_deref(), Some("fixed"));

    // valid TOML but an invalid config (base layer -> missing layout) is rejected too
    d.save_atomic(
        "config.toml",
        "[bar]\nname = \"bad\"\n[layers]\nbase = \"nope\"\n",
    );
    match next_outcome(&mut r, 5) {
        Some(Outcome::Rejected(e)) => assert!(e.iter().any(|e| e.contains("nope")), "{e:?}"),
        other => panic!("expected Rejected, got {}", describe(&other)),
    }
    assert_eq!(bar_name(&r).as_deref(), Some("fixed"));
}

#[test]
fn unrelated_files_and_content_free_touches_do_nothing() {
    let d = Dir::new("quiet");
    d.write("config.toml", "[bar]\nname = \"x\"\n");
    let (mut r, _) = Reloader::start(Some(&d.path("config.toml"))).unwrap();
    d.write("notes.txt", "hi");
    assert!(quiet(&mut r, 400), "a file nobody asked about is ignored");
    // same content saved again: the event fires but the result is Unchanged
    d.save_atomic("config.toml", "[bar]\nname = \"x\"\n");
    assert!(matches!(next_outcome(&mut r, 5), Some(Outcome::Unchanged)));
}

#[test]
fn includes_and_stylesheets_are_watched() {
    let d = Dir::new("inc");
    d.write(
        "config.toml",
        "include = [\"mods.toml\"]\nstylesheet = \"style.css\"\n",
    );
    d.write("mods.toml", "[bar]\nname = \"a\"\n");
    d.write("style.css", "module { color: red }\n");
    let (mut r, first) = Reloader::start(Some(&d.path("config.toml"))).unwrap();
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert_eq!(bar_name(&r).as_deref(), Some("a"));

    d.save_atomic("mods.toml", "[bar]\nname = \"b\"\n");
    assert!(
        matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))),
        "include change"
    );
    assert_eq!(bar_name(&r).as_deref(), Some("b"));

    d.save_atomic(
        "style.css",
        "module { color: blue }\n@import \"more.css\";\n",
    );
    d.write("more.css", "module { color: green }\n");
    assert!(
        matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))),
        "stylesheet change"
    );

    // the new import is now watched too
    std::thread::sleep(Duration::from_millis(200));
    d.save_atomic("more.css", "module { color: white }\n");
    assert!(
        matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))),
        "import change"
    );
}

#[test]
fn a_config_created_later_is_picked_up_and_reload_now_works() {
    let d = Dir::new("late");
    let path = d.path("config.toml");
    let (mut r, first) = Reloader::start(Some(&path)).unwrap();
    assert!(first.config_path.is_some());
    d.write("config.toml", "[bar]\nname = \"born\"\n");
    assert!(matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))));
    assert_eq!(bar_name(&r).as_deref(), Some("born"));
    // forced reload (SIGUSR2 / sliverctl reload) of an unchanged config reports Unchanged
    assert!(matches!(r.reload_now(), Outcome::Unchanged));
}

#[test]
fn invalid_config_at_start_falls_back_to_the_defaults_with_reasons() {
    let d = Dir::new("bad");
    d.write("config.toml", "version = 99\n");
    let (r, first) = Reloader::start(Some(&d.path("config.toml"))).unwrap();
    assert!(!first.errors.is_empty());
    assert!(
        r.current().config.layouts.contains_key("default"),
        "built-in layouts are there"
    );
}

fn describe(o: &Option<Outcome>) -> &'static str {
    match o {
        None => "nothing",
        Some(Outcome::Applied(_)) => "Applied",
        Some(Outcome::Rejected(_)) => "Rejected",
        Some(Outcome::Unchanged) => "Unchanged",
    }
}

#[test]
fn a_new_accent_rebuilds_the_stylesheet_and_unchanged_is_unchanged() {
    let d = Dir::new("accent");
    d.write("config.toml", "[bar]\nname = \"x\"\n");
    let (mut r, _) = Reloader::start(Some(&d.path("config.toml"))).unwrap();
    assert!(matches!(r.reload_now(), Outcome::Unchanged));
    let c = sliver_render::Color::rgb(200, 30, 90);
    assert!(r.set_accent(Some(c)));
    assert!(!r.set_accent(Some(c)), "same accent: nothing to do");
    assert!(
        matches!(r.reload_now(), Outcome::Applied(_)),
        "the accent changes the sheet"
    );
    assert!(matches!(r.reload_now(), Outcome::Unchanged));
    // the theme really uses it: a checked module is drawn in the accent colour
    use sliver_render::{Fonts, Item, Pixmap, Renderer, Scene};
    let mut item = Item::text("x").fill(1);
    item.checked = true;
    let mut px = Pixmap::new(120, 60).unwrap();
    Renderer::new(Fonts::embedded()).render(&Scene::new(vec![item]), &r.current().sheet, &mut px);
    let at = (5 * 120 + 8) * 4;
    assert_eq!(&px.data()[at..at + 3], &[200, 30, 90], "accent background");
}

#[test]
fn writing_active_profile_switches_the_config() {
    // The only test here that uses the search path (and so the environment).
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/reload-profile");
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("sliver");
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    // SAFETY: no other test in this binary reads XDG_CONFIG_HOME or HOME.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &root) };
    std::fs::write(dir.join("config.toml"), "[bar]\nname = \"main\"\n").unwrap();
    std::fs::write(
        dir.join("profiles/talk.toml"),
        "include = [\"../config.toml\"]\n[bar]\nname = \"talk\"\n",
    )
    .unwrap();
    let (mut r, _) = Reloader::start(None).unwrap();
    assert_eq!(bar_name(&r).as_deref(), Some("main"));

    sliver_config::profile::set_active("talk").unwrap();
    assert!(matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))));
    assert_eq!(bar_name(&r).as_deref(), Some("talk"));
    // Edits to the active profile reload too.
    std::fs::write(
        dir.join("profiles/talk.toml"),
        "include = [\"../config.toml\"]\n[bar]\nname = \"talk2\"\n",
    )
    .unwrap();
    assert!(matches!(next_outcome(&mut r, 5), Some(Outcome::Applied(_))));
    assert_eq!(bar_name(&r).as_deref(), Some("talk2"));

    sliver_config::profile::set_active("default").unwrap();
    let o = next_outcome(&mut r, 5);
    assert!(matches!(o, Some(Outcome::Applied(_))), "{}", describe(&o));
    assert_eq!(bar_name(&r).as_deref(), Some("main"));
    let _ = std::fs::remove_dir_all(&root);
}
