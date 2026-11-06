use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/test-tmp"
    ))
    .join(format!("preset-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn base64_round_trips() {
    for n in 0..20 {
        let data: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
        assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
    }
    assert_eq!(base64_encode(b"Man"), "TWFu");
    assert_eq!(base64_encode(b"Ma"), "TWE=");
    assert!(base64_decode("TW=u").is_none());
    assert!(base64_decode("TWF").is_none());
}

#[test]
fn names_stay_inside() {
    for ok in ["config.toml", "assets/a.png", "include/x/y.toml"] {
        check_name(ok).unwrap();
    }
    for bad in [
        "",
        "/etc/passwd",
        "../x",
        "a/../../x",
        "./a",
        "a//b",
        "a/",
        "a\\b",
        "a/b/c/d/e",
    ] {
        assert!(check_name(bad).is_err(), "{bad:?}");
    }
    let evil = "[preset]\nformat = 1\n[files]\n\"config.toml\" = \"\"\n\"../x\" = \"\"\n";
    assert!(Bundle::parse(evil).unwrap_err().contains("../x"));
}

#[test]
fn parse_wants_a_preset() {
    assert!(Bundle::parse("version = 1").is_err());
    assert!(
        Bundle::parse("[preset]\nformat = 9\n[files]\n")
            .unwrap_err()
            .contains("9")
    );
    assert!(
        Bundle::parse("[preset]\nformat = 1\n[files]\n")
            .unwrap_err()
            .contains(CONFIG)
    );
}

#[test]
fn lists_and_strips_everything_that_runs() {
    let config = r#"
version = 1
[modules."custom/vpn"]
exec = "~/bin/vpn --json"
exec-if = "test -x ~/bin/vpn"
on-tap = { exec = ["foot", "-e", "htop"] }
on-hold = { layer = "fn" }
on-swipe-left = [{ key = "VolumeDown" }, { dbus = { dest = "org.x", path = "/x", interface = "org.x.Y", method = "Go" } }]
menu = [{ label = "Lock", on-tap = "loginctl lock-session" }, { label = "Web", on-tap = { url = "https://x" } }]
[modules.clock]
on-tap = { key = "F1" }
"#;
    let b = Bundle {
        name: None,
        files: BTreeMap::from([(CONFIG.to_owned(), config.as_bytes().to_vec())]),
    };
    let runs: Vec<String> = b.runs().iter().map(|r| r.what.clone()).collect();
    assert_eq!(
        runs,
        [
            "sh: ~/bin/vpn --json",
            "sh: test -x ~/bin/vpn",
            "sh: loginctl lock-session",
            "url: https://x",
            "dbus: session org.x /x org.x.Y.Go",
            "exec: foot -e htop",
        ]
        .map(String::from)
        .to_vec(),
        "{:#?}",
        b.runs()
    );
    assert!(
        b.runs()[0]
            .at
            .starts_with("config.toml: modules.\"custom/vpn\".exec")
    );
    let (clean, n) = b.without_commands();
    assert_eq!(n, 6);
    assert!(clean.runs().is_empty(), "{:?}", clean.runs());
    let text = String::from_utf8(clean.files[CONFIG].clone()).unwrap();
    assert!(text.contains("on-hold = { layer = \"fn\" }"), "{text}");
    assert!(text.contains("on-tap = { key = \"F1\" }"), "{text}");
    assert!(text.contains("label = \"Lock\""), "{text}");
}

#[test]
fn export_import_round_trip() {
    let home = tmp("home");
    let cfg = home.join("cfg");
    let elsewhere = home.join("pics");
    std::fs::create_dir_all(cfg.join("parts")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(
        elsewhere.join("logo.png"),
        [0x89, b'P', b'N', b'G', 0, 1, 2],
    )
    .unwrap();
    std::fs::write(
        elsewhere.join("extra.toml"),
        "[modules.\"custom/x\"]\nformat = \"x\"\nicon = \"../pics/logo.png\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg.join("parts/keys.toml"),
        "[modules.\"custom/k\"]\nformat = \"k\"\n",
    )
    .unwrap();
    std::fs::write(cfg.join("bg.svg"), "<svg/>").unwrap();
    std::fs::write(
        cfg.join("style.css"),
        "#clock { background-image: url(\"bg.svg\"); }\n",
    )
    .unwrap();
    let logo = elsewhere.join("logo.png").display().to_string();
    std::fs::write(
        cfg.join(CONFIG),
        format!(
            "version = 1\ninclude = [\"parts/keys.toml\", \"../pics/extra.toml\"]\n\
             [modules.\"custom/vpn\"]\nformat = \"vpn\"\nicon = \"{logo}\" # logo\n\
             env = {{ TOKEN = \"hunter2\" }}\n\
             [modules.\"custom/sys\"]\nformat = \"s\"\nicon = \"/usr/share/icons/x.svg\"\n"
        ),
    )
    .unwrap();
    let ex = export(&cfg.join(CONFIG), Some("Desk")).unwrap();
    let names: Vec<&str> = ex.bundle.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "assets/logo.png",
            "bg.svg",
            CONFIG,
            "include/extra.toml",
            "parts/keys.toml",
            "style.css"
        ]
    );
    let main = String::from_utf8(ex.bundle.files[CONFIG].clone()).unwrap();
    assert!(main.contains("icon = \"assets/logo.png\" # logo"), "{main}");
    assert!(
        main.contains("\"parts/keys.toml\", \"include/extra.toml\""),
        "{main}"
    );
    assert!(main.contains("/usr/share/icons/x.svg"), "{main}");
    assert!(!main.contains("hunter2"), "{main}");
    assert!(
        ex.notes.iter().any(|n| n.contains("env left out")),
        "{:?}",
        ex.notes
    );
    let extra = String::from_utf8(ex.bundle.files["include/extra.toml"].clone()).unwrap();
    assert!(extra.contains("icon = \"../assets/logo.png\""), "{extra}");

    // Through text and back, then into a fresh config dir.
    let text = ex.bundle.to_text();
    assert!(text.contains("base64 = "), "{text}");
    assert!(text.starts_with("# A sliver preset"));
    assert!(text.find("[preset]") < text.find("[files]"), "{text}");
    let back = Bundle::parse(&text).unwrap();
    assert_eq!(back, ex.bundle);
    let scratch = tmp("scratch");
    assert!(back.check(&scratch).unwrap().ok());
    let dest = tmp("dest");
    std::fs::write(dest.join(CONFIG), "version = 1 # old\n").unwrap();
    let inst = back.install(&dest).unwrap();
    assert_eq!(inst.written.last().unwrap(), &dest.join(CONFIG));
    let backup = inst.backup.unwrap();
    assert_eq!(
        std::fs::read_to_string(backup.join(CONFIG)).unwrap(),
        "version = 1 # old\n"
    );
    let installed = std::fs::read_to_string(dest.join(CONFIG)).unwrap();
    let abs_logo = dest.join("assets/logo.png").display().to_string();
    assert!(
        installed.contains(&format!("icon = \"{abs_logo}\" # logo")),
        "{installed}"
    );
    let extra = std::fs::read_to_string(dest.join("include/extra.toml")).unwrap();
    assert!(extra.contains(&abs_logo), "{extra}");
    // Exporting the installed config gives the same bundle back (absolute paths under the
    // config directory turn relative again).
    let again = export(&dest.join(CONFIG), Some("Desk")).unwrap().bundle;
    assert_eq!(
        again.files.keys().collect::<Vec<_>>(),
        ex.bundle.files.keys().collect::<Vec<_>>()
    );
    assert_eq!(again.files[CONFIG], ex.bundle.files[CONFIG]);
    let loaded = load_file(&dest.join(CONFIG));
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    assert!(
        loaded
            .config
            .modules
            .keys()
            .any(|k| k.to_string() == "custom/x")
    );
    assert!(
        loaded
            .config
            .modules
            .keys()
            .any(|k| k.to_string() == "custom/k")
    );
    assert_eq!(
        std::fs::read(dest.join("assets/logo.png")).unwrap(),
        [0x89, b'P', b'N', b'G', 0, 1, 2]
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".preset-"))
        .collect();
    assert!(leftovers.is_empty());
    for d in [home, scratch, dest] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn export_from_a_profile_and_install_as_one() {
    // A profile (profiles/work.toml) overlays the main config, like `profile::create` makes.
    let home = tmp("prof");
    let cfg = home.join("sliver");
    std::fs::create_dir_all(cfg.join("profiles")).unwrap();
    std::fs::write(cfg.join("logo.svg"), "<svg/>").unwrap();
    std::fs::write(cfg.join("style.css"), "#clock { color: red; }\n").unwrap();
    std::fs::write(
        cfg.join(CONFIG),
        "version = 1\n[modules.\"custom/a\"]\nformat = \"a\"\nicon = \"logo.svg\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg.join("profiles/work.toml"),
        "version = 1\ninclude = [\"../config.toml\"]\nstylesheet = \"../style.css\"\n\
         [modules.\"custom/w\"]\nformat = \"w\"\n",
    )
    .unwrap();
    let ex = export(&cfg.join("profiles/work.toml"), None).unwrap();
    let names: Vec<&str> = ex.bundle.files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "assets/logo.svg",
            "assets/style.css",
            CONFIG,
            "include/config.toml"
        ],
        "{:?}",
        ex.notes
    );
    let main = String::from_utf8(ex.bundle.files[CONFIG].clone()).unwrap();
    assert!(
        main.contains("include = [\"include/config.toml\"]"),
        "{main}"
    );
    assert!(main.contains("stylesheet = \"assets/style.css\""), "{main}");
    let inc = String::from_utf8(ex.bundle.files["include/config.toml"].clone()).unwrap();
    assert!(inc.contains("icon = \"../assets/logo.svg\""), "{inc}");

    // Installed as profile "shared" elsewhere: profiles/shared.toml + profiles/shared/….
    let dest = tmp("prof-dest");
    let prof = dest.join("profiles/shared.toml");
    let files = dest.join("profiles/shared");
    let inst = ex.bundle.install_at(&prof, &files, &dest).unwrap();
    assert!(inst.backup.is_none());
    assert_eq!(inst.written.last().unwrap(), &prof);
    let text = std::fs::read_to_string(&prof).unwrap();
    let abs = |n: &str| files.join(n).display().to_string();
    assert!(text.contains(&abs("include/config.toml")), "{text}");
    assert!(text.contains(&abs("assets/style.css")), "{text}");
    let loaded = load_file(&prof);
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let ids: Vec<String> = loaded
        .config
        .modules
        .keys()
        .map(|k| k.to_string())
        .collect();
    assert!(ids.contains(&"custom/a".into()) && ids.contains(&"custom/w".into()));
    // An implicit style.css is named explicitly when the config moves.
    let b = Bundle {
        name: None,
        files: BTreeMap::from([
            (CONFIG.to_owned(), b"version = 1\n".to_vec()),
            ("style.css".to_owned(), b"x {}".to_vec()),
        ]),
    };
    b.install_at(
        &dest.join("profiles/plain.toml"),
        &dest.join("profiles/plain"),
        &dest,
    )
    .unwrap();
    let text = std::fs::read_to_string(dest.join("profiles/plain.toml")).unwrap();
    assert!(
        text.contains(&format!(
            "stylesheet = \"{}\"",
            dest.join("profiles/plain/style.css").display()
        )),
        "{text}"
    );
    for d in [home, dest] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn a_broken_preset_changes_nothing() {
    let dest = tmp("broken");
    std::fs::write(dest.join(CONFIG), "version = 1\n").unwrap();
    let b = Bundle {
        name: None,
        files: BTreeMap::from([
            (CONFIG.to_owned(), b"version = 1\nlayouts = 3\n".to_vec()),
            ("style.css".to_owned(), b"x".to_vec()),
        ]),
    };
    assert!(
        b.install(&dest)
            .unwrap_err()
            .contains("nothing was changed")
    );
    assert_eq!(
        std::fs::read_to_string(dest.join(CONFIG)).unwrap(),
        "version = 1\n"
    );
    assert!(!dest.join("style.css").exists());
    let _ = std::fs::remove_dir_all(dest);
}
