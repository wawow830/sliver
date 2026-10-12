//! Bisecting idle wakeups (invariant 3): starts the chosen session sources on the buses named by
//! DBUS_SESSION_BUS_ADDRESS / DBUS_SYSTEM_BUS_ADDRESS, sleeps, and prints each thread's voluntary
//! context switches (sleeps after a wakeup) per second. Not run by tests.
//!
//!   idle-probe SECS [conn] [session] [audio] [notifications] [tray] [appearance] [screensaver]
use std::time::Duration;

fn switches() -> Vec<(String, u64)> {
    let mut v = Vec::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let name = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        let st = std::fs::read_to_string(t.path().join("status")).unwrap_or_default();
        let n = st
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        v.push((
            format!("{}:{}", name.trim(), t.file_name().to_string_lossy()),
            n,
        ));
    }
    v.sort();
    v
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(20);
    let has = |w: &str| args.iter().any(|a| a == w);
    let sys = sliver_bus::address(&std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap()).unwrap();
    let ses = || sliver_bus::address(&std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap()).unwrap();
    if has("dump") {
        // Which system-bus signals does the session watcher's subscription receive?
        let bus = zbus::blocking::fdo::DBusProxy::new(&sys).unwrap();
        for r in [
            "type='signal',sender='org.freedesktop.NetworkManager'",
            "type='signal',sender='org.bluez'",
            "type='signal',sender='org.freedesktop.login1'",
            "type='signal',sender='org.freedesktop.UPower'",
            "type='signal',sender='org.freedesktop.UPower.PowerProfiles'",
            "type='signal',sender='net.hadess.PowerProfiles'",
        ] {
            let _ = bus.add_match_rule(zbus::MatchRule::try_from(r).unwrap());
        }
        let start = std::time::Instant::now();
        let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
        for m in zbus::blocking::MessageIterator::from(sys.clone()).filter_map(Result::ok) {
            if m.message_type() == zbus::message::Type::Signal {
                let h = m.header();
                let k = format!(
                    "{} {} {}",
                    h.interface().map_or("", |i| i.as_str()),
                    h.member().map_or("", |i| i.as_str()),
                    h.path().map_or("", |i| i.as_str())
                );
                *counts.entry(k).or_default() += 1;
            }
            if start.elapsed().as_secs() >= secs {
                break;
            }
        }
        for (k, n) in counts {
            println!("{n:4} {k}");
        }
        return;
    }
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    if has("conn") {
        keep.push(Box::new(ses()));
    }
    if has("session") {
        let s = sliver_session::Session::start(sys.clone(), ses());
        let h = s.handle();
        if has("audio") {
            h.start_audio(sliver_session::audio::Tools::default());
        }
        if has("notifications") {
            h.start_notifications(ses(), ses()).unwrap();
        }
        if has("tray") {
            h.start_tray(ses());
        }
        if has("appearance") {
            h.start_appearance(ses(), None);
        }
        if has("screensaver") {
            h.start_screensaver(ses());
        }
        if has("dark") {
            h.set_awake(false);
        }
        keep.push(Box::new(s));
    }
    std::thread::sleep(Duration::from_secs(
        std::env::var("SETTLE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3),
    ));
    let before = switches();
    std::thread::sleep(Duration::from_secs(secs));
    let after = switches();
    let total: u64 = after
        .iter()
        .map(|(k, n)| n - before.iter().find(|(b, _)| b == k).map_or(0, |(_, b)| *b))
        .sum();
    println!("{:.2} wakeups/s", total as f64 / secs as f64);
    for (k, n) in &after {
        let d = n - before.iter().find(|(b, _)| b == k).map_or(0, |(_, b)| *b);
        if d > 0 {
            println!("  {k}: {d}");
        }
    }
    drop(keep);
}
