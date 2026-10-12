//! Renders a bar to PNG from the command line, for quick looks and mockups.
//!
//! ```text
//! cargo run -p sliver-render --example bar -- out.png esc F1 F2 icon:play_pause x3:Hello! - _
//! ```
//! Each item is a label, `icon:NAME` (built-in) or `icon:/path/to.svg`, `-` (spacer) or `_` (blank
//! button). Prefix `xN:` to stretch, `!` suffix to show pressed. `--system-fonts` uses installed
//! fonts (with fallback) instead of only the embedded one. `--size WxH` changes the bar size.

use std::time::Instant;

use sliver_render::{Content, Fonts, Icon, Item, Renderer, Scene, Theme};

fn parse_item(arg: &str) -> Item {
    let (stretch, rest) = match arg.split_once(':') {
        Some((n, rest)) if n.starts_with('x') && n[1..].parse::<u32>().is_ok() => {
            (n[1..].parse().unwrap(), rest)
        }
        _ => (1, arg),
    };
    let (pressed, rest) = match rest.strip_suffix('!') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, rest),
    };
    let content = match rest {
        "-" => Content::Spacer,
        "_" => Content::Blank,
        _ => match rest.strip_prefix("icon:") {
            Some(name) if name.contains('/') => {
                Content::Icon(Icon::load(name).unwrap_or_else(|e| panic!("{name}: {e}")))
            }
            Some(name) => Content::Icon(Icon::builtin(name).unwrap_or_else(|| {
                panic!(
                    "no built-in icon {name:?}; have {:?}",
                    sliver_render::BUILTIN_ICONS
                )
            })),
            None => Content::Text(rest.to_string()),
        },
    };
    Item::new(content).stretch(stretch).pressed(pressed)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let system = args.iter().any(|a| a == "--system-fonts");
    args.retain(|a| a != "--system-fonts");
    let (mut w, mut h) = (2008, 60);
    if let Some(i) = args.iter().position(|a| a == "--size") {
        let size = args.remove(i + 1);
        args.remove(i);
        let (sw, sh) = size.split_once('x').expect("--size WxH");
        (w, h) = (sw.parse().unwrap(), sh.parse().unwrap());
    }
    let Some((out, items)) = args.split_first() else {
        eprintln!("usage: bar OUT.png [--system-fonts] [--size WxH] ITEM...");
        std::process::exit(2);
    };
    let scene = Scene::new(items.iter().map(|a| parse_item(a)).collect());
    let fonts = if system {
        Fonts::system()
    } else {
        Fonts::embedded()
    };
    let mut renderer = Renderer::new(fonts);
    let theme = Theme::default();
    let t = Instant::now();
    let pixmap = renderer.render_new(&scene, &theme, w, h);
    let first = t.elapsed();
    let t = Instant::now();
    let pixmap2 = renderer.render_new(&scene, &theme, w, h);
    let cached = t.elapsed();
    assert_eq!(pixmap.data(), pixmap2.data());
    pixmap.save_png(out).expect("writing PNG");
    eprintln!("{out}: {w}x{h}, first frame {first:?}, cached frame {cached:?}");
}
