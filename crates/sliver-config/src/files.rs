//! Writing the user's files: each is replaced atomically (a temp file beside it, then a rename).

use std::io;
use std::path::Path;

/// Replaces `path` with `text` atomically, creating its directory if needed.
pub(crate) fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
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

fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}
