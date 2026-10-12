//! Album art: turn an MPRIS `mpris:artUrl` into a local image file the renderer can load.
//!
//! `file://` URLs are used as they are (copied into the cache when the file has no usable
//! extension); `http(s)://` URLs are fetched once with `curl` (8 s, 2 MiB cap) into the cache.
//! Only PNG and JPEG are accepted, sniffed by their magic bytes. Anything else is `None`.
//! Call it off the main thread: a download can take seconds.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The image type of `data`, as a file extension.
pub fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else {
        None
    }
}

/// `%20` → ` ` etc. Invalid escapes are kept as they are.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A stable name for a URL (FNV-1a), so the cache survives restarts.
fn cache_name(url: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `$XDG_CACHE_HOME/sliver/art` (or `~/.cache/sliver/art`).
pub fn default_cache_dir(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = get("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            get("HOME")
                .filter(|s| !s.is_empty())
                .map(|h| Path::new(&h).join(".cache"))
        })?;
    Some(base.join("sliver/art"))
}

/// A local image file for `url`, or `None` (unsupported, unreachable, too big).
pub fn resolve(url: &str, cache_dir: &Path) -> Option<PathBuf> {
    if let Some(path) = url.strip_prefix("file://") {
        let p = PathBuf::from(percent_decode(path.trim_start_matches("localhost")));
        let data = std::fs::read(&p)
            .ok()
            .filter(|d| d.len() as u64 <= MAX_BYTES)?;
        let ext = sniff(&data)?;
        // The renderer picks the decoder by extension: keep a correctly named file as it is.
        if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            e.eq_ignore_ascii_case(ext) || (ext == "jpg" && e.eq_ignore_ascii_case("jpeg"))
        }) {
            return Some(p);
        }
        return store(cache_dir, url, ext, &data);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        for ext in ["png", "jpg"] {
            let p = cache_dir.join(format!("{}.{ext}", cache_name(url)));
            if p.is_file() {
                return Some(p);
            }
        }
        let data = fetch(url)?;
        let ext = sniff(&data)?;
        return store(cache_dir, url, ext, &data);
    }
    None
}

fn store(dir: &Path, url: &str, ext: &str, data: &[u8]) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let final_path = dir.join(format!("{}.{ext}", cache_name(url)));
    let tmp = dir.join(format!("{}.part", cache_name(url)));
    std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(data))
        .ok()?;
    std::fs::rename(&tmp, &final_path).ok()?;
    Some(final_path)
}

/// Download with `curl`, tied to this process and killed after a deadline.
fn fetch(url: &str) -> Option<Vec<u8>> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSL",
        "--max-time",
        "8",
        "--max-filesize",
        &MAX_BYTES.to_string(),
        "-o",
        "-",
        url,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let child = sliver_proc::spawn_tied(cmd).ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(out)) if out.status.success() && out.stdout.len() as u64 <= MAX_BYTES => {
            Some(out.stdout)
        }
        Ok(_) => None,
        Err(_) => {
            if let Some(p) = rustix::process::Pid::from_raw(pid as i32) {
                let _ = rustix::process::kill_process(p, rustix::process::Signal::KILL);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPG: &[u8] = include_bytes!("../tests/art-48x32.jpg");

    #[test]
    fn decoding_and_sniffing() {
        assert_eq!(percent_decode("/tmp/a%20b%2Fc%zz%"), "/tmp/a b/c%zz%");
        assert_eq!(sniff(JPG), Some("jpg"));
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 1]), Some("png"));
        assert_eq!(sniff(b"RIFF....WEBP"), None);
        assert_eq!(cache_name("a"), cache_name("a"));
        assert_ne!(cache_name("a"), cache_name("b"));
    }

    #[test]
    fn file_urls() {
        let d = std::env::temp_dir().join(format!("sliver-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cache = d.join("cache");
        // a properly named file is used in place, spaces percent-encoded in the URL
        let named = d.join("cover art.jpg");
        std::fs::write(&named, JPG).unwrap();
        let url = format!("file://{}", named.display().to_string().replace(' ', "%20"));
        assert_eq!(resolve(&url, &cache), Some(named.clone()));
        // a temp name without a useful extension is copied under its real type
        let odd = d.join("blob.tmp");
        std::fs::write(&odd, JPG).unwrap();
        let got = resolve(&format!("file://{}", odd.display()), &cache).unwrap();
        assert!(
            got.starts_with(&cache) && got.extension().unwrap() == "jpg",
            "{got:?}"
        );
        // not an image / missing / unknown scheme
        std::fs::write(d.join("x.png"), b"text").unwrap();
        assert_eq!(
            resolve(&format!("file://{}/x.png", d.display()), &cache),
            None
        );
        assert_eq!(resolve("file:///nonexistent.png", &cache), None);
        assert_eq!(resolve("data:image/png;base64,AAAA", &cache), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn http_urls_are_fetched_once_and_cached() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let mut c = c.unwrap();
                let mut buf = [0u8; 512];
                let _ = c.read(&mut buf);
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    JPG.len()
                );
                let _ = c.write_all(JPG);
            }
        });
        let cache = std::env::temp_dir().join(format!("sliver-art-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let url = format!("http://127.0.0.1:{port}/cover");
        let p = resolve(&url, &cache).expect("fetched");
        assert_eq!(std::fs::read(&p).unwrap(), JPG);
        assert_eq!(
            resolve(&url, &cache),
            Some(p),
            "second time: from the cache"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn cache_dir_defaults() {
        assert_eq!(
            default_cache_dir(|k| (k == "HOME").then(|| "/h".into())),
            Some(PathBuf::from("/h/.cache/sliver/art"))
        );
        assert_eq!(default_cache_dir(|_| None), None);
    }
}
