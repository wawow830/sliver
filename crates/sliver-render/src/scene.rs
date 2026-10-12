use crate::{Color, Icon};

/// What one item shows.
#[derive(Clone, Debug, Default)]
pub enum Content {
    /// Empty space that takes up slots but draws nothing (tiny-dfr's spacer).
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
    /// How many slots the item spans (tiny-dfr's `Stretch`). 0 is treated as 1.
    pub stretch: u32,
    pub pressed: bool,
    /// Overrides the theme's button fill (e.g. a charging battery turns green).
    pub background: Option<Color>,
    /// Overrides the theme's label color.
    pub foreground: Option<Color>,
}

impl Item {
    pub fn new(content: Content) -> Item {
        Item {
            content,
            stretch: 1,
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

    pub fn spacer(stretch: u32) -> Item {
        Item::new(Content::Spacer).stretch(stretch)
    }

    pub fn stretch(mut self, stretch: u32) -> Item {
        self.stretch = stretch;
        self
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

/// A full row of items: one frame of the bar.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub items: Vec<Item>,
}

impl Scene {
    pub fn new(items: Vec<Item>) -> Scene {
        Scene { items }
    }

    pub fn stretches(&self) -> Vec<u32> {
        self.items.iter().map(|i| i.stretch).collect()
    }
}
