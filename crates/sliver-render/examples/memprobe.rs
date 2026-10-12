//! Memory under font fallback: render texts that need fallback fonts and print RSS after each.
//! `cargo run --release -p sliver-render --example memprobe`

use sliver_render::{Content, Fonts, Item, Pixmap, Renderer, Scene, Stylesheet};

fn mem() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .filter(|l| l.starts_with("VmHWM") || l.starts_with("RssAnon") || l.starts_with("RssFile"))
        .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("  ")
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
