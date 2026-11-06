use super::*;
use chrono::TimeZone;
use sliver_config::{Context, load_str};

fn world() -> World {
    World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap())
}

fn build(toml: &str, layer: &str) -> Vec<Cell> {
    let loaded = load_str(toml, std::path::Path::new("."));
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let ctx = Context::new().with("physical_esc", true);
    let r = loaded.config.resolve(layer, &ctx).unwrap();
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: &world(),
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    out
}

#[test]
fn fkeys_are_key_regions() {
    let cells = build("", "fn");
    assert_eq!(cells.len(), 12, "Esc is hidden with a physical Esc");
    assert_eq!(cells[0].touch, Touch::Keys(vec![59]));
    assert!(matches!(&cells[0].look.content, Content::Label { text: t, .. } if t == "F1"));
    assert_eq!(cells[0].look.id.as_deref(), Some("key-f1"));
    assert_eq!(cells[0].look.width, Width::Fill(1));
}

/// What the agent's loop would read for `toml`'s modules from the fixture tree `dir`
/// (`proc/`, `sys/`).
fn read_stats(toml: &str, dir: &std::path::Path) -> crate::stats::Readings {
    let config = load_str(toml, std::path::Path::new(".")).config;
    crate::stats::Sampler::with_roots(dir.join("proc"), dir.join("sys"))
        .readings(&crate::stats::wants(&config), std::time::Instant::now())
}

fn stat_cells(toml: &str, w: &World) -> Vec<Cell> {
    let loaded = load_str(toml, std::path::Path::new("."));
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let r = loaded.config.resolve("base", &Context::new()).unwrap();
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: w,
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    out
}

#[test]
fn clock_time_zones_and_locale() {
    let toml = r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["clock", "clock#de", "clock#bad"]
        [modules.clock]
        format = "{:%H:%M} {timezone}"
        timezones = ["", "Asia/Tokyo"]
        [modules."clock#de"]
        format = "{:%A}"
        locale = "de_DE.UTF-8"
        [modules."clock#bad"]
        timezone = "Mars/Olympus"
        "#;
    let loaded = load_str(toml, std::path::Path::new("."));
    let r = loaded.config.resolve("base", &Context::new()).unwrap();
    let at = |tz: i32| {
        let mut out = Vec::new();
        let ui = move |_: &str| UiState {
            tz,
            ..UiState::default()
        };
        let mut icons = Icons::builtin_only();
        cells(
            &r.zones[0].modules,
            &mut Build {
                world: &world(),
                icons: &mut icons,
                icon_size: 36,
                ui: &ui,
            },
            &mut out,
        );
        out.iter()
            .map(|c| match &c.look.content {
                Content::Label { text: t, .. } => t.clone(),
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let now = world().now.wall;
    let tokyo = now.with_timezone(&chrono_tz::Asia::Tokyo).format("%H:%M");
    assert_eq!(at(0)[0], format!("{} local", now.format("%H:%M")));
    assert_eq!(at(1)[0], format!("{tokyo} Asia/Tokyo"));
    assert_eq!(at(-1)[0], at(1)[0], "swiping back wraps around");
    assert_eq!(at(0)[1], "Montag", "2026-10-12 in German");
    assert!(at(0)[2].contains("unknown time zone"), "{:?}", at(0)[2]);

    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    cells(
        &r.zones[0].modules,
        &mut Build {
            world: &world(),
            icons: &mut icons,
            icon_size: 36,
            ui: &|_| UiState::default(),
        },
        &mut out,
    );
    let swipes = |c: &Cell| {
        c.on.iter()
            .filter(|(g, _)| *g == Gesture::SwipeLeft)
            .count()
    };
    assert_eq!(
        (swipes(&out[0]), swipes(&out[1])),
        (1, 0),
        "only a list cycles"
    );
}

#[test]
fn window_title_rewrite() {
    let mut w = World::sample(world().now.wall);
    w.desktop.window.as_mut().unwrap().title = "Docs <b> - Mozilla Firefox".into();
    let cells = stat_cells(
        r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["window", "window#plain"]
        [modules.window]
        rewrite = [{ match = "(.*) - Mozilla Firefox", replace = "web: $1" }]
        "#,
        &w,
    );
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        text(&cells[0]),
        "web: Docs &lt;b&gt;",
        "rewritten, still escaped"
    );
    assert_eq!(text(&cells[1]), "Docs &lt;b&gt; - Mozilla Firefox");
}

#[test]
fn stats_modules_read_files_and_schedule_their_own_refresh() {
    let dir = std::env::temp_dir().join(format!("sliver-agent-stats-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let put = |rel: &str, text: &str| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    put("proc/stat", "cpu  100 0 100 800 0 0 0 0 0 0\n");
    put(
        "proc/meminfo",
        "MemTotal: 16777216 kB\nMemAvailable: 4194304 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
    );
    put("proc/loadavg", "1.50 1.00 0.50 1/100 1\n");
    put("sys/class/hwmon/hwmon0/name", "chip\n");
    put("sys/class/hwmon/hwmon0/temp1_input", "61500\n");
    let mut w = world();
    let toml = r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["cpu", "memory", "load", "temperature", "temperature#hot", "disk", "cpu#fast"]
        [modules.cpu]
        format = "{usage}% @{avg_frequency}"
        [modules.memory]
        format = "{used}/{total}G {percentage}%"
        [modules.load]
        format = "L {load1}/{load5}"
        [modules."temperature#hot"]
        critical-threshold = 60
        [modules.disk]
        path = "/"
        [modules."cpu#fast"]
        interval = "2s"
        "#;
    w.stats = read_stats(toml, &dir);
    let cells = stat_cells(toml, &w);
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(text(&cells[0]), "20% @0");
    assert_eq!(text(&cells[1]), "12/16G 75%");
    assert_eq!(text(&cells[2]), "L 1.5/1");
    assert_eq!(text(&cells[3]), "62°C");
    assert!(!cells[3].look.classes.contains(&"critical".to_string()));
    assert!(
        cells[4].look.classes.contains(&"critical".to_string()),
        "61.5 >= 60: {:?}",
        cells[4].look.classes
    );
    assert!(text(&cells[5]).ends_with('%'));
    // refresh: the module's interval, else its default; the clock-like wake-up the loop uses
    let due = |c: &Cell| (c.next_change.unwrap() - w.now.wall).num_seconds();
    assert_eq!(
        (due(&cells[0]), due(&cells[1]), due(&cells[6])),
        (10, 30, 2)
    );
    // with no data source (no /proc here) the module hides instead of showing zeros
    let mut empty = world();
    let toml = "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"cpu\", \"memory\", \"temperature\"]\n";
    empty.stats = read_stats(toml, std::path::Path::new("/nonexistent"));
    let none = stat_cells(toml, &empty);
    assert!(none.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn network_bandwidth_and_cpu_sparkline() {
    let dir = std::env::temp_dir().join(format!("sliver-agent-bw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("proc/net")).unwrap();
    std::fs::write(dir.join("proc/stat"), "cpu  100 0 100 800 0 0 0 0 0 0\n").unwrap();
    std::fs::write(
        dir.join("proc/net/dev"),
        " eth0: 1000 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n",
    )
    .unwrap();
    let mut w = world();
    w.session.network.state = sliver_session::network::NetState::Connected;
    w.session.network.kind = sliver_session::network::NetKind::Ethernet;
    w.session.network.iface = Some("eth0".into());
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => panic!("{other:?}"),
    };
    let toml = |extra: &str| {
        format!(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"network\", \"cpu\"]\n[modules.network]\nformat = \"{{bandwidthDownBytes}} {{bandwidthUpBits}}\"\n{extra}[modules.cpu]\nformat = \"{{spark}}\"\n"
        )
    };
    // no `interval`: not sampled, no wake-ups
    let mut sampler = crate::stats::Sampler::with_roots(dir.join("proc"), dir.join("sys"));
    let mut read = |toml: &str| {
        let config = load_str(toml, std::path::Path::new(".")).config;
        sampler.readings(&crate::stats::wants(&config), w.now.instant)
    };
    w.stats = read(&toml(""));
    let cells = stat_cells(&toml(""), &w);
    assert!(cells[0].next_change.is_none());
    assert_eq!(text(&cells[0]), "0B/s 0b/s");
    assert_eq!(text(&cells[1]).chars().count(), 1, "one cpu sample so far");
    // with one: the first read primes the counters
    w.stats = read(&toml("interval = 2\n"));
    let cells = stat_cells(&toml("interval = 2\n"), &w);
    assert_eq!(
        (cells[0].next_change.unwrap() - w.now.wall).num_seconds(),
        2
    );
    assert_eq!(text(&cells[0]), "0B/s 0b/s");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspaces_and_taskbar_follow_the_compositor_lists() {
    use sliver_desktop::{WindowInfo, WorkspaceInfo};
    let ws = |id: i64, name: &str, windows: u32, active: bool, mon: &str| WorkspaceInfo {
        id,
        name: name.into(),
        active,
        urgent: false,
        monitor: Some(mon.into()),
        windows,
    };
    let win = |addr: &str, class: &str, ws: i64, focused: bool, urgent: bool| WindowInfo {
        address: addr.into(),
        class: class.into(),
        title: format!("{class} window"),
        workspace: ws,
        focused,
        urgent,
        pid: None,
    };
    let mut w = world();
    let d = &mut w.desktop;
    d.connected = true;
    d.monitor = Some("eDP-1".into());
    d.workspace = Some(sliver_desktop::Workspace {
        id: 2,
        name: "web".into(),
    });
    d.workspaces = vec![
        ws(1, "1", 1, false, "eDP-1"),
        ws(2, "web", 2, true, "eDP-1"),
        ws(3, "3", 0, false, "eDP-1"),
        ws(4, "ext", 1, false, "HDMI-A-1"),
    ];
    d.windows = vec![
        win("a1", "kitty", 1, false, false),
        win("b2", "firefox", 2, true, false),
        win("c3", "mpv", 2, false, true),
    ];
    let cells = stat_cells(
        r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["workspaces", "taskbar"]
        [modules.workspaces]
        format = "{name}:{windows}"
        "#,
        &w,
    );
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => format!("{other:?}"),
    };
    let wsc: Vec<&Cell> = cells.iter().filter(|c| c.kind == "workspaces").collect();
    assert_eq!(
        wsc.iter().map(|c| text(c)).collect::<Vec<_>>(),
        ["1:1", "web:2", "3:0"],
        "other output hidden"
    );
    assert!(wsc[1].look.checked && wsc[1].look.classes.contains(&"active".to_string()));
    assert!(wsc[2].look.classes.contains(&"empty".to_string()));
    assert_eq!(wsc[0].touch, Touch::Agent);
    assert_eq!(wsc[1].key, "workspaces/2");
    assert!(matches!(
        wsc[0].binding(Gesture::Tap),
        Some(Do::Agent(Act::Desktop(sliver_desktop::control::Action::FocusWorkspace(x)))) if x.id == 1
    ));
    let tb: Vec<&Cell> = cells.iter().filter(|c| c.kind == "taskbar").collect();
    assert_eq!(tb.len(), 2, "only the active workspace's windows");
    assert!(tb[0].look.classes.contains(&"focused".to_string()) && tb[0].look.checked);
    assert!(tb[1].look.classes.contains(&"urgent".to_string()));
    // no icon theme in tests: the app's first letters stand in
    assert_eq!(text(tb[0]), "fi");
    assert!(matches!(
        tb[0].binding(Gesture::Hold),
        Some(Do::Agent(Act::Desktop(
            sliver_desktop::control::Action::CloseWindow { .. }
        )))
    ));
    // no compositor list (no backend): the modules show nothing
    let none = stat_cells(
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"workspaces\", \"taskbar\"]\n",
        &world(),
    );
    assert!(none.is_empty());
}

#[test]
fn notifications_and_dnd() {
    let toml =
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"notifications\", \"dnd\"]\n";
    let mut w = world();
    let c = stat_cells(toml, &w);
    assert_eq!(
        c.len(),
        1,
        "dnd is hidden while the daemon doesn't report it"
    );
    assert!(
        matches!(&c[0].look.content, Content::Label { text, .. } if text.is_empty()),
        "no text at zero: {:?}",
        c[0].look.content
    );
    w.session.notifications.count = 3;
    w.session.notifications.dnd = Some(true);
    let c = stat_cells(toml, &w);
    assert_eq!(c.len(), 2);
    assert!(
        matches!(&c[0].look.content, Content::Label { text: t, .. } if t == "3"),
        "{:?}",
        c[0].look.content
    );
    assert!(
        c[0].look.classes.contains(&"dnd".to_string())
            && c[0].look.classes.contains(&"pending".to_string())
    );
    assert!(c[1].look.checked);
}

#[test]
fn network_and_bluetooth_modules() {
    use sliver_session::bluetooth::BtDevice;
    use sliver_session::network::{NetKind, NetState};
    let toml =
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"network\", \"bluetooth\"]\n";
    let mut w = world();
    assert!(
        stat_cells(toml, &w).is_empty(),
        "no NetworkManager / BlueZ: nothing to show"
    );
    let n = &mut w.session.network;
    n.state = NetState::Connected;
    n.kind = NetKind::Wifi;
    n.name = "Home".into();
    n.ssid = Some("Home <5G>".into());
    n.signal_pct = Some(70);
    n.iface = Some("wlan0".into());
    w.session.bluetooth.available = true;
    w.session.bluetooth.powered = true;
    w.session.bluetooth.connected = vec![BtDevice {
        name: "Buds".into(),
        address: "x".into(),
        battery: Some(55),
    }];
    let c = stat_cells(toml, &w);
    assert_eq!(c.len(), 2);
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => format!("{other:?}"),
    };
    assert_eq!(
        text(&c[0]),
        "Home &lt;5G&gt;",
        "an SSID cannot inject markup"
    );
    assert!(c[0].look.classes.contains(&"wifi".to_string()));
    assert_eq!(text(&c[1]), "Buds");
    assert!(c[1].look.checked && c[1].look.classes.contains(&"connected".to_string()));
    w.session.network.state = NetState::Disconnected;
    w.session.bluetooth.powered = false;
    let c = stat_cells(toml, &w);
    assert!(c[0].look.classes.contains(&"disconnected".to_string()));
    assert!(c[1].look.classes.contains(&"off".to_string()));
}

#[test]
fn taskbar_pinned_apps() {
    use sliver_desktop::WindowInfo;
    let dir = std::env::temp_dir().join(format!("sliver-pinned-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("org.mozilla.firefox.desktop"),
        "[Desktop Entry]\nType=Application\nName=Firefox\nIcon=firefox\nStartupWMClass=firefox\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("kitty.desktop"),
        "[Desktop Entry]\nType=Application\nName=kitty\nIcon=kitty\n",
    )
    .unwrap();
    let mut w = world();
    w.apps = std::sync::Arc::new(sliver_icons::desktop::DesktopEntries::scan(
        std::slice::from_ref(&dir),
    ));
    w.desktop.connected = true;
    w.desktop.workspace = Some(sliver_desktop::Workspace {
        id: 1,
        name: "1".into(),
    });
    let win = |addr: &str, class: &str, focused: bool| WindowInfo {
        address: addr.into(),
        class: class.into(),
        title: class.into(),
        workspace: 1,
        focused,
        ..Default::default()
    };
    w.desktop.windows = vec![win("a1", "mpv", false), win("b2", "firefox", true)];
    let cells = stat_cells(
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"taskbar\"]\n[modules.taskbar]\npinned = [\"org.mozilla.firefox.desktop\", \"kitty\"]\n",
        &w,
    );
    let keys: Vec<&str> = cells.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(
        keys,
        ["taskbar/pin0", "taskbar/pin1", "taskbar/a1"],
        "pinned first, running pinned app not repeated"
    );
    assert!(
        cells[0].look.classes.contains(&"running".to_string())
            && cells[0].look.classes.contains(&"focused".to_string())
            && cells[0]
                .look
                .classes
                .contains(&"app-org-mozilla-firefox".to_string())
            && cells[0].look.checked
    );
    assert!(matches!(
        cells[0].binding(Gesture::Tap),
        Some(Do::Agent(Act::Desktop(sliver_desktop::control::Action::FocusWindow { address }))) if address == "b2"
    ));
    assert!(cells[1].look.classes.contains(&"idle".to_string()));
    assert!(
        matches!(
            cells[1].binding(Gesture::Tap),
            Some(Do::Run(Action::Open(id))) if id == "kitty"
        ),
        "an idle launcher starts the app"
    );
    // `format-focused` labels only the app you're in, with its name (`{name}`): a pinned
    // app by its desktop entry, a window without one by its class.
    let label = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        _ => String::new(),
    };
    let conf = "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"taskbar\"]\n[modules.taskbar]\npinned = [\"org.mozilla.firefox.desktop\", \"kitty\"]\nformat-focused = \"{name}\"\n";
    let cells = stat_cells(conf, &w);
    let labels: Vec<String> = cells.iter().map(label).collect();
    // No icons here: the others fall back to their first letters.
    assert_eq!(labels, ["Firefox", "ki", "mp"]);
    w.desktop.windows = vec![win("a1", "mpv", true), win("b2", "firefox", false)];
    let cells = stat_cells(conf, &w);
    assert!(cells[2].look.classes.contains(&"focused".to_string()));
    let labels: Vec<String> = cells.iter().map(label).collect();
    assert_eq!(labels, ["Fi", "ki", "mpv"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clock_formats_and_ticks() {
    let cells = build(
        r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["clock", "clock#s", "custom/hi"]
        [modules."clock#s"]
        format = "{:%H:%M:%S}"
        [modules."custom/hi"]
        format = "hi"
        on-tap = "notify-send hi"
        "#,
        "base",
    );
    assert!(matches!(&cells[0].look.content, Content::Label { text: t, .. } if t == "09:41"));
    assert_eq!(
        cells[0].next_change.unwrap().format("%H:%M:%S").to_string(),
        "09:42:00"
    );
    assert_eq!(
        cells[1].next_change.unwrap().format("%H:%M:%S").to_string(),
        "09:41:08"
    );
    assert_eq!(cells[1].look.classes, ["s"]);
    assert_eq!(cells[2].touch, Touch::Agent);
    assert_eq!(cells[0].touch, Touch::None);
}

#[test]
fn preview_shows_samples_and_placeholders() {
    let l = load_str("", std::path::Path::new("."));
    let ctx = Context::new().with("physical_esc", true);
    let r = l.config.resolve("base", &ctx).unwrap();
    let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: &w,
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    // The default main zone shows F1-F12 until an app fills App controls (group/fkeys).
    let fkeys = out
        .iter()
        .filter(|c| c.key.starts_with("group/fkeys/"))
        .count();
    assert_eq!(fkeys, 12);
    let kinds: Vec<&str> = out
        .iter()
        .filter(|c| !c.key.starts_with("group/fkeys/"))
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "app-controls",
            "status-items",
            "mpris",
            "custom/expand",
            "backlight/slider",
            "audio/slider",
            "key/Mute"
        ],
    );
    assert!(out[0].look.classes.contains(&"placeholder".to_string()));
    // Collapsed by default: an icon that opens into a slider.
    let sliders = |c: &&Cell| {
        matches!(c.look.content, Content::Slider { .. })
            || c.drawer.as_ref().is_some_and(|d| d.opens == Opens::Slider)
    };
    assert_eq!(out.iter().filter(sliders).count(), 2);
}

#[test]
fn slider_is_one_switch_for_every_module_with_a_level() {
    let l = load_str(
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"audio/slider\", \"audio#s\", \"audio#b\"]\n[modules.\"audio/slider\"]\ncollapsed = false\n[modules.\"audio#s\"]\nslider = true\n",
        std::path::Path::new("."),
    );
    assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
    let r = l.config.resolve("base", &Context::new()).unwrap();
    let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
    let mut out = Vec::new();
    for z in &r.zones {
        let ui = |_: &str| UiState::default();
        cells(
            &z.modules,
            &mut Build {
                world: &w,
                icons: &mut Icons::builtin_only(),
                icon_size: 36,
                ui: &ui,
            },
            &mut out,
        );
    }
    let drags = |c: &Cell| c.on.iter().any(|(g, _)| *g == Gesture::Drag);
    let taps = |c: &Cell| c.on.iter().any(|(g, _)| *g == Gesture::Tap);
    // Written as waybar's `audio/slider` or as `slider = true`: the same slider.
    for c in &out[..2] {
        assert!(matches!(c.look.content, Content::Slider { .. }), "{}", c.id);
        assert!(drags(c) && !taps(c), "{}", c.id);
    }
    // Without it, the volume is a button whose tap mutes.
    assert!(taps(&out[2]) && !drags(&out[2]));
    assert!(!matches!(out[2].look.content, Content::Slider { .. }));
}

#[test]
fn graphs_preview_with_a_made_up_history() {
    let l = load_str(
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"graph#cpu\"]\n[modules.\"graph#cpu\"]\nsource = \"cpu\"\nsamples = 20\n",
        std::path::Path::new("."),
    );
    assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
    let r = l.config.resolve("base", &Context::new()).unwrap();
    let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
    assert!(w.preview);
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: &w,
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    match &out[0].look.content {
        Content::Graph { values, capacity } => {
            assert_eq!((values.len(), *capacity), (20, 20));
            assert!(values.iter().all(|v| (0.0..=1.0).contains(v)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn pinned_app_placeholders_have_short_labels() {
    let l = load_str(
        "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"app/org.example.Pomodoro#timer\", \"app/org.example.Pomodoro\"]\n",
        std::path::Path::new("."),
    );
    assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
    let r = l.config.resolve("base", &Context::new()).unwrap();
    let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: &w,
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    let labels: Vec<String> = out
        .iter()
        .filter(|c| c.look.classes.contains(&"placeholder".to_string()))
        .map(|c| match &c.look.content {
            Content::Label { text: t, .. } => t.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(labels, ["timer", "Pomodoro"]);
}

/// Waybar's systemd-failed-units: counts from both managers, hidden while none failed
/// (unless `hide-on-ok = false`), `system`/`user` filters, and a tap lists the units.
#[test]
fn systemd_failed_units() {
    use sliver_session::failed::{FailedUnits, Manager};
    let run = |toml: &str, f: &FailedUnits| -> Vec<Cell> {
        let l = load_str(
            &format!(
                "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"systemd-failed-units\"]\n\
                 [modules.systemd-failed-units]\n{toml}"
            ),
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        let r = l.config.resolve("base", &Context::new()).unwrap();
        let mut w = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        w.session.failed_units = f.clone();
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                &mut Build {
                    world: &w,
                    icons: &mut icons,
                    icon_size: 36,
                    ui: &|_| UiState::default(),
                },
                &mut out,
            );
        }
        out
    };
    let text = |c: &Cell| match &c.look.content {
        Content::Label { text: t, .. } => t.clone(),
        other => format!("{other:?}"),
    };
    let mgr = |u: &[&str]| {
        Some(Manager {
            count: u.len() as u32,
            units: u.iter().map(|s| s.to_string()).collect(),
        })
    };
    let none = FailedUnits {
        watched: true,
        system: mgr(&[]),
        user: mgr(&[]),
    };
    assert!(run("", &none).is_empty(), "hidden while all is well");
    assert!(
        run("", &FailedUnits::default()).is_empty(),
        "unknown: hidden"
    );
    let ok = run("hide-on-ok = false", &none);
    assert_eq!(text(&ok[0]), "✓");
    assert!(
        ok[0].look.classes.contains(&"ok".to_owned()),
        "{:?}",
        ok[0].look.classes
    );

    let some = FailedUnits {
        watched: true,
        system: mgr(&["nfs.mount"]),
        user: mgr(&["pipewire.service"]),
    };
    let c = run("", &some);
    assert_eq!(text(&c[0]), "2 failed");
    assert!(c[0].look.classes.contains(&"degraded".to_owned()));
    let Some(Do::Agent(Act::Menu { items, .. })) = c[0].binding(Gesture::Tap) else {
        panic!("a tap opens the menu: {:?}", c[0].on);
    };
    let labels: Vec<&str> = items.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["nfs.mount", "pipewire.service (user)"]);
    assert_eq!(
        items[1].on_tap,
        Do::Run(Action::Exec(
            [
                "xdg-terminal-exec",
                "systemctl",
                "--user",
                "status",
                "pipewire.service"
            ]
            .map(String::from)
            .into()
        ))
    );
    let c = run(
        "user = false\nformat = \"{nr_failed_system}/{nr_failed_user}: {units}\"",
        &some,
    );
    assert_eq!(text(&c[0]), "1/0: nfs.mount");
}

#[test]
fn placeholders_take_the_module_width() {
    let l = load_str(
        "[modules.status-items]\nwidth = 332\n",
        std::path::Path::new("."),
    );
    assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
    let ctx = Context::new().with("physical_esc", true);
    let r = l.config.resolve("base", &ctx).unwrap();
    let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    for z in &r.zones {
        cells(
            &z.modules,
            &mut Build {
                world: &w,
                icons: &mut icons,
                icon_size: 36,
                ui: &|_| UiState::default(),
            },
            &mut out,
        );
    }
    let status = out.iter().find(|c| c.kind == "status-items").unwrap();
    assert_eq!(status.look.width, Width::Fixed(332.0));
    let app = out.iter().find(|c| c.kind == "app-controls").unwrap();
    assert_eq!(app.look.width, Width::Auto, "unset stays auto");
}

#[test]
fn closed_drawer_shows_its_handle() {
    let cells = build("", "base");
    // group/fkeys (F1-F12 until an app publishes) comes first; it's not a drawer.
    let cells: Vec<Cell> = cells
        .into_iter()
        .filter(|c| !c.key.starts_with("group/fkeys/"))
        .collect();
    let keys: Vec<&str> = cells.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["group/control-strip/custom/expand", "key/Mute"]);
    assert!(cells[0].look.classes.contains(&"drawer-child".to_string()));
    let d = cells[0].drawer.as_ref().unwrap();
    assert_eq!(
        (d.group.as_str(), d.collapse_after),
        (
            "group/control-strip",
            Some(std::time::Duration::from_secs(5))
        )
    );
    assert_eq!(cells[0].touch, Touch::Agent, "the handle opens the drawer");
    assert!(
        matches!(&cells[1].look.content, Content::Label { icon: Some(_), text } if text.is_empty())
    );
}

#[test]
fn drawer_options_class_direction_and_start() {
    let loaded = load_str(
        r#"
        [[layouts.t.zones]]
        name = "main"
        modules = ["group/d"]
        [modules."group/d"]
        modules = ["key/F1", "key/F2", "key/F3"]
        drawer = { start-expanded = true, transition-left-to-right = false, children-class = "kid" }
        [[rules]]
        layout = "t"
        "#,
        std::path::Path::new("."),
    );
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let r = loaded.config.resolve("base", &Context::new()).unwrap();
    assert_eq!(start_expanded_drawers(&r.zones), ["group/d"]);

    let open = |k: &str| UiState {
        expanded: k == "group/d",
        ..UiState::default()
    };
    let mut out = Vec::new();
    let mut icons = Icons::builtin_only();
    cells(
        &r.zones[0].modules,
        &mut Build {
            world: &world(),
            icons: &mut icons,
            icon_size: 36,
            ui: &open,
        },
        &mut out,
    );
    let keys: Vec<&str> = out.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(
        keys,
        ["group/d/key/F2", "group/d/key/F3", "group/d/key/F1"],
        "opens towards the left: the leader comes last"
    );
    assert!(out[2].drawer.is_some(), "the leader is still the handle");
    assert!(out[0].drawer.is_none());
    assert!(
        out.iter()
            .all(|c| c.look.classes.contains(&"kid".to_string())
                && !c.look.classes.contains(&"drawer-child".to_string()))
    );
}

/// A user module's output is formatted like any module's: `format`, `format-alt`,
/// `format-icons`, `states` and `hide-empty-text` over its text, alt and percentage.
#[test]
fn user_module_output_is_formatted_like_any_module() {
    let cell = |block: &str, out: sliver_modules::Output, alt: bool| {
        let toml = format!(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"custom/x\"]\n[modules.\"custom/x\"]\nexec = \"x\"\n{block}\n"
        );
        let loaded = load_str(&toml, std::path::Path::new("."));
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let r = loaded.config.resolve("base", &Context::new()).unwrap();
        let mut w = world();
        w.outputs.insert("custom/x".into(), out);
        let ui = |_: &str| UiState {
            show_alt: alt,
            ..UiState::default()
        };
        let mut out = Vec::new();
        cells(
            &r.zones[0].modules,
            &mut Build {
                world: &w,
                icons: &mut Icons::builtin_only(),
                icon_size: 36,
                ui: &ui,
            },
            &mut out,
        );
        out.into_iter().next()
    };
    let text = |c: &Cell| c.look.content.label_text().unwrap_or_default().to_owned();
    let plain = sliver_modules::Output {
        text: "hi".into(),
        ..Default::default()
    };
    assert_eq!(text(&cell("", plain, false).unwrap()), "hi");

    let block = "format = \"{icon} {text} {percentage}%\"\nformat-alt = \"{alt}!\"\nformat-icons = [\"a\", \"b\", \"c\"]\nstates = { warning = 70, critical = 90 }";
    let busy = sliver_modules::Output {
        text: "cpu".into(),
        alt: Some("busy".into()),
        percentage: Some(95.0),
        classes: vec!["mine".into()],
        ..Default::default()
    };
    let c = cell(block, busy.clone(), false).unwrap();
    assert_eq!(text(&c), "c cpu 95%");
    assert!(c.look.classes.contains(&"mine".into()) && c.look.classes.contains(&"critical".into()));
    assert_eq!(text(&cell(block, busy, true).unwrap()), "busy!");

    let empty = sliver_modules::Output::default();
    assert!(
        cell("hide-empty-text = true", empty, false).is_none(),
        "no text: hidden"
    );
    assert!(cell("", sliver_modules::Output::hidden(), false).is_none());
}
