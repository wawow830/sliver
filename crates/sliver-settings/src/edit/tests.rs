use super::*;
use sliver_config::{Context, Hardware, Width, load_str};

const HERE: Hardware = Hardware { physical_esc: true };

fn fresh(text: &str) -> (Editor, Table) {
    let ed = Editor::parse(text).unwrap();
    let merged = load_str(text, std::path::Path::new(".")).merged;
    (ed, merged)
}

fn resolved_ids(ed: &Editor, layer: &str, ctx: &Context) -> Vec<Vec<String>> {
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}\n{}", l.diagnostics, ed.text());
    let r = l.config.resolve(layer, ctx).unwrap();
    r.zones
        .iter()
        .map(|z| z.modules.iter().map(|m| m.id.to_string()).collect())
        .collect()
}

/// `layout`'s zones as the config resolves them: (name, module ids).
fn zones_of(ed: &Editor, layout: &str) -> Vec<(String, Vec<String>)> {
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}\n{}", l.diagnostics, ed.text());
    l.config
        .layout_zones(layout)
        .unwrap()
        .into_iter()
        .map(|z| {
            let ids = z.modules.iter().map(|m| m.id().to_string()).collect();
            (z.name, ids)
        })
        .collect()
}

fn names(ed: &Editor, layout: &str) -> Vec<String> {
    zones_of(ed, layout).into_iter().map(|(n, _)| n).collect()
}

/// Re-derive the merged table after an edit (the app reloads between operations too).
fn merged(ed: &Editor) -> Table {
    load_str(&ed.text(), std::path::Path::new(".")).merged
}

#[test]
fn zones_of_a_plain_layout() {
    let text = "version = 1\n[[layouts.mine.zones]]\nname = \"a\"\nmodules = [\"clock\"]\n\
                [[layouts.mine.zones]]\nname = \"b\"\nmodules = [\"battery\"]\n";
    let (mut ed, m) = fresh(text);
    add_zone(&mut ed, &m, "mine", "c").unwrap();
    assert_eq!(names(&ed, "mine"), ["a", "b", "c"]);
    let m = merged(&ed);
    move_zone(&mut ed, &m, "mine", "c", -1).unwrap();
    assert_eq!(names(&ed, "mine"), ["a", "c", "b"]);
    assert!(
        !ed.text().contains("zone-order"),
        "plain layouts reorder the list:\n{}",
        ed.text()
    );
    let m = merged(&ed);
    rename_zone(&mut ed, &m, "mine", "a", "left").unwrap();
    assert_eq!(
        zones_of(&ed, "mine")[0],
        ("left".into(), vec!["clock".into()])
    );
    let m = merged(&ed);
    set_zone_key(
        &mut ed,
        &m,
        "mine",
        "b",
        "width",
        Some(Width::Fill(2).to_value()),
    )
    .unwrap();
    set_zone_key(
        &mut ed,
        &m,
        "mine",
        "b",
        "align",
        Some(Value::String("end".into())),
    )
    .unwrap();
    let l = ed.load();
    let b = l
        .config
        .layout_zones("mine")
        .unwrap()
        .into_iter()
        .find(|z| z.name == "b")
        .unwrap();
    assert_eq!(
        (b.width, b.align),
        (Width::Fill(2), sliver_config::Align::End)
    );
    let m = merged(&ed);
    remove_zone(&mut ed, &m, "mine", "c").unwrap();
    assert_eq!(names(&ed, "mine"), ["left", "b"]);
}

#[test]
fn zones_of_the_builtin_default_layout() {
    let (mut ed, m) = fresh("version = 1\n");
    let builtin = names(&ed, "default");
    assert_eq!(builtin, ["esc", "main", "controls"]);
    add_zone(&mut ed, &m, "default", "extra").unwrap();
    assert_eq!(names(&ed, "default"), ["esc", "main", "controls", "extra"]);
    let m = merged(&ed);
    let controls = zones_of(&ed, "default")[2].1.clone();
    rename_zone(&mut ed, &m, "default", "controls", "ctl").unwrap();
    assert_eq!(
        zones_of(&ed, "default")[2],
        ("ctl".into(), controls),
        "modules kept"
    );
    let m = merged(&ed);
    move_zone(&mut ed, &m, "default", "extra", -1).unwrap();
    assert_eq!(names(&ed, "default"), ["esc", "main", "extra", "ctl"]);
    let m = merged(&ed);
    remove_zone(&mut ed, &m, "default", "extra").unwrap();
    assert_eq!(names(&ed, "default"), ["esc", "main", "ctl"]);
    assert!(ed.check(HERE).ok());
}

#[test]
fn zones_inherited_through_extends() {
    // `work` extends the built-in default and overrides only `main`.
    let text = "version = 1\n[layouts.work]\nextends = \"default\"\n\
                [[layouts.work.zones]]\nname = \"main\"\nmodules = [\"clock\"]\n";
    let (mut ed, m) = fresh(text);
    assert_eq!(names(&ed, "work"), ["esc", "main", "controls"]);
    // Removing an inherited zone hides it with zone-order; the parent is untouched.
    remove_zone(&mut ed, &m, "work", "controls").unwrap();
    assert_eq!(names(&ed, "work"), ["esc", "main"]);
    assert_eq!(names(&ed, "default"), ["esc", "main", "controls"]);
    // Renaming an inherited zone copies it (with its modules) under the new name.
    let m = merged(&ed);
    let esc = zones_of(&ed, "work")[0].1.clone();
    rename_zone(&mut ed, &m, "work", "esc", "left").unwrap();
    assert_eq!(zones_of(&ed, "work")[0], ("left".into(), esc));
    // Reordering writes zone-order.
    let m = merged(&ed);
    move_zone(&mut ed, &m, "work", "main", -1).unwrap();
    assert_eq!(names(&ed, "work"), ["main", "left"]);
    let m = merged(&ed);
    add_zone(&mut ed, &m, "work", "new").unwrap();
    assert_eq!(
        names(&ed, "work"),
        ["main", "left", "new"],
        "added to zone-order too"
    );
    assert_eq!(names(&ed, "default"), ["esc", "main", "controls"]);
}

#[test]
fn zone_names_and_limits() {
    let (mut ed, m) = fresh("version = 1\n");
    assert!(add_zone(&mut ed, &m, "default", "Bad Name").is_err());
    assert!(add_zone(&mut ed, &m, "default", "").is_err());
    assert!(
        add_zone(&mut ed, &m, "default", "main").is_err(),
        "duplicate"
    );
    assert!(
        rename_zone(&mut ed, &m, "default", "esc", "main").is_err(),
        "duplicate"
    );
    assert!(
        move_zone(&mut ed, &m, "default", "esc", -1).is_err(),
        "already first"
    );
    let text = "version = 1\n[[layouts.one.zones]]\nname = \"only\"\nmodules = []\n";
    let (mut ed, m) = fresh(text);
    assert!(
        remove_zone(&mut ed, &m, "one", "only").is_err(),
        "last zone stays"
    );
}

#[test]
fn editing_a_builtin_zone_keeps_the_others() {
    let (mut ed, merged) = fresh("version = 1\n# mine\n");
    add_module(&mut ed, &merged, "default", "main", "clock").unwrap();
    let ids = resolved_ids(&ed, "base", &Context::new());
    assert_eq!(
        ids.len(),
        3,
        "esc/main/controls all still there:\n{}",
        ed.text()
    );
    assert_eq!(ids[1], ["app-controls", "group/fkeys", "clock"]);
    assert!(ids[2].contains(&"mpris".to_owned()));
    assert!(ed.text().contains("# mine"));
    assert!(ed.check(HERE).ok());
}

#[test]
fn remove_and_move() {
    let (mut ed, merged) = fresh("version = 1\n");
    remove_module(&mut ed, &merged, "fkeys", "main", 0).unwrap();
    let merged = ed.load().merged;
    move_module(&mut ed, &merged, "fkeys", "main", 0, 1).unwrap();
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
    assert_eq!(&ids[1][..3], ["key/F3", "key/F2", "key/F4"]);
    let merged = ed.load().merged;
    assert!(move_module(&mut ed, &merged, "fkeys", "main", 0, -1).is_err());
}

#[test]
fn group_members_and_make_group() {
    let (mut ed, merged) = fresh("version = 1\n");
    let strip = "group/control-strip";
    let built_in = group_members(&ed, &merged, strip).unwrap();
    assert_eq!(built_in.len(), 6, "the built-in strip, before any edit");
    remove_group_member(&mut ed, &merged, strip, 2).unwrap();
    let merged = ed.load().merged;
    move_group_member(&mut ed, &merged, strip, 0, 1).unwrap();
    let merged = ed.load().merged;
    add_group_member(&mut ed, &merged, strip, "clock").unwrap();
    let merged = ed.load().merged;
    let m: Vec<String> = group_members(&ed, &merged, strip)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        m,
        [
            "kbd-backlight/slider",
            "custom/expand",
            "key/PreviousSong",
            "key/PlayPause",
            "key/NextSong",
            "clock"
        ]
    );
    assert!(move_group_member(&mut ed, &merged, strip, 5, 1).is_err());
    assert!(add_group_member(&mut ed, &merged, strip, strip).is_err());
    assert!(ed.check(HERE).ok());

    // Group F1, F3 and F5 of the fn layout's main zone.
    let merged = ed.load().merged;
    make_group(&mut ed, &merged, "fkeys", "main", &[4, 0, 2], "odd").unwrap();
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
    assert_eq!(&ids[1][..4], ["group/odd", "key/F2", "key/F4", "key/F6"]);
    assert!(make_group(&mut ed, &l.merged, "fkeys", "main", &[0], "odd").is_err());
    assert!(make_group(&mut ed, &l.merged, "fkeys", "main", &[], "x").is_err());
    assert!(make_group(&mut ed, &l.merged, "fkeys", "main", &[0], "a b").is_err());
}

#[test]
fn library_drop_inserts_at_the_caret() {
    let (mut ed, merged) = fresh("version = 1\n");
    insert_module(&mut ed, &merged, "fkeys", "main", Some(2), "clock").unwrap();
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
    assert_eq!(&ids[1][..4], ["key/F1", "key/F2", "clock", "key/F3"]);
    let merged = ed.load().merged;
    insert_module(&mut ed, &merged, "fkeys", "main", Some(99), "battery").unwrap();
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
    assert_eq!(
        ids[1].last().map(String::as_str),
        Some("battery"),
        "past the end = last"
    );
}

#[test]
fn drag_and_drop_moves() {
    let (mut ed, merged) = fresh("version = 1\n");
    // F1 dropped after F3 (caret position 3, counted before the move).
    move_module_to(&mut ed, &merged, "fkeys", "main", 0, "main", 3).unwrap();
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
    assert_eq!(&ids[1][..4], ["key/F2", "key/F3", "key/F1", "key/F4"]);
    let merged = ed.load().merged;
    assert!(
        move_module_to(&mut ed, &merged, "fkeys", "main", 0, "main", 1).is_err(),
        "no-op"
    );
    // Across zones: F12 into the esc zone, first.
    move_module_to(&mut ed, &merged, "fkeys", "main", 11, "esc", 0).unwrap();
    let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", false));
    assert_eq!(ids[0], ["key/F12", "key/Esc#auto"]);
    assert_eq!(ids[1].len(), 11);
}

#[test]
fn inherited_zone_becomes_an_override() {
    let (mut ed, merged) = fresh("version = 1\n[layouts.ff]\nextends = \"default\"\n");
    add_module(&mut ed, &merged, "ff", "controls", "clock").unwrap();
    let t = ed.text();
    assert!(
        t.contains("[[layouts.ff.zones]]\nname = \"controls\""),
        "{t}"
    );
    let l = ed.load();
    let zones = l.config.layout_zones("ff").unwrap();
    assert_eq!(
        zones.iter().map(|z| z.name.as_str()).collect::<Vec<_>>(),
        ["esc", "main", "controls"]
    );
    assert_eq!(zones[2].modules.last().unwrap().id().to_string(), "clock");
    assert_eq!(
        l.config.layout_zones("default").unwrap()[2].modules.len(),
        6,
        "parent untouched"
    );
}

#[test]
fn module_keys_named_and_inline() {
    let src = "version = 1\n[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"clock\", { type = \"custom\", format = \"hi\" }]\n";
    let (mut ed, merged) = fresh(src);
    set_module_key(
        &mut ed,
        &merged,
        "default",
        "main",
        0,
        "format",
        Some(Value::String("{:%H}".into())),
    )
    .unwrap();
    set_module_key(
        &mut ed,
        &merged,
        "default",
        "main",
        1,
        "format",
        Some(Value::String("yo".into())),
    )
    .unwrap();
    let merged = ed.load().merged;
    set_module_key(
        &mut ed,
        &merged,
        "default",
        "main",
        1,
        "on-tap",
        Some(Value::String("notify-send hi".into())),
    )
    .unwrap();
    assert_eq!(
        ed.get("modules.clock.format").unwrap(),
        Some(Value::String("{:%H}".into()))
    );
    assert_eq!(
        ed.get("layouts.default.zones[0].modules[1].format")
            .unwrap(),
        Some(Value::String("yo".into()))
    );
    let merged = ed.load().merged;
    set_module_key(&mut ed, &merged, "default", "main", 0, "format", None).unwrap();
    assert_eq!(ed.get("modules.clock.format").unwrap(), None);
    assert!(ed.check(HERE).ok(), "{:#?}", ed.check(HERE));
}

#[test]
fn rules_crud_keeps_order_and_comments() {
    let src = "version = 1\n# my rules\n[[rules]]\nname = \"locked\"\nwhen = { locked = true }\nlayout = \"fkeys\"\n";
    let (mut ed, merged) = fresh(src);
    let mut r = Table::new();
    r.insert("name".into(), "terminals".into());
    r.insert(
        "when".into(),
        "v = { app-id = [\"foot\", \"kitty\"] }"
            .parse::<Table>()
            .unwrap()["v"]
            .clone(),
    );
    r.insert("layout".into(), "fkeys".into());
    add_rule(&mut ed, &merged, r).unwrap();
    let names = |ed: &Editor| {
        ed.load()
            .config
            .rules
            .iter()
            .map(|r| r.name.clone().unwrap_or_default())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&ed),
        ["locked", "terminals"],
        "new rules go below the lock screen"
    );
    let merged = ed.load().merged;
    move_rule(&mut ed, &merged, 1, -1).unwrap();
    assert_eq!(
        names(&ed),
        ["terminals", "locked"],
        "but can be moved by hand"
    );
    let merged = ed.load().merged;
    set_rule_key(&mut ed, &merged, 0, "layout", Some("media".into())).unwrap();
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
    let ctx = Context::new().with("app_id", "kitty");
    assert_eq!(l.config.resolve("base", &ctx).unwrap().layout, "media");
    assert!(ed.text().contains("# my rules"));
    remove_rule(&mut ed, &l.merged, 0).unwrap();
    let merged = ed.load().merged;
    remove_rule(&mut ed, &merged, 0).unwrap();
    assert!(ed.load().config.rules.is_empty());
    assert!(ed.check(HERE).ok());
    let merged = ed.load().merged;
    assert!(remove_rule(&mut ed, &merged, 0).is_err());
}

#[test]
fn move_rule_down_with_comments() {
    let src = "version = 1\n\n[[rules]]\nname = \"New rule\"\nlayout = \"default\"\nwhen = { app-id = \"change-me\" }\n# terminals want F-keys\n[[rules]]\nname = \"Terminals\"\nwhen = { app-id = [\"foot\", \"kitty\"] }\nlayout = \"fkeys\"\n";
    let (mut ed, merged) = fresh(src);
    move_rule(&mut ed, &merged, 0, 1).unwrap();
    let names: Vec<_> = ed
        .load()
        .config
        .rules
        .iter()
        .map(|r| r.name.clone().unwrap())
        .collect();
    assert_eq!(names, ["Terminals", "New rule"], "{}", ed.text());
}

#[test]
fn customize_for_app_adds_layout_and_rule_first() {
    let (mut ed, merged) =
        fresh("version = 1\n[[rules]]\nwhen = { locked = true }\nlayout = \"fkeys\"\n");
    let (name, _) = customize_for_app(&mut ed, &merged, "org.mozilla.firefox", "default").unwrap();
    assert_eq!(name, "firefox");
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
    assert_eq!(
        l.config.rules[0].layout, "fkeys",
        "the lock-screen rule stays first"
    );
    assert_eq!(l.config.rules[1].layout, "firefox");
    let ctx = Context::new().with("app_id", "org.mozilla.firefox");
    assert_eq!(l.config.resolve("base", &ctx).unwrap().layout, "firefox");
    let locked = ctx.clone().with("locked", true);
    assert_eq!(l.config.resolve("base", &locked).unwrap().layout, "fkeys");
    // Doing it twice doesn't duplicate the rule.
    customize_for_app(&mut ed, &l.merged, "org.mozilla.firefox", "default").unwrap();
    assert_eq!(ed.load().config.rules.len(), 2);
    assert!(customize_for_app(&mut ed, &l.merged, "  ", "default").is_err());

    // With no rules of the user's own, the built-in lock-screen rule is copied along, first.
    let (mut ed, merged) = fresh("version = 1\n");
    customize_for_app(&mut ed, &merged, "foot", "default").unwrap();
    let l = ed.load();
    let layouts: Vec<&str> = l.config.rules.iter().map(|r| r.layout.as_str()).collect();
    assert_eq!(layouts, ["locked", "foot"]);
    let ctx = Context::new().with("app_id", "foot").with("locked", true);
    assert_eq!(l.config.resolve("base", &ctx).unwrap().layout, "locked");
}

#[test]
fn fkeys_can_be_put_back() {
    // an Fn layer with only some F-keys
    let src = "version = 1\n[layers]\nfn = \"mine\"\n[[layouts.mine.zones]]\nname = \"main\"\nmodules = [\"key/F1\", \"key/F2\"]\n";
    let (mut ed, merged) = fresh(src);
    let config = ed.load().config;
    let (layout, missing) = missing_fkeys(&config).unwrap();
    assert_eq!(layout, "mine");
    assert_eq!(missing.len(), 10);
    assert_eq!(missing[0], "key/F3");
    add_keys(&mut ed, &merged, &layout, &missing).unwrap();
    let l = ed.load();
    assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
    assert_eq!(missing_fkeys(&l.config).unwrap().1.len(), 0);
    // the built-in Fn layer is complete already
    let (ed2, _) = fresh("version = 1\n");
    assert!(missing_fkeys(&ed2.load().config).unwrap().1.is_empty());
}

#[test]
fn block_changes_only_touches_what_differs() {
    let t = |s: &str| s.parse::<Table>().unwrap();
    let old = t("format = \"{}\"\non-tap = \"a\"\ninterval = 5");
    let new = t("format = \"{}\"\non-tap = \"b\"\nicon = \"x\"");
    assert_eq!(
        block_changes(&old, &new),
        vec![
            ("icon".into(), Some(Value::String("x".into()))),
            ("interval".into(), None),
            ("on-tap".into(), Some(Value::String("b".into()))),
        ]
    );
    assert!(block_changes(&old, &old).is_empty());
}

#[test]
fn named_module_keys_for_group_members() {
    let (mut ed, _) = fresh("version = 1\n");
    set_named_module_key(
        &mut ed,
        "custom/b",
        "format",
        Some(Value::String("B".into())),
    )
    .unwrap();
    assert_eq!(
        ed.get("modules.\"custom/b\".format").unwrap(),
        Some(Value::String("B".into()))
    );
    set_named_module_key(&mut ed, "custom/b", "format", None).unwrap();
    assert_eq!(ed.get("modules.\"custom/b\".format").unwrap(), None);
    assert!(set_named_module_key(&mut ed, "", "format", None).is_err());
}

#[test]
fn an_alias_moves_to_the_canonical_key() {
    let (mut ed, merged) = fresh(
        "version = 1\n[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"clock\"]\n\
         [modules.clock]\non-click = \"gnome-clocks\"\n",
    );
    let changes: Vec<(String, Option<Value>)> = vec![
        ("on-tap".into(), Some(Value::String("foot".into()))),
        ("on-click".into(), None),
    ];
    for (k, v) in changes {
        set_module_key(&mut ed, &merged, "default", "main", 0, &k, v).unwrap();
    }
    assert_eq!(
        ed.get("modules.clock.on-tap").unwrap(),
        Some(Value::String("foot".into()))
    );
    assert_eq!(ed.get("modules.clock.on-click").unwrap(), None);
    assert!(ed.check(HERE).ok(), "{:#?}", ed.check(HERE));
}
