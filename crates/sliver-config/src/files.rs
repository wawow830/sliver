//! Writing the user's files: each is replaced atomically (a temp file beside it, then a rename),
//! and [`commit`] changes several all or nothing.

use std::io;
use std::path::{Path, PathBuf};

/// Files and their whole text (`None`: the file doesn't exist).
pub type Snapshot = Vec<(PathBuf, Option<String>)>;

/// `None` when the file doesn't exist. An unreadable file is an error, not empty, so nothing
/// writes over it.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(at(path, e)),
    }
}

/// Replaces `path` with `text` atomically, creating its directory if needed.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.sliver-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| at(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        at(path, e)
    })
}

/// Makes every file in `to` read as it says, all or nothing: if one can't be written, those
/// already written go back. Returns what they were, which committed undoes this.
pub fn commit(to: &Snapshot) -> io::Result<Snapshot> {
    let before = to
        .iter()
        .map(|(p, _)| Ok((p.clone(), read(p)?)))
        .collect::<io::Result<Snapshot>>()?;
    for (i, (p, text)) in to.iter().enumerate() {
        if let Err(e) = put(p, text.as_deref()) {
            for (p, text) in before[..i].iter().rev() {
                let _ = put(p, text.as_deref());
            }
            return Err(e);
        }
    }
    Ok(before)
}

fn put(path: &Path, text: Option<&str>) -> io::Result<()> {
    match text {
        Some(t) => write_atomic(path, t),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(at(path, e)),
            _ => Ok(()),
        },
    }
}

fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_commit_returns_what_undoes_it() {
        let d = dir("undo");
        let (a, b) = (d.join("a"), d.join("sub/b"));
        std::fs::write(&a, "old").unwrap();
        let before = commit(&vec![
            (a.clone(), Some("new".into())),
            (b.clone(), Some("b".into())),
        ])
        .unwrap();
        assert_eq!(read(&a).unwrap().as_deref(), Some("new"));
        assert_eq!(read(&b).unwrap().as_deref(), Some("b"));
        assert_eq!(
            before,
            vec![(a.clone(), Some("old".into())), (b.clone(), None)]
        );
        commit(&before).unwrap();
        assert_eq!(read(&a).unwrap().as_deref(), Some("old"));
        assert_eq!(read(&b).unwrap(), None, "a file the commit made goes again");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// One file can't be written: the others go back.
    #[test]
    fn a_commit_is_all_or_nothing() {
        let d = dir("atomic");
        let (a, blocked) = (d.join("a"), d.join("file/b"));
        std::fs::write(&a, "old").unwrap();
        std::fs::write(d.join("file"), "a file, not a directory").unwrap();
        let r = commit(&vec![
            (a.clone(), Some("new".into())),
            (blocked, Some("b".into())),
        ]);
        assert!(r.is_err());
        assert_eq!(read(&a).unwrap().as_deref(), Some("old"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file that exists but can't be read is an error, so nothing writes over it.
    #[test]
    fn unreadable_is_not_empty() {
        let d = dir("unreadable");
        let a = d.join("a");
        std::fs::create_dir(&a).unwrap(); // reading a directory fails, and isn't NotFound
        assert!(read(&a).is_err());
        assert!(commit(&vec![(a.clone(), Some("x".into()))]).is_err());
        assert!(a.is_dir(), "left alone");
        let _ = std::fs::remove_dir_all(&d);
    }
}
