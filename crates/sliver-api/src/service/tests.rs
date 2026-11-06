use super::desktop::focus_window;
use super::*;
use sliver_ipc::dbus;

#[test]
fn error_names_match_the_spec() {
    use zbus::DBusError;
    assert_eq!(
        Error::AccessDenied(String::new()).name().as_str(),
        dbus::error::ACCESS_DENIED
    );
    assert_eq!(
        Error::LimitExceeded(String::new()).name().as_str(),
        dbus::error::LIMIT_EXCEEDED
    );
    assert_eq!(
        Error::InvalidArgs(String::new()).name().as_str(),
        dbus::error::INVALID_ARGS
    );
    assert_eq!(
        Error::Busy(String::new()).name().as_str(),
        dbus::error::BUSY
    );
}

#[test]
fn value_roundtrip() {
    let p = Prop::Map(Props::from([
        (
            "a".into(),
            Prop::List(vec![Prop::Int(1), Prop::Str("x".into())]),
        ),
        ("b".into(), Prop::Bytes(vec![1, 2, 3])),
        ("c".into(), Prop::Num(0.5)),
        ("d".into(), Prop::Int(1 << 40)),
    ]));
    assert_eq!(prop_from_value(&prop_to_value(&p)), Some(p));
}

#[test]
fn focus_keys() {
    let p = Props::from([
        ("app-id".into(), Prop::Str("firefox".into())),
        ("pid".into(), Prop::Int(42)),
        ("window".into(), Prop::Str("hyprland:0xabc".into())),
        ("fullscreen".into(), Prop::Int(2)),
    ]);
    let w = focus_window(&p).unwrap();
    assert_eq!(
        (w.class.as_str(), w.pid, w.address.as_str(), w.fullscreen),
        ("firefox", Some(42), "abc", true)
    );
    assert!(focus_window(&Props::new()).is_none());
}
