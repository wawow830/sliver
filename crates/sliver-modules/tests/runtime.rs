//! The runtime against real `sh` commands (no hardware, no desktop).

use sliver_modules::{Context, Mode, Runtime, Spec, Update};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sliver-mod-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn next(rx: &Receiver<Update>) -> Update {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("an update within 10s")
}

fn quiet(rx: &Receiver<Update>, ms: u64) {
    if let Ok(u) = rx.recv_timeout(Duration::from_millis(ms)) {
        panic!("unexpected update: {u:?}");
    }
}

fn every(key: &str, exec: &str, ms: u64) -> Spec {
    Spec {
        mode: Mode::Every(Duration::from_millis(ms)),
        ..Spec::new(key, exec)
    }
}

#[test]
fn once_runs_text_and_parses_lines() {
    let (rt, rx) = Runtime::start(vec![Spec::new(
        "custom/a",
        "printf 'hello\\ndetail\\nwarn big'",
    )]);
    let u = next(&rx);
    assert_eq!(u.module, "custom/a");
    assert_eq!(
        (u.output.text.as_str(), u.output.detail.as_deref()),
        ("hello", Some("detail"))
    );
    assert_eq!(u.output.classes, ["warn", "big"]);
    quiet(&rx, 300);
    drop(rt);
}

#[test]
fn static_modules_are_not_scheduled() {
    let mut s = Spec::new("custom/static", "x");
    s.exec = None;
    let (rt, rx) = Runtime::start(vec![s]);
    quiet(&rx, 200);
    assert_eq!(rt.keys(), ["custom/static"]);
}

#[test]
fn json_output_and_escape() {
    let mut s = Spec::new(
        "custom/j",
        r#"echo '{"text":"a<b","percentage":80,"class":"hot"}'"#,
    );
    s.json = true;
    s.escape = true;
    let (_rt, rx) = Runtime::start(vec![s]);
    let o = next(&rx).output;
    assert_eq!(
        (o.text.as_str(), o.percentage, o.classes.clone()),
        ("a&lt;b", Some(80.0), vec!["hot".to_string()])
    );
}

#[test]
fn bad_json_and_failures_show_the_error_state() {
    let mut bad = Spec::new("custom/bad", "echo nope");
    bad.json = true;
    let fail = Spec::new("custom/fail", "echo oops >&2; exit 3");
    let nostderr = Spec::new("custom/quiet", "exit 4");
    let (_rt, rx) = Runtime::start(vec![bad, fail, nostderr]);
    let mut got = std::collections::HashMap::new();
    for _ in 0..3 {
        let u = next(&rx);
        got.insert(u.module, u.output);
    }
    assert!(
        got["custom/bad"]
            .detail
            .as_deref()
            .unwrap()
            .starts_with("invalid JSON")
    );
    assert_eq!(got["custom/fail"].detail.as_deref(), Some("oops"));
    assert_eq!(
        got["custom/quiet"].detail.as_deref(),
        Some("exited with status 4")
    );
    for o in got.values() {
        assert_eq!(
            (o.text.as_str(), o.classes.clone()),
            ("!", vec!["error".to_string()])
        );
    }
}

#[test]
fn interval_reruns_and_unchanged_output_is_not_an_update() {
    let d = tmp("interval");
    let counter = d.join("n");
    // prints 1,2,3,... but stays at "3" after the third run
    let script = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; [ $n -gt 3 ] && n=3; echo $n",
        c = counter.display()
    );
    let (_rt, rx) = Runtime::start(vec![every("custom/i", &script, 80)]);
    let texts: Vec<String> = (0..3).map(|_| next(&rx).output.text).collect();
    assert_eq!(texts, ["1", "2", "3"]);
    quiet(&rx, 500); // keeps running, keeps printing 3: no more updates
    let runs: u32 = std::fs::read_to_string(&counter)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(runs >= 5, "it kept running ({runs} runs)");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn timeout_kills_the_whole_tree() {
    let d = tmp("timeout");
    let pidfile = d.join("pid");
    let mut s = Spec::new(
        "custom/slow",
        format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
    );
    s.timeout = Duration::from_millis(300);
    let t0 = Instant::now();
    let (_rt, rx) = Runtime::start(vec![s]);
    let o = next(&rx).output;
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(o.text, "!");
    assert!(o.detail.unwrap().starts_with("timed out"));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: signal 0 only checks existence.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the background sleep was killed with its group");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn exec_if_hides_the_module() {
    let mut s = Spec::new("custom/c", "echo shown");
    s.exec_if = Some("false".into());
    let (_rt, rx) = Runtime::start(vec![s]);
    assert!(next(&rx).output.hidden);
    let mut s = Spec::new("custom/d", "echo shown");
    s.exec_if = Some("true".into());
    let (_rt, rx) = Runtime::start(vec![s]);
    assert_eq!(next(&rx).output.text, "shown");
}

#[test]
fn continuous_streams_lines_then_restarts_with_backoff() {
    let d = tmp("cont");
    let runs = d.join("runs");
    let script = format!("echo x >> {r}; echo one; echo two", r = runs.display());
    let mut s = Spec::new("custom/s", &script);
    s.mode = Mode::Continuous;
    s.restart_interval = Duration::from_millis(100);
    let (rt, rx) = Runtime::start(vec![s]);
    assert_eq!(next(&rx).output.text, "one");
    assert_eq!(next(&rx).output.text, "two");
    // exits 0, restarts, prints the same two lines: "one" is a change again after "two"
    assert_eq!(next(&rx).output.text, "one");
    assert!(std::fs::read_to_string(&runs).unwrap().lines().count() >= 2);
    drop(rt);
    std::thread::sleep(Duration::from_millis(500));
    let after = std::fs::read_to_string(&runs).unwrap().lines().count();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        std::fs::read_to_string(&runs).unwrap().lines().count(),
        after,
        "dropped runtime stops restarts"
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn continuous_json_lines_and_long_running_process_is_killed_on_drop() {
    let d = tmp("contjson");
    let pidfile = d.join("pid");
    let script = format!(
        "echo $$ > {p}; while :; do echo '{{\"text\":\"tick\",\"value\":1}}'; sleep 0.1; echo '{{\"text\":\"tock\"}}'; sleep 30; done",
        p = pidfile.display()
    );
    let mut s = Spec::new("custom/cj", &script);
    s.mode = Mode::Continuous;
    s.json = true;
    let (rt, rx) = Runtime::start(vec![s]);
    assert_eq!(next(&rx).output.text, "tick");
    assert_eq!(next(&rx).output.text, "tock");
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(rt);
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: signal 0 only checks existence.
    assert!(
        unsafe { libc::kill(pid, 0) } != 0,
        "script killed when the runtime dropped"
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn trigger_signal_and_action_events() {
    let d = tmp("trig");
    let counter = d.join("n");
    let script = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; echo $n",
        c = counter.display()
    );
    let mut a = Spec::new("custom/sig", &script);
    a.signal = Some(5);
    let script_b = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; echo $n",
        c = d.join("m").display()
    );
    let mut b = Spec::new("custom/noevent", &script_b);
    b.exec_on_event = false;
    let (rt, rx) = Runtime::start(vec![a, b]);
    let mut first = [next(&rx), next(&rx)];
    first.sort_by(|x, y| x.module.cmp(&y.module));
    assert_eq!(first[1].module, "custom/sig");
    assert_eq!(rt.signal(9), 0, "nobody listens on 9");
    assert_eq!(rt.signal(5), 1);
    let u = next(&rx);
    assert_eq!(
        (u.module.as_str(), u.output.text.as_str()),
        ("custom/sig", "2")
    );
    rt.action_fired("custom/sig");
    assert_eq!(next(&rx).output.text, "3");
    rt.action_fired("custom/noevent");
    quiet(&rx, 300);
    assert!(
        rt.trigger("custom/noevent"),
        "explicit update works regardless"
    );
    assert_eq!(next(&rx).module, "custom/noevent");
    assert!(!rt.trigger("custom/nope"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn inactive_runtime_arms_no_timers_and_catches_up_on_resume() {
    let d = tmp("active");
    let counter = d.join("n");
    let script = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; echo $n",
        c = counter.display()
    );
    let (rt, rx) = Runtime::start(vec![every("custom/t", &script, 100)]);
    assert_eq!(next(&rx).output.text, "1");
    rt.set_active(false);
    std::thread::sleep(Duration::from_millis(150)); // let a run in flight finish
    while rx.try_recv().is_ok() {}
    let frozen = std::fs::read_to_string(&counter).unwrap();
    quiet(&rx, 600);
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        frozen,
        "no runs while inactive"
    );
    rt.trigger("custom/t");
    quiet(&rx, 300);
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        frozen,
        "even triggers wait for activation"
    );
    rt.set_active(true);
    let u = next(&rx);
    assert!(
        u.output.text.parse::<u32>().unwrap() > 1,
        "ran immediately on resume"
    );
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn environment_reaches_the_command() {
    let mut s = Spec::new(
        "custom/env",
        "echo \"$SLIVER_MODULE|$SLIVER_LAYER|$SLIVER_LAYOUT|$SLIVER_APP_ID|$MY_VAR\"",
    );
    s.env = vec![("MY_VAR".into(), "v".into())];
    let (rt, rx) = Runtime::start(vec![s.clone()]);
    drop(rt);
    drop(rx);
    // set the context before the first run is not possible (it starts at once), so trigger again
    let (rt, rx) = Runtime::start(vec![s]);
    let _ = next(&rx);
    rt.set_context(Context {
        layer: "base".into(),
        layout: "default".into(),
        app_id: "foot".into(),
    });
    rt.trigger("custom/env");
    assert_eq!(next(&rx).output.text, "custom/env|base|default|foot|v");
}

#[test]
fn output_is_bounded() {
    let (_rt, rx) = Runtime::start(vec![Spec::new(
        "custom/big",
        "head -c 5000000 /dev/zero | tr '\\0' 'x'; echo",
    )]);
    let o = next(&rx).output;
    assert!(o.text.len() <= 64 * 1024);
}
