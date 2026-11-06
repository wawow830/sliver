//! The module runtime: turns resolved [`ModuleInstance`]s into drawable [`Cell`]s
//! (sliver-config(5)).
//!
//! Every module goes through the same path: gather values → `format_module` (waybar's
//! format/format-icons/states engine) → content + classes. Built-ins differ only in where their
//! values come from, so user modules get the same powers.

use chrono::format::{Item as FmtItem, StrftimeItems};
use chrono::{DateTime, Local, Timelike};
use sliver_config::{
    Action, FireOn, FmtValue, Gesture, ModuleInstance, State, ValueMap, Values, format_module,
};
use std::collections::HashMap;

use sliver_config::Context;
use sliver_desktop::DesktopState;
use sliver_modules::Output;
use sliver_render::{Content, Width};
use sliver_session::{BatteryState, SessionState};

use crate::act::{Act, Do, MenuItem, UiChange, Verbs};
use crate::icons::Icons;

/// What a touch on a cell does.
#[derive(Debug, Clone, PartialEq)]
pub enum Touch {
    /// Not touchable.
    None,
    /// sliverd handles it alone: press/release these keys (key modules with only a key on tap).
    Keys(Vec<u16>),
    /// Touches come to the agent for gesture recognition.
    Agent,
}

/// One drawable module.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Stable identity across frames: the module id plus its position in groups.
    pub key: String,
    /// The module id (`custom/vpn#work`), as the user module runtime knows it.
    pub id: String,
    /// The module type (`clock`, `audio`, …).
    pub kind: String,
    pub css_id: String,
    pub classes: Vec<String>,
    pub width: Width,
    pub content: Content,
    pub touch: Touch,
    /// Gesture bindings, for the agent's recognizer.
    pub on: Vec<(Gesture, Do)>,
    /// When this cell's content changes next on its own (clock ticks), if ever.
    pub next_change: Option<DateTime<Local>>,
    /// Being touched (agent regions; sliverd draws key regions' feedback itself).
    pub pressed: bool,
    /// A toggle that is on (`:checked`).
    pub checked: bool,
    /// This cell is the handle of a drawer group: tapping it opens/closes the group.
    pub drawer: Option<Drawer>,
    /// `:disabled`.
    pub disabled: bool,
    /// Inline colours (published items' `fg`/`bg`).
    pub foreground: Option<sliver_render::Color>,
    pub background: Option<sliver_render::Color>,
    /// A published item: touches go to the app that owns it.
    pub published: Option<crate::published::PubRef>,
    /// Published items: also send the app its event when a user binding handled the gesture.
    pub pass_through: bool,
    /// Part of a sized App controls / Status items slot (`width` = px or fill), by the slot's
    /// key: its published items, and the trailing `kind = "slot"` spacer that reserves the rest.
    pub slot: Option<String>,
    /// `spectrum = true`: the live spectrum's levels, drawn under the content. Empty: none.
    pub spectrum: Vec<f32>,
}

impl Cell {
    /// A cell showing `content` that does nothing yet.
    pub fn new(
        key: impl Into<String>,
        id: impl Into<String>,
        kind: impl Into<String>,
        css_id: impl Into<String>,
        content: Content,
    ) -> Cell {
        Cell {
            key: key.into(),
            id: id.into(),
            kind: kind.into(),
            css_id: css_id.into(),
            classes: Vec::new(),
            width: Width::Auto,
            content,
            touch: Touch::None,
            on: Vec::new(),
            next_change: None,
            pressed: false,
            checked: false,
            drawer: None,
            disabled: false,
            foreground: None,
            background: None,
            published: None,
            pass_through: false,
            slot: None,
            spectrum: Vec::new(),
        }
    }

    /// The binding for gesture `g`, if any.
    pub fn binding(&self, g: Gesture) -> Option<&Do> {
        self.on.iter().find(|(x, _)| *x == g).map(|(_, d)| d)
    }

    /// A tap here does `d` (handled by the agent).
    pub(crate) fn tapped(mut self, d: impl Into<Do>) -> Cell {
        self.on = vec![(Gesture::Tap, d.into())];
        self.touch = Touch::Agent;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    /// The group's key (for [`UiState::expanded`]).
    pub group: String,
    /// Close again this long after opening (`drawer.collapse-after`).
    pub collapse_after: Option<std::time::Duration>,
    pub opens: Opens,
}

/// Where a drawer's contents show when it opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Opens {
    /// In place: the group's other modules join the bar.
    Inline,
    /// `mode = "bar"`: the group's other modules cover the bar as a sub-bar.
    SubBar,
    /// A `collapsed = true` slider: its track replaces its icon.
    Slider,
}

/// Builds the cells for one zone's modules. Hidden modules produce nothing; groups flatten.
pub fn cells(
    modules: &[ModuleInstance],
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &dyn Fn(&str) -> UiState,
    out: &mut Vec<Cell>,
) {
    cells_in(modules, "", world, icons, icon_size, ui, out);
}

fn cells_in(
    modules: &[ModuleInstance],
    prefix: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &dyn Fn(&str) -> UiState,
    out: &mut Vec<Cell>,
) {
    for m in modules {
        let key = format!("{prefix}{}", m.id);
        let strip = m.config.get("mode").and_then(|v| v.as_str()) == Some("strip");
        match m.id.kind.as_str() {
            "group" => group_cells(m, &key, world, icons, icon_size, ui, out),
            "app-controls" | "status-items" => {
                placeholder_cells(m, &key, prefix, world, icons, icon_size, out)
            }
            // A published item (or a whole context) placed by the layout: `app/<provider>#<item>`
            // (api.md §3.1). It's styled, bound and formatted like any module.
            "app" => pinned_app(m, &key, world, icons, icon_size, out),
            "emoji" if strip => emoji_strip(m, &key, world, &ui(&key).emoji, out),
            // Any other module is a cell, or one per item it lists (workspaces, windows, who's
            // recording).
            _ => {
                let cells: Vec<(String, Option<Item>)> = match items(m, world) {
                    Some(list) => list
                        .into_iter()
                        .map(|(suffix, item)| (format!("{key}/{suffix}"), Some(item)))
                        .collect(),
                    None => vec![(key, None)],
                };
                for (key, item) in cells {
                    let ui = ui(&key);
                    let at = At {
                        m,
                        key: &key,
                        world,
                        ui: &ui,
                        item,
                    };
                    out.extend(cell(at, icons, icon_size));
                }
            }
        }
    }
}

/// Text from outside (titles, names) is escaped so it can't inject markup.
fn esc(s: &str) -> FmtValue {
    FmtValue::Text(sliver_render::markup::escape(s))
}

/// `image` pictures are kept at most this tall: twice the 60 px bar, so scaling down stays sharp.
pub(crate) const IMAGE_MAX_HEIGHT: u32 = 120;

mod apps;
mod clock;
mod desktop;
mod emoji;
mod groups;
mod media;
mod menus;
mod placed;
mod spec;
mod spectrum;
mod system;
mod widgets;
mod world;

use apps::*;
use clock::*;
use desktop::*;
use emoji::*;
pub use groups::*;
pub use menus::*;
pub use placed::*;
use spec::*;
pub use spectrum::*;
use system::*;
pub use world::*;

#[cfg(test)]
mod tests;
