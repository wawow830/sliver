//! Untrusted input never panics the agent: script output (plain or JSON lines), user format
//! strings, and both together through a full frame. Deterministic pseudo-random inputs; the
//! failing input is printed.

use chrono::{Local, TimeZone};
use sliver_agent::Agent;
use sliver_agent::icons::Icons;
use sliver_agent::modules::World;
use sliver_render::{Fonts, Renderer, Stylesheet};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }

    fn soup(&mut self, xs: &[&str], max: usize) -> String {
        let n = self.below(max) + 1;
        (0..n).map(|_| self.pick(xs)).collect()
    }
}

const FORMAT_BITS: &[&str] = &[
    "{",
    "}",
    "{}",
    "{:",
    "{:%H",
    "{:%",
    "%",
    "%Q",
    "{text}",
    "{percentage}",
    "{icon}",
    "{alt}",
    "{0}",
    "{-1}",
    "{:>9999}",
    "{:.99999}",
    "{text:",
    "<b>",
    "</b>",
    "&",
    "&#xFFFFFF;",
    "\\",
    "\u{1f44d}",
    "\u{202e}",
    "x",
    " ",
    "{{",
    "}}",
    "{:%a %d %b}",
    "{percentage:>3}",
];

const OUTPUT_BITS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    "\"",
    ":",
    ",",
    "\"text\"",
    "\"alt\"",
    "\"class\"",
    "\"percentage\"",
    "\"tooltip\"",
    "null",
    "true",
    "1e999",
    "-1",
    "NaN",
    "\"\\u0000\"",
    "\"\\ud800\"",
    "<b>",
    "&amp;",
    "&",
    "\n",
    "\t",
    "\u{0}",
    "\u{1f600}",
    "x",
    " ",
    "999999999999999999999999",
];

/// A TOML basic string holding `s` exactly.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn agent(config: &str) -> Agent {
    let l = sliver_config::load_str(config, std::path::Path::new("."));
    Agent::new(
        l.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    )
}

#[test]
fn random_formats_and_outputs_render() {
    let mut rng = Rng(0x5eed_0004);
    let now = Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap();
    for i in 0..60 {
        let format = rng.soup(FORMAT_BITS, 10);
        let config = format!(
            "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"custom/x\", \"clock\"]\n\
             [modules.\"custom/x\"]\nformat = {}\nformat-icons = [\"a\", \"b\"]\n[modules.clock]\nformat = {}\n",
            toml_str(&format),
            toml_str(&format),
        );
        let raw = rng.soup(OUTPUT_BITS, 12);
        let pct = [
            None,
            Some(-1.0),
            Some(50.0),
            Some(1e300),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ];
        let output = sliver_modules::Output {
            text: raw.clone(),
            alt: (i % 3 == 0).then(|| rng.soup(OUTPUT_BITS, 4)),
            classes: vec![rng.soup(OUTPUT_BITS, 3)],
            percentage: pct[rng.below(pct.len())],
            value: pct[rng.below(pct.len())],
            icon: (i % 5 == 0).then(|| rng.soup(OUTPUT_BITS, 3)),
            ..Default::default()
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut a = agent(&config);
            let mut w = World::at(now);
            w.outputs.insert("custom/x".into(), output.clone());
            a.render(&w);
        }));
        assert!(
            r.is_ok(),
            "panicked on format {format:?} with output {raw:?}"
        );
    }
}
