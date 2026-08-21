//! sliver-edit: the customizer. Drag-free v1: a live preview of the strip,
//! a widget list, property fields, and Apply — which pushes the current
//! layout down sliverd's socket straight onto the glass.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::rc::Rc;

use anyhow::{Context, Result};
use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use sliver_core::{Config, WidgetCfg};

const CONFIG_PATH: &str = "sliver.toml";

/// Everything the UI touches, shared by reference: the config being
/// edited, the selected widget index, and handles to the living widgets
/// that need refreshing.
struct Editor {
    cfg: Rc<RefCell<Config>>,
    selected: Rc<Cell<Option<usize>>>,
    preview: gtk::DrawingArea,
    list: gtk::ListBox,
    editor_box: gtk::Box,
    toasts: adw::ToastOverlay,
}

fn main() -> Result<()> {
    let app = adw::Application::builder()
        .application_id("dev.sliver.edit")
        .build();
    app.connect_activate(build_ui);
    app.run();
    Ok(())
}

fn load_or_default() -> Config {
    sliver_core::load_config(CONFIG_PATH.as_ref()).unwrap_or_else(|_| Config {
        background: "#111111".into(),
        widgets: vec![
            WidgetCfg::Label { text: "sliver".into(), color: Some("#ff88aa".into()), width: Some(160.0) },
            WidgetCfg::Spacer { flex: 1.0 },
            WidgetCfg::Clock { format: "%a %H:%M".into(), color: Some("#aaddff".into()), width: Some(260.0) },
        ],
    })
}

fn build_ui(app: &adw::Application) {
    let cfg = Rc::new(RefCell::new(load_or_default()));
    let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    // --- the preview: sliver-core's own renderer, scaled to fit ----------
    let preview = gtk::DrawingArea::builder()
        .hexpand(true)
        .height_request(56)
        .build();
    {
        let cfg = cfg.clone();
        preview.set_draw_func(move |area, cr, _w, _h| {
            let scale = f64::from(area.width()) / sliver_core::STRIP_W;
            let _ = cr.scale(scale, scale);
            let _ = sliver_core::render(&cfg.borrow(), cr, None);
        });
    }
    let preview_frame = gtk::Frame::builder()
        .margin_top(12)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .child(&preview)
        .build();

    // --- the widget list ---------------------------------------------------
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    list.add_css_class("boxed-list");

    // --- palette and arrangement buttons ----------------------------------
    let palette = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    palette.set_halign(gtk::Align::Center);
    palette.set_margin_bottom(6);

    // --- the per-widget property editor ------------------------------------
    let editor_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    editor_box.set_margin_top(6);
    editor_box.set_margin_bottom(12);
    editor_box.set_margin_start(12);
    editor_box.set_margin_end(12);

    let toasts = adw::ToastOverlay::new();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("sliver customizer")
        .default_width(860)
        .build();

    let editor = Editor {
        cfg,
        selected,
        preview,
        list,
        editor_box,
        toasts,
    };

    // Header bar with the two verbs that matter.
    let header = adw::HeaderBar::new();
    let apply_btn = gtk::Button::with_label("Apply to strip");
    apply_btn.add_css_class("suggested-action");
    {
        let ed = editor.clone_rc();
        apply_btn.connect_clicked(move |_| match apply_to_daemon(&ed.cfg.borrow()) {
            Ok(()) => toast(&ed, "The strip obeys."),
            Err(e) => toast(&ed, &format!("apply failed: {e:#}")),
        });
    }
    let save_btn = gtk::Button::with_label("Save");
    {
        let ed = editor.clone_rc();
        save_btn.connect_clicked(move |_| match save_config(&ed.cfg.borrow()) {
            Ok(()) => toast(&ed, &format!("saved to {CONFIG_PATH}")),
            Err(e) => toast(&ed, &format!("save failed: {e:#}")),
        });
    }
    header.pack_end(&apply_btn);
    header.pack_end(&save_btn);

    // Palette buttons add widgets and select them.
    for (label, make) in [
        ("+ Label", (|| WidgetCfg::Label { text: "new label".into(), color: None, width: Some(160.0) }) as fn() -> WidgetCfg),
        ("+ Clock", (|| WidgetCfg::Clock { format: "%H:%M".into(), color: None, width: Some(160.0) }) as fn() -> WidgetCfg),
        ("+ Battery", (|| WidgetCfg::Battery { format: "{capacity}%".into(), color: None, width: Some(160.0) }) as fn() -> WidgetCfg),
        ("+ Spacer", (|| WidgetCfg::Spacer { flex: 1.0 }) as fn() -> WidgetCfg),
    ] {
        let btn = gtk::Button::with_label(label);
        let ed = editor.clone_rc();
        btn.connect_clicked(move |_| {
            let mut c = ed.cfg.borrow_mut();
            c.widgets.push(make());
            ed.selected.set(Some(c.widgets.len() - 1));
            drop(c);
            refresh_list(&ed);
            rebuild_editor(&ed);
            ed.preview.queue_draw();
        });
        palette.append(&btn);
    }

    for (label, shift) in [("◀ move", -1isize), ("move ▶", 1isize)] {
        let btn = gtk::Button::with_label(label);
        let ed = editor.clone_rc();
        btn.connect_clicked(move |_| {
            let Some(i) = ed.selected.get() else { return };
            let mut c = ed.cfg.borrow_mut();
            let n = c.widgets.len();
            let j = i as isize + shift;
            if j < 0 || j >= n as isize {
                return;
            }
            c.widgets.swap(i, j as usize);
            ed.selected.set(Some(j as usize));
            drop(c);
            refresh_list(&ed);
            ed.preview.queue_draw();
        });
        palette.append(&btn);
    }
    {
        let btn = gtk::Button::with_label("delete");
        let ed = editor.clone_rc();
        btn.connect_clicked(move |_| {
            let Some(i) = ed.selected.get() else { return };
            let mut c = ed.cfg.borrow_mut();
            if i < c.widgets.len() {
                c.widgets.remove(i);
            }
            ed.selected.set(None);
            drop(c);
            refresh_list(&ed);
            rebuild_editor(&ed);
            ed.preview.queue_draw();
        });
        palette.append(&btn);
    }

    // List selection drives the editor.
    {
        let ed = editor.clone_rc();
        editor.list.connect_row_selected(move |_lb, row| {
            ed.selected.set(row.map(|r| r.index() as usize).filter(|&i| i >= 0).map(|i| i as usize));
            rebuild_editor(&ed);
        });
    }

    refresh_list(&editor);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&preview_frame);
    content.append(&gtk::Label::new(Some("widgets — order is left-to-right on the strip")));
    content.append(&editor.list);
    content.append(&palette);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&editor.editor_box);

    let scroller = gtk::ScrolledWindow::builder().child(&content).build();
    editor.toasts.set_child(Some(&scroller));

    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&header);
    layout.append(&editor.toasts);
    window.set_content(Some(&layout));

    window.present();
}

/// Editor borrows Rcs constantly; this keeps the lines tolerable.
trait CloneRc {
    fn clone_rc(&self) -> Editor;
}
impl CloneRc for Editor {
    fn clone_rc(&self) -> Editor {
        Editor {
            cfg: self.cfg.clone(),
            selected: self.selected.clone(),
            preview: self.preview.clone(),
            list: self.list.clone(),
            editor_box: self.editor_box.clone(),
            toasts: self.toasts.clone(),
        }
    }
}

fn toast(ed: &Editor, msg: &str) {
    ed.toasts.add_toast(adw::Toast::new(msg));
}

fn summarize(w: &WidgetCfg) -> String {
    match w {
        WidgetCfg::Label { text, .. } => format!("label — “{text}”"),
        WidgetCfg::Clock { format, .. } => format!("clock — {format}"),
        WidgetCfg::Battery { format, .. } => format!("battery — {format}"),
        WidgetCfg::Spacer { flex } => format!("spacer — flex {flex}"),
    }
}

fn refresh_list(ed: &Editor) {
    while let Some(row) = ed.list.row_at_index(0) {
        ed.list.remove(&row);
    }
    let sel = ed.selected.get();
    for (i, w) in ed.cfg.borrow().widgets.iter().enumerate() {
        let label = gtk::Label::builder()
            .label(summarize(w))
            .xalign(0.0)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .build();
        ed.list.append(&label);
        if sel == Some(i) {
            let row = ed.list.row_at_index(i as i32).unwrap();
            ed.list.select_row(Some(&row));
        }
    }
}

/// One labeled property row: caption on the left, widget on the right.
fn prop_row(caption: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::builder()
        .label(caption)
        .xalign(0.0)
        .hexpand(true)
        .build();
    row.append(&label);
    row.append(widget);
    row
}

fn rebuild_editor(ed: &Editor) {
    while let Some(child) = ed.editor_box.first_child() {
        ed.editor_box.remove(&child);
    }
    let Some(i) = ed.selected.get() else { return };
    let mut cfg = ed.cfg.borrow_mut();
    let Some(widget) = cfg.widgets.get_mut(i) else { return };

    match widget {
        WidgetCfg::Label { text, color, width } => {
            add_text_row(ed, i, "text", text);
            add_color_row(ed, i, color);
            add_width_row(ed, i, width);
        }
        WidgetCfg::Clock { format, color, width } => {
            add_text_row(ed, i, "format (strftime)", format);
            add_color_row(ed, i, color);
            add_width_row(ed, i, width);
        }
        WidgetCfg::Battery { format, color, width } => {
            add_text_row(ed, i, "format ({capacity})", format);
            add_color_row(ed, i, color);
            add_width_row(ed, i, width);
        }
        WidgetCfg::Spacer { flex } => {
            add_flex_row(ed, i, flex);
        }
    }
}

/// Shared tail for every edit: repaint the preview and rename the list row.
fn after_edit(ed: &Editor, i: usize) {
    ed.preview.queue_draw();
    if let Some(row) = ed.list.row_at_index(i as i32) {
        if let Some(child) = row.child() {
            if let Ok(label) = child.downcast::<gtk::Label>() {
                let text = summarize(&ed.cfg.borrow().widgets[i]);
                label.set_text(&text);
            }
        }
    }
}

fn add_text_row(ed: &Editor, i: usize, caption: &str, field: &mut String) {
    let entry = gtk::Entry::builder().text(field.as_str()).build();
    let cb = ed.clone_rc();
    entry.connect_changed(move |e| {
        let text = e.text().to_string();
        let mut cfg = cb.cfg.borrow_mut();
        if let Some(w) = cfg.widgets.get_mut(i) {
            match w {
                WidgetCfg::Label { text: t, .. } => *t = text,
                WidgetCfg::Clock { format: f, .. } | WidgetCfg::Battery { format: f, .. } => *f = text,
                _ => {}
            }
        }
        drop(cfg);
        after_edit(&cb, i);
    });
    ed.editor_box.append(&prop_row(caption, &entry));
}

fn add_color_row(ed: &Editor, i: usize, field: &mut Option<String>) {
    let entry = gtk::Entry::builder()
        .text(field.as_deref().unwrap_or(""))
        .placeholder_text("#rrggbb (empty = auto)")
        .build();
    let cb = ed.clone_rc();
    entry.connect_changed(move |e| {
        let raw = e.text().to_string();
        let value = (!raw.trim().is_empty()).then_some(raw);
        let mut cfg = cb.cfg.borrow_mut();
        if let Some(w) = cfg.widgets.get_mut(i) {
            match w {
                WidgetCfg::Label { color, .. }
                | WidgetCfg::Clock { color, .. }
                | WidgetCfg::Battery { color, .. } => *color = value,
                _ => {}
            }
        }
        drop(cfg);
        after_edit(&cb, i);
    });
    ed.editor_box.append(&prop_row("color", &entry));
}

fn add_width_row(ed: &Editor, i: usize, field: &mut Option<f64>) {
    let spin = gtk::SpinButton::with_range(0.0, 2000.0, 1.0);
    spin.set_value(field.unwrap_or(0.0));
    let cb = ed.clone_rc();
    spin.connect_value_changed(move |s| {
        let v = s.value();
        let mut cfg = cb.cfg.borrow_mut();
        if let Some(w) = cfg.widgets.get_mut(i) {
            match w {
                WidgetCfg::Label { width, .. }
                | WidgetCfg::Clock { width, .. }
                | WidgetCfg::Battery { width, .. } => *width = (v > 0.0).then_some(v),
                _ => {}
            }
        }
        drop(cfg);
        after_edit(&cb, i);
    });
    ed.editor_box.append(&prop_row("width px (0 = flex)", &spin));
}

fn add_flex_row(ed: &Editor, i: usize, field: &mut f64) {
    let spin = gtk::SpinButton::with_range(0.0, 64.0, 0.5);
    spin.set_value(*field);
    let cb = ed.clone_rc();
    spin.connect_value_changed(move |s| {
        let v = s.value();
        if let Some(WidgetCfg::Spacer { flex }) = cb.cfg.borrow_mut().widgets.get_mut(i) {
            *flex = v.max(0.1);
        }
        after_edit(&cb, i);
    });
    ed.editor_box.append(&prop_row("flex", &spin));
}

/// Write the config back to disk for the daemon's next cold start.
fn save_config(cfg: &Config) -> Result<()> {
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(CONFIG_PATH, text)?;
    Ok(())
}

/// One connection, one TOML document, one-word reply — the same protocol
/// as `sliverd --apply`. The daemon's socket is the whole bridge.
fn apply_to_daemon(cfg: &Config) -> Result<()> {
    let text = toml::to_string_pretty(cfg)?;
    let mut stream = std::os::unix::net::UnixStream::connect(sliver_core::socket_path())
        .context("connecting to sliverd (is it running?)")?;
    stream.write_all(text.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    if reply.trim() == "ok" {
        Ok(())
    } else {
        anyhow::bail!(reply.trim().to_string())
    }
}
