use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use cairo::{FontSlant, FontWeight};

use crate::frame_canvas::FrameCanvas;
use crate::hardware::{ContactId, LogicalFrame, TouchEvent, TouchPhase};

const KEY_COUNT: usize = 12;
const PRESSED_RGB: (f64, f64, f64) = (0.22, 0.22, 0.22);

struct RecoveryContact {
    key: usize,
    pressed: bool,
}

pub(crate) struct RecoveryState {
    contacts: BTreeMap<ContactId, RecoveryContact>,
    owner_is_healthy: bool,
}

pub(crate) enum RecoveryTouchResult {
    Ignored,
    RowPressChanged,
    Activate(usize),
}

impl RecoveryState {
    pub(crate) fn new(owner_is_healthy: bool) -> Self {
        Self {
            contacts: BTreeMap::new(),
            owner_is_healthy,
        }
    }

    pub(crate) fn owner_is_healthy(&self) -> bool {
        self.owner_is_healthy
    }

    pub(crate) fn mark_unhealthy(&mut self) {
        self.owner_is_healthy = false;
    }

    fn update_row_press(&self, row: &mut RecoveryRow, key: usize) {
        if self
            .contacts
            .values()
            .any(|contact| contact.key == key && contact.pressed)
        {
            row.press(key);
        } else {
            row.release(key);
        }
    }

    pub(crate) fn route_touch(
        &mut self,
        event: TouchEvent,
        row: &mut RecoveryRow,
    ) -> RecoveryTouchResult {
        match event.phase {
            TouchPhase::Down => {
                let Some(index) = RecoveryRow::hit_test(event.x, event.y) else {
                    return RecoveryTouchResult::Ignored;
                };
                self.contacts.insert(
                    event.id,
                    RecoveryContact {
                        key: index,
                        pressed: true,
                    },
                );
                if row.is_pressed(index) {
                    RecoveryTouchResult::Ignored
                } else {
                    row.press(index);
                    RecoveryTouchResult::RowPressChanged
                }
            }
            TouchPhase::Move => {
                let key = {
                    let Some(contact) = self.contacts.get_mut(&event.id) else {
                        return RecoveryTouchResult::Ignored;
                    };
                    let inside = RecoveryRow::hit_test(event.x, event.y) == Some(contact.key);
                    if inside == contact.pressed {
                        return RecoveryTouchResult::Ignored;
                    }
                    contact.pressed = inside;
                    contact.key
                };
                self.update_row_press(row, key);
                RecoveryTouchResult::RowPressChanged
            }
            TouchPhase::Up | TouchPhase::Cancel => {
                let Some(contact) = self.contacts.remove(&event.id) else {
                    return RecoveryTouchResult::Ignored;
                };
                let activate = event.phase == TouchPhase::Up
                    && RecoveryRow::hit_test(event.x, event.y) == Some(contact.key);
                self.update_row_press(row, contact.key);
                if activate {
                    RecoveryTouchResult::Activate(contact.key)
                } else {
                    RecoveryTouchResult::RowPressChanged
                }
            }
        }
    }
}

/// The compiled escape row. It owns the recovery layout and drawing policy so
/// the supervisor only has to route contacts and key events.
pub(crate) struct RecoveryRow {
    pressed: BTreeSet<usize>,
}

impl RecoveryRow {
    pub(crate) fn new() -> Self {
        Self {
            pressed: BTreeSet::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pressed.clear();
    }

    pub(crate) fn hit_test(x: f64, y: f64) -> Option<usize> {
        if !(0.0..sliver_core::STRIP_H).contains(&y) || !(0.0..sliver_core::STRIP_W).contains(&x) {
            return None;
        }
        let index = (x / (sliver_core::STRIP_W / KEY_COUNT as f64)).floor() as usize;
        (index < KEY_COUNT).then_some(index)
    }

    pub(crate) fn press(&mut self, index: usize) {
        self.pressed.insert(index);
    }

    pub(crate) fn release(&mut self, index: usize) {
        self.pressed.remove(&index);
    }

    pub(crate) fn is_pressed(&self, index: usize) -> bool {
        self.pressed.contains(&index)
    }

    pub(crate) fn render(&self) -> Result<LogicalFrame> {
        let frame = FrameCanvas::new().context("creating recovery frame")?;
        let context = frame.context();
        context.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
        context.set_font_size(24.0);

        let key_width = sliver_core::STRIP_W / KEY_COUNT as f64;
        for index in 0..KEY_COUNT {
            let left = index as f64 * key_width;
            if self.is_pressed(index) {
                context.set_source_rgb(PRESSED_RGB.0, PRESSED_RGB.1, PRESSED_RGB.2);
                context.rectangle(left, 0.0, key_width, sliver_core::STRIP_H);
                context.fill().context("filling recovery press feedback")?;
            }

            let label = format!("F{}", index + 1);
            let extents = context
                .text_extents(&label)
                .context("measuring recovery label")?;
            let x = left + (key_width - extents.width()) / 2.0 - extents.x_bearing();
            let y = (sliver_core::STRIP_H - extents.height()) / 2.0 - extents.y_bearing();
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
    use super::RecoveryRow;

    #[test]
    fn row_has_twelve_keys_and_black_background() {
        let row = RecoveryRow::new();
        let frame = row.render().expect("recovery row rendered");
        assert_eq!(frame.width(), 2008);
        assert_eq!(frame.height(), 60);
        assert_eq!(frame.pixels()[3], 255);
        assert_eq!(RecoveryRow::hit_test(0.0, 30.0), Some(0));
        assert_eq!(RecoveryRow::hit_test(2007.0, 30.0), Some(11));
        assert_eq!(RecoveryRow::hit_test(2008.0, 30.0), None);
    }
}
