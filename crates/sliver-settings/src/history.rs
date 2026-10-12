//! Undo and redo of the edits the window makes (ux.md §10). Each entry is the whole text of the
//! user's file on one side of an edit (`None` = the file didn't exist), so going back and forth
//! is exact, whatever the edit was.

type Entry = (Option<String>, String);

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    /// An edit was made; `before` is the file as it was. A new edit ends the redo chain.
    pub fn record(&mut self, before: Option<String>, msg: String) {
        self.undo.push((before, msg));
        self.redo.clear();
    }

    /// Step back. `current` is the file now; returns what to restore and what is being undone.
    pub fn undo(&mut self, current: Option<String>) -> Option<Entry> {
        let (before, msg) = self.undo.pop()?;
        self.redo.push((current, msg.clone()));
        Some((before, msg))
    }

    /// Step forward again after an undo.
    pub fn redo(&mut self, current: Option<String>) -> Option<Entry> {
        let (after, msg) = self.redo.pop()?;
        self.undo.push((current, msg.clone()));
        Some((after, msg))
    }

    #[cfg(test)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[cfg(test)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> Option<String> {
        Some(t.to_owned())
    }

    #[test]
    fn undo_and_redo_walk_the_same_states() {
        let mut h = History::default();
        // file: None -> "a" -> "ab"
        h.record(None, "first".into());
        h.record(s("a"), "second".into());
        assert_eq!(h.undo(s("ab")), Some((s("a"), "second".into())));
        assert_eq!(h.undo(s("a")), Some((None, "first".into())));
        assert_eq!(h.undo(None), None, "nothing left");
        assert!(h.can_redo());
        assert_eq!(h.redo(None), Some((s("a"), "first".into())));
        assert_eq!(h.redo(s("a")), Some((s("ab"), "second".into())));
        assert_eq!(h.redo(s("ab")), None);
        // and back again, exactly
        assert_eq!(h.undo(s("ab")), Some((s("a"), "second".into())));
    }

    #[test]
    fn a_new_edit_ends_the_redo_chain() {
        let mut h = History::default();
        h.record(s("a"), "x".into());
        h.undo(s("b"));
        assert!(h.can_redo());
        h.record(s("a"), "y".into());
        assert!(!h.can_redo());
        assert!(h.can_undo());
    }
}
