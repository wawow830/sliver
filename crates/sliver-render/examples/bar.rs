//! Renders a bar to PNG from the command line, for quick looks and mockups.
//!
//! ```text
//! cargo run -p sliver-render --example bar -- out.png esc F1 F2 icon:play_pause x3:Hello! - _
//! cargo run -p sliver-render --example bar -- out.png esc '|' icon:play_pause 'Song' '|' 12:34
//! ```
//! Each item is a label, `icon:NAME` (built-in) or `icon:/path/to.svg`, `-` (spacer) or `_` (blank
//! button). Items are `Auto` width; prefix `xN:` for `Fill(N)` or `pN:` for `Fixed(N px)`. A `!`
//! suffix shows it pressed. `|` splits Esc | Main | Controls zones. `--system-fonts` uses
//! installed fonts (with fallback) instead of only the embedded one. `--size WxH` sets the bar size.
//! `--style FILE.css` uses a stylesheet (default: the built-in classic theme). An item may be
//! followed by `#id` and `.class`es, e.g. `icon:bolt#battery.charging`.

use std::time::Instant;

use sliver_render::{Content, Fonts, Icon, Item, Renderer, Scene, Stylesheet, Width};

fn parse_item(arg: &str) -> Item {
    // Trailing `#id.class.class` selectors (not inside paths).
    let (arg, sel) = match arg.rfind('#').filter(|&i| !arg[i..].contains('/') && i > 0) {
        Some(i) => (&arg[..i], &arg[i + 1..]),
        None => (arg, ""),
    };
    let mut sel = sel.split('.');
    let id = sel.next().filter(|s| !s.is_empty());
    let classes: Vec<&str> = sel.collect();
    let mut item = parse_content(arg);
    item.id = id.map(str::to_string);
    item.classes = classes.into_iter().map(str::to_string).collect();
    item
}

fn parse_content(arg: &str) -> Item {
    let (width, rest) = match arg.split_once(':') {
        Some((n, rest)) if n.starts_with('x') && n[1..].parse::<u32>().is_ok() => {
            (Width::Fill(n[1..].parse().unwrap()), rest)
        }
        Some((n, rest)) if n.starts_with('p') && n[1..].parse::<f32>().is_ok() => {
            (Width::Fixed(n[1..].parse().unwrap()), rest)
        }
        _ => (Width::Auto, arg),
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
    Item::new(content).width(width).pressed(pressed)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let system = args.iter().any(|a| a == "--system-fonts");
    args.retain(|a| a != "--system-fonts");
    let mut sheet = Stylesheet::classic();
    if let Some(i) = args.iter().position(|a| a == "--style") {
        let path = args.remove(i + 1);
        args.remove(i);
        let (s, diags) = Stylesheet::load(path.as_ref()).expect("reading stylesheet");
        diags.iter().for_each(|d| eprintln!("{d}"));
        sheet = s;
    }
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
    let zones: Vec<Vec<Item>> = items
        .split(|a| a == "|")
        .map(|z| z.iter().map(|a| parse_item(a)).collect())
        .collect();
    let scene = match <[_; 3]>::try_from(zones) {
        Ok([esc, main, controls]) => Scene::zoned(esc, main, controls),
        Err(zones) if zones.len() == 1 => Scene::new(zones.into_iter().next().unwrap()),
        Err(_) => panic!("use 0 or 2 '|' separators (Esc | Main | Controls)"),
    };
    let fonts = if system {
        Fonts::system()
    } else {
        Fonts::embedded()
    };
    let mut renderer = Renderer::new(fonts);
    let t = Instant::now();
    let pixmap = renderer.render_new(&scene, &sheet, w, h);
    let first = t.elapsed();
    let t = Instant::now();
    let pixmap2 = renderer.render_new(&scene, &sheet, w, h);
    let cached = t.elapsed();
    assert_eq!(pixmap.data(), pixmap2.data());
    pixmap.save_png(out).expect("writing PNG");
    eprintln!("{out}: {w}x{h}, first frame {first:?}, cached frame {cached:?}");
}
