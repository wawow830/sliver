//! Tiny `org.freedesktop.DBus.Properties` helpers over a blocking connection.

use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

pub(crate) type Props = HashMap<String, OwnedValue>;

/// `GetAll(iface)` on `dest`/`path`; `None` if the service or object is absent.
///
/// Sent with `NoAutoStart`: reading state must never *start* a service (a notification daemon,
/// say). Without it, on a bus where the service is activatable but not running, the call
/// blocks on the activation, which stalled agent startup on fresh buses.
pub(crate) fn get_all(conn: &Connection, dest: &str, path: &str, iface: &str) -> Option<Props> {
    call_no_autostart(
        conn,
        dest,
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        &(iface,),
    )
}

/// A method call that fails at once instead of activating `dest` when it isn't running.
pub(crate) fn call_no_autostart<B, R>(
    conn: &Connection,
    dest: &str,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> Option<R>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(conn)
        .destination(dest.to_owned())
        .ok()?
        .path(path.to_owned())
        .ok()?
        .interface(iface.to_owned())
        .ok()?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .ok()?;
    proxy
        .call_with_flags::<_, _, R>(method, zbus::proxy::MethodFlags::NoAutoStart.into(), body)
        .ok()
        .flatten()
}

/// `Properties.Set(iface, name, value)` that fails at once instead of starting `dest`.
pub(crate) fn set_no_autostart(
    conn: &Connection,
    dest: &str,
    path: &str,
    iface: &str,
    name: &str,
    value: zbus::zvariant::Value<'_>,
) -> Result<(), String> {
    let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(conn)
        .destination(dest.to_owned())
        .and_then(|b| b.path(path.to_owned()))
        .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
        .and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build())
        .map_err(|e| e.to_string())?;
    proxy
        .call_with_flags::<_, _, ()>(
            "Set",
            zbus::proxy::MethodFlags::NoAutoStart.into(),
            &(iface, name, value),
        )
        .map_err(|e| format!("{dest}: {e}"))?;
    Ok(())
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
