use crate::{Color, Icon, Width};

/// What one item shows.
#[derive(Clone, Debug, Default)]
pub enum Content {
    /// Empty space that draws nothing (tiny-dfr's spacer).
    #[default]
    Spacer,
    /// A button with no content.
    Blank,
    Text(String),
    Icon(Icon),
    /// An icon followed by a label, centered together.
    IconText(Icon, String),
}

/// One item on the bar.
#[derive(Clone, Debug)]
pub struct Item {
    pub content: Content,
    pub width: Width,
    pub pressed: bool,
    /// Overrides the theme's button fill (e.g. a charging battery turns green).
    pub background: Option<Color>,
    /// Overrides the theme's label and symbolic icon color.
    pub foreground: Option<Color>,
}

impl Item {
    /// A new item, `Auto` width.
    pub fn new(content: Content) -> Item {
        Item {
            content,
            width: Width::Auto,
            pressed: false,
            background: None,
            foreground: None,
        }
    }

    pub fn text(text: impl Into<String>) -> Item {
        Item::new(Content::Text(text.into()))
    }

    pub fn icon(icon: Icon) -> Item {
        Item::new(Content::Icon(icon))
    }

    pub fn spacer() -> Item {
        Item::new(Content::Spacer)
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

    pub fn background(mut self, color: Color) -> Item {
        self.background = Some(color);
        self
    }

    pub fn foreground(mut self, color: Color) -> Item {
        self.foreground = Some(color);
        self
    }
}

/// A run of items laid out together. Zones are separated by the theme's zone gap.
#[derive(Clone, Debug, Default)]
pub struct Zone {
    pub items: Vec<Item>,
    /// Takes the leftover space (at its end) when no item is `Fill`.
    pub grow: bool,
}

/// One frame of the bar: zones from left to right.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub zones: Vec<Zone>,
}

impl Scene {
    /// A single growing zone.
    pub fn new(items: Vec<Item>) -> Scene {
        Scene {
            zones: vec![Zone { items, grow: true }],
        }
    }

    /// The standard layout (ux.md §2): Esc (left), Main (grows), Controls (pushed right).
    pub fn zoned(esc: Vec<Item>, main: Vec<Item>, controls: Vec<Item>) -> Scene {
        Scene {
            zones: vec![
                Zone {
                    items: esc,
                    grow: false,
                },
                Zone {
                    items: main,
                    grow: true,
                },
                Zone {
                    items: controls,
                    grow: false,
                },
            ],
        }
    }

    /// All items, in layout order. [`crate::Renderer::layout`] returns rects in this order.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.zones.iter().flat_map(|z| z.items.iter())
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.zones.iter_mut().flat_map(|z| z.items.iter_mut())
    }
}
