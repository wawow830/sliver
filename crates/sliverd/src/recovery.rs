use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use cairo::{FontSlant, FontWeight};

use crate::frame_canvas::FrameCanvas;
use crate::hardware::{function_key_output, ContactId, LogicalFrame, OutputKey, TouchEvent};

const KEY_COUNT: usize = 12;
const PRESSED_RGB: (f64, f64, f64) = (0.22, 0.22, 0.22);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RecoveryKey(usize);

impl RecoveryKey {
    fn all() -> impl Iterator<Item = Self> {
        (0..KEY_COUNT).filter_map(Self::from_index)
    }

    fn from_index(index: usize) -> Option<Self> {
        (index < KEY_COUNT).then_some(Self(index))
    }

    fn index(self) -> usize {
        self.0
    }

    fn hit_test(x: f64, y: f64) -> Option<Self> {
        if !(0.0..crate::DISPLAY_HEIGHT_F64).contains(&y)
            || !(0.0..crate::DISPLAY_WIDTH_F64).contains(&x)
        {
            return None;
        }
        let index = (x / (crate::DISPLAY_WIDTH_F64 / KEY_COUNT as f64)).floor() as usize;
        Self::from_index(index)
    }

    fn label(self) -> String {
        format!("F{}", self.index() + 1)
    }

    fn output(self) -> OutputKey {
        function_key_output(self.index()).expect("validated recovery key has an output")
    }
}

struct RecoveryContact {
    key: RecoveryKey,
    pressed: bool,
}

pub(crate) struct RecoverySession {
    contacts: BTreeMap<ContactId, RecoveryContact>,
    row: RecoveryRow,
    owner_is_healthy: bool,
}

pub(crate) enum RecoveryTouchResult {
    Ignored,
    RowPressChanged,
    Activate(OutputKey),
}

impl RecoverySession {
    pub(crate) fn new(owner_is_healthy: bool) -> Self {
        Self {
            contacts: BTreeMap::new(),
            row: RecoveryRow::new(),
            owner_is_healthy,
        }
    }

    pub(crate) fn owner_is_healthy(&self) -> bool {
        self.owner_is_healthy
    }

    pub(crate) fn mark_unhealthy(&mut self) {
        self.owner_is_healthy = false;
    }

    fn update_row_press(&mut self, key: RecoveryKey) {
        if self
            .contacts
            .values()
            .any(|contact| contact.key == key && contact.pressed)
        {
            self.row.press(key);
        } else {
            self.row.release(key);
        }
    }

    pub(crate) fn touch_down(&mut self, event: TouchEvent) -> RecoveryTouchResult {
        let Some(key) = RecoveryKey::hit_test(event.x, event.y) else {
            return RecoveryTouchResult::Ignored;
        };
        self.contacts
            .insert(event.id, RecoveryContact { key, pressed: true });
        if self.row.is_pressed(key) {
            RecoveryTouchResult::Ignored
        } else {
            self.row.press(key);
            RecoveryTouchResult::RowPressChanged
        }
    }

    pub(crate) fn touch_move(&mut self, event: TouchEvent) -> RecoveryTouchResult {
        let key = {
            let Some(contact) = self.contacts.get_mut(&event.id) else {
                return RecoveryTouchResult::Ignored;
            };
            let inside = RecoveryKey::hit_test(event.x, event.y) == Some(contact.key);
            if inside == contact.pressed {
                return RecoveryTouchResult::Ignored;
            }
            contact.pressed = inside;
            contact.key
        };
        self.update_row_press(key);
        RecoveryTouchResult::RowPressChanged
    }

    pub(crate) fn touch_up(&mut self, event: TouchEvent) -> RecoveryTouchResult {
        let Some(contact) = self.contacts.remove(&event.id) else {
            return RecoveryTouchResult::Ignored;
        };
        let activate = RecoveryKey::hit_test(event.x, event.y) == Some(contact.key);
        self.update_row_press(contact.key);
        if activate {
            RecoveryTouchResult::Activate(contact.key.output())
        } else {
            RecoveryTouchResult::RowPressChanged
        }
    }

    pub(crate) fn touch_cancel(&mut self, event: TouchEvent) -> RecoveryTouchResult {
        let Some(contact) = self.contacts.remove(&event.id) else {
            return RecoveryTouchResult::Ignored;
        };
        self.update_row_press(contact.key);
        RecoveryTouchResult::RowPressChanged
    }

    pub(crate) fn render(&self) -> Result<LogicalFrame> {
        self.row.render()
    }
}

/// The compiled escape row. It owns the recovery layout and drawing policy so
/// the supervisor only has to route contacts and key events.
struct RecoveryRow {
    pressed: BTreeSet<RecoveryKey>,
}

impl RecoveryRow {
    fn new() -> Self {
        Self {
            pressed: BTreeSet::new(),
        }
    }

    fn press(&mut self, key: RecoveryKey) {
        self.pressed.insert(key);
    }

    fn release(&mut self, key: RecoveryKey) {
        self.pressed.remove(&key);
    }

    fn is_pressed(&self, key: RecoveryKey) -> bool {
        self.pressed.contains(&key)
    }

    fn render(&self) -> Result<LogicalFrame> {
        let frame = FrameCanvas::new().context("creating recovery frame")?;
        let context = frame.context();
        context.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
        context.set_font_size(24.0);

        let key_width = crate::DISPLAY_WIDTH_F64 / KEY_COUNT as f64;
        for key in RecoveryKey::all() {
            let left = key.index() as f64 * key_width;
            if self.is_pressed(key) {
                context.set_source_rgb(PRESSED_RGB.0, PRESSED_RGB.1, PRESSED_RGB.2);
                context.rectangle(left, 0.0, key_width, crate::DISPLAY_HEIGHT_F64);
                context.fill().context("filling recovery press feedback")?;
            }

            let label = key.label();
            let extents = context
                .text_extents(&label)
                .context("measuring recovery label")?;
            let x = left + (key_width - extents.width()) / 2.0 - extents.x_bearing();
            let y = (crate::DISPLAY_HEIGHT_F64 - extents.height()) / 2.0 - extents.y_bearing();
            context.set_source_rgb(1.0, 1.0, 1.0);
            context.move_to(x, y);
            context
                .show_text(&label)
                .context("drawing recovery label")?;
        }
        frame.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{RecoveryKey, RecoveryRow, KEY_COUNT};

    #[test]
    fn row_has_twelve_keys_and_black_background() {
        let row = RecoveryRow::new();
        let frame = row.render().expect("recovery row rendered");
        assert_eq!(frame.width(), 2008);
        assert_eq!(frame.height(), 60);
        assert_eq!(frame.pixels()[3], 255);
        assert_eq!(RecoveryKey::hit_test(0.0, 30.0), RecoveryKey::from_index(0));
        assert_eq!(
            RecoveryKey::hit_test(2007.0, 30.0),
            RecoveryKey::from_index(KEY_COUNT - 1)
        );
        assert_eq!(RecoveryKey::hit_test(2008.0, 30.0), None);
        assert!(RecoveryKey::from_index(KEY_COUNT).is_none());
    }
}
