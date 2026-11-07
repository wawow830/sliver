//! Undo and redo of the window's edits. Each entry is the files an edit changed, as they were on
//! each side of it ([`sliver_config::files::Snapshot`]). A step is only taken once its files
//! are written, and only if they are still as the window left them.

use sliver_config::files::Snapshot;

/// The files to put back, as they are now, and what the step did.
type Entry = (Snapshot, Snapshot, String);

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    /// An edit made the files `after`, replacing `before`. A new edit ends the redo chain.
    pub fn record(&mut self, before: Snapshot, after: Snapshot, msg: String) {
        self.undo.push((before, after, msg));
        self.redo.clear();
    }

    /// Step back: `commit(files, expect)` puts the files back if they are still `expect`
    /// ([`sliver_config::files::commit_if`]) and returns what it replaced. If it fails, nothing
    /// moves. `None`: nothing to undo.
    pub fn undo<E>(
        &mut self,
        commit: impl FnOnce(&Snapshot, &Snapshot) -> Result<Snapshot, E>,
    ) -> Option<Result<String, E>> {
        step(&mut self.undo, &mut self.redo, commit)
    }

    pub fn redo<E>(
        &mut self,
        commit: impl FnOnce(&Snapshot, &Snapshot) -> Result<Snapshot, E>,
    ) -> Option<Result<String, E>> {
        step(&mut self.redo, &mut self.undo, commit)
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

fn step<E>(
    from: &mut Vec<Entry>,
    to: &mut Vec<Entry>,
    commit: impl FnOnce(&Snapshot, &Snapshot) -> Result<Snapshot, E>,
) -> Option<Result<String, E>> {
    let (files, now, _) = from.last()?;
    Some(commit(files, now).map(|other_side| {
        let (files, _, msg) = from.pop().expect("just looked");
        to.push((other_side, files, msg.clone()));
        msg
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// One file, in memory: commit swaps it.
    fn file(text: Option<&str>) -> std::cell::RefCell<Option<String>> {
        std::cell::RefCell::new(text.map(str::to_owned))
    }

    fn snap(t: Option<&str>) -> Snapshot {
        vec![(PathBuf::from("f"), t.map(str::to_owned))]
    }

    /// Fails like `commit_if` when the file isn't what the step left.
    fn commit(
        f: &std::cell::RefCell<Option<String>>,
    ) -> impl Fn(&Snapshot, &Snapshot) -> Result<Snapshot, &'static str> + '_ {
        |to, expect| {
            if *f.borrow() != expect[0].1 {
                return Err("changed since");
            }
            Ok(snap(f.replace(to[0].1.clone()).as_deref()))
        }
    }

    #[test]
    fn undo_and_redo_walk_the_same_states() {
        let mut h = History::default();
        // file: None -> "a" -> "ab"
        let f = file(Some("ab"));
        h.record(snap(None), snap(Some("a")), "first".into());
        h.record(snap(Some("a")), snap(Some("ab")), "second".into());
        assert_eq!(h.undo(commit(&f)), Some(Ok("second".into())));
        assert_eq!(*f.borrow(), Some("a".into()));
        assert_eq!(h.undo(commit(&f)), Some(Ok("first".into())));
        assert_eq!(*f.borrow(), None);
        assert_eq!(h.undo(commit(&f)), None, "nothing left");
        assert!(h.can_redo());
        assert_eq!(h.redo(commit(&f)), Some(Ok("first".into())));
        assert_eq!(h.redo(commit(&f)), Some(Ok("second".into())));
        assert_eq!(*f.borrow(), Some("ab".into()));
        assert_eq!(h.redo(commit(&f)), None);
        // and back again, exactly
        assert_eq!(h.undo(commit(&f)), Some(Ok("second".into())));
        assert_eq!(*f.borrow(), Some("a".into()));
    }

    /// An undo whose files can't be written changes nothing: it can be tried again.
    #[test]
    fn a_failed_step_stays_where_it_was() {
        let mut h = History::default();
        h.record(snap(Some("a")), snap(Some("b")), "x".into());
        assert_eq!(
            h.undo(|_: &Snapshot, _: &Snapshot| Err::<Snapshot, _>("read-only")),
            Some(Err("read-only"))
        );
        assert!(h.can_undo() && !h.can_redo());
        let f = file(Some("b"));
        assert_eq!(h.undo(commit(&f)), Some(Ok("x".into())));
        assert!(h.can_redo());
    }

    /// A file edited outside the window since the step stops undo, rather than being lost.
    #[test]
    fn undo_does_not_write_over_an_outside_edit() {
        let mut h = History::default();
        h.record(snap(Some("a")), snap(Some("b")), "x".into());
        let f = file(Some("b, then edited elsewhere"));
        assert_eq!(h.undo(commit(&f)), Some(Err("changed since")));
        assert_eq!(*f.borrow(), Some("b, then edited elsewhere".into()));
        assert!(h.can_undo());
    }

    #[test]
    fn a_new_edit_ends_the_redo_chain() {
        let mut h = History::default();
        h.record(snap(Some("a")), snap(Some("b")), "x".into());
        let f = file(Some("b"));
        h.undo(commit(&f)).unwrap().unwrap();
        assert!(h.can_redo());
        h.record(snap(Some("a")), snap(Some("c")), "y".into());
        assert!(!h.can_redo());
        assert!(h.can_undo());
    }
}
