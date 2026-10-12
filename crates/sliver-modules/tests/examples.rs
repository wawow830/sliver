//! The shipped presets (`examples/config/*.toml`) load, and their scripts (`examples/shell`,
//! `examples/python`) print what the module runtime accepts. Network calls go to a fake `curl`.

use std::path::{Path, PathBuf};
use std::process::Command;

use sliver_modules::{Output, parse_json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A scratch dir with `bin/curl`, which prints `$FAKE_CURL` or fails when that's unset.
fn sandbox(name: &str) -> PathBuf {
    let d = repo().join(format!(
        "target/test-tmp/examples-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let curl = d.join("bin/curl");
    std::fs::write(
        &curl,
        "#!/bin/sh\n[ -n \"${FAKE_CURL:-}\" ] || exit 7\nprintf '%s' \"$FAKE_CURL\"\n",
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&curl).status().unwrap();
    d
}

fn run(sandbox: &Path, script: &str, args: &[&str], fake: Option<&str>) -> Output {
    let path = format!(
        "{}:{}",
        sandbox.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(repo().join(script));
    c.args(args)
        .env("PATH", path)
        .env("HOME", sandbox)
        .env("XDG_CACHE_HOME", sandbox.join("cache"))
        .env("TZ", "UTC")
        .env_remove("FAKE_CURL");
    if let Some(f) = fake {
        c.env("FAKE_CURL", f);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "{script}: {out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "{script}: one JSON line: {stdout:?}"
    );
    parse_json(&stdout).unwrap_or_else(|e| panic!("{script}: {e}: {stdout}"))
}

#[test]
fn presets_load() {
    let dir = repo().join("examples/config");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "toml") {
            let loaded = sliver_config::load_file(&p);
            assert!(
                loaded.diagnostics.ok(),
                "{}: {:?}",
                p.display(),
                loaded.diagnostics.errors
            );
            n += 1;
        }
    }
    assert!(n >= 4, "presets not found in {}", dir.display());
}

#[test]
fn weather_script() {
    let d = sandbox("weather");
    let s = "examples/shell/sliver-weather";
    let o = run(&d, s, &["Sydney"], Some("☁️ |+24°C|Overcast |64%|←4km/h"));
    assert_eq!(o.text, "☁️ 24°C");
    assert_eq!(o.classes, ["cloudy"]);
    assert_eq!(
        o.detail.as_deref(),
        Some("Overcast, humidity 64%, wind ←4km/h")
    );
    let o = run(
        &d,
        s,
        &["Sydney"],
        Some("🌦 |+18°C|Light rain shower|80%|↓9km/h"),
    );
    assert_eq!(o.classes, ["rain"]);
    // Offline: the last reading, marked stale.
    let o = run(&d, s, &["Sydney"], None);
    assert_eq!(o.text, "🌦 18°C");
    assert_eq!(o.classes, ["rain", "stale"]);
    // Offline and never online for this place: an error, not a crash.
    let o = run(&d, s, &["Oslo"], None);
    assert_eq!(
        (o.text.as_str(), o.classes.as_slice()),
        ("?", &["error".to_owned()][..])
    );
    // A garbage answer (an HTML error page) counts as offline.
    let o = run(&d, s, &["Oslo"], Some("<html>Unknown location</html>"));
    assert_eq!(o.classes, ["error"]);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn currency_script() {
    let d = sandbox("currency");
    let s = "examples/shell/sliver-currency";
    let rate = |r: &str| {
        format!(r#"{{"amount":1.0,"base":"EUR","date":"2026-10-09","rates":{{"USD":{r}}}}}"#)
    };
    let o = run(&d, s, &["eur", "usd"], Some(&rate("1.1206")));
    assert_eq!(o.text, "EUR → 1.1206 USD");
    assert_eq!(o.classes, ["same"]);
    let o = run(&d, s, &["EUR", "USD"], Some(&rate("1.1300")));
    assert_eq!(o.text, "EUR → 1.1300 USD ▲");
    assert_eq!(o.classes, ["up"]);
    // The arrow stays until the rate moves again.
    let o = run(&d, s, &["EUR", "USD"], Some(&rate("1.1300")));
    assert_eq!(o.classes, ["up"]);
    let o = run(&d, s, &["EUR", "USD", "100"], Some(&rate("1.1")));
    assert_eq!(o.text, "100 EUR → 110.00 USD ▼");
    let o = run(&d, s, &["EUR", "USD"], None);
    assert_eq!(o.classes, ["down", "stale"]);
    let o = run(&d, s, &["EURO", "USD"], Some(&rate("1")));
    assert_eq!(o.classes, ["error"]);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn next_event_script() {
    if Command::new("python3").arg("-V").output().is_err() {
        eprintln!("skipped: no python3");
        return;
    }
    let d = sandbox("calendar");
    let cal = d.join("cal");
    std::fs::create_dir_all(&cal).unwrap();
    // Standup Mon/Wed/Fri 09:30 Berlin (07:30 UTC in October), one Friday cancelled; a moved
    // instance; a one-off; an all-day event (skipped); a cancelled event.
    std::fs::write(
        cal.join("work.ics"),
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Standup\r\n\
         DTSTART;TZID=Europe/Berlin:20250106T093000\r\nDTEND;TZID=Europe/Berlin:20250106T094500\r\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r\nEXDATE;TZID=Europe/Berlin:20261016T093000\r\n\
         END:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Standup\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261014T093000\r\n\
         DTSTART;TZID=Europe/Berlin:20261014T110000\r\nDTEND;TZID=Europe/Berlin:20261014T111500\r\n\
         END:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Design review with a rather long\r\n  title\r\nLOCATION:Room 4\\, 2nd floor\r\n\
         DTSTART:20261012T130000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261012\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Gone\r\nSTATUS:CANCELLED\r\nDTSTART:20261012T073500Z\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
    )
    .unwrap();
    let s = "examples/python/sliver-next-event";
    let at = |t: &str| {
        let out = Command::new(repo().join(s))
            .arg(&cal)
            .env("SLIVER_NOW", t)
            .env("TZ", "UTC")
            .env("XDG_CACHE_HOME", d.join("cache"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        parse_json(&String::from_utf8(out.stdout).unwrap()).unwrap()
    };
    // Monday 2026-10-12.
    let o = at("2026-10-12T07:18:00+00:00");
    assert_eq!(o.text, "in 12 min: Standup");
    assert_eq!(o.classes, ["soon"]);
    let detail = o.detail.unwrap();
    assert!(
        detail.starts_with("07:30–07:45 Standup\n13:00–14:00 Design review"),
        "{detail}"
    );
    assert!(detail.contains("(Room 4, 2nd floor)"), "{detail}");
    let o = at("2026-10-12T07:40:00+00:00");
    assert_eq!(
        (o.text.as_str(), o.classes.as_slice()),
        ("now: Standup", &["now".to_owned()][..])
    );
    assert_eq!(o.percentage, Some(67.0));
    let o = at("2026-10-12T08:00:00+00:00");
    assert_eq!(o.text, "13:00 Design review with a rather…");
    assert_eq!(o.classes, ["later"]);
    // Wednesday: the standup moved to 11:00 Berlin (09:00 UTC).
    let o = at("2026-10-14T07:00:00+00:00");
    assert_eq!(o.text, "09:00 Standup");
    // Friday's is cancelled (EXDATE), and nothing else is on within 12 h: hidden.
    let o = at("2026-10-16T06:00:00+00:00");
    assert!(o.hidden, "{o:?}");
    let _ = std::fs::remove_dir_all(d);
}
