//! Items that apps publish over the public D-Bus API (api.md §3) as bar cells: the App controls
//! and Status items placeholders (customization.md §5) fill from here.

use sliver_api::item::{Item, ItemKind, SpacerSize};
use sliver_api::registry::ContextId;
use sliver_api::service::Api;
use sliver_render::{Color, Content, Icon, Width};

use crate::icons::Icons;
use crate::modules::{Cell, Touch};

/// One published context, copied out of the registry for this frame.
#[derive(Debug, Clone)]
pub struct PubContext {
    pub id: ContextId,
    pub provider: String,
    pub items: Vec<Item>,
}

/// Who fills the placeholders right now.
#[derive(Debug, Clone, Default)]
pub struct Published {
    pub app: Option<PubContext>,
    /// The app context may cover Main + Controls (api.md §4).
    pub modal: bool,
    pub status: Vec<PubContext>,
}

impl Published {
    /// What the API's registry says, for the current focus.
    pub fn snapshot(api: &Api) -> Published {
        let r = api.resolve();
        let st = api.state();
        let get = |id: ContextId| {
            st.reg.context(id).map(|c| PubContext {
                id,
                provider: c.provider.clone(),
                items: c.items.clone(),
            })
        };
        Published {
            app: r.app.and_then(get),
            modal: r.modal,
            status: r.status.into_iter().filter_map(get).collect(),
        }
    }

    pub fn contexts(&self) -> impl Iterator<Item = &PubContext> {
        self.app.iter().chain(&self.status)
    }
}

/// Which published item a cell shows, and how a touch on it reaches the app.
#[derive(Debug, Clone, PartialEq)]
pub struct PubRef {
    pub context: ContextId,
    pub item: String,
    pub kind: PubKind,
    /// Gestures beyond tap the item opted into.
    pub gestures: Vec<sliver_api::item::Gesture>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PubKind {
    Button,
    Toggle,
    /// Slider range, to turn a 0–100 position back into the app's units.
    Slider {
        min: f64,
        max: f64,
        step: f64,
    },
    /// One segment of a segmented control.
    Segment(i32),
    /// The ✕ that closes a modal context.
    Close,
}

fn color(s: &Option<String>) -> Option<Color> {
    let c = sliver_render::markup::color(s.as_deref()?)?;
    Some(Color::rgba(c[0], c[1], c[2], c[3]))
}

/// The ✕ at the end of a modal context (api.md §4).
pub fn close_cell(ctx: &PubContext, icons: &mut Icons, icon_size: u32) -> Cell {
    let icon = icons.get("window-close-symbolic", None, icon_size);
    Cell {
        key: format!("modal/{}#close", ctx.provider),
        id: format!("app/{}#close", ctx.provider),
        kind: "app".into(),
        mic: false,
        css_id: "modal-close".into(),
        classes: vec!["app".into(), "modal-close".into()],
        width: Width::Auto,
        content: icon.map_or_else(|| Content::Text("×".into()), Content::Icon),
        touch: Touch::Agent,
        on: Vec::new(),
        next_change: None,
        pressed: false,
        checked: false,
        disabled: false,
        drawer: None,
        foreground: None,
        background: None,
        published: Some(PubRef {
            context: ctx.id,
            item: String::new(),
            kind: PubKind::Close,
            gestures: Vec::new(),
        }),
        pass_through: false,
    }
}

/// Cells for a context's items. `class` is `app` or `status`; `focused` adds `.focused-app`.
pub fn cells(
    ctx: &PubContext,
    class: &str,
    focused: bool,
    icons: &mut Icons,
    icon_size: u32,
    out: &mut Vec<Cell>,
) {
    for item in &ctx.items {
        push(ctx, item, class, focused, icons, icon_size, out);
    }
}

fn push(
    ctx: &PubContext,
    item: &Item,
    class: &str,
    focused: bool,
    icons: &mut Icons,
    icon_size: u32,
    out: &mut Vec<Cell>,
) {
    if !item.visible {
        return;
    }
    let icon = item
        .icon_data
        .as_deref()
        .and_then(|png| Icon::from_png(png).ok())
        .or_else(|| {
            item.icon
                .as_deref()
                .and_then(|n| icons.get(n, None, icon_size))
        });
    let label = item.label.clone().unwrap_or_default();
    let label_content = |icon: Option<Icon>, text: String| match (icon, text.is_empty()) {
        (Some(i), true) => Content::Icon(i),
        (Some(i), false) => Content::IconText(i, text),
        (None, false) => Content::Text(text),
        (None, true) => Content::Blank,
    };
    let mut classes = vec![class.to_owned(), item.kind.type_name().to_owned()];
    if focused {
        classes.push("focused-app".into());
    }
    classes.extend(item.classes.iter().cloned());
    let base = |content: Content, kind: Option<PubKind>, checked: bool, id: &str| Cell {
        key: format!("{class}/{}#{id}", ctx.provider),
        id: format!("app/{}#{id}", ctx.provider),
        kind: class.into(),
        mic: false,
        css_id: format!(
            "{class}-{}",
            ctx.provider.replace(['.', '/'], "-").to_ascii_lowercase()
        ),
        classes: classes.clone(),
        width: match (item.min_width, &item.kind) {
            (
                _,
                ItemKind::Spacer {
                    size: SpacerSize::Flexible,
                },
            ) => Width::Fill(1),
            (Some(w), _) => Width::Fixed(w as f32),
            _ => Width::Auto,
        },
        content,
        touch: if kind.is_some() && item.enabled {
            Touch::Agent
        } else {
            Touch::None
        },
        on: Vec::new(),
        next_change: None,
        pressed: false,
        checked,
        disabled: !item.enabled,
        drawer: None,
        foreground: color(&item.fg),
        background: color(&item.bg),
        published: kind.map(|kind| PubRef {
            context: ctx.id,
            item: item.id.clone(),
            kind,
            gestures: item.gestures.clone(),
        }),
        pass_through: false,
    };
    match &item.kind {
        ItemKind::Button | ItemKind::Popover { .. } => out.push(base(
            label_content(icon, label),
            Some(PubKind::Button),
            false,
            &item.id,
        )),
        ItemKind::Toggle { state } => out.push(base(
            label_content(icon, label),
            Some(PubKind::Toggle),
            *state,
            &item.id,
        )),
        ItemKind::Label => out.push(base(label_content(icon, label), None, false, &item.id)),
        ItemKind::Slider {
            value,
            min,
            max,
            step,
        } => {
            let v = if max > min {
                ((value - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            out.push(base(
                Content::Slider {
                    icon,
                    value: v as f32,
                },
                Some(PubKind::Slider {
                    min: *min,
                    max: *max,
                    step: *step,
                }),
                false,
                &item.id,
            ));
        }
        ItemKind::Progress { value, .. } => {
            out.push(base(
                Content::Slider {
                    icon,
                    value: value.clamp(0.0, 1.0) as f32,
                },
                None,
                false,
                &item.id,
            ));
        }
        ItemKind::Spacer { .. } => out.push(base(Content::Spacer, None, false, &item.id)),
        ItemKind::Segmented { segments, selected } => {
            for (i, seg) in segments.iter().enumerate() {
                let icon = seg
                    .icon
                    .as_deref()
                    .and_then(|n| icons.get(n, None, icon_size));
                let content = label_content(icon, seg.label.clone().unwrap_or_default());
                out.push(base(
                    content,
                    Some(PubKind::Segment(i as i32)),
                    *selected == i as i32,
                    &format!("{}.{i}", item.id),
                ));
            }
        }
        ItemKind::Group { items } => {
            for child in items {
                push(ctx, child, class, focused, icons, icon_size, out);
            }
        }
    }
}
