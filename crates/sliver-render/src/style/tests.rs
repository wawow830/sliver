use super::*;

fn sheet(css: &str) -> Stylesheet {
    let (s, d) = Stylesheet::parse("t.css", css, None);
    assert!(d.is_empty(), "{d:?}");
    s
}

struct Tree {
    bar: Vec<String>,
    zone: Vec<String>,
    module: Vec<String>,
}

fn classes(s: &[&str]) -> Vec<String> {
    s.iter().map(|c| c.to_string()).collect()
}

/// Computes bar, zone and module styles for a 3-level path.
fn styles(sheet: &Stylesheet, t: &Tree, module_id: &str, active: bool) -> [Style; 3] {
    let bar = Element {
        tag: "bar",
        id: None,
        classes: &t.bar,
        active: false,
        disabled: false,
        checked: false,
        first: true,
        last: true,
    };
    let zone = Element {
        tag: "zone",
        id: None,
        classes: &t.zone,
        ..bar
    };
    let module = Element {
        tag: "module",
        id: Some(module_id),
        classes: &t.module,
        active,
        ..bar
    };
    let b = sheet.compute(&[bar], None);
    let z = sheet.compute(&[bar, zone], Some(&b));
    let m = sheet.compute(&[bar, zone, module], Some(&z));
    [b, z, m]
}

fn tree(module: &[&str]) -> Tree {
    Tree {
        bar: classes(&[]),
        zone: classes(&[]),
        module: classes(module),
    }
}

#[test]
fn cascade_specificity_order_important() {
    let s = sheet(
        "module { color: red; background: #111 }
         #battery { color: lime }
         module.critical { background: #222 }
         module { color: blue }
         module { background: #333 !important }",
    );
    let [_, _, m] = styles(&s, &tree(&["critical"]), "battery", false);
    assert_eq!(m.color, Color::rgb(0, 255, 0), "id beats later type rule");
    assert_eq!(
        m.background,
        Color::rgb(0x33, 0x33, 0x33),
        "!important wins"
    );
    let [_, _, m] = styles(&s, &tree(&[]), "clock", false);
    assert_eq!(
        m.color,
        Color::rgb(0, 0, 255),
        "later rule wins at equal specificity"
    );
}

#[test]
fn inheritance_and_em() {
    let s = sheet(
        "bar { color: #abc; font-size: 20px; background: red; padding: 2px; font-family: Inter, sans }
         zone { font-size: 1.5em; padding: inherit }
         module { padding: 0 1em; border: 2px solid; }",
    );
    let [b, z, m] = styles(&s, &tree(&[]), "x", false);
    assert_eq!(b.background, Color::rgb(255, 0, 0));
    assert_eq!(
        z.background,
        Color::TRANSPARENT,
        "background isn't inherited"
    );
    assert_eq!(z.padding, [2.0; 4], "explicit inherit");
    assert_eq!(m.color, Color::rgb(0xaa, 0xbb, 0xcc), "color is inherited");
    assert_eq!(m.font_size, 30.0);
    assert_eq!(m.padding, [0.0, 30.0, 0.0, 30.0], "em uses own font size");
    assert_eq!(
        (m.border_width, m.border_color()),
        (2.0, m.color),
        "currentColor border"
    );
    assert_eq!(m.font_family, ["Inter", "sans"]);
}

#[test]
fn active_and_bar_classes() {
    let s = sheet(
        "@define-color accent #3584e4;
         module:active { background: alpha(@accent, 0.5) }
         bar.layer-fn module { color: @accent }",
    );
    let mut t = tree(&[]);
    let [_, _, m] = styles(&s, &t, "x", true);
    assert_eq!(m.background, Color::rgba(0x35, 0x84, 0xe4, 128));
    assert_eq!(m.color, Color::WHITE);
    t.bar = classes(&["layer-fn"]);
    let [_, _, m] = styles(&s, &t, "x", false);
    assert_eq!(m.color, Color::rgb(0x35, 0x84, 0xe4));
}

#[test]
fn diagnostics_skip_bad_parts_only() {
    let (s, d) = Stylesheet::parse(
        "user.css",
        "module {\n  colour: red;\n  color: nope;\n  background: url(x.png);\n  padding: 4px;\n}\n\
         module:hover, #ok { color: @missing; font-size: 10px }\n\
         a + b { color: red }\n\
         @media (x) { a { } }",
        None,
    );
    let d: Vec<String> = d.iter().map(|d| d.to_string()).collect();
    assert_eq!(
        d,
        [
            "user.css:2:3: unsupported property 'colour' (ignored)",
            "user.css:3:3: color: expected a color, got 'nope'",
            "user.css:4:3: background: only colors are supported, got 'url(x.png)'",
            "user.css:7:21: color: unknown color '@missing' (define it with @define-color)",
            "user.css:7:1: ':hover' never matches on the Touch Bar",
            "user.css:8:1: selector 'a + b': sibling combinators ('+', '~') are not supported",
            "user.css:9:1: '@media' is not supported (ignored)",
        ]
    );
    let [_, _, m] = styles(&s, &tree(&[]), "ok", false);
    assert_eq!(
        m.padding, [4.0; 4],
        "valid declarations in a rule with errors still apply"
    );
    assert_eq!(m.font_size, 10.0);
}

#[test]
fn imports_files_and_builtins() {
    let dir = std::env::temp_dir().join(format!("sliver-style-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("themes")).unwrap();
    std::fs::write(
        dir.join("themes/colors.css"),
        "@define-color fg #102030;\n@import \"missing.css\";",
    )
    .unwrap();
    std::fs::write(
        dir.join("style.css"),
        "@import \"sliver:classic\";\n@import \"themes/colors.css\";\nmodule { color: @fg }\n@import \"sliver:nope\";",
    )
    .unwrap();
    let (s, d) = Stylesheet::load(&dir.join("style.css")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d[0].message.contains("missing.css"), "{d:?}");
    assert!(d[0].source.ends_with("themes/colors.css"));
    assert!(
        d[1].message
            .contains("no built-in theme 'nope' (have: sliver, light, high-contrast, classic)")
    );
    assert_eq!(s.files().len(), 2);
    let [b, _, m] = styles(&s, &tree(&[]), "x", false);
    assert_eq!(b.font_size, 32.0, "classic applied");
    assert_eq!(m.color, Color::rgb(0x10, 0x20, 0x30));
    assert_eq!(m.background, Color::rgb(0x33, 0x33, 0x33));
}

#[test]
fn builtin_themes_are_clean() {
    for (name, text) in BUILTIN_THEMES {
        let (_, d) = Stylesheet::parse(name, text, None);
        assert!(d.is_empty(), "{name}: {d:?}");
    }
}

#[test]
fn predefined_colors_and_disabled() {
    let mut s = Stylesheet::new();
    s.define_color("accent", Color::rgb(1, 2, 3));
    let mut d = Vec::new();
    s.add_str(
        "user.css",
        "module { color: @accent; transition: background 200ms }\nmodule:disabled { opacity: 0.5 }",
        None,
        &mut d,
    );
    assert!(d.is_empty(), "{d:?}");
    let bar = Element {
        tag: "bar",
        id: None,
        classes: &[],
        active: false,
        disabled: false,
        checked: false,
        first: true,
        last: true,
    };
    let m = Element {
        tag: "module",
        disabled: true,
        ..bar
    };
    let st = s.compute(&[bar, m], None);
    assert_eq!((st.color, st.opacity), (Color::rgb(1, 2, 3), 0.5));
}
