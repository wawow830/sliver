//! Writing the user's files: each is replaced atomically (a temp file beside it, then a rename),
//! and [`commit`] changes several together: every new text is written before any file is
//! replaced, so a failed write changes nothing. A crash between the renames can leave some files
//! replaced and others not.

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

/// The files in `paths` as they are now.
pub fn snapshot<'a>(paths: impl IntoIterator<Item = &'a Path>) -> io::Result<Snapshot> {
    paths
        .into_iter()
        .map(|p| Ok((p.to_owned(), read(p)?)))
        .collect()
}

/// Replaces `path` with `text` atomically, creating its directory if needed.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = stage(path, text)?;
    replace(&tmp, path)
}

/// Makes every file in `to` read as it says. Returns what they were, which committed undoes
/// this. If a file can't be replaced, those already replaced are put back; the error names any
/// that couldn't be.
pub fn commit(to: &Snapshot) -> io::Result<Snapshot> {
    commit_if(to, &[])
}

/// [`commit`], only if each file in `expect` still reads as it says: nothing written over a
/// change made since. An [`io::ErrorKind::Other`] error names the file that changed.
pub fn commit_if(to: &Snapshot, expect: &[(PathBuf, Option<String>)]) -> io::Result<Snapshot> {
    let before = snapshot(to.iter().map(|(p, _)| p.as_path()))?;
    for (p, want) in expect {
        if read(p)? != *want {
            return Err(io::Error::other(format!("{} changed since", p.display())));
        }
    }
    let mut staged = Vec::with_capacity(to.len());
    for (p, text) in to {
        match text.as_deref().map(|t| stage(p, t)).transpose() {
            Ok(tmp) => staged.push(tmp),
            Err(e) => {
                staged
                    .iter()
                    .flatten()
                    .for_each(|t| drop(std::fs::remove_file(t)));
                return Err(e);
            }
        }
    }
    for (i, ((p, _), tmp)) in to.iter().zip(&staged).enumerate() {
        let r = match tmp {
            Some(tmp) => replace(tmp, p),
            None => remove(p),
        };
        if let Err(e) = r {
            staged[i + 1..]
                .iter()
                .flatten()
                .for_each(|t| drop(std::fs::remove_file(t)));
            let stuck: Vec<String> = before[..i]
                .iter()
                .rev()
                .filter(|(p, text)| match text {
                    Some(t) => write_atomic(p, t).is_err(),
                    None => remove(p).is_err(),
                })
                .map(|(p, _)| p.display().to_string())
                .collect();
            if stuck.is_empty() {
                return Err(e);
            }
            return Err(io::Error::new(
                e.kind(),
                format!("{e}; and changed, unable to go back: {}", stuck.join(", ")),
            ));
        }
    }
    Ok(before)
}

/// `text` in a temp file beside `path`, creating the directory.
fn stage(path: &Path, text: &str) -> io::Result<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.sliver-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| at(&tmp, e))?;
    Ok(tmp)
}

fn replace(tmp: &Path, path: &Path) -> io::Result<()> {
    std::fs::rename(tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        at(path, e)
    })
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(at(path, e)),
        _ => Ok(()),
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

    /// A file changed since it was expected stops the commit before anything is written.
    #[test]
    fn commit_if_refuses_over_a_change() {
        let d = dir("expect");
        let a = d.join("a");
        std::fs::write(&a, "theirs").unwrap();
        let e = commit_if(
            &vec![(a.clone(), Some("undo".into()))],
            &[(a.clone(), Some("ours".into()))],
        )
        .unwrap_err();
        assert!(e.to_string().contains("changed since"), "{e}");
        assert_eq!(read(&a).unwrap().as_deref(), Some("theirs"));
        commit_if(
            &vec![(a.clone(), Some("undo".into()))],
            &[(a.clone(), Some("theirs".into()))],
        )
        .unwrap();
        assert_eq!(read(&a).unwrap().as_deref(), Some("undo"));
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            1,
            "no temp files left"
        );
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
