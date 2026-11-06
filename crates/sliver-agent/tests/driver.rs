//! The driver as a state machine: inputs in, outputs out, no sliverd and no I/O.

use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use sliver_agent::Agent;
use sliver_agent::agent::Effect;
use sliver_agent::driver::{Driver, Input, Now, Output};
use sliver_agent::icons::Icons;
use sliver_agent::modules::World;
use sliver_config::load_str;
use sliver_ipc::proto::{DisplayState, TouchPhase};
use sliver_render::{Fonts, Renderer, Stylesheet};

const BAR: &str = r#"
[[layouts.default.zones]]
name = "main"
modules = ["custom/a"]
[modules."custom/a"]
format = "A"
on-tap = { keys = ["A"] }
"#;

fn driver(toml: &str) -> Driver {
    let loaded = load_str(toml, std::path::Path::new("."));
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let agent = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    Driver::new(agent, 255)
}

/// A clock that starts at a fixed moment and moves only when told.
struct Clock(Now);

impl Clock {
    fn new() -> Clock {
        Clock(Now {
            mono_us: 1_000_000,
            instant: Instant::now(),
            wall: Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 5).unwrap(),
        })
    }

    fn advance(&mut self, d: Duration) -> Now {
        self.0.mono_us += d.as_micros() as u64;
        self.0.instant += d;
        self.0.wall += chrono::Duration::from_std(d).unwrap();
        self.0
    }
}

fn world(now: &Now) -> World {
    World::at(now.wall)
}

fn presents(out: &[Output]) -> usize {
    out.iter()
        .filter(|o| matches!(o, Output::Present(_)))
        .count()
}

#[test]
fn it_draws_at_start_tells_sliverd_once_and_then_sleeps() {
    let mut d = driver(BAR);
    let mut clock = Clock::new();
    let now = clock.0;
    let out = d.draw(&now, || world(&now));
    assert!(presents(&out) >= 1, "{out:?}");
    assert_eq!(
        out.iter()
            .filter(|o| matches!(o, Output::Policy(_)))
            .count(),
        1
    );
    // Nothing changed: no frame, no policy, and nothing to wake for.
    let now = clock.advance(Duration::from_secs(1));
    assert!(d.handle(&now, Input::Tick).is_empty());
    assert_eq!(d.draw(&now, || panic!("no world needed")), []);
    assert_eq!(d.next_deadline(&now), None);
    // A new role means sliverd forgot the policy.
    d.handle(&now, Input::Role);
    let out = d.draw(&now, || world(&now));
    assert!(out.iter().any(|o| matches!(o, Output::Policy(_))));
}

#[test]
fn content_changes_are_paced_but_never_lost() {
    let mut d = driver(BAR);
    let mut clock = Clock::new();
    let now = clock.0;
    d.draw(&now, || world(&now));
    // A change right after a frame waits for the pace…
    let now = clock.advance(Duration::from_millis(5));
    d.handle(&now, Input::Changed);
    d.handle(&now, Input::Tick);
    assert_eq!(presents(&d.draw(&now, || world(&now))), 0);
    assert!(!d.wants_content(&now), "more changes can't make it sooner");
    let wait = d.next_deadline(&now).expect("the held-back change is due");
    assert!(wait <= Duration::from_millis(34), "{wait:?}");
    // …and is drawn when it comes due.
    let now = clock.advance(wait);
    d.handle(&now, Input::Tick);
    assert!(presents(&d.draw(&now, || world(&now))) >= 1);
    assert!(d.wants_content(&now));
}

#[test]
fn a_dark_bar_draws_nothing_and_wakes_for_nothing() {
    let mut d = driver(BAR);
    let mut clock = Clock::new();
    let now = clock.0;
    d.draw(&now, || world(&now));
    d.handle(&now, Input::Display(DisplayState::Off));
    let now = clock.advance(Duration::from_secs(1));
    d.handle(&now, Input::Changed);
    d.handle(&now, Input::Redraw);
    assert_eq!(presents(&d.draw(&now, || panic!("no world while dark"))), 0);
    assert_eq!(d.next_deadline(&now), None);
    // Lighting up again draws what changed in the dark.
    d.handle(&now, Input::Display(DisplayState::On));
    assert!(presents(&d.draw(&now, || world(&now))) >= 1);
}

#[test]
fn a_tap_passes_its_effects_through_and_draws_its_press() {
    let mut d = driver(BAR);
    let mut clock = Clock::new();
    let now = clock.0;
    d.draw(&now, || world(&now));
    let region = d
        .agent()
        .frame(sliver_ipc::proto::Layer::Base)
        .unwrap()
        .regions
        .first()
        .expect("a touchable button")
        .id;
    let touch = |phase, now: &Now| Input::Touch {
        region,
        slot: 0,
        phase,
        x: 10.0,
        time_us: now.mono_us,
    };
    let mut effects = d.handle(&now, touch(TouchPhase::Down, &now));
    assert!(
        presents(&d.draw(&now, || world(&now))) >= 1,
        "pressed feedback"
    );
    let now = clock.advance(Duration::from_millis(40));
    effects.extend(d.handle(&now, touch(TouchPhase::Up, &now)));
    d.handle(&now, Input::Tick);
    assert!(
        effects
            .iter()
            .any(|o| matches!(o, Output::Effect(Effect::EmitKeys(..)))),
        "{effects:?}"
    );
    // Redraw requests are the driver's business, never main's.
    assert!(
        !effects
            .iter()
            .any(|o| matches!(o, Output::Effect(Effect::Redraw | Effect::RedrawPress)))
    );
}
