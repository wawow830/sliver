use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;

/// A stand-in for fcitx5's `/controller`: the current IM is whatever `cur` says, the group is
/// keyboard-us + pinyin, and every read is counted.
struct Controller {
    cur: Arc<Mutex<String>>,
    reads: Arc<AtomicUsize>,
    calls: Arc<Mutex<Vec<String>>>,
}

fn info(id: &str) -> Info {
    let (name, label) = match id {
        "pinyin" => ("Pinyin", "拼"),
        "mozc" => ("Mozc", "あ"),
        _ => ("Keyboard - English (US)", "en"),
    };
    (
        id.into(),
        name.into(),
        String::new(),
        "icon".into(),
        label.into(),
        "xx".into(),
        "addon".into(),
        true,
        String::new(),
        HashMap::new(),
    )
}

#[zbus::interface(name = "org.fcitx.Fcitx.Controller1")]
impl Controller {
    fn current_input_method_info(&self) -> Info {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let cur = self.cur.lock().unwrap().clone();
        if cur.is_empty() { info("") } else { info(&cur) }
    }
    fn current_input_method_group(&self) -> String {
        "Default".into()
    }
    fn full_input_method_group_info(
        &self,
        _group: String,
    ) -> (
        String,
        String,
        String,
        HashMap<String, OwnedValue>,
        Vec<Info>,
    ) {
        (
            "Default".into(),
            "pinyin".into(),
            "us".into(),
            HashMap::new(),
            vec![info("keyboard-us"), info("pinyin")],
        )
    }
    fn toggle(&self) {
        let mut c = self.cur.lock().unwrap();
        *c = if *c == "pinyin" {
            "keyboard-us"
        } else {
            "pinyin"
        }
        .into();
        self.calls.lock().unwrap().push("toggle".into());
    }
    #[zbus(name = "SetCurrentIM")]
    fn set_current_im(&self, im: String) {
        *self.cur.lock().unwrap() = im.clone();
        self.calls.lock().unwrap().push(format!("set {im}"));
    }
}

struct Fixture {
    tb: sliver_testbus::TestBus,
    cur: Arc<Mutex<String>>,
    reads: Arc<AtomicUsize>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        Some(Fixture {
            tb: sliver_testbus::TestBus::start()?,
            cur: Arc::new(Mutex::new("keyboard-us".into())),
            reads: Arc::default(),
            calls: Arc::default(),
        })
    }

    fn conn(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.tb.address())
            .unwrap()
            .build()
            .unwrap()
    }

    /// fcitx5 appears on the bus.
    fn serve(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.tb.address())
            .unwrap()
            .name(NAME)
            .unwrap()
            .serve_at(
                PATH,
                Controller {
                    cur: self.cur.clone(),
                    reads: self.reads.clone(),
                    calls: self.calls.clone(),
                },
            )
            .unwrap()
            .build()
            .unwrap()
    }

    /// A session whose input-method states arrive on the returned channel.
    fn follow(&self) -> (Handle, std::sync::mpsc::Receiver<Option<InputMethod>>) {
        let mut session = crate::Session::start(self.conn(), self.conn());
        let handle = session.handle();
        handle.start_input_method(self.conn());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            use crate::SessionSource;
            while let Ok(Some(s)) = session.next_change() {
                if tx.send(s.input_method).is_err() {
                    return;
                }
            }
        });
        (handle, rx)
    }
}

fn expect(
    rx: &std::sync::mpsc::Receiver<Option<InputMethod>>,
    f: impl Fn(&Option<InputMethod>) -> bool,
) -> Option<InputMethod> {
    let t = std::time::Instant::now();
    let mut last = None;
    while t.elapsed() < Duration::from_secs(8) {
        if let Ok(s) = rx.recv_timeout(Duration::from_millis(100)) {
            if f(&s) {
                return s;
            }
            last = s;
        }
    }
    panic!("no matching state in 8 s; last: {last:?}");
}

#[test]
fn reads_the_current_input_method_its_label_and_the_group() {
    let Some(fx) = Fixture::new() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let _fcitx = fx.serve();
    *fx.cur.lock().unwrap() = "pinyin".into();
    let (up, im) = read(&fx.conn());
    assert!(up);
    let im = im.unwrap();
    assert_eq!(im.current.id, "pinyin");
    assert_eq!(im.current.label, "拼");
    assert_eq!(im.current.name, "Pinyin");
    assert_eq!(
        im.group.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["keyboard-us", "pinyin"]
    );
    // No window focused: running, but nothing to show.
    *fx.cur.lock().unwrap() = String::new();
    assert_eq!(read(&fx.conn()), (true, None));
}

#[test]
fn not_running_is_none_and_never_starts_it() {
    let Some(fx) = Fixture::new() else {
        return;
    };
    assert_eq!(read(&fx.conn()), (false, None));
}

#[test]
fn follows_fcitx_appearing_a_hotkey_switch_and_leaving() {
    let Some(fx) = Fixture::new() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let (_handle, rx) = fx.follow();
    expect(&rx, |s| s.is_none());
    // fcitx5 starts: seen through NameOwnerChanged.
    let fcitx = fx.serve();
    let s = expect(&rx, |s| s.is_some()).unwrap();
    assert_eq!(s.current.id, "keyboard-us");
    // A hotkey switch sends nothing: the poll finds it.
    *fx.cur.lock().unwrap() = "pinyin".into();
    let s = expect(&rx, |s| {
        s.as_ref().is_some_and(|m| m.current.id == "pinyin")
    })
    .unwrap();
    assert_eq!(s.current.label, "拼");
    drop(fcitx);
    expect(&rx, |s| s.is_none());
}

#[test]
fn a_dark_bar_stops_asking_and_wake_reads_afresh() {
    let Some(fx) = Fixture::new() else {
        return;
    };
    let _fcitx = fx.serve();
    let (handle, rx) = fx.follow();
    expect(&rx, |s| s.is_some());
    handle.set_awake(false);
    std::thread::sleep(Duration::from_millis(300)); // let an in-flight read finish
    let before = fx.reads.load(Ordering::SeqCst);
    *fx.cur.lock().unwrap() = "pinyin".into();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        fx.reads.load(Ordering::SeqCst),
        before,
        "no reads while dark"
    );
    handle.set_awake(true);
    expect(&rx, |s| {
        s.as_ref().is_some_and(|m| m.current.id == "pinyin")
    });
}

#[test]
fn switching_toggles_or_selects() {
    let Some(fx) = Fixture::new() else {
        return;
    };
    let _fcitx = fx.serve();
    switch(&fx.conn(), None).unwrap();
    switch(&fx.conn(), Some("mozc")).unwrap();
    assert_eq!(*fx.calls.lock().unwrap(), ["toggle", "set mozc"]);
}
