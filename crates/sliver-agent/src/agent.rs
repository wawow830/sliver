//! The agent core: config + context → resolved layers → frames. Sans-IO: the caller feeds it
//! events and time, and presents the frames it returns (see `run` in main.rs and the tests).

use std::collections::HashMap;

use chrono::{DateTime, Local};
use sliver_config::{Config, Context, Resolved};
use sliver_ipc::proto::{DisplayState, Layer};
use sliver_render::{Renderer, Stylesheet};

use crate::frame::{self, Frame};
use crate::icons::Icons;
use crate::modules::{self, Cell, UiState, World};

/// Icons are looked up at this size unless a module says otherwise.
const ICON_LOOKUP_SIZE: u32 = 48;

pub struct Agent {
    pub config: Config,
    pub sheet: Stylesheet,
    renderer: Renderer,
    icons: Icons,
    pub width: u32,
    pub height: u32,
    pub ctx: Context,
    /// Custom layers opened over `base` (last = on top).
    pub layers: Vec<String>,
    ui: HashMap<String, UiState>,
    pub display: DisplayState,
    /// sliverd's safe hatch is showing its own layer.
    pub hatch: bool,
    frames: [Option<Frame>; 2],
    /// Problems resolving layers, for logging/status. Cleared on success.
    pub errors: Vec<String>,
}

impl Agent {
    pub fn new(
        config: Config,
        sheet: Stylesheet,
        renderer: Renderer,
        icons: Icons,
        width: u32,
        height: u32,
    ) -> Agent {
        let mut ctx = Context::new();
        // tiny-dfr's heuristic (docs/hardware.md): only the 2170 px bars lack a physical Esc.
        ctx.set("physical_esc", width < 2170);
        Agent {
            config,
            sheet,
            renderer,
            icons,
            width,
            height,
            ctx,
            layers: Vec::new(),
            ui: HashMap::new(),
            display: DisplayState::On,
            hatch: false,
            frames: [None, None],
            errors: Vec::new(),
        }
    }

    /// The config layer currently shown in sliverd's `base` slot.
    pub fn base_layer(&self) -> &str {
        self.layers.last().map_or("base", String::as_str)
    }

    pub fn frame(&self, layer: Layer) -> Option<&Frame> {
        self.frames[slot(layer)].as_ref()
    }

    pub fn ui(&mut self, key: &str) -> &mut UiState {
        self.ui.entry(key.to_owned()).or_default()
    }

    /// Re-resolves and re-renders both layers for `world`. Returns the layers whose frames were
    /// rebuilt (callers present them; the link drops unchanged pixels).
    pub fn render(&mut self, world: &World) -> Vec<Layer> {
        self.errors.clear();
        let mut out = Vec::new();
        let base = self.base_layer().to_owned();
        for (layer, name) in [(Layer::Base, base.as_str()), (Layer::Fn, "fn")] {
            match self.config.resolve(name, &self.ctx) {
                Ok(r) => {
                    let f = self.build(&r, world);
                    self.frames[slot(layer)] = Some(f);
                    out.push(layer);
                }
                Err(e) => self.errors.push(format!("layer {name}: {e}")),
            }
        }
        out
    }

    fn build(&mut self, r: &Resolved, world: &World) -> Frame {
        let ui = &self.ui;
        let lookup = |k: &str| ui.get(k).cloned().unwrap_or_default();
        let per_zone: Vec<Vec<Cell>> = r
            .zones
            .iter()
            .map(|z| {
                let mut cells = Vec::new();
                modules::cells(
                    &z.modules,
                    world,
                    &mut self.icons,
                    ICON_LOOKUP_SIZE,
                    &lookup,
                    &mut cells,
                );
                cells
            })
            .collect();
        let classes = vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)];
        let scene = frame::scene(&r.zones, &per_zone, classes);
        let cells = per_zone.into_iter().flatten().collect();
        frame::render(
            &mut self.renderer,
            &self.sheet,
            &scene,
            cells,
            self.width,
            self.height,
        )
    }

    /// When some module's content changes on its own next (the earliest clock tick), if ever.
    /// Nothing ticks while the display is off (invariant 3).
    pub fn next_change(&self) -> Option<DateTime<Local>> {
        if self.display == DisplayState::Off {
            return None;
        }
        self.frames
            .iter()
            .flatten()
            .flat_map(|f| f.cells.iter().filter_map(|c| c.next_change))
            .min()
    }
}

fn slot(layer: Layer) -> usize {
    match layer {
        Layer::Base => 0,
        Layer::Fn => 1,
    }
}
