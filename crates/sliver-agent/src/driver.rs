//! The agent as one state machine: [`Input`]s in, [`Output`]s out.
//!
//! The driver decides when [`Agent`] draws: a touch or a layer change at once, content changes
//! (feeds, script output) at most [`CONTENT_FPS`](crate::pace::CONTENT_FPS) times a second, a
//! running transition on its next frame and a clock on its next tick. It also keeps what sliverd
//! has been told (the display policy).
//! It does no I/O: `main` turns file descriptors into inputs, carries out the outputs and sleeps
//! until [`Driver::next_deadline`].

use std::time::Duration;

use sliver_ipc::proto::{DisplayPolicy, DisplayState, Layer, TouchPhase};

use crate::agent::{Agent, Effect};
use crate::modules::World;
pub use crate::now::Now;
use crate::pace::Pacer;

/// A deadline this close counts as due: waking a few ms early must not sleep again.
const SLACK: Duration = Duration::from_millis(5);

#[derive(Debug, Clone)]
pub enum Input {
    /// Time passed: run whatever is due.
    Tick,
    /// What a module shows changed (feeds, script output): drawn when the pace allows.
    Changed,
    /// The whole frame may differ (config, stylesheet, accent colours): draw now.
    Redraw,
    /// Whether the laptop runs on battery, which picks the display policy.
    OnBattery(Option<bool>),
    Touch {
        region: u32,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        time_us: u64,
    },
    /// sliverd turned the bar on, dimmed it or turned it off.
    Display(DisplayState),
    /// The hatch (the bar hidden behind Fn) opened or closed.
    Hatch(bool),
    Fn(bool),
    /// sliverd changed our role. It ignored what we sent as a standby agent, so send it again.
    Role,
    /// Effects the agent produced answering a control request.
    Effects(Vec<Effect>),
    /// A newly loaded config and stylesheet.
    Config(Box<crate::reload::Applied>),
    /// The desktop's reduce-motion setting.
    ReduceMotion(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Present this layer's frame ([`Agent::frame`]).
    Present(Layer),
    /// Send sliverd this display policy.
    Policy(DisplayPolicy),
    /// Anything else the agent asks for: keys, processes, session changes, logs.
    Effect(Effect),
}

pub struct Driver {
    agent: Agent,
    panel_max: u32,
    on_battery: Option<bool>,
    /// Everything must be drawn again.
    dirty: bool,
    /// Only pressed feedback changed.
    press_dirty: bool,
    pacer: Pacer,
    sent_policy: Option<DisplayPolicy>,
}

impl Driver {
    pub fn new(agent: Agent, panel_max: u32) -> Driver {
        Driver {
            agent,
            panel_max,
            on_battery: None,
            dirty: true,
            press_dirty: false,
            pacer: Pacer::default(),
            sent_policy: None,
        }
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// A `Control1` request, answered at once ([`Agent::control`]). Its effects come back as
    /// [`Input::Effects`]; `Err(req)` is the caller's to answer.
    pub fn control(
        &mut self,
        req: crate::api::Control,
        now: &Now,
    ) -> (Result<crate::api::Reply, crate::api::Control>, Vec<Effect>) {
        let mut fx = Vec::new();
        let r = self.agent.control(req, now.mono_us, &mut fx);
        (r, fx)
    }

    pub fn handle(&mut self, now: &Now, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        let fx = match input {
            Input::Tick => {
                let fx = self.agent.tick(now.mono_us);
                self.dirty |= self.pacer.due(now.instant)
                    || self
                        .agent
                        .next_change()
                        .is_some_and(|t| t <= now.wall + SLACK)
                    || self
                        .agent
                        .animation_deadline()
                        .is_some_and(|t| t <= now.instant + SLACK);
                fx
            }
            Input::Changed => {
                self.pacer.changed();
                Vec::new()
            }
            Input::Redraw => {
                self.dirty = true;
                Vec::new()
            }
            Input::OnBattery(b) => {
                self.on_battery = b;
                Vec::new()
            }
            Input::Touch {
                region,
                slot,
                phase,
                x,
                time_us,
            } => self.agent.touch(region, slot, phase, x, time_us),
            Input::Display(state) => self.agent.set_display(state),
            Input::Hatch(open) => self.agent.set_hatch(open),
            Input::Fn(held) => {
                self.agent.set_fn_held(held);
                Vec::new()
            }
            Input::Config(a) => self.agent.apply(&a),
            Input::ReduceMotion(on) => {
                self.agent.set_reduce_motion(on);
                Vec::new()
            }
            Input::Role => {
                self.dirty = true;
                self.sent_policy = None;
                Vec::new()
            }
            Input::Effects(fx) => fx,
        };
        for e in fx {
            match e {
                Effect::Redraw => self.dirty = true,
                Effect::RedrawPress => self.press_dirty = true,
                e => out.push(Output::Effect(e)),
            }
        }
        out
    }

    /// Brings sliverd up to date and draws what's due. `world` is only built when a
    /// full frame is drawn, and is built at `now`.
    pub fn draw(&mut self, now: &Now, world: impl FnOnce() -> World) -> Vec<Output> {
        let mut out = Vec::new();
        let policy = crate::display::policy(
            &self.agent.config().display,
            self.on_battery,
            self.panel_max,
        );
        if self.sent_policy.as_ref() != Some(&policy) {
            self.sent_policy = Some(policy.clone());
            out.push(Output::Policy(policy));
        }
        let lit = self.agent.display() != DisplayState::Off;
        if lit && self.dirty {
            self.pacer.drawn(now.instant);
            out.extend(self.agent.render(&world()).into_iter().map(Output::Present));
            out.extend(
                self.agent
                    .errors()
                    .iter()
                    .map(|e| Output::Effect(Effect::Log(e.clone()))),
            );
        } else if lit && self.press_dirty {
            out.extend(
                self.agent
                    .render_press(now.instant)
                    .into_iter()
                    .map(Output::Present),
            );
        }
        self.dirty = false;
        self.press_dirty = false;
        out
    }

    /// How long until something is due, measured from `now` (`None`: nothing will be until an
    /// input arrives).
    pub fn next_deadline(&self, now: &Now) -> Option<Duration> {
        let a = &self.agent;
        let lit = a.display() != DisplayState::Off;
        let gesture = a
            .next_deadline()
            .map(|t| Duration::from_micros(t.saturating_sub(now.mono_us)));
        let change = a
            .next_change()
            .map(|t| (t - now.wall).to_std().unwrap_or_default());
        let animation = a
            .animation_deadline()
            .map(|t| t.saturating_duration_since(now.instant));
        let pace = self.pacer.wait(now.instant).filter(|_| lit);
        let pending = (lit && (self.dirty || self.press_dirty)).then_some(Duration::ZERO);
        [gesture, change, animation, pace, pending]
            .into_iter()
            .flatten()
            .min()
    }

    /// Whether more content changes could make the next frame sooner. While one is held back by
    /// the pace they can't, so `main` needn't wake for them.
    pub fn wants_content(&self, now: &Now) -> bool {
        !self.pacer.pending() || self.pacer.due(now.instant)
    }
}
