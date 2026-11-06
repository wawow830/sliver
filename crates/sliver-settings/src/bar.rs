//! The Bar page: live preview of any layer in a simulated context, and editing of the
//! layout it shows (add from the Library, reorder, remove, inspect), plus "Customize for app…".

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Context, Gesture, ModuleRef, Width, parse_cli_value};
use toml::Value;

use crate::pages::{group, row, surface, width_text};
use crate::preview::{Feedback, Preview, width_note};
use crate::{Ui, apply, edit};

mod groups;
mod inspector;
mod keyboard;
mod library;
mod select;
mod zones;

use groups::*;
pub(crate) use inspector::*;
use keyboard::*;
use library::*;
use select::*;
use zones::*;

/// Feedback drawn on the preview while dragging (widget x coordinates).
#[derive(Clone, Copy, Default)]
enum Cue {
    #[default]
    None,
    /// Where a moved or dropped module goes.
    Insert(f64),
    /// The module that leaves the bar when released here.
    Remove(f64, f64),
    /// The module's new extent while dragging its right edge.
    Resize(f64, f64),
}

/// Dragged this far (px) above or below the preview, a module is removed on release.
const OFF_BAR: f64 = 40.0;
/// The narrowest width the edge handle sets (bar px).
const MIN_WIDTH: f32 = 30.0;

/// What the Library offers. Anything else can be typed as a module reference.
const LIBRARY: &[(&str, &str)] = &[
    ("clock", "Clock"),
    ("battery", "Battery"),
    ("mpris", "Now playing"),
    ("workspaces", "Workspaces"),
    ("backlight/slider", "Brightness slider"),
    ("audio/slider", "Volume slider"),
    ("kbd-backlight/slider", "Keyboard light slider"),
    ("key/Mute", "Mute key"),
    ("key/Esc", "Esc key"),
    ("idle-inhibitor", "Caffeine"),
    ("dnd", "Do not disturb"),
    ("dark-mode", "Dark mode"),
    ("screenshot", "Screenshot"),
    ("lock", "Lock screen"),
    ("power", "Power menu"),
    ("language", "Keyboard layout"),
    ("window", "Window title"),
    ("cpu", "CPU"),
    ("memory", "Memory"),
    ("graph#cpu", "CPU graph"),
    ("graph#memory", "Memory graph"),
    ("network", "Network"),
    ("spacer", "Spacer"),
    ("app-controls", "App controls (focused app's buttons)"),
    ("status-items", "Status items (background apps)"),
];

/// The preview's overlay state: the rendered bar and what's drawn over it. Shared by the draw
/// function and the controllers that change it.
#[derive(Clone, Default)]
struct Marks {
    /// The rendered bar, full scale (drawn at half).
    frame: Rc<RefCell<Option<gtk::cairo::ImageSurface>>>,
    /// Feedback while dragging (widget px).
    caret: Rc<Cell<Cue>>,
    /// Overflow and small touch targets in the current render.
    sizes: Rc<RefCell<Feedback>>,
    /// The keyboard-selected module's span (bar px), ringed while the preview has focus.
    focus: Rc<Cell<Option<(f32, f32)>>>,
    /// The Shift/Ctrl-click multi-selection's spans (bar px), tinted and ringed.
    multi: Rc<RefCell<Vec<(f32, f32)>>>,
}

impl Marks {
    /// Paints the bar at half scale, then the size feedback, selections and drag cue over it.
    fn draw(&self, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, h: f64) {
        if let Some(s) = self.frame.borrow().as_ref() {
            cr.save().ok();
            cr.scale(0.5, 0.5);
            let _ = cr.set_source_surface(s, 0.0, 0.0);
            cr.source().set_filter(gtk::cairo::Filter::Best);
            let _ = cr.paint();
            cr.restore().ok();
        }
        // Bar px -> widget px (half scale). Overflowing zones: a red wash and a red right edge.
        let fb = self.sizes.borrow();
        for (_, x, w) in &fb.overflow {
            let (x, w) = (f64::from(*x) / 2.0, f64::from(*w) / 2.0);
            cr.set_source_rgba(0.88, 0.11, 0.14, 0.18);
            cr.rectangle(x, 0.0, w, h);
            let _ = cr.fill();
            cr.set_source_rgba(0.88, 0.11, 0.14, 0.9);
            cr.rectangle(x + w - 2.0, 0.0, 2.0, h);
            let _ = cr.fill();
        }
        // Small touch targets: a bar under the item, amber below 80 px, red below 60 px.
        for (_, (x, w), strong) in &fb.narrow {
            let (x, w) = (f64::from(*x) / 2.0, f64::from(*w) / 2.0);
            if *strong {
                cr.set_source_rgba(0.88, 0.11, 0.14, 0.95);
            } else {
                cr.set_source_rgba(0.96, 0.62, 0.0, 0.95);
            }
            cr.rectangle(x + 1.0, h - 3.0, (w - 2.0).max(1.0), 3.0);
            let _ = cr.fill();
        }
        drop(fb);
        for (x, w) in self.multi.borrow().iter() {
            let (x, w) = (f64::from(*x) / 2.0, f64::from(*w) / 2.0);
            cr.set_source_rgba(0.21, 0.52, 0.89, 0.22);
            cr.rectangle(x + 1.0, 1.0, (w - 2.0).max(1.0), h - 2.0);
            let _ = cr.fill_preserve();
            cr.set_source_rgb(0.21, 0.52, 0.89);
            cr.set_line_width(2.0);
            let _ = cr.stroke();
        }
        if area.has_focus()
            && let Some((x, w)) = self.focus.get()
        {
            let (x, w) = (f64::from(x) / 2.0, f64::from(w) / 2.0);
            cr.set_source_rgb(0.21, 0.52, 0.89);
            cr.set_line_width(2.0);
            cr.rectangle(x + 1.0, 1.0, (w - 2.0).max(1.0), h - 2.0);
            let _ = cr.stroke();
        }
        match self.caret.get() {
            Cue::None => {}
            Cue::Insert(x) => {
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.rectangle(x - 1.5, 0.0, 3.0, h);
                let _ = cr.fill();
            }
            Cue::Remove(x0, x1) => {
                cr.set_source_rgba(0.88, 0.11, 0.14, 0.45);
                cr.rectangle(x0, 0.0, x1 - x0, h);
                let _ = cr.fill();
            }
            Cue::Resize(x0, x1) => {
                cr.set_source_rgba(0.21, 0.52, 0.89, 0.25);
                cr.rectangle(x0, 0.0, x1 - x0, h);
                let _ = cr.fill();
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.rectangle(x1 - 1.5, 0.0, 3.0, h);
                let _ = cr.fill();
            }
        }
    }
}

/// What the preview's controllers share.
#[derive(Clone)]
struct View {
    ui: Rc<Ui>,
    preview: Rc<RefCell<Preview>>,
    /// The layout the preview shows, which is what edits apply to.
    shown_layout: Rc<RefCell<String>>,
    /// Weak: the preview's own controllers hold a `View`, so a strong reference would keep the
    /// page (and its renderer) alive after a rebuild replaces it.
    picture: glib::WeakRef<gtk::DrawingArea>,
    marks: Marks,
}

impl View {
    fn redraw(&self) {
        if let Some(p) = self.picture.upgrade() {
            p.queue_draw();
        }
    }

    fn clear_cue(&self) {
        self.marks.caret.set(Cue::None);
        self.redraw();
    }

    /// Zone `zone` of the shown layout: its name and module ids.
    fn zone(&self, zone: usize) -> Option<(String, Vec<String>)> {
        let state = self.ui.state();
        let zones = state
            .loaded
            .config
            .layout_zones(&self.shown_layout.borrow())
            .ok()?;
        let z = zones.get(zone)?;
        Some((
            z.name.clone(),
            z.modules.iter().map(|m| m.id().to_string()).collect(),
        ))
    }

    /// The drag cue for inserting at widget x: where the module will really land (after a
    /// whole group, not inside it).
    fn insert_cue(&self, x: f64) -> Cue {
        match self.preview.borrow().insertion_x(x as f32 * 2.0) {
            Some(bx) => Cue::Insert(bx as f64 / 2.0),
            None => Cue::None,
        }
    }
}

/// The controls above the preview.
struct Header {
    row: gtk::Box,
    layers: Vec<String>,
    layer: gtk::DropDown,
    app: gtk::Entry,
    customize: gtk::Button,
    locked: gtk::Switch,
    mirror: gtk::Switch,
}

impl Header {
    fn new(ui: &Rc<Ui>) -> Header {
        let state = ui.state();
        let layers: Vec<String> = state.loaded.config.layers.keys().cloned().collect();
        let names: Vec<&str> = layers.iter().map(String::as_str).collect();
        let layer = gtk::DropDown::from_strings(&names);
        let want = ui.layer.borrow().clone();
        layer.set_selected(
            layers
                .iter()
                .position(|l| *l == want)
                .or_else(|| layers.iter().position(|l| l == "base"))
                .unwrap_or(0) as u32,
        );
        let app = gtk::Entry::builder()
            .placeholder_text("app id, e.g. firefox")
            .width_chars(20)
            .text(ui.app.borrow().as_str())
            .build();
        let customize = gtk::Button::with_label("Customize for app…");
        customize.set_tooltip_text(Some("Give this app its own layout, based on the one shown"));
        let locked = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(ui.locked.get())
            .build();
        let mirror = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(ui.mirror.get())
            .build();
        mirror.set_tooltip_text(Some(
            "Show the layout you're editing on the real Touch Bar while this window is focused",
        ));
        // Touches on the mirrored bar always select here; this also stops them firing.
        let select_only = gtk::CheckButton::builder()
            .label("Select only")
            .active(ui.select_only.get())
            .sensitive(ui.mirror.get())
            .margin_start(8)
            .tooltip_text("Touching the real bar selects the item here without running its action")
            .build();
        {
            let ui = ui.clone();
            select_only.connect_toggled(move |c| ui.set_select_only(c.is_active()));
        }
        mirror
            .bind_property("active", &select_only, "sensitive")
            .build();

        let label = |text: &str| gtk::Label::builder().label(text).margin_start(16).build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Label::new(Some("Layer")));
        row.append(&layer);
        row.append(&label("Preview as"));
        row.append(&app);
        row.append(&customize);
        row.append(&label("Locked"));
        row.append(&locked);
        row.append(&label("Mirror"));
        row.append(&mirror);
        row.append(&select_only);
        Header {
            row,
            layers,
            layer,
            app,
            customize,
            locked,
            mirror,
        }
    }

    /// The layer, app and lock switches re-render; Customize and Mirror act on the shown layout.
    fn connect(&self, v: &View, refresh: &Hook) {
        let r = refresh.clone();
        self.layer.connect_selected_notify(move |_| r());
        let r = refresh.clone();
        self.locked.connect_active_notify(move |_| r());
        let r = refresh.clone();
        self.app.connect_changed(move |_| r());
        {
            let (v, app) = (v.clone(), self.app.clone());
            self.customize.connect_clicked(move |_| {
                let app = app.text().to_string();
                let base = v.shown_layout.borrow().clone();
                *v.ui.layer.borrow_mut() = "base".into();
                apply(&v.ui, |ed, merged| {
                    edit::customize_for_app(ed, merged, &app, &base).map(|(_, msg)| msg)
                });
            });
        }
        let v = v.clone();
        self.mirror.connect_active_notify(move |m| {
            v.ui.mirror.set(m.is_active());
            if m.is_active() {
                v.ui.mirror_layout(&v.shown_layout.borrow());
            } else {
                v.ui.stop_mirror();
            }
        });
    }
}

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(18);
    page.set_margin_bottom(18);
    page.set_margin_start(18);
    page.set_margin_end(18);
    let header = Header::new(ui);
    page.append(&header.row);

    // Drawn at half scale (1004x30 or 1085x30) so it stays legible; narrower windows scroll
    // sideways.
    let size = crate::preview::bar_size(state.hw);
    let marks = Marks::default();
    let picture = gtk::DrawingArea::builder()
        .content_width(size.0 as i32 / 2)
        .content_height(size.1 as i32 / 2)
        .halign(gtk::Align::Center)
        // Room for the overlay scrollbar, so it never sits on (and swallows clicks on) the bar.
        .margin_bottom(12)
        .build();
    {
        let marks = marks.clone();
        picture.set_draw_func(move |area, cr, _, h| marks.draw(area, cr, h as f64));
    }
    let bar_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&picture)
        .build();
    page.append(&bar_scroll);
    let v = View {
        ui: ui.clone(),
        preview: Rc::new(RefCell::new(Preview::new(
            state.loaded.config.clone(),
            state.sheet.clone(),
            size,
        ))),
        shown_layout: Rc::default(),
        picture: picture.downgrade(),
        marks,
    };

    let (multi_bar, update_multi) = multi_select(&v);
    page.append(&multi_bar);
    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    why.add_css_class("dim-label");
    page.append(&why);
    let size_warning = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    size_warning.add_css_class("warning");
    page.append(&size_warning);
    let zones = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&zones)
            .build(),
    );

    let refresh = refresher(
        &v,
        &header,
        &why,
        &size_warning,
        &zones,
        update_multi.clone(),
    );
    header.connect(&v, &refresh);
    connect_select(&v, &picture, update_multi);
    connect_hover(&v, &picture);
    let (drop_target, drop_module) = drop_target(&v);
    let lib = palette(ui, &v.shown_layout);
    page.insert_child_after(&lib, Some(&bar_scroll));
    page.add_controller(drag_gesture(&v, drop_module));
    picture.add_controller(drop_target);
    keyboard_access(ui, &picture, &v.preview, &v.shown_layout, &v.marks.focus);
    refresh();
    page.upcast()
}

/// Renders the chosen layer for the simulated context (layer, app, locked), then rebuilds the
/// zone list, the "why this layout" line and the size warnings under it.
fn refresher(
    v: &View,
    h: &Header,
    why: &gtk::Label,
    size_warning: &gtk::Label,
    zones: &gtk::Box,
    update_multi: Hook,
) -> Hook {
    let v = v.clone();
    // Weak: these three run this function when they change, so they hold it.
    let (layers, layer_dd, app_entry, locked) = (
        h.layers.clone(),
        h.layer.downgrade(),
        h.app.downgrade(),
        h.locked.downgrade(),
    );
    let (why, size_warning, zones) = (why.clone(), size_warning.clone(), zones.clone());
    Rc::new(move || {
        let (Some(layer_dd), Some(app_entry), Some(locked)) =
            (layer_dd.upgrade(), app_entry.upgrade(), locked.upgrade())
        else {
            return;
        };
        let ui = &v.ui;
        let state = ui.state();
        let layer = layers
            .get(layer_dd.selected() as usize)
            .cloned()
            .unwrap_or_else(|| "base".into());
        *ui.layer.borrow_mut() = layer.clone();
        *ui.app.borrow_mut() = app_entry.text().to_string();
        ui.locked.set(locked.is_active());
        let mut ctx = Context::new()
            .with("physical_esc", state.hw.physical_esc)
            .with("locked", locked.is_active());
        if !app_entry.text().is_empty() {
            ctx.set("app_id", app_entry.text().as_str());
        }
        while let Some(c) = zones.first_child() {
            zones.remove(&c);
        }
        let config = &state.loaded.config;
        let (layout, rule) = match config.select_layout(&layer, &ctx) {
            Ok((l, r)) => (l.to_owned(), r),
            Err(e) => {
                why.set_text(&format!("Can't show this layer: {e}"));
                return;
            }
        };
        *v.shown_layout.borrow_mut() = layout.clone();
        // Only base-layer layouts are mirrored: the agent's SetLayout replaces the base layer.
        if layer == "base" {
            ui.mirror_layout(&layout);
        } else {
            ui.stop_mirror();
        }
        match v.preview.borrow_mut().render(&layer, ctx.clone()) {
            Ok(p) => *v.marks.frame.borrow_mut() = surface(&p),
            Err(e) => {
                *v.marks.frame.borrow_mut() = None;
                zones.append(&row("Can't render", &e));
            }
        }
        let fb = v.preview.borrow().feedback();
        size_warning.set_visible(fb.summary().is_some());
        size_warning.set_text(&fb.summary().unwrap_or_default());
        // Overflow is red, small targets amber (libadwaita style classes).
        size_warning.set_css_classes(if fb.overflow.is_empty() {
            &["warning"]
        } else {
            &["error"]
        });
        *v.marks.sizes.borrow_mut() = fb;
        v.redraw();
        let profile = sliver_config::profile::active();
        let in_profile = if profile == sliver_config::profile::DEFAULT {
            String::new()
        } else {
            format!("Profile “{profile}”: ")
        };
        why.set_text(&format!("{in_profile}{}", match rule {
            Some(i) => format!(
                "Editing layout “{layout}”, picked by rules[{i}]{}. Click a module to edit it; changes apply instantly.",
                config.rules[i].name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
            ),
            None => format!("Editing layout “{layout}”, the {layer} layer's default. Click a module to edit it; changes apply instantly."),
        }));
        zone_groups(ui, &zones, &layout);
        update_multi();
    })
}
