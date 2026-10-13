//! Memory under font fallback: render texts that need fallback fonts and print RSS after each.
//! `cargo build --release -p sliver-render --example memprobe &&
//!  systemd-run --user --scope -q target/release/examples/memprobe`

use sliver_render::{Content, Fonts, Item, Pixmap, Renderer, Scene, Stylesheet};

fn mem() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .filter(|l| l.starts_with("VmHWM") || l.starts_with("RssAnon") || l.starts_with("RssFile"))
        .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("  ")
        + &cgroup_file()
        + &font_rss()
}

/// Resident pages of font file mappings (they should only be the fonts in use).
fn font_rss() -> String {
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let (mut font, mut kb) = (false, 0u64);
    for l in smaps.lines() {
        if l.split_whitespace()
            .next()
            .is_some_and(|w| w.contains('-') && !w.ends_with(':'))
        {
            font = l.contains("/fonts/");
        } else if font && let Some(v) = l.strip_prefix("Rss:") {
            kb += v
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
        }
    }
    format!("  fonts resident: {kb} kB")
}

/// Page cache charged to this process's cgroup (run under `systemd-run --user --scope` to see
/// only this process's): what systemd's MemoryPeak counts on top of RSS.
fn cgroup_file() -> String {
    let cg = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let path = cg.trim().rsplit("::").next().unwrap_or("").to_owned();
    let stat = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.stat"));
    let file = stat.ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("file ")?.parse::<u64>().ok())
    });
    file.map_or(String::new(), |f| {
        format!("  cgroup page cache: {} kB", f / 1024)
    })
}

fn main() {
    let mut r = Renderer::new(Fonts::system());
    let sheet = Stylesheet::sliver();
    println!("{:<28} {}", "start", mem());
    let texts = [
        ("latin", "Song & Dance 09:41"),
        ("emoji", "😀👍🏽👨‍👩‍👧🇯🇵"),
        ("cjk", "漢字かなカナ한국어"),
        ("arabic+hebrew", "مرحبا שלום"),
        ("symbols", "⏸ ▶ ⏭ ⌘ ⌥ ⇧ ✕ ‹ ›"),
        ("rare", "𓀀 𐎀 ꦲ ᚠ ⵣ"),
        ("missing", "\u{10fffd}\u{e000}"),
    ];
    // Each new text with a glyph no font has searches every font again.
    for i in 0..3 {
        let t0 = std::time::Instant::now();
        let mut p = Pixmap::new(2008, 60).unwrap();
        let text = format!("title {i} \u{10fffd}");
        r.render(
            &Scene::new(vec![Item::new(Content::Markup(text))]),
            &sheet,
            &mut p,
        );
        println!(
            "{:<28} {:?} {}",
            format!("missing-glyph text #{i}"),
            t0.elapsed(),
            font_rss()
        );
    }
    for (name, t) in texts {
        let mut p = Pixmap::new(2008, 60).unwrap();
        r.render(
            &Scene::new(vec![Item::new(Content::Markup(t.into()))]),
            &sheet,
            &mut p,
        );
        println!("{name:<28} {}", mem());
    }
}
