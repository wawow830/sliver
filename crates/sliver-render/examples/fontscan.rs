//! How much the system font scan reads into the page cache (and so into the cgroup's memory).
//!
//!   python3 -c '…posix_fadvise DONTNEED on /usr/share/fonts…'   # cold cache first
//!   systemd-run --user --scope -q target/release/examples/fontscan [fontdb]
//!
//! Prints the cgroup's `file` (page cache charged to it) and peak after building the font
//! database: sliver's scan by default, or fontdb's `load_system_fonts` with `fontdb`.

use cosmic_text::fontdb;

fn cgroup_stat() -> String {
    let cg = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let path = cg.trim().rsplit("::").next().unwrap_or("").to_owned();
    let dir = format!("/sys/fs/cgroup{path}");
    let stat = std::fs::read_to_string(format!("{dir}/memory.stat")).unwrap_or_default();
    let file = stat
        .lines()
        .find_map(|l| l.strip_prefix("file "))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let peak = std::fs::read_to_string(format!("{dir}/memory.peak"))
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    format!(
        "page cache {:.1} MB, cgroup peak {:.1} MB",
        file as f64 / 1048576.0,
        peak as f64 / 1048576.0
    )
}

fn main() {
    let t = std::time::Instant::now();
    let faces = if std::env::args().nth(1).as_deref() == Some("fontdb") {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db.len()
    } else {
        sliver_render::Fonts::system().face_count()
    };
    println!("{faces} faces in {:?}; {}", t.elapsed(), cgroup_stat());
}
