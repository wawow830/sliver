//! What the user does to published items: optimistic state, and the events their owners get.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Begin,
    Change,
    End,
}

/// Something the user did to a published item, after the agent's gesture recognition and after
/// the user's own gesture bindings decided the app should hear about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interaction {
    Gesture(Gesture),
    Slider {
        value: f64,
        phase: Phase,
    },
    /// A segment or scrubber candidate picked.
    Select(i32),
    /// A scrubber scrolled so its first visible candidate is this one (clamped).
    Scroll(i32),
    Popover(bool),
    /// The ✕ on a modal context.
    CloseModal,
}

/// An `Event` signal to send, unicast to `destination`.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub destination: String,
    pub context: ContextId,
    pub item: String,
    pub kind: &'static str,
    pub data: Props,
}

impl Registry {
    /// Apply an interaction: update optimistic state and return the event for the owner, if any.
    pub fn interact(
        &mut self,
        ctx: ContextId,
        item_id: &str,
        what: Interaction,
    ) -> Option<Outgoing> {
        let c = self.contexts.get_mut(&ctx)?;
        let owner = c.owner.clone();
        let out = |item: &str, kind: &'static str, data: Props| Outgoing {
            destination: owner.clone(),
            context: ctx,
            item: item.to_owned(),
            kind,
            data,
        };
        if let Interaction::CloseModal = what {
            if !c.opts.modal {
                return None;
            }
            c.visible = false;
            self.generation += 1;
            return Some(out("", "closed", Props::new()));
        }
        let it = c.find_mut(item_id)?;
        if !it.enabled || !it.visible {
            return None;
        }
        let one = |k: &str, v: Prop| Props::from([(k.to_owned(), v)]);
        let ev = match (what, &mut it.kind) {
            (Interaction::Gesture(Gesture::Tap), ItemKind::Toggle { state }) => {
                *state = !*state;
                out(item_id, "toggled", one("state", Prop::Bool(*state)))
            }
            (Interaction::Gesture(Gesture::Tap), ItemKind::Button) => {
                out(item_id, "tap", Props::new())
            }
            (Interaction::Gesture(g), k)
                if g != Gesture::Tap && g != Gesture::Drag && interactive(k) =>
            {
                if !it.gestures.contains(&g) {
                    return None;
                }
                out(item_id, g.name(), Props::new())
            }
            (
                Interaction::Slider { value, phase },
                ItemKind::Slider {
                    value: v,
                    min,
                    max,
                    step,
                },
            ) => {
                let mut x = value.clamp(*min, *max);
                if *step > 0.0 {
                    x = (*min + ((x - *min) / *step).round() * *step).clamp(*min, *max);
                }
                *v = x;
                let phase = match phase {
                    Phase::Begin => "begin",
                    Phase::Change => "change",
                    Phase::End => "end",
                };
                let mut d = one("value", Prop::Num(x));
                d.insert("phase".into(), Prop::Str(phase.into()));
                out(item_id, "value", d)
            }
            (Interaction::Select(i), ItemKind::Segmented { segments, selected }) => {
                if i < -1 || i >= segments.len() as i32 {
                    return None;
                }
                *selected = i;
                out(item_id, "selected", one("index", Prop::Int(i.into())))
            }
            (
                Interaction::Select(i),
                ItemKind::Scrubber {
                    candidates,
                    selected,
                    ..
                },
            ) => {
                if i < -1 || i >= candidates.len() as i32 {
                    return None;
                }
                *selected = i;
                out(item_id, "selected", one("index", Prop::Int(i.into())))
            }
            (
                Interaction::Scroll(to),
                ItemKind::Scrubber {
                    candidates,
                    offset,
                    visible_count,
                    ..
                },
            ) => {
                let to = to.clamp(
                    0,
                    crate::item::scrubber_max_offset(candidates.len(), *visible_count),
                );
                if to == *offset {
                    return None;
                }
                *offset = to;
                out(item_id, "scrolled", one("offset", Prop::Int(to.into())))
            }
            (Interaction::Popover(open), ItemKind::Popover { .. }) => {
                out(item_id, "popover", one("open", Prop::Bool(open)))
            }
            _ => return None,
        };
        self.generation += 1;
        Some(ev)
    }

    /// `Control1.Signal(n)`: one `signal` event to every listed context.
    pub fn signal(&self, contexts: &[ContextId], n: u32) -> Vec<Outgoing> {
        contexts
            .iter()
            .filter_map(|id| self.contexts.get(id))
            .map(|c| Outgoing {
                destination: c.owner.clone(),
                context: c.id,
                item: String::new(),
                kind: "signal",
                data: Props::from([("n".to_owned(), Prop::Int(n.into()))]),
            })
            .collect()
    }
}

fn interactive(k: &ItemKind) -> bool {
    !matches!(k, ItemKind::Spacer { .. } | ItemKind::Group { .. })
}
