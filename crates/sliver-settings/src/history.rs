//! Undo and redo of the window's edits. Each entry is the files an edit changed, as they were on
//! the other side of it ([`sliver_config::files::Snapshot`]). A step is only taken once its files
//! are written.

use sliver_config::files::Snapshot;

type Entry = (Snapshot, String);

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    /// An edit was made; `before` is what it replaced. A new edit ends the redo chain.
    pub fn record(&mut self, before: Snapshot, msg: String) {
        self.undo.push((before, msg));
        self.redo.clear();
    }

    /// Step back: `commit` puts the files back (`sliver_config::files::commit`) and returns what
    /// it replaced. If it fails, nothing moves. `None`: nothing to undo.
    pub fn undo<E>(
        &mut self,
        commit: impl FnOnce(&Snapshot) -> Result<Snapshot, E>,
    ) -> Option<Result<String, E>> {
        step(&mut self.undo, &mut self.redo, commit)
    }

    pub fn redo<E>(
        &mut self,
        commit: impl FnOnce(&Snapshot) -> Result<Snapshot, E>,
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
    commit: impl FnOnce(&Snapshot) -> Result<Snapshot, E>,
) -> Option<Result<String, E>> {
    let (files, _) = from.last()?;
    Some(commit(files).map(|other_side| {
        let (_, msg) = from.pop().expect("just looked");
        to.push((other_side, msg.clone()));
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

    fn commit(
        f: &std::cell::RefCell<Option<String>>,
    ) -> impl Fn(&Snapshot) -> Result<Snapshot, ()> + '_ {
        |to| Ok(snap(f.replace(to[0].1.clone()).as_deref()))
    }

    #[test]
    fn undo_and_redo_walk_the_same_states() {
        let mut h = History::default();
        // file: None -> "a" -> "ab"
        let f = file(Some("ab"));
        h.record(snap(None), "first".into());
        h.record(snap(Some("a")), "second".into());
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
        h.record(snap(Some("a")), "x".into());
        assert_eq!(
            h.undo(|_| Err::<Snapshot, _>("read-only")),
            Some(Err("read-only"))
        );
        assert!(h.can_undo() && !h.can_redo());
        let f = file(Some("b"));
        assert_eq!(h.undo(commit(&f)), Some(Ok("x".into())));
        assert!(h.can_redo());
    }

    #[test]
    fn a_new_edit_ends_the_redo_chain() {
        let mut h = History::default();
        h.record(snap(Some("a")), "x".into());
        let f = file(Some("b"));
        h.undo(commit(&f));
        assert!(h.can_redo());
        h.record(snap(Some("a")), "y".into());
        assert!(!h.can_redo());
        assert!(h.can_undo());
    }
}
