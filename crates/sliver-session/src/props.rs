//! Tiny `org.freedesktop.DBus.Properties` helpers over a blocking connection.

use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

pub(crate) type Props = HashMap<String, OwnedValue>;

/// `GetAll(iface)` on `dest`/`path`; `None` if the service or object is absent.
pub(crate) fn get_all(conn: &Connection, dest: &str, path: &str, iface: &str) -> Option<Props> {
    conn.call_method(
        Some(dest),
        path,
        Some("org.freedesktop.DBus.Properties"),
        "GetAll",
        &(iface,),
    )
    .ok()?
    .body()
    .deserialize::<Props>()
    .ok()
}

pub(crate) fn boolean(p: &Props, k: &str) -> Option<bool> {
    bool::try_from(p.get(k)?.try_clone().ok()?).ok()
}

pub(crate) fn uint(p: &Props, k: &str) -> Option<u32> {
    u32::try_from(p.get(k)?.try_clone().ok()?).ok()
}

pub(crate) fn float(p: &Props, k: &str) -> Option<f64> {
    f64::try_from(p.get(k)?.try_clone().ok()?).ok()
}

pub(crate) fn string(p: &Props, k: &str) -> Option<String> {
    String::try_from(p.get(k)?.try_clone().ok()?).ok()
}

/// Integer of either sign/width (MPRIS players disagree on `mpris:length`'s type).
pub(crate) fn int(v: &OwnedValue) -> Option<i64> {
    let v = v.try_clone().ok()?;
    i64::try_from(v.try_clone().ok()?)
        .ok()
        .or_else(|| {
            u64::try_from(v.try_clone().ok()?)
                .ok()
                .and_then(|n| i64::try_from(n).ok())
        })
        .or_else(|| i32::try_from(v.try_clone().ok()?).ok().map(i64::from))
        .or_else(|| u32::try_from(v).ok().map(i64::from))
}
