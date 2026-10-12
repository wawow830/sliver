//! `sliver-settings`: the settings app and WYSIWYG Touch Bar editor (GTK4 + libadwaita).
//!
//! docs/ux.md is the design. Every edit goes through `sliver_config::Editor` on the user's config
//! file: it's applied instantly (Q3), validated before it's written (a broken config never lands),
//! and can be undone from the toast. The running agent hot-reloads the file.

mod bar;
mod edit;
mod live;
mod pages;
mod prefs;
mod preview;
mod rules;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Editor, Hardware, Loaded};
use toml::Table;

use preview::Preview;

/// D-Bus-style app id under the project prefix (H1).
const APP_ID: &str = "io.github.wawow830.Sliver.Settings";

pub(crate) struct State {
    /// The config file in use (`None` = built-in defaults only).
    pub path: Option<PathBuf>,
    pub loaded: Loaded,
    pub hw: Hardware,
    pub sheet: sliver_render::Stylesheet,
    pub problems: sliver_config::Diagnostics,
    pub style_problems: Vec<String>,
}

impl State {
    fn load() -> State {
        let path = sliver_config::find_config();
        let loaded = match &path {
            Some(p) => sliver_config::load_file(p),
            None => sliver_config::load(),
        };
        let hw = Hardware::detect();
        let mut problems = loaded.diagnostics.clone();
        if problems.ok() {
            problems.extend(loaded.config.validate(hw));
        }
        let dir = path.as_deref().and_then(|p| p.parent());
        let (sheet, style_problems) = Preview::stylesheet(&loaded.config, dir);
        State {
            path,
            loaded,
            hw,
            sheet,
            problems,
            style_problems,
        }
    }

    /// The file edits go to: always the user's own config. If the active config is a system file,
    /// the user's new file includes it, so nothing is lost.
    fn edit_target(&self) -> (PathBuf, Option<PathBuf>) {
        let user = sliver_config::search_path()
            .into_iter()
            .next()
            .unwrap_or_else(|| "config.toml".into());
        let include = self.path.clone().filter(|p| *p != user);
        (user, include)
    }
}

/// Window-level state that survives rebuilds (which happen after every edit).
pub(crate) struct Ui {
    win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    pub state: RefCell<Rc<State>>,
    pub page: RefCell<String>,
    pub layer: RefCell<String>,
    pub app: RefCell<String>,
    pub locked: Cell<bool>,
    /// Earlier contents of the user's file (`None` = it didn't exist), newest last.
    undo: RefCell<Vec<(Option<String>, String)>>,
    /// Watch the config's directories so hand edits (or `sliverctl config set`) refresh the window.
    monitors: RefCell<Vec<gtk::gio::FileMonitor>>,
    /// A refresh is already scheduled (debounce).
    refresh_pending: Cell<bool>,
    /// The user file's text as we last loaded or wrote it, to tell our own saves from others'.
    last_text: RefCell<Option<String>>,
    /// Mirror to Touch Bar (Q10: on by default) and what we last asked the agent to show.
    pub mirror: Cell<bool>,
    mirrored: RefCell<Option<String>>,
    /// The layout the Bar page shows, mirrored again when the window regains focus.
    mirror_wanted: RefCell<Option<String>>,
}

impl Ui {
    pub fn state(&self) -> Rc<State> {
        self.state.borrow().clone()
    }

    pub fn win_widget(&self) -> adw::ApplicationWindow {
        self.win.clone()
    }

    /// Shows `layout` on the real bar if mirroring is on and the window is focused.
    pub fn mirror_layout(&self, layout: &str) {
        *self.mirror_wanted.borrow_mut() = Some(layout.to_owned());
        if !self.mirror.get() || !self.win.is_active() {
            return;
        }
        if self.mirrored.borrow().as_deref() != Some(layout) {
            live::set_layout(layout);
            *self.mirrored.borrow_mut() = Some(layout.to_owned());
        }
    }

    /// Back to what the rules pick.
    pub fn stop_mirror(&self) {
        if self.mirrored.borrow_mut().take().is_some() {
            live::set_layout("");
        }
    }

    pub fn toast(&self, msg: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(msg)));
    }
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliver-settings {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Sliver")
            .default_width(1180)
            .default_height(760)
            .build();
        let toasts = adw::ToastOverlay::new();
        win.set_content(Some(&toasts));
        let ui = Rc::new(Ui {
            win: win.clone(),
            toasts,
            state: RefCell::new(Rc::new(State::load())),
            page: RefCell::new("bar".into()),
            layer: RefCell::new("base".into()),
            app: RefCell::default(),
            locked: Cell::new(false),
            undo: RefCell::default(),
            monitors: RefCell::default(),
            refresh_pending: Cell::new(false),
            last_text: RefCell::default(),
            mirror: Cell::new(true),
            mirrored: RefCell::default(),
            mirror_wanted: RefCell::default(),
        });
        // Mirroring only while the window is focused, and never after it closes (ux.md §4).
        {
            let ui = ui.clone();
            win.connect_is_active_notify(move |w| {
                // Clone out first: mirror_layout borrows mirror_wanted mutably.
                let wanted = ui.mirror_wanted.borrow().clone();
                if !w.is_active() {
                    ui.stop_mirror();
                } else if let Some(l) = wanted {
                    ui.mirror_layout(&l);
                }
            });
        }
        {
            let ui = ui.clone();
            win.connect_close_request(move |_| {
                if ui.mirrored.borrow_mut().take().is_some() {
                    live::set_layout_now("");
                }
                glib::Propagation::Proceed
            });
        }
        rebuild(&ui);
        win.present();
    });
    app.run()
}

/// Applies one edit to the user's config: edit → validate → write, with an Undo toast. On any
/// problem nothing is written and the reason is shown.
pub(crate) fn apply(ui: &Rc<Ui>, op: impl FnOnce(&mut Editor, &Table) -> Result<String, String>) {
    let state = ui.state();
    let (path, include) = state.edit_target();
    let before = std::fs::read_to_string(&path).ok();
    let mut ed = match Editor::open(&path) {
        Ok(e) => e,
        Err(e) => return ui.toast(&format!("Can't open {}: {e}", path.display())),
    };
    if before.is_none()
        && let Some(inc) = include
    {
        let _ = ed.set(
            "include",
            &toml::Value::Array(vec![inc.display().to_string().into()]),
        );
    }
    let msg = match op(&mut ed, &state.loaded.merged) {
        Ok(m) => m,
        Err(e) => return ui.toast(&e),
    };
    match ed.save(state.hw) {
        Ok(_) => {
            ui.undo.borrow_mut().push((before, msg.clone()));
            let toast = adw::Toast::builder()
                .title(glib::markup_escape_text(&msg))
                .button_label("Undo")
                .timeout(6)
                .build();
            let ui2 = ui.clone();
            toast.connect_button_clicked(move |_| undo(&ui2));
            ui.toasts.add_toast(toast);
            rebuild(ui);
        }
        Err(d) => {
            let why = d
                .errors
                .first()
                .map(ToString::to_string)
                .unwrap_or_else(|| "invalid".into());
            ui.toast(&format!("Not applied: {why}"));
        }
    }
}

fn undo(ui: &Rc<Ui>) {
    let Some((before, msg)) = ui.undo.borrow_mut().pop() else {
        return;
    };
    let (path, _) = ui.state().edit_target();
    let result = match before {
        Some(text) => std::fs::write(&path, text),
        None => std::fs::remove_file(&path),
    };
    match result {
        Ok(()) => ui.toast(&format!("Undid: {msg}")),
        Err(e) => ui.toast(&format!("Couldn't undo: {e}")),
    }
    rebuild(ui);
}

/// (Re)builds the window content from a fresh config load, keeping the user's place.
pub(crate) fn rebuild(ui: &Rc<Ui>) {
    let state = Rc::new(State::load());
    *ui.state.borrow_mut() = state.clone();
    *ui.last_text.borrow_mut() = std::fs::read_to_string(state.edit_target().0).ok();
    watch_config(ui, &state);
    let pages: Vec<(&str, &str, &str, gtk::Widget)> = vec![
        ("bar", "Bar", "input-keyboard-symbolic", bar::page(ui)),
        (
            "rules",
            "Layouts & Rules",
            "view-list-symbolic",
            rules::page(ui),
        ),
        (
            "gestures",
            "Gestures",
            "input-touchpad-symbolic",
            prefs::gestures_page(ui),
        ),
        (
            "appearance",
            "Appearance",
            "applications-graphics-symbolic",
            prefs::appearance_page(ui),
        ),
        (
            "display",
            "Display & Power",
            "display-brightness-symbolic",
            prefs::display_page(ui),
        ),
        (
            "apps",
            "Apps",
            "application-x-executable-symbolic",
            pages::todo_page(
                "Apps",
                "Apps that put buttons on the bar: allow or deny each one, and hide or pin their items.",
                &state,
            ),
        ),
        (
            "profiles",
            "Profiles & Presets",
            "folder-symbolic",
            pages::todo_page(
                "Profiles & Presets",
                "Switch profiles, import your tiny-dfr setup (`sliverctl config import-tiny-dfr` works today), and export or import presets.",
                &state,
            ),
        ),
        (
            "status",
            "Status",
            "dialog-information-symbolic",
            pages::status_page(&state),
        ),
    ];

    let stack = gtk::Stack::new();
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    for (id, title, icon, widget) in &pages {
        stack.add_titled(widget, Some(id), title);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_box.append(&gtk::Image::from_icon_name(icon));
        let label = gtk::Label::new(Some(title));
        row_box.append(&label);
        if *id == "status" && !state.problems.ok() {
            let badge = gtk::Image::from_icon_name("dialog-error-symbolic");
            badge.add_css_class("error");
            row_box.append(&badge);
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_widget_name(id);
        sidebar.append(&row);
    }

    let content_page = adw::NavigationPage::new(&toolbar(&stack, None), "Bar");
    let split = adw::NavigationSplitView::new();
    {
        let (stack, content_page, split, ui) = (
            stack.clone(),
            content_page.clone(),
            split.clone(),
            ui.clone(),
        );
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let id = row.widget_name();
            stack.set_visible_child_name(&id);
            if let Some(child) = stack.child_by_name(&id) {
                content_page.set_title(&stack.page(&child).title().unwrap_or_default());
            }
            *ui.page.borrow_mut() = id.to_string();
            split.set_show_content(true);
        });
    }
    let start = if state.problems.ok() {
        ui.page.borrow().clone()
    } else {
        "status".into()
    };
    let index = pages.iter().position(|p| p.0 == start).unwrap_or(0);
    sidebar.select_row(sidebar.row_at_index(index as i32).as_ref());

    let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload.set_tooltip_text(Some("Reload config"));
    {
        let ui = ui.clone();
        reload.connect_clicked(move |_| rebuild(&ui));
    }
    let sidebar_page = adw::NavigationPage::new(&toolbar(&sidebar, Some(&reload)), "Sliver");
    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(220.0);
    ui.toasts.set_child(Some(&split));
}

/// Refreshes the window when the config changes on disk from outside the app (an editor,
/// `sliverctl config set`). Directories are watched, so atomic rename-saves count. Our own saves
/// are recognized (`last_text`) and skipped.
fn watch_config(ui: &Rc<Ui>, state: &State) {
    use gtk::gio::prelude::*;
    let mut files: Vec<PathBuf> = state.loaded.files.clone();
    files.extend(state.sheet.files().iter().cloned());
    files.push(state.edit_target().0);
    if let Some(p) = &state.path {
        files.push(p.with_file_name("style.css"));
    }
    let names: std::collections::HashSet<std::ffi::OsString> = files
        .iter()
        .filter_map(|f| f.file_name().map(Into::into))
        .collect();
    let mut dirs: Vec<PathBuf> = files
        .iter()
        .filter_map(|f| f.parent().map(PathBuf::from))
        .filter(|d| d.is_dir())
        .collect();
    dirs.sort();
    dirs.dedup();
    let mut monitors = Vec::new();
    for d in dirs {
        let Ok(m) = gtk::gio::File::for_path(&d).monitor_directory(
            gtk::gio::FileMonitorFlags::WATCH_MOVES,
            gtk::gio::Cancellable::NONE,
        ) else {
            continue;
        };
        let (ui, names) = (ui.clone(), names.clone());
        m.connect_changed(move |_, file, other, _| {
            let hit = |f: Option<&gtk::gio::File>| {
                f.and_then(|f| f.basename())
                    .is_some_and(|n| names.contains(n.as_os_str()))
            };
            if !(hit(Some(file)) || hit(other)) || ui.refresh_pending.replace(true) {
                return;
            }
            let ui = ui.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                ui.refresh_pending.set(false);
                let text = std::fs::read_to_string(ui.state().edit_target().0).ok();
                if text != *ui.last_text.borrow() {
                    rebuild(&ui);
                    ui.toast("The config changed on disk; reloaded");
                }
            });
        });
        monitors.push(m);
    }
    *ui.monitors.borrow_mut() = monitors;
}

fn toolbar(content: &impl IsA<gtk::Widget>, start: Option<&gtk::Button>) -> adw::ToolbarView {
    let tv = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    if let Some(b) = start {
        header.pack_start(b);
    }
    tv.add_top_bar(&header);
    tv.set_content(Some(content));
    tv
}
