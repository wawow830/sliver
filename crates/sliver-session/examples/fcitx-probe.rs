//! Follows fcitx5 on the session bus for N seconds (default 30) and prints how often every thread
//! of this process woke up: the cost of the input-method poll. Read-only.
//! `cargo run -p sliver-session --example fcitx-probe -- 30`
use sliver_session::SessionSource;

fn switches() -> Vec<(String, u64)> {
    let mut v = Vec::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let name = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        let st = std::fs::read_to_string(t.path().join("status")).unwrap_or_default();
        let n = st
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0);
        v.push((name.trim().to_owned(), n));
    }
    v.sort();
    v
}

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let mut session = sliver_session::Session::start(
        sliver_bus::system().expect("system bus"),
        sliver_bus::session().expect("session bus"),
    );
    if std::env::var_os("PROBE_BASELINE").is_none() {
        session
            .handle()
            .start_input_method(sliver_bus::session().expect("session bus"));
    }
    std::thread::spawn(move || while let Ok(Some(_)) = session.next_change() {});
    std::thread::sleep(std::time::Duration::from_secs(3));
    let before = switches();
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let after = switches();
    let mut total = 0;
    for (name, n) in &after {
        let b = before
            .iter()
            .find(|(m, _)| m == name)
            .map_or(0, |(_, b)| *b);
        // Same-named threads are summed in `before`/`after` order; good enough for a probe.
        if n > &b {
            println!("{name}: {}", n - b);
            total += n - b;
        }
    }
    println!(
        "total {total} in {secs}s = {:.1}/s",
        total as f64 / secs as f64
    );
}
