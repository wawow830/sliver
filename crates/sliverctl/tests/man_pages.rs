//! The man pages in `man/` must cover what the code knows about, so they can't silently drift:
//! every `sliverctl` command and option in `sliverctl help`, and every module type, common module
//! key, context key and bar gesture in `sliver-config`. Also lints the pages with groff if it's
//! installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn man_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../man")
}

/// Page text with roff escapes for `-` and fonts removed, so `on\-tap` reads `on-tap`.
fn page(name: &str) -> String {
    let raw =
        std::fs::read_to_string(man_dir().join(name)).unwrap_or_else(|e| panic!("man/{name}: {e}"));
    let mut s = raw.replace("\\-", "-").replace("\\&", "");
    for f in ["\\fB", "\\fI", "\\fR", "\\fP"] {
        s = s.replace(f, "");
    }
    s
}

/// `word` appears with non-identifier characters (or line ends) on both sides.
fn mentions(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

fn missing(text: &str, words: &[&str]) -> Vec<String> {
    words
        .iter()
        .filter(|w| !mentions(text, w))
        .map(|w| w.to_string())
        .collect()
}

fn help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sliverctl"))
        .arg("help")
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

/// Command words from the help: on indented lines, the leading lowercase words, split on `|`,
/// up to the first argument placeholder (`MODULE`, `[NAME]`, `--opt`, `...`).
fn help_commands(help: &str) -> Vec<String> {
    let mut words = Vec::new();
    for line in help
        .lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("      "))
    {
        // The command part ends where the description column starts (two or more spaces).
        let cmd = line.trim_start().split("  ").next().unwrap_or_default();
        for tok in cmd.split_whitespace() {
            let first = tok.chars().next().unwrap();
            if !first.is_ascii_lowercase() {
                break;
            }
            words.extend(tok.split('|').filter(|w| !w.is_empty()).map(str::to_owned));
        }
    }
    words.sort();
    words.dedup();
    words
}

#[test]
fn sliverctl_page_covers_the_help() {
    let help = help();
    let man = page("sliverctl.1");
    let cmds = help_commands(&help);
    assert!(cmds.len() > 30, "help parsing broke: {cmds:?}");
    let words: Vec<&str> = cmds.iter().map(String::as_str).collect();
    assert_eq!(
        missing(&man, &words),
        Vec::<String>::new(),
        "commands in `sliverctl help` missing from man/sliverctl.1"
    );
    let opts: Vec<&str> = help
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .filter(|w| w.starts_with("--") || *w == "-p" || *w == "-o")
        .collect();
    assert_eq!(
        missing(&man, &opts),
        Vec::<String>::new(),
        "options in `sliverctl help` missing from man/sliverctl.1"
    );
}

#[test]
fn help_commands_are_real() {
    // The other direction: every word the help (and so the man page) promises parses.
    for cmd in [
        "status",
        "modules",
        "contexts",
        "reload",
        "reset-visibility",
        "help",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sliverctl"))
            .arg(cmd)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("unknown command"), "{cmd}: {err}");
    }
}

#[test]
fn config_page_covers_the_schema() {
    let man = page("sliver-config.5");
    for (what, words) in [
        ("module types", sliver_config::MODULE_TYPES),
        ("common module keys", sliver_config::COMMON_KEYS),
        ("context keys", sliver_config::CONTEXT_KEYS),
        ("bar gestures", sliver_config::BAR_GESTURES),
    ] {
        assert_eq!(
            missing(&man, words),
            Vec::<String>::new(),
            "{what} missing from man/sliver-config.5"
        );
    }
}

#[test]
fn pages_are_lint_clean() {
    let pages: Vec<PathBuf> = std::fs::read_dir(man_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    for want in [
        "sliverctl.1",
        "sliver-agent.1",
        "sliverd.8",
        "sliver-config.5",
        "sliver-stylesheet.5",
    ] {
        assert!(
            pages.iter().any(|p| p.ends_with(want)),
            "man/{want} missing"
        );
    }
    if Command::new("groff").arg("-v").output().is_err() {
        eprintln!("groff not installed; skipping lint");
        return;
    }
    for p in pages {
        let out = Command::new("groff")
            .args(["-man", "-ww", "-z"])
            .arg(&p)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && err.trim().is_empty(),
            "{}: {err}",
            p.display()
        );
    }
}
