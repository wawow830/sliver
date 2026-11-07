//! The ops threads: carry out session changes (brightness, volume, mute, MPRIS, caffeine) off
//! the main loop, so rendering never waits on D-Bus or `wpctl`. The live state itself comes
//! from [`crate::feeds`].

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use sliver_session::mpris::Command;
use sliver_session::{audio, brightness};
use zbus::blocking::Connection;

/// A session change for the ops threads. Built by the main loop, which knows the current state
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
    /// fcitx5: toggle (`None`) or select an input method.
    InputMethod(Option<String>),
    /// Open (invoke) or close notification `id`.
    OpenNotification(u32),
    CloseNotification(u32),
    /// A configured D-Bus method call.
    DBus(sliver_config::DBusCall),
}

impl Op {
    /// The service this op waits on. Each lane runs its ops in order on its own thread, so a
    /// service that hangs holds up only its own ops.
    fn lane(&self) -> (&'static str, &str) {
        match self {
            Op::Brightness { .. } => ("backlight", ""),
            Op::Volume { .. } | Op::Mute { .. } => ("audio", ""),
            Op::Media { player, .. } | Op::Seek { player, .. } => ("player", player),
            Op::Caffeine(_) => ("caffeine", ""),
            Op::Wifi(_) => ("wifi", ""),
            Op::Bluetooth { .. } => ("bluetooth", ""),
            Op::Dnd(_) | Op::OpenNotification(_) | Op::CloseNotification(_) => {
                ("notifications", "")
            }
            Op::Dark(_) => ("appearance", ""),
            Op::InputMethod(_) => ("input-method", ""),
            Op::DBus(c) if c.system_bus => ("system-bus", &c.dest),
            Op::DBus(c) => ("session-bus", &c.dest),
        }
    }

    /// Whether running this op makes an `older` one still queued pointless: both set the same
    /// thing to a value.
    fn replaces(&self, older: &Op) -> bool {
        match (self, older) {
            (Op::Brightness { keyboard: a, .. }, Op::Brightness { keyboard: b, .. })
            | (Op::Volume { mic: a, .. }, Op::Volume { mic: b, .. }) => a == b,
            (Op::Seek { player: a, .. }, Op::Seek { player: b, .. })
            | (Op::Bluetooth { adapter: a, .. }, Op::Bluetooth { adapter: b, .. }) => a == b,
            (Op::Caffeine(_), Op::Caffeine(_))
            | (Op::Wifi(_), Op::Wifi(_))
            | (Op::Dnd(_), Op::Dnd(_))
            | (Op::Dark(_), Op::Dark(_))
            | (Op::InputMethod(Some(_)), Op::InputMethod(Some(_))) => true,
            _ => false,
        }
    }
}

/// Starts the ops threads. `handle` comes from the session feed.
pub fn start_ops(handle: sliver_session::Handle) -> io::Result<Sender<Op>> {
    let sysfs = brightness::Sysfs::default();
    let ctx = Arc::new(Ctx {
        sys: sliver_bus::system().map_err(io::Error::other)?,
        ses: sliver_bus::session().map_err(io::Error::other)?,
        tools: audio::Tools::default(),
        screen: sysfs.screen(),
        keyboard: sysfs.keyboard(),
        sysfs,
        handle,
    });
    let (tx, rx) = channel::<Op>();
    thread::Builder::new()
        .name("agent-ops".into())
        .spawn(move || route(&rx, &ctx))?;
    Ok(tx)
}

/// Hands each op to its lane, starting the lane's thread on first use, and again if it died.
fn route(rx: &Receiver<Op>, ctx: &Arc<Ctx>) {
    let mut lanes: HashMap<(&'static str, String), Sender<Op>> = HashMap::new();
    for mut op in rx {
        let (kind, name) = op.lane();
        let key = (kind, name.to_owned());
        if let Some(tx) = lanes.get(&key) {
            match tx.send(op) {
                Ok(()) => continue,
                Err(e) => op = e.0,
            }
        }
        let (tx, lane) = channel();
        let ctx = ctx.clone();
        let spawned = thread::Builder::new()
            .name(format!("agent-ops {kind}"))
            .spawn(move || {
                let mut queue = VecDeque::new();
                while let Some(op) = next(&mut queue, &lane) {
                    if let Err(e) = ctx.run(op) {
                        eprintln!("sliver-agent: {e}");
                    }
                }
            });
        match spawned {
            Ok(_) => {
                let _ = tx.send(op);
                lanes.insert(key, tx);
            }
            Err(e) => eprintln!("sliver-agent: ops {kind}: {e}"),
        }
    }
}

/// The oldest op queued that no newer one replaces. `None` once the router is gone.
fn next(queue: &mut VecDeque<Op>, rx: &Receiver<Op>) -> Option<Op> {
    loop {
        if queue.is_empty() {
            queue.push_back(rx.recv().ok()?);
        }
        queue.extend(rx.try_iter());
        let op = queue.pop_front()?;
        if !queue.iter().any(|newer| newer.replaces(&op)) {
            return Some(op);
        }
    }
}

struct Ctx {
    handle: sliver_session::Handle,
    sys: Connection,
    ses: Connection,
    tools: audio::Tools,
    sysfs: brightness::Sysfs,
    screen: Option<brightness::Device>,
    keyboard: Option<brightness::Device>,
}

impl Ctx {
    fn run(&self, op: Op) -> Result<(), String> {
        let Ctx {
            handle,
            sys,
            ses,
            tools,
            sysfs,
            screen,
            keyboard,
        } = self;
        match op {
            Op::Brightness { keyboard: kbd, pct } => match if kbd { keyboard } else { screen } {
                Some(d) => {
                    // Keep the main screen visible: never all the way off from a slider.
                    let pct = if kbd { pct } else { pct.max(1) };
                    let r = brightness::set(sys, d, pct).map_err(|e| e.to_string());
                    // sysfs has no change events: read back after our own write.
                    handle.refresh_brightness(sysfs);
                    r
                }
                None => Err("no such backlight".into()),
            },
            Op::Volume { mic: false, pct } => {
                audio::set_volume(tools, pct).map_err(|e| e.to_string())
            }
            Op::Volume { mic: true, .. } => Err("mic volume isn't supported yet".into()),
            Op::Mute { mic } => {
                let f = if mic {
                    audio::set_mic_mute
                } else {
                    audio::set_mute
                };
                f(tools, audio::Mute::Toggle).map_err(|e| e.to_string())
            }
            Op::Media { player, cmd } => {
                sliver_session::mpris::control(ses, &player, cmd).map_err(|e| e.to_string())
            }
            Op::Seek {
                player,
                track,
                pos_us,
            } => sliver_session::mpris::set_position(ses, &player, &track, pos_us)
                .map_err(|e| e.to_string()),
            Op::Caffeine(on) => handle.set_caffeine(on).map_err(|e| e.to_string()),
            Op::Wifi(on) => sliver_session::network::set_wifi(sys, on),
            Op::Bluetooth { adapter, on } => {
                sliver_session::bluetooth::set_powered(sys, &adapter, on)
            }
            Op::CloseNotification(id) => sliver_session::notifications::close(ses, id),
            Op::OpenNotification(id) => open_notification(ses, id),
            Op::Dnd(on) => handle.set_dnd(ses, on),
            Op::DBus(call) => {
                let conn = if call.system_bus { sys } else { ses };
                dbus_call(conn, &call)
                    .map_err(|e| format!("dbus {}.{}: {e}", call.interface, call.method))
            }
            Op::Dark(on) => sliver_session::appearance::set_dark(on),
            Op::InputMethod(id) => {
                let r = sliver_session::fcitx::switch(ses, id.as_deref());
                // No change signal from fcitx5: read back after our own switch.
                handle.refresh_input_method();
                r
            }
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
        let mut cmd = std::process::Command::new("makoctl");
        cmd.args(["invoke", "-n", &id.to_string()]);
        let ok = sliver_proc::output(cmd, std::time::Duration::from_secs(3))
            .is_ok_and(|o| o.status.success());
        if ok {
            return Ok(());
        }
    }
    sliver_session::notifications::close(ses, id)
}

/// Make a GNOME/KDE desktop-control call on the session bus. It never starts the destination
/// (no extension, no effect) and gives up after a few seconds.
pub fn bus_call(call: &sliver_desktop::control::BusCall) -> Result<(), String> {
    let conn = sliver_bus::session().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&conn)
        .destination(call.dest)
        .and_then(|b| b.path(call.path))
        .and_then(|b| b.interface(call.iface))
        .and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build())
        .map_err(|e| e.to_string())?;
    let flags = zbus::proxy::MethodFlags::NoAutoStart.into();
    let r = match call.arg {
        _ if call.text.len() == 2 => proxy
            .call_with_flags::<_, _, ()>(
                call.method,
                flags,
                &(call.text[0].as_str(), call.text[1].as_str()),
            )
            .map(|_| ()),
        _ if call.text.len() == 1 => proxy
            .call_with_flags::<_, _, ()>(call.method, flags, &(call.text[0].as_str(),))
            .map(|_| ()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_settings_replace_queued_ones_and_commands_all_run() {
        let (tx, rx) = channel();
        for op in [
            Op::Volume {
                mic: false,
                pct: 10,
            },
            Op::Mute { mic: false },
            Op::Volume { mic: true, pct: 5 },
            Op::Volume {
                mic: false,
                pct: 20,
            },
            Op::Mute { mic: false },
            Op::Volume {
                mic: false,
                pct: 30,
            },
        ] {
            tx.send(op).unwrap();
        }
        drop(tx);
        let mut queue = VecDeque::new();
        let ran: Vec<String> = std::iter::from_fn(|| next(&mut queue, &rx))
            .map(|op| format!("{op:?}"))
            .collect();
        assert_eq!(
            ran,
            [
                "Mute { mic: false }",
                "Volume { mic: true, pct: 5 }",
                "Mute { mic: false }",
                "Volume { mic: false, pct: 30 }",
            ]
        );
    }

    #[test]
    fn ops_on_different_services_get_different_lanes() {
        let call = |system_bus, dest: &str| {
            Op::DBus(sliver_config::DBusCall {
                system_bus,
                dest: dest.into(),
                path: "/".into(),
                interface: "x.y".into(),
                method: "Z".into(),
                args: Vec::new(),
            })
        };
        let ops = [
            Op::Mute { mic: true },
            Op::Brightness {
                keyboard: true,
                pct: 1,
            },
            Op::Dnd(true),
            call(false, "a.b"),
            call(false, "c.d"),
            call(true, "a.b"),
        ];
        let lanes: Vec<_> = ops.iter().map(Op::lane).collect();
        let distinct: std::collections::HashSet<_> = lanes.iter().collect();
        assert_eq!(distinct.len(), lanes.len(), "{lanes:?}");
        assert_eq!(
            Op::Mute { mic: false }.lane(),
            Op::Volume { mic: true, pct: 1 }.lane()
        );
    }
}
