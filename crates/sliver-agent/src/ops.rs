//! The ops thread: carries out session changes (brightness, volume, mute, MPRIS, caffeine) off
//! the main loop, so rendering never waits on D-Bus or `wpctl`. The live state itself comes
//! from `sliver-feeds`.

use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use sliver_session::mpris::Command;
use sliver_session::{audio, brightness};
use zbus::blocking::Connection;

/// A session change for the ops thread. Built by the main loop, which knows the current state
/// (which player, whether caffeine is on).
#[derive(Debug, Clone)]
pub enum Op {
    Brightness {
        keyboard: bool,
        pct: u8,
    },
    Volume {
        mic: bool,
        pct: u8,
    },
    Mute {
        mic: bool,
    },
    Media {
        player: String,
        cmd: Command,
    },
    /// Seek `player` to this position (µs) on this track.
    Seek {
        player: String,
        track: String,
        pos_us: i64,
    },
    Caffeine(bool),
    /// Switch Wi-Fi / Bluetooth / Do Not Disturb to this state.
    Wifi(bool),
    Bluetooth {
        adapter: String,
        on: bool,
    },
    Dnd(bool),
    Dark(bool),
    /// Open (invoke) or close notification `id`.
    OpenNotification(u32),
    CloseNotification(u32),
    /// A configured D-Bus method call.
    DBus(sliver_config::DBusCall),
}

/// Starts the ops thread. `handle` comes from the session feed.
pub fn start_ops(handle: sliver_session::Handle) -> io::Result<Sender<Op>> {
    let (tx, rx) = channel::<Op>();
    let (sys, ses) = (
        Connection::system().map_err(io::Error::other)?,
        Connection::session().map_err(io::Error::other)?,
    );
    thread::Builder::new()
        .name("agent-ops".into())
        .spawn(move || run_ops(rx, handle, sys, ses, brightness::Sysfs::default()))?;
    Ok(tx)
}

fn run_ops(
    rx: Receiver<Op>,
    handle: sliver_session::Handle,
    sys: Connection,
    ses: Connection,
    sysfs: brightness::Sysfs,
) {
    let tools = audio::Tools::default();
    let (screen, keyboard) = (sysfs.screen(), sysfs.keyboard());
    for op in rx {
        let r: Result<(), String> = match op {
            Op::Brightness { keyboard: kbd, pct } => match if kbd { &keyboard } else { &screen } {
                Some(d) => {
                    // Keep the main screen visible: never all the way off from a slider.
                    let pct = if kbd { pct } else { pct.max(1) };
                    let r = brightness::set(&sys, d, pct).map_err(|e| e.to_string());
                    // sysfs has no change events: read back after our own write.
                    handle.refresh_brightness(&sysfs);
                    r
                }
                None => Err("no such backlight".into()),
            },
            Op::Volume { mic: false, pct } => {
                audio::set_volume(&tools, pct).map_err(|e| e.to_string())
            }
            Op::Volume { mic: true, .. } => Err("mic volume isn't supported yet".into()),
            Op::Mute { mic } => {
                let f = if mic {
                    audio::set_mic_mute
                } else {
                    audio::set_mute
                };
                f(&tools, audio::Mute::Toggle).map_err(|e| e.to_string())
            }
            Op::Media { player, cmd } => {
                sliver_session::mpris::control(&ses, &player, cmd).map_err(|e| e.to_string())
            }
            Op::Seek {
                player,
                track,
                pos_us,
            } => sliver_session::mpris::set_position(&ses, &player, &track, pos_us)
                .map_err(|e| e.to_string()),
            Op::Caffeine(on) => handle.set_caffeine(on).map_err(|e| e.to_string()),
            Op::Wifi(on) => sliver_session::network::set_wifi(&sys, on),
            Op::Bluetooth { adapter, on } => {
                sliver_session::bluetooth::set_powered(&sys, &adapter, on)
            }
            Op::CloseNotification(id) => sliver_session::notifications::close(&ses, id),
            Op::OpenNotification(id) => open_notification(&ses, id),
            Op::Dnd(on) => handle.set_dnd(&ses, on),
            Op::DBus(call) => {
                // On its own thread: a method that never answers must not stall the other ops.
                let conn = if call.system_bus {
                    sys.clone()
                } else {
                    ses.clone()
                };
                thread::spawn(move || {
                    if let Err(e) = dbus_call(&conn, &call) {
                        eprintln!("sliver-agent: dbus {}.{}: {e}", call.interface, call.method);
                    }
                });
                Ok(())
            }
            Op::Dark(on) => sliver_session::appearance::set_dark(on),
        };
        if let Err(e) = r {
            eprintln!("sliver-agent: {e}");
        }
    }
}

/// Invoke a notification's default action. The spec has no client call for it (actions are
/// invoked by the server), so use the daemon's own tool; without one, just dismiss it.
fn open_notification(ses: &Connection, id: u32) -> Result<(), String> {
    let installed = |t: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(t).is_file()))
    };
    if installed("makoctl") {
        let ok = std::process::Command::new("makoctl")
            .args(["invoke", "-n", &id.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }
    sliver_session::notifications::close(ses, id)
}

/// Make a GNOME/KDE desktop-control call on the session bus. It never starts the destination
/// (no extension, no effect) and gives up after a few seconds.
pub fn bus_call(call: &sliver_desktop::control::BusCall) -> Result<(), String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&conn)
        .destination(call.dest)
        .and_then(|b| b.path(call.path))
        .and_then(|b| b.interface(call.iface))
        .and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build())
        .map_err(|e| e.to_string())?;
    let flags = zbus::proxy::MethodFlags::NoAutoStart.into();
    let r = match call.arg {
        Some(n) => proxy
            .call_with_flags::<_, _, zbus::zvariant::OwnedValue>(call.method, flags, &(n,))
            .map(|_| ()),
        None => proxy
            .call_with_flags::<_, _, zbus::zvariant::OwnedValue>(call.method, flags, &())
            .map(|_| ()),
    };
    r.map_err(|e| {
        format!(
            "{}: {e} (is the Sliver extension/script running?)",
            call.dest
        )
    })
}

/// Call `call` with its arguments coerced to the method's real types (see [`crate::dbus_call`]).
pub fn dbus_call(conn: &Connection, call: &sliver_config::DBusCall) -> Result<(), String> {
    let types = if call.args.is_empty() {
        Vec::new()
    } else {
        // Introspection tells us `b` from `i` from `u`. Without it, guess from the TOML values.
        conn.call_method(
            Some(call.dest.as_str()),
            call.path.as_str(),
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .ok()
        .and_then(|r| r.body().deserialize::<String>().ok())
        .and_then(|xml| crate::dbus_call::in_signatures(&xml, &call.interface, &call.method))
        .unwrap_or_default()
    };
    let reply = if call.args.is_empty() {
        conn.call_method(
            Some(call.dest.as_str()),
            call.path.as_str(),
            Some(call.interface.as_str()),
            call.method.as_str(),
            &(),
        )
    } else {
        let body = crate::dbus_call::body(&types, &call.args)?;
        conn.call_method(
            Some(call.dest.as_str()),
            call.path.as_str(),
            Some(call.interface.as_str()),
            call.method.as_str(),
            &body,
        )
    };
    reply.map(|_| ()).map_err(|e| e.to_string())
}
