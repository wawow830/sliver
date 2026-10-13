use super::*;
use std::os::unix::fs::symlink;
use std::sync::mpsc::channel;

/// A fake /dev and /proc: `video0` is a file, and a "process" is a directory with `comm` and
/// `fd/N` symlinks, as /proc has them.
struct Fake {
    root: PathBuf,
}

impl Fake {
    fn new(tag: &str) -> Fake {
        let root = std::env::temp_dir().join(format!("sliver-v4l2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dev")).unwrap();
        std::fs::create_dir_all(root.join("proc")).unwrap();
        Fake { root }
    }

    fn dev(&self) -> PathBuf {
        self.root.join("dev")
    }

    fn proc(&self) -> PathBuf {
        self.root.join("proc")
    }

    fn node(&self, name: &str) {
        std::fs::write(self.dev().join(name), "").unwrap();
    }

    fn open(&self, pid: u32, comm: &str, node: &str) {
        let fd = self.proc().join(pid.to_string()).join("fd");
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(
            self.proc().join(pid.to_string()).join("comm"),
            format!("{comm}\n"),
        )
        .unwrap();
        symlink(self.dev().join(node), fd.join("7")).unwrap();
    }

    fn close(&self, pid: u32) {
        std::fs::remove_dir_all(self.proc().join(pid.to_string())).unwrap();
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn holders_are_named_and_the_plumbing_is_ignored() {
    let f = Fake::new("holders");
    f.node("video0");
    f.node("video1");
    f.node("videocard"); // not a video node
    f.open(10, "zoom", "video0");
    f.open(11, "chrome", "video1");
    f.open(12, "chrome", "video1");
    f.open(13, "wireplumber", "video0");
    f.open(14, "v4l_id", "video0");
    f.open(15, "bash", "videocard");
    // a process holding something else
    let other = f.proc().join("16/fd");
    std::fs::create_dir_all(&other).unwrap();
    symlink("/dev/null", other.join("3")).unwrap();
    std::fs::write(f.proc().join("16/comm"), "mpv\n").unwrap();
    std::fs::write(f.proc().join("self"), "").unwrap(); // not a pid
    assert_eq!(holders(&f.dev(), &f.proc()), ["chrome", "zoom"]);
    assert!(holders(&f.root.join("nope"), &f.proc()).is_empty());
}

fn expect(rx: &std::sync::mpsc::Receiver<Vec<String>>, want: &[&str]) {
    let t = std::time::Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        if let Ok(h) = rx.recv_timeout(Duration::from_millis(100))
            && h == want
        {
            return;
        }
    }
    panic!("never saw {want:?}");
}

/// Real inotify on a real directory: opening and closing the node (what a camera app does)
/// changes the report; nothing polls, so the fake process table is only read after an event.
#[test]
fn follows_opens_and_closes_of_the_node() {
    let f = Fake::new("watch");
    f.node("video0");
    let (dev, proc_root) = (f.dev(), f.proc());
    let (tx, rx) = channel();
    std::thread::spawn(move || watch(&dev, &proc_root, move |h| tx.send(h).is_ok()));
    expect(&rx, &[]);
    // The app opens the node: the open event makes the watcher look.
    f.open(20, "zoom", "video0");
    let _ = std::fs::File::open(f.dev().join("video0")).unwrap();
    expect(&rx, &["zoom"]);
    // It closes: the close event (the File above was dropped) makes it look again.
    f.close(20);
    let _ = std::fs::File::open(f.dev().join("video0")).unwrap();
    expect(&rx, &[]);
    // A node that appears later is watched too.
    f.node("video2");
    std::thread::sleep(Duration::from_millis(200));
    f.open(21, "obs", "video2");
    let _ = std::fs::File::open(f.dev().join("video2")).unwrap();
    expect(&rx, &["obs"]);
}
