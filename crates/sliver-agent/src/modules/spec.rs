//! A module's source gives a [`Spec`] (values, verbs, level), and [`cell`] formats it with the
//! module's config into a [`Cell`].

use super::*;

/// A cell's gestures: user bindings first, then `actions = { on-x = "name" }`, then the module's
/// default ones, all resolved against the module's `verbs`. With a `menu` verb, its trigger
/// (default: tap, or hold when the module also taps) opens the sub-bar instead of whatever that
/// gesture did.
pub(super) fn bindings(
    m: &ModuleInstance,
    verbs: &[(&'static str, Do)],
    defaults: &[(Gesture, &'static str)],
) -> Vec<(Gesture, Do)> {
    let c = &m.common;
    let kind = m.id.kind.as_str();
    let verb = |name: &str| Do::bind(&Action::ModuleAction(name.into()), kind, verbs);
    let mut on: Vec<(Gesture, Do)> =
        c.on.iter()
            .map(|(g, a)| (*g, Do::bind(a, kind, verbs)))
            .collect();
    let named = c.actions.iter().map(|(g, n)| (*g, n.as_str()));
    for (g, name) in named.chain(defaults.iter().map(|(g, n)| (*g, *n))) {
        if !on.iter().any(|(x, _)| *x == g) {
            on.push((g, verb(name)));
        }
    }
    if verbs.iter().any(|(n, _)| *n == "menu") {
        let has_tap = on.iter().any(|(g, _)| *g == Gesture::Tap);
        let trigger = c
            .menu_trigger
            .unwrap_or(if has_tap { Gesture::Hold } else { Gesture::Tap });
        on.retain(|(g, _)| *g != trigger);
        on.push((trigger, verb("menu")));
    }
    on
}

/// What a module contributes before the shared formatting path.
pub(super) struct Spec {
    pub(super) values: Box<dyn Values>,
    pub(super) default_format: &'static str,
    pub(super) percentage: Option<f64>,
    pub(super) status: Option<&'static str>,
    /// Battery-style: lower percentages are worse.
    pub(super) lower_is_worse: bool,
    pub(super) default_icon: Option<String>,
    /// A level (0..1): what a slider shows. A `set` verb lets a drag change it.
    pub(super) level: Option<f32>,
    /// What it draws.
    pub(super) face: Face,
    pub(super) checked: bool,
    /// What the module's names mean ([`crate::act`]).
    pub(super) verbs: Verbs,
    /// Gestures bound to verbs unless the user bound them.
    pub(super) defaults: Vec<(Gesture, &'static str)>,
    /// Classes from the module's own output.
    pub(super) extra_classes: Vec<String>,
    /// waybar's `alt`: picks a `format-alt` / `format-icons` entry (user modules).
    pub(super) alt: Option<String>,
    /// When what it shows changes next on its own (a clock's tick, a poll), if ever.
    pub(super) next_change: Option<DateTime<Local>>,
    /// A sub-bar built from live data (the `language` picker), used when the config has no `menu`.
    pub(super) menu: Vec<MenuItem>,
}

/// What a module draws. Its formatted label is the usual; the others are what some modules
/// show instead.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Face {
    /// The formatted label (text and icon), or `fallback` when both come out empty.
    Label { fallback: Option<String> },
    /// A track at this level (0..1), with the label's icon.
    Slider(f32),
    /// A picture (path or icon name) filling the cell.
    Image(String),
    /// Samples, oldest first, 0..=1, and how many fit.
    Graph { values: Vec<f32>, capacity: usize },
    /// A spectrum's levels, 0..=1.
    Bars(Vec<f32>),
    /// Nothing but room.
    Spacer,
}

impl Spec {
    pub(super) fn new(values: impl Values + 'static, default_format: &'static str) -> Spec {
        Spec {
            values: Box::new(values),
            default_format,
            percentage: None,
            status: None,
            lower_is_worse: false,
            default_icon: None,
            level: None,
            face: Face::Label { fallback: None },
            checked: false,
            verbs: Vec::new(),
            defaults: Vec::new(),
            extra_classes: Vec::new(),
            alt: None,
            next_change: None,
            menu: Vec::new(),
        }
    }

    pub(super) fn icon(mut self, name: impl Into<String>) -> Spec {
        self.default_icon = Some(name.into());
        self
    }

    /// The module's `name` means `d`.
    pub(super) fn verb(mut self, name: &'static str, d: impl Into<Do>) -> Spec {
        self.verbs.retain(|(n, _)| *n != name);
        self.verbs.push((name, d.into()));
        self
    }

    /// Gesture `g` does the verb `name` unless the user bound it.
    pub(super) fn on(mut self, g: Gesture, name: &'static str) -> Spec {
        self.defaults.retain(|(x, _)| *x != g);
        self.defaults.push((g, name));
        self
    }

    /// Redraw `after` this long from `now` (a poll, a moving position).
    pub(super) fn redraw(&mut self, now: DateTime<Local>, after: std::time::Duration) {
        self.next_change = chrono::Duration::from_std(after).ok().map(|d| now + d);
    }

    /// [`Spec::verb`] and [`Spec::on`] at once.
    pub(super) fn does(self, g: Gesture, name: &'static str, d: impl Into<Do>) -> Spec {
        self.verb(name, d).on(g, name)
    }
}

/// A 0–100 reading as a percentage value.
pub(super) fn pct(v: Option<u8>) -> Option<f64> {
    v.map(f64::from)
}

/// Which item of a module that shows several ([`items`]) a cell is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Part {
    /// The `n`th of what the module lists (a workspace, a window, a kind of capture).
    Nth(usize),
    /// The taskbar's `n`th pinned app.
    Pin(usize),
}

impl Part {
    pub(super) fn nth(self) -> Option<usize> {
        match self {
            Part::Nth(i) => Some(i),
            Part::Pin(_) => None,
        }
    }
}

/// The items of a module that shows several, each with its key suffix; `None` for a module that
/// is one cell.
pub(super) fn items(m: &ModuleInstance, world: &World) -> Option<Vec<(String, Part)>> {
    Some(match m.id.kind.as_str() {
        // One per kind in use (screen, camera, microphone), nothing while idle.
        "privacy" => world
            .session
            .capture
            .kinds()
            .into_iter()
            .enumerate()
            .map(|(i, (kind, _))| (kind.to_owned(), Part::Nth(i)))
            .collect(),
        "workspaces" => listed(m, world)
            .into_iter()
            .map(|i| (world.desktop.workspaces[i].id.to_string(), Part::Nth(i)))
            .collect(),
        "taskbar" => desktop::taskbar_items(m, world),
        _ => return None,
    })
}

/// Where a module is drawn, for its spec: the module, its cell's key, the world, its state, and
/// which of its items this cell is.
pub(super) struct At<'a> {
    pub(super) m: &'a ModuleInstance,
    pub(super) key: &'a str,
    pub(super) world: &'a World,
    pub(super) ui: &'a UiState,
    pub(super) item: Option<Part>,
}

/// The module's spec for this world, or `None` to hide it (no data, nothing to show).
pub(super) fn spec(at: &At) -> Option<Spec> {
    let sp = source(at)?;
    Some(if is_slider(at.m) {
        as_slider(sp, at.m, at.ui)
    } else {
        sp
    })
}

/// Does the module draw as a slider? `slider = true`, which the resolver also sets for a module
/// written as waybar's `<type>/slider`.
pub(super) fn is_slider(m: &ModuleInstance) -> bool {
    m.takes("slider") && flag(m, "slider")
}

/// `collapsed = true`: a slider shows just its icon until it's tapped open.
pub(super) fn is_collapsible(m: &ModuleInstance) -> bool {
    m.takes("collapsed") && flag(m, "collapsed")
}

pub(super) fn flag(m: &ModuleInstance, key: &str) -> bool {
    m.flag(key).unwrap_or(false)
}

/// The slider presentation, for any module with a [`Spec::level`]: no text, a drag sets the level
/// (when the module has a `set` verb), and a tap doesn't do the button's thing. A collapsed
/// slider shows just its icon until it's tapped open (like macOS's control strip).
pub(super) fn as_slider(mut sp: Spec, m: &ModuleInstance, ui: &UiState) -> Spec {
    let Some(level) = sp.level else {
        return sp;
    };
    sp.default_format = "";
    sp.defaults.retain(|(g, _)| *g != Gesture::Tap);
    if is_collapsible(m) && !ui.expanded {
        return sp;
    }
    sp.face = Face::Slider(level);
    if sp.verbs.iter().any(|(n, _)| *n == "set") {
        sp = sp.on(Gesture::Drag, "set");
    }
    sp
}

/// What the module shows, before any presentation: its type's entry in [`SOURCES`].
pub(super) fn source(at: &At) -> Option<Spec> {
    let (_, values) = SOURCES.iter().find(|(kind, _)| *kind == at.m.id.kind)?;
    values(at)
}

/// Where each module type's values come from. With the types [`cells_in`] composes from other
/// modules (groups) or from what apps publish, this is the config's list of types
/// (`sliver_config::MODULE_TYPES`; a test holds them together).
pub(super) type Source = fn(&At) -> Option<Spec>;
pub(super) const SOURCES: &[(&str, Source)] = &[
    ("key", |_| Some(Spec::new(ValueMap::new(), ""))),
    ("spacer", |_| {
        let mut sp = Spec::new(ValueMap::new(), "");
        sp.face = Face::Spacer;
        Some(sp)
    }),
    ("custom", |at| {
        if at.world.outputs.contains_key(&at.m.id.to_string()) {
            widgets::custom_output(at.m, at.world)
        } else {
            // No script, or none has printed yet: its `format` alone.
            Some(Spec::new(ValueMap::new(), ""))
        }
    }),
    ("graph", |at| widgets::graph(at.m, at.world, at.ui)),
    ("cava", |at| widgets::cava(at.m, at.world)),
    ("image", |at| widgets::image(at.m, at.world)),
    ("clock", widgets::clock),
    ("emoji", |_| widgets::emoji()),
    ("timer", widgets::timer),
    ("battery", |at| system::battery(at.world)),
    ("backlight", |at| system::backlight(at.m, at.world)),
    ("kbd-backlight", |at| system::backlight(at.m, at.world)),
    ("audio", |at| system::audio(at.m, at.world)),
    ("idle-inhibitor", |at| system::idle_inhibitor(at.world)),
    ("power-profiles", |at| system::power_profiles(at.world)),
    ("network", |at| system::network(at.m, at.world)),
    ("bluetooth", |at| system::bluetooth(at.world)),
    ("notifications", |at| system::notifications(at.world)),
    ("screenshot", |_| system::screenshot()),
    ("systemd-failed-units", |at| {
        system::failed_units(at.m, at.world)
    }),
    ("lock", |_| system::lock()),
    ("power", |at| system::power(at.key)),
    ("dnd", |at| system::dnd(at.world)),
    ("dark-mode", |at| system::dark_mode(at.world)),
    ("privacy", |at| system::privacy(at.world, at.item)),
    ("cpu", |at| stat_spec(at.m, at.world)),
    ("memory", |at| stat_spec(at.m, at.world)),
    ("temperature", |at| stat_spec(at.m, at.world)),
    ("disk", |at| stat_spec(at.m, at.world)),
    ("load", |at| stat_spec(at.m, at.world)),
    ("mpris", media::mpris),
    // fcitx5 (CJK input methods) takes over while it runs; else the compositor's layout.
    ("language", |at| match at.world.session.input_method {
        Some(_) => desktop::language_input_method(at.world),
        None => desktop::language(at.world),
    }),
    ("window", |at| desktop::window(at.m, at.world)),
    ("mode", |at| desktop::mode(at.world)),
    ("workspaces", |at| desktop::workspaces(at.world, at.item)),
    ("taskbar", |at| desktop::taskbar(at.m, at.world, at.item)),
];

/// A formatted label, for every module's (and configured published item's) text: waybar's
/// `format`/`format-icons`/`states` over `values`, then the icon: the format's,
/// else the module's `icon`, else `fallback` (a name looked up the same way). `None` when the
/// format hides the cell.
pub(crate) struct Shown {
    pub icon: Option<sliver_render::Icon>,
    pub text: String,
    pub classes: Vec<String>,
}

pub(crate) fn present(
    c: &sliver_config::Common,
    default_format: &str,
    st: &State,
    values: &dyn Values,
    fallback: Option<&str>,
    icons: &mut Icons,
    icon_size: u32,
) -> Option<Shown> {
    let f = format_module(c, default_format, st, values);
    if f.hidden {
        return None;
    }
    let name = f.icon.map(|i| i.0).or_else(|| c.icon.clone());
    let icon = name
        .as_deref()
        .or(fallback)
        .and_then(|n| icons.get(n, c.icon_theme.as_deref(), c.icon_size.unwrap_or(icon_size)));
    Some(Shown {
        icon,
        text: f.text,
        classes: f.classes,
    })
}

pub(super) fn cell(at: At, icons: &mut Icons, icon_size: u32) -> Option<Cell> {
    let (m, world, ui) = (at.m, at.world, at.ui);
    let c = &m.common;
    let mut sp = spec(&at)?;
    let key = at.key.to_owned();
    let st = State {
        percentage: sp.percentage,
        status: sp.status,
        alt: sp.alt.as_deref(),
        show_alt: ui.show_alt,
        lower_is_worse: sp.lower_is_worse,
    };
    let mut classes = m.classes.clone();
    let open_slider = matches!(sp.face, Face::Slider(_));
    let content = match std::mem::replace(&mut sp.face, Face::Spacer) {
        Face::Spacer => Content::Spacer,
        Face::Bars(levels) => Content::Bars(levels),
        // A missing or unreadable picture hides the module (it may not exist yet).
        Face::Image(name) => Content::Image(icons.image(&name, IMAGE_MAX_HEIGHT)?),
        Face::Graph { values, capacity } => {
            // `states` still apply (on the latest value): `.warning`, `.critical`.
            let f = format_module(c, "", &st, sp.values.as_ref());
            classes.extend(f.classes);
            Content::Graph { values, capacity }
        }
        face => {
            let f = present(
                c,
                sp.default_format,
                &st,
                sp.values.as_ref(),
                sp.default_icon.as_deref(),
                icons,
                icon_size,
            )?;
            classes.extend(sp.extra_classes.iter().cloned());
            classes.extend(
                f.classes
                    .into_iter()
                    .filter(|c| !sp.extra_classes.contains(c)),
            );
            match (face, f.icon, f.text.is_empty()) {
                (Face::Slider(value), icon, _) => Content::Slider { icon, value },
                (Face::Label { fallback }, None, true) => {
                    Content::text(fallback.unwrap_or_default())
                }
                // Format strings are Pango-style markup, like waybar (values from outside are
                // escaped).
                (_, icon, _) => Content::label(icon, f.text),
            }
        }
    };
    // Every module can be opened or closed (`expand`), and one with a `menu` opens it.
    let menu = if c.menu.is_empty() {
        std::mem::take(&mut sp.menu)
    } else {
        button_menu(&c.menu, sp.values.as_ref(), &m.id.kind, &sp.verbs)
    };
    let mut verbs = std::mem::take(&mut sp.verbs);
    verbs.push((
        "expand",
        Do::Agent(Act::Ui {
            key: key.clone(),
            change: UiChange::Expand,
        }),
    ));
    if !menu.is_empty() {
        verbs.push((
            "menu",
            Do::Agent(Act::Menu {
                key: key.clone(),
                items: menu,
            }),
        ));
    }
    let on = bindings(m, &verbs, &sp.defaults);
    let collapsed = is_slider(m) && is_collapsible(m) && sp.level.is_some();
    let mut touch = touch_kind(m, &on);
    if collapsed && !open_slider {
        touch = Touch::Agent;
    }
    let mut cell = Cell::new(
        key.clone(),
        m.id.to_string(),
        m.id.kind.clone(),
        m.css_id.clone(),
        content,
    );
    cell.look.classes = classes;
    cell.look.width = width(c.width);
    cell.touch = touch;
    cell.on = on;
    cell.next_change = sp.next_change;
    cell.look.checked = sp.checked;
    cell.look.spectrum = spectrum(m, world);
    // A collapsed slider opens like a drawer handle and closes on its own.
    cell.drawer = collapsed.then(|| Drawer {
        group: key,
        collapse_after: Some(std::time::Duration::from_secs(5)),
        opens: Opens::Slider,
    });
    Some(cell)
}

/// Key regions are the fast path: sliverd presses and releases the keys itself, so they work
/// even if the agent hangs. Only plain "tap = key, fire on press" bindings qualify (repeat is
/// the kernel's job once the key is down).
pub(super) fn touch_kind(m: &ModuleInstance, on: &[(Gesture, Do)]) -> Touch {
    let press = m.common.fire_on == Some(FireOn::Press);
    match on {
        [(Gesture::Tap, Do::Run(Action::Key(k)))] if press => Touch::Keys(vec![k.code()]),
        [(Gesture::Tap, Do::Run(Action::Keys(ks)))] if press => {
            Touch::Keys(ks.iter().map(|k| k.code()).collect())
        }
        [] if m.common.actions.is_empty() && m.common.format_alt.is_none() => Touch::None,
        _ => Touch::Agent,
    }
}

/// A module's configured `width` for the renderer.
pub(super) fn width(w: sliver_config::Width) -> Width {
    match w {
        sliver_config::Width::Auto => Width::Auto,
        sliver_config::Width::Fixed(px) => Width::Fixed(px as f32),
        sliver_config::Width::Fill(n) => Width::Fill(n),
    }
}
