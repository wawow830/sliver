use crate::{Align, Color, Icon, Width};

/// What one item shows.
#[derive(Clone, Debug, Default)]
pub enum Content {
    /// Empty space that draws nothing, whatever the stylesheet says (tiny-dfr's spacer).
    #[default]
    Spacer,
    /// An icon and a label, either or both, centered together with the style's `gap` between
    /// them. Neither is an empty button. The label is in the Pango-markup subset of
    /// [`crate::markup`] (`<b>`, `<span foreground=…>`); [`Content::text`] escapes plain text.
    Label { icon: Option<Icon>, text: String },
    /// A picture (the `image` module, a published `image` item): fills the content box, keeping
    /// its aspect ratio, and its natural width follows that ratio. Never recolored, unlike a
    /// symbolic icon.
    Image(Icon),
    /// A sparkline (the `graph` module): `values` in `0.0..=1.0`, oldest first, the newest at
    /// the right edge; `capacity` samples span the width, so a short history starts mid-way.
    /// Drawn in the item's `color`: a line over an area filled at a quarter of its alpha.
    Graph { values: Vec<f32>, capacity: usize },
    /// A horizontal slider, `value` in `0.0..=1.0`, with an optional icon on its left. Styled
    /// with child nodes like GTK/waybar: `module > trough > highlight` (the filled part) and
    /// `trough > slider` (the knob).
    Slider { icon: Option<Icon>, value: f32 },
}

impl Content {
    /// A button with nothing on it.
    pub fn blank() -> Content {
        Content::label(None, String::new())
    }

    /// A label of plain text (escaped, so `<` and `&` show as written).
    pub fn text(text: impl AsRef<str>) -> Content {
        Content::markup(crate::markup::escape(text.as_ref()))
    }

    /// A label in the markup subset.
    pub fn markup(text: impl Into<String>) -> Content {
        Content::label(None, text.into())
    }

    pub fn icon(icon: Icon) -> Content {
        Content::label(Some(icon), String::new())
    }

    /// An icon and a markup label, either of which may be absent.
    pub fn label(icon: Option<Icon>, text: String) -> Content {
        Content::Label { icon, text }
    }

    /// A label's markup, if it has any.
    pub fn label_text(&self) -> Option<&str> {
        match self {
            Content::Label { text, .. } if !text.is_empty() => Some(text),
            _ => None,
        }
    }
}

/// One item (a `module` in the stylesheet).
#[derive(Clone, Debug)]
pub struct Item {
    pub content: Content,
    /// CSS `#id`. Waybar-style: module `battery` → `battery`, `custom/vpn` → `custom-vpn`.
    pub id: Option<String>,
    /// CSS classes: instance names (`battery#bat2` → `bat2`), states (`charging`, `critical`) and
    /// classes returned by scripts.
    pub classes: Vec<String>,
    pub width: Width,
    /// Matches `:active` in the stylesheet.
    pub pressed: bool,
    /// Matches `:disabled` (e.g. a control with nothing to control).
    pub disabled: bool,
    /// Matches `:checked`: a toggle that is on.
    pub checked: bool,
    /// Inline override of the computed `background-color` (like a `style` attribute).
    pub background: Option<Color>,
    /// Inline override of the computed `color` (labels and symbolic icons).
    pub foreground: Option<Color>,
}

impl Item {
    /// A new item, `Auto` width.
    pub fn new(content: Content) -> Item {
        Item {
            content,
            id: None,
            classes: Vec::new(),
            width: Width::Auto,
            pressed: false,
            disabled: false,
            checked: false,
            background: None,
            foreground: None,
        }
    }

    pub fn text(text: impl AsRef<str>) -> Item {
        Item::new(Content::text(text))
    }

    /// A label in the markup subset.
    pub fn markup(text: impl Into<String>) -> Item {
        Item::new(Content::markup(text))
    }

    pub fn icon(icon: Icon) -> Item {
        Item::new(Content::icon(icon))
    }

    pub fn spacer() -> Item {
        Item::new(Content::Spacer)
    }

    pub fn id(mut self, id: impl Into<String>) -> Item {
        self.id = Some(id.into());
        self
    }

    pub fn class(mut self, class: impl Into<String>) -> Item {
        self.classes.push(class.into());
        self
    }

    pub fn width(mut self, width: Width) -> Item {
        self.width = width;
        self
    }

    /// Shorthand for `width(Width::Fill(n))`: tiny-dfr's `Stretch`.
    pub fn fill(self, n: u32) -> Item {
        self.width(Width::Fill(n))
    }

    pub fn pressed(mut self, pressed: bool) -> Item {
        self.pressed = pressed;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Item {
        self.disabled = disabled;
        self
    }

    pub fn checked(mut self, checked: bool) -> Item {
        self.checked = checked;
        self
    }

    pub fn background(mut self, color: Color) -> Item {
        self.background = Some(color);
        self
    }

    pub fn foreground(mut self, color: Color) -> Item {
        self.foreground = Some(color);
        self
    }
}

/// A run of items laid out together (a `zone` in the stylesheet).
#[derive(Clone, Debug)]
pub struct Zone {
    /// Becomes the CSS class `.zone-<name>` (`#id` is only for modules).
    pub name: Option<String>,
    pub classes: Vec<String>,
    pub width: Width,
    /// Where items sit when none of them is `Fill` and there's room left.
    pub align: Align,
    pub items: Vec<Item>,
}

impl Zone {
    pub fn new(name: impl Into<String>, width: Width, align: Align, items: Vec<Item>) -> Zone {
        Zone {
            name: Some(name.into()),
            classes: Vec::new(),
            width,
            align,
            items,
        }
    }
}

/// One frame of the bar: zones from left to right.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// Classes on the `bar` element, e.g. the current layer or context (`layer-fn`).
    pub classes: Vec<String>,
    pub zones: Vec<Zone>,
}

impl Scene {
    /// A single zone `main` that fills the bar.
    pub fn new(items: Vec<Item>) -> Scene {
        Scene {
            classes: Vec::new(),
            zones: vec![Zone::new("main", Width::Fill(1), Align::Start, items)],
        }
    }

    /// The default layout: `esc` (Auto), `main` (Fill, items from the left),
    /// `controls` (Auto, pushed to the right edge).
    pub fn zoned(esc: Vec<Item>, main: Vec<Item>, controls: Vec<Item>) -> Scene {
        Scene {
            classes: Vec::new(),
            zones: vec![
                Zone::new("esc", Width::Auto, Align::Start, esc),
                Zone::new("main", Width::Fill(1), Align::Start, main),
                Zone::new("controls", Width::Auto, Align::End, controls),
            ],
        }
    }

    pub fn class(mut self, class: impl Into<String>) -> Scene {
        self.classes.push(class.into());
        self
    }

    /// All items, in layout order. [`crate::Layout::items`] is in this order.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.zones.iter().flat_map(|z| z.items.iter())
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.zones.iter_mut().flat_map(|z| z.items.iter_mut())
    }
}
