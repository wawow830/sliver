use super::*;
use crate::identity::tests::FakeProc;
use crate::item::tests::{button, props, s};
use std::time::Duration;

struct World {
    reg: Registry,
    proc: FakeProc,
    now: Instant,
}

impl World {
    fn new() -> World {
        World {
            reg: Registry::new(),
            proc: FakeProc::default(),
            now: Instant::now(),
        }
    }
    fn client(&mut self, name: &str, pid: u32, id: ProcIdentity) {
        self.proc.add(pid, 1, id.clone());
        self.reg.connect(name, Some(pid), id, self.now);
    }
    fn ctx(&mut self, client: &str, opts: &[(&str, Prop)]) -> ContextId {
        let id = self
            .reg
            .create_context(client, &props(opts), self.now)
            .unwrap()
            .0;
        self.reg
            .set_items(client, id, &[button("b")], self.now)
            .unwrap();
        id
    }
    fn focus(&self, class: &str, pid: u32) -> Focus {
        Focus {
            window: Some(Window {
                address: "abc".into(),
                class: class.into(),
                title: "Doc - Editor".into(),
                pid: Some(pid),
                ..Default::default()
            }),
            workspace: Some("2".into()),
            locked: false,
        }
    }
    fn resolve(&self, f: &Focus, p: &Policy) -> Resolved {
        self.reg.resolve(f, p, &self.proc)
    }
}

fn flatpak(app: &str, inst: &str) -> ProcIdentity {
    ProcIdentity {
        flatpak: Some((app.into(), inst.into())),
        ..Default::default()
    }
}

#[test]
fn own_app_context_follows_focus() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let c = w.ctx(":1.1", &[]);
    let p = Policy::default();
    assert_eq!(w.resolve(&w.focus("editor", 100), &p).app, Some(c));
    assert_eq!(w.resolve(&w.focus("other", 200), &p).app, None);
    // Claiming an app id doesn't help: binding is by pid/instance only.
    w.client(":1.2", 300, ProcIdentity::default());
    let _ = w.ctx(":1.2", &[("provider", s("editor"))]);
    assert_eq!(w.resolve(&w.focus("editor", 100), &p).app, Some(c));
}

#[test]
fn same_flatpak_instance_or_app_unit_counts() {
    let mut w = World::new();
    w.client(":1.1", 101, flatpak("org.gimp.GIMP", "7"));
    w.proc.add(100, 1, flatpak("org.gimp.GIMP", "7"));
    let c = w.ctx(":1.1", &[]);
    assert_eq!(
        w.resolve(&w.focus("gimp", 100), &Policy::default()).app,
        Some(c)
    );
    assert_eq!(w.reg.context(c).unwrap().provider, "org.gimp.GIMP");
}

#[test]
fn precedence_window_app_children_target() {
    let mut w = World::new();
    let p = Policy::default();
    w.client(":1.term", 100, ProcIdentity::default());
    w.client(":1.vim", 150, ProcIdentity::default());
    w.proc
        .add(150, 120, ProcIdentity::default())
        .add(120, 100, ProcIdentity::default());
    w.client(":1.script", 900, ProcIdentity::default());
    let target = w.ctx(
        ":1.script",
        &[
            ("scope", s("target")),
            ("target", Prop::Map(props(&[("app-id", s("Term"))]))),
        ],
    );
    let f = w.focus("term", 100);
    assert_eq!(w.resolve(&f, &p).app, Some(target));
    let vim = w.ctx(":1.vim", &[("include-children", Prop::Bool(true))]);
    assert_eq!(w.resolve(&f, &p).app, Some(vim));
    let term = w.ctx(":1.term", &[]);
    assert_eq!(w.resolve(&f, &p).app, Some(term));
    let win = w.ctx(":1.term", &[("window", s("hyprland:0xABC"))]);
    assert_eq!(w.resolve(&f, &p).app, Some(win));
    // Bound to another window: never shown for this one.
    let _other = w.ctx(":1.term", &[("window", s("hyprland:0xdef"))]);
    assert_eq!(w.resolve(&f, &p).app, Some(win));
    // Users can disable targets.
    w.reg.destroy(":1.term", win).unwrap();
    w.reg.destroy(":1.term", term).unwrap();
    w.reg.destroy(":1.vim", vim).unwrap();
    let no_targets = Policy {
        allow_target: false,
        ..Policy::default()
    };
    assert_eq!(w.resolve(&f, &no_targets).app, None);
}

#[test]
fn most_recently_shown_wins_within_a_tier() {
    let mut w = World::new();
    let p = Policy::default();
    w.client(":1.1", 100, ProcIdentity::default());
    let a = w.ctx(":1.1", &[]);
    let b = w.ctx(":1.1", &[]);
    let f = w.focus("x", 100);
    assert_eq!(w.resolve(&f, &p).app, Some(b));
    w.reg.set_visible(":1.1", a, false, w.now).unwrap();
    w.reg.set_visible(":1.1", a, true, w.now).unwrap();
    assert_eq!(w.resolve(&f, &p).app, Some(a));
    w.reg.set_visible(":1.1", a, false, w.now).unwrap();
    assert_eq!(w.resolve(&f, &p).app, Some(b));
}

#[test]
fn user_policy_wins() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    w.client(":1.2", 900, ProcIdentity::default());
    let own = w.ctx(":1.1", &[("provider", s("org.app"))]);
    let tgt = w.ctx(
        ":1.2",
        &[
            ("provider", s("org.script")),
            ("scope", s("target")),
            ("target", Prop::Map(props(&[("workspace", s("2"))]))),
        ],
    );
    let f = w.focus("x", 100);
    let mut p = Policy::default();
    p.per_app.insert(
        "x".into(),
        AppPolicy {
            source: AppSource::Provider("org.script".into()),
            allow_modal: true,
        },
    );
    assert_eq!(w.resolve(&f, &p).app, Some(tgt));
    p.per_app.insert(
        "x".into(),
        AppPolicy {
            source: AppSource::Disabled,
            allow_modal: true,
        },
    );
    assert_eq!(w.resolve(&f, &p).app, None);
    let p = Policy {
        hidden_providers: HashSet::from(["org.app".to_owned()]),
        ..Policy::default()
    };
    assert_eq!(w.resolve(&f, &p).app, Some(tgt));
    let _ = own;
    // Locked: nothing at all.
    let mut locked = f.clone();
    locked.locked = true;
    assert_eq!(w.resolve(&locked, &Policy::default()), Resolved::default());
}

#[test]
fn app_policy_lookup_is_case_insensitive_with_default() {
    let mut p = Policy::default();
    let off = AppPolicy {
        source: AppSource::Disabled,
        allow_modal: true,
    };
    let no_modal = AppPolicy {
        source: AppSource::Auto,
        allow_modal: false,
    };
    p.per_app.insert("*".into(), no_modal.clone());
    p.per_app.insert("Firefox".into(), off.clone());
    assert_eq!(p.for_app("firefox"), off);
    assert_eq!(p.for_app("FIREFOX"), off);
    assert_eq!(p.for_app("foot"), no_modal);
    assert_eq!(Policy::default().for_app("foot"), AppPolicy::default());
}

#[test]
fn modal_needs_scope_and_permission() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let m = w.ctx(":1.1", &[("modal", Prop::Bool(true))]);
    let f = w.focus("x", 100);
    let r = w.resolve(&f, &Policy::default());
    assert_eq!((r.app, r.modal), (Some(m), true));
    let mut p = Policy::default();
    p.per_app.insert(
        "x".into(),
        AppPolicy {
            source: AppSource::Auto,
            allow_modal: false,
        },
    );
    assert!(!w.resolve(&f, &p).modal);
    let ev = w.reg.interact(m, "", Interaction::CloseModal).unwrap();
    assert_eq!((ev.kind, ev.item.as_str()), ("closed", ""));
    assert_eq!(w.resolve(&f, &Policy::default()).app, None);
}

#[test]
fn status_items_order_and_sandbox_consent() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    w.client(":1.2", 200, ProcIdentity::default());
    w.client(":1.3", 300, flatpak("org.flat.App", "1"));
    let z = w.ctx(":1.1", &[("scope", s("status")), ("provider", s("org.z"))]);
    let a = w.ctx(":1.2", &[("scope", s("status")), ("provider", s("org.a"))]);
    let f = w.ctx(":1.3", &[("scope", s("status"))]);
    let mut p = Policy::default();
    assert_eq!(w.resolve(&Focus::default(), &p).status, vec![a, z]);
    p.status_order = vec!["org.z".into()];
    p.sandbox_status_allowed.insert("org.flat.App".into());
    assert_eq!(w.resolve(&Focus::default(), &p).status, vec![z, a, f]);
}

#[test]
fn sandbox_restrictions() {
    let mut w = World::new();
    w.client(":1.f", 100, flatpak("org.flat.App", "1"));
    let t = props(&[
        ("scope", s("target")),
        ("target", Prop::Map(props(&[("app-id", s("firefox"))]))),
    ]);
    assert!(matches!(
        w.reg.create_context(":1.f", &t, w.now),
        Err(ApiError::AccessDenied(_))
    ));
    let spoof = props(&[("provider", s("org.mozilla.firefox"))]);
    assert!(matches!(
        w.reg.create_context(":1.f", &spoof, w.now),
        Err(ApiError::AccessDenied(_))
    ));
    let sub = props(&[("provider", s("org.flat.App.Player"))]);
    w.reg.create_context(":1.f", &sub, w.now).unwrap();
}

#[test]
fn ownership_limits_and_cleanup() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    w.client(":1.2", 200, ProcIdentity::default());
    let c = w.ctx(":1.1", &[]);
    assert!(matches!(
        w.reg.set_items(":1.2", c, &[], w.now),
        Err(ApiError::AccessDenied(_))
    ));
    assert!(matches!(
        w.reg.destroy(":1.2", c),
        Err(ApiError::AccessDenied(_))
    ));
    assert!(matches!(
        w.reg.create_context(":1.9", &Props::new(), w.now),
        Err(ApiError::AccessDenied(_))
    ));
    for _ in 1..limits::CONTEXTS_PER_CLIENT {
        w.ctx(":1.1", &[]);
    }
    assert!(matches!(
        w.reg.create_context(":1.1", &Props::new(), w.now),
        Err(ApiError::LimitExceeded(_))
    ));
    let before = w.reg.generation();
    assert_eq!(w.reg.client_gone(":1.1").len(), limits::CONTEXTS_PER_CLIENT);
    assert!(w.reg.generation() > before);
    assert_eq!(w.reg.contexts().count(), 0);
}

#[test]
fn rate_limit_refills() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let c = w.ctx(":1.1", &[]);
    let burst = (0..1000)
        .take_while(|_| w.reg.set_visible(":1.1", c, true, w.now).is_ok())
        .count();
    assert!((200..=240).contains(&burst), "burst {burst}");
    assert!(matches!(
        w.reg.set_visible(":1.1", c, true, w.now),
        Err(ApiError::LimitExceeded(_))
    ));
    let later = w.now + Duration::from_secs(1);
    assert!(w.reg.set_visible(":1.1", c, true, later).is_ok());
}

#[test]
fn update_is_atomic() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let c = w
        .reg
        .create_context(":1.1", &Props::new(), w.now)
        .unwrap()
        .0;
    w.reg
        .set_items(":1.1", c, &[button("a"), button("b"), button("c")], w.now)
        .unwrap();
    let ok = BTreeMap::from([
        ("a".to_owned(), props(&[("label", s("New"))])),
        ("nope".to_owned(), Props::new()),
    ]);
    let warns = w.reg.update(":1.1", c, &ok, w.now).unwrap();
    assert!(warns.iter().any(|x| x.contains("nope")));
    assert_eq!(
        w.reg
            .context(c)
            .unwrap()
            .find("a")
            .unwrap()
            .label
            .as_deref(),
        Some("New")
    );
    // Three max-size icons exceed the per-context budget: nothing at all is applied.
    let icon = |l: &str| {
        props(&[
            ("label", s(l)),
            ("icon-data", Prop::Bytes(vec![0; limits::ICON_BYTES])),
        ])
    };
    let big = BTreeMap::from([
        ("a".to_owned(), icon("X")),
        ("b".to_owned(), icon("Y")),
        ("c".to_owned(), icon("Z")),
    ]);
    assert!(matches!(
        w.reg.update(":1.1", c, &big, w.now),
        Err(ApiError::LimitExceeded(_))
    ));
    let ctx = w.reg.context(c).unwrap();
    assert_eq!(ctx.find("a").unwrap().label.as_deref(), Some("New"));
    assert!(ctx.find("b").unwrap().icon_data.is_none());
}

#[test]
fn scrubber_picks_and_scrolls() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let c = w
        .reg
        .create_context(":1.1", &Props::new(), w.now)
        .unwrap()
        .0;
    let cands = (0..12)
        .map(|i| Prop::Map(props(&[("label", s(&format!("tab {i}")))])))
        .collect();
    let items = [props(&[
        ("id", s("tabs")),
        ("type", s("scrubber")),
        ("candidates", Prop::List(cands)),
        ("visible-count", Prop::Int(5)),
        ("offset", Prop::Int(99)),
    ])];
    w.reg.set_items(":1.1", c, &items, w.now).unwrap();
    let scrub = |w: &World| match &w.reg.context(c).unwrap().items[0].kind {
        ItemKind::Scrubber {
            selected, offset, ..
        } => (*selected, *offset),
        k => panic!("{k:?}"),
    };
    assert_eq!(
        scrub(&w),
        (-1, 7),
        "offset clamped so the last page is full"
    );
    let ev = w.reg.interact(c, "tabs", Interaction::Select(9)).unwrap();
    assert_eq!(
        (ev.kind, ev.data.get("index")),
        ("selected", Some(&Prop::Int(9)))
    );
    assert!(w.reg.interact(c, "tabs", Interaction::Select(12)).is_none());
    let ev = w.reg.interact(c, "tabs", Interaction::Scroll(2)).unwrap();
    assert_eq!(
        (ev.kind, ev.data.get("offset")),
        ("scrolled", Some(&Prop::Int(2)))
    );
    assert_eq!(scrub(&w), (9, 2));
    // Past the end clamps; no change, no event.
    assert!(w.reg.interact(c, "tabs", Interaction::Scroll(-4)).is_some());
    assert!(w.reg.interact(c, "tabs", Interaction::Scroll(-1)).is_none());
    assert_eq!(scrub(&w), (9, 0));
    w.reg.interact(c, "tabs", Interaction::Scroll(50)).unwrap();
    assert_eq!(scrub(&w), (9, 7));
}

#[test]
fn interactions_update_state_and_emit() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let c = w
        .reg
        .create_context(":1.1", &Props::new(), w.now)
        .unwrap()
        .0;
    let items = [
        props(&[("id", s("t")), ("type", s("toggle"))]),
        props(&[
            ("id", s("v")),
            ("type", s("slider")),
            ("max", Prop::Num(10.0)),
            ("step", Prop::Num(2.0)),
        ]),
        props(&[
            ("id", s("seg")),
            ("type", s("segmented")),
            (
                "segments",
                Prop::List(vec![
                    Prop::Map(props(&[("label", s("A"))])),
                    Prop::Map(props(&[("label", s("B"))])),
                ]),
            ),
        ]),
        props(&[
            ("id", s("b")),
            ("type", s("button")),
            ("gestures", Prop::List(vec![s("hold")])),
        ]),
        props(&[
            ("id", s("off")),
            ("type", s("button")),
            ("enabled", Prop::Bool(false)),
        ]),
    ];
    w.reg.set_items(":1.1", c, &items, w.now).unwrap();
    let ev = w
        .reg
        .interact(c, "t", Interaction::Gesture(Gesture::Tap))
        .unwrap();
    assert_eq!(
        (ev.kind, ev.data.get("state")),
        ("toggled", Some(&Prop::Bool(true)))
    );
    assert_eq!(ev.destination, ":1.1");
    let ev = w
        .reg
        .interact(
            c,
            "v",
            Interaction::Slider {
                value: 4.9,
                phase: Phase::Change,
            },
        )
        .unwrap();
    assert_eq!(ev.data.get("value"), Some(&Prop::Num(4.0)));
    assert_eq!(ev.data.get("phase"), Some(&s("change")));
    let ev = w.reg.interact(c, "seg", Interaction::Select(1)).unwrap();
    assert_eq!(ev.data.get("index"), Some(&Prop::Int(1)));
    assert!(w.reg.interact(c, "seg", Interaction::Select(2)).is_none());
    assert_eq!(
        w.reg
            .interact(c, "b", Interaction::Gesture(Gesture::Hold))
            .unwrap()
            .kind,
        "hold"
    );
    assert!(
        w.reg
            .interact(c, "b", Interaction::Gesture(Gesture::SwipeLeft))
            .is_none(),
        "not opted in"
    );
    assert!(
        w.reg
            .interact(c, "off", Interaction::Gesture(Gesture::Tap))
            .is_none()
    );
    assert!(
        w.reg
            .interact(c, "nope", Interaction::Gesture(Gesture::Tap))
            .is_none()
    );
    let sig = w.reg.signal(&[c], 8);
    assert_eq!(
        (sig[0].kind, sig[0].data.get("n")),
        ("signal", Some(&Prop::Int(8)))
    );
}

#[test]
fn on_screen_changes() {
    let mut w = World::new();
    w.client(":1.1", 100, ProcIdentity::default());
    let a = w.ctx(":1.1", &[]);
    let b = w.ctx(":1.1", &[]);
    assert_eq!(w.reg.set_on_screen(&HashSet::from([a])), vec![(a, true)]);
    assert_eq!(
        w.reg.set_on_screen(&HashSet::from([b])),
        vec![(a, false), (b, true)]
    );
    assert!(w.reg.set_on_screen(&HashSet::from([b])).is_empty());
}

#[test]
fn option_errors() {
    let mut w = Warnings::new();
    assert!(parse_options(&props(&[("scope", s("everywhere"))]), &mut w).is_err());
    assert!(parse_options(&props(&[("scope", s("target"))]), &mut w).is_err());
    assert!(
        parse_options(
            &props(&[("scope", s("target")), ("target", Prop::Map(Props::new()))]),
            &mut w
        )
        .is_err()
    );
    assert!(
        parse_options(
            &props(&[
                ("scope", s("target")),
                ("target", Prop::Map(props(&[("title-regex", s("(("))])))
            ]),
            &mut w
        )
        .is_err()
    );
    assert!(parse_options(&props(&[("modal", s("yes"))]), &mut w).is_err());
    assert!(parse_options(&props(&[("provider", s("has space"))]), &mut w).is_err());
    parse_options(
        &props(&[
            ("scope", s("status")),
            ("modal", Prop::Bool(true)),
            ("x", Prop::Int(1)),
        ]),
        &mut w,
    )
    .unwrap();
    assert_eq!(w.len(), 2);
}
