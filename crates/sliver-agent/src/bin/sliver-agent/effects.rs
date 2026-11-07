//! Carrying out the agent's effects: session changes, processes, compositor calls, keys.

use super::*;

/// The `systemd-run` prefix that puts a command in its own transient scope, so it outlives
/// the agent: under `sliver-agent.service` (KillMode=control-group) children in our cgroup die
/// with every agent restart.
pub(super) fn scope_prefix(argv: &[String], seq: u64) -> Vec<String> {
    let name: String = argv
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("cmd")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    vec![
        "systemd-run".into(),
        "--user".into(),
        "--scope".into(),
        "--collect".into(),
        "--quiet".into(),
        format!(
            "--unit=app-sliver-{name}-{}-{seq}.scope",
            std::process::id()
        ),
        "--".into(),
    ]
}

/// Starts a user command: in its own systemd scope when we run as a systemd service (falling
/// back to a plain spawn), always in its own process group with stdin closed.
pub(super) fn spawn_detached(argv: &[String]) -> std::io::Result<Child> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let plain = |argv: &[String]| {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty command"))?;
        Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
    };
    if std::env::var_os("INVOCATION_ID").is_some() {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut full = scope_prefix(argv, seq);
        full.extend(argv.iter().cloned());
        if let Ok(child) = plain(&full) {
            return Ok(child);
        }
    }
    plain(argv)
}

pub(super) fn apply(
    e: Effect,
    link: &Link,
    children: &mut Vec<Child>,
    live: &Live,
) -> std::io::Result<()> {
    {
        match e {
            Effect::Session(op) => {
                let op = match op {
                    SessionOp::ScreenBrightness(pct) => Op::Brightness {
                        keyboard: false,
                        pct,
                    },
                    SessionOp::KeyboardBrightness(pct) => Op::Brightness {
                        keyboard: true,
                        pct,
                    },
                    SessionOp::Volume { mic, pct } => Op::Volume { mic, pct },
                    SessionOp::ToggleMute { mic } => Op::Mute { mic },
                    SessionOp::ToggleCaffeine => Op::Caffeine(!live.agg.session.caffeine),
                    SessionOp::ToggleWifi => {
                        Op::Wifi(!live.agg.session.network.wifi_enabled.unwrap_or(true))
                    }
                    SessionOp::ToggleBluetooth => match &live.agg.session.bluetooth.adapter {
                        Some(a) => Op::Bluetooth {
                            adapter: a.clone(),
                            on: !live.agg.session.bluetooth.powered,
                        },
                        None => return Ok(()),
                    },
                    SessionOp::OpenNotification | SessionOp::DismissNotification => {
                        match live.agg.session.notifications.latest.as_ref().map(|l| l.id) {
                            Some(id) if op == SessionOp::OpenNotification => {
                                Op::OpenNotification(id)
                            }
                            Some(id) => Op::CloseNotification(id),
                            None => return Ok(()),
                        }
                    }
                    SessionOp::DBus(call) => Op::DBus(call),
                    SessionOp::ToggleDark => {
                        Op::Dark(!live.agg.session.appearance.dark.unwrap_or(false))
                    }
                    SessionOp::ToggleDnd => {
                        Op::Dnd(!live.agg.session.notifications.dnd.unwrap_or(false))
                    }
                    SessionOp::MediaOn { player, cmd } => Op::Media { player, cmd },
                    SessionOp::Seek { player, pct } => {
                        let s = &live.agg.session;
                        let m = match &player {
                            Some(p) => s.players.iter().find(|m| &m.player == p),
                            None => s.media.as_ref(),
                        };
                        match m.and_then(|m| {
                            Some((m.player.clone(), m.track_id.clone()?, m.length_us?))
                        }) {
                            Some((player, track, len)) => Op::Seek {
                                player,
                                track,
                                pos_us: len * i64::from(pct) / 100,
                            },
                            None => return Ok(()),
                        }
                    }
                    SessionOp::Media(cmd) => match &live.agg.session.media {
                        Some(m) => Op::Media {
                            player: m.player.clone(),
                            cmd,
                        },
                        None => return Ok(()),
                    },
                };
                live.op(op);
            }
            Effect::ModuleFired(key) => {
                if let Some(rt) = live.modules() {
                    rt.action_fired(&key);
                }
            }
            Effect::Signal(n) => {
                if let Some(rt) = live.modules() {
                    rt.signal(n.min(u8::MAX as u32) as u8);
                }
            }
            Effect::Profile(name) => {
                // The reloader watches `active-profile`, so writing it is the switch.
                if let Err(e) = sliver_config::profile::set_active(&name) {
                    eprintln!("sliver-agent: profile: {e}");
                }
            }
            Effect::Update(key) => {
                if !live.modules().is_some_and(|rt| rt.trigger(&key)) {
                    eprintln!("sliver-agent: update: no user module {key:?}");
                }
            }
            Effect::Api {
                context,
                item,
                what,
            } => {
                if let Some(api) = &live.api {
                    api.interact(context, &item, what);
                }
            }
            Effect::Desktop(action) => {
                // A compositor round trip: keep it off the main loop.
                std::thread::spawn(move || {
                    let backend = sliver_desktop::Backend::detect(|k| std::env::var(k).ok());
                    if let Some(call) = sliver_desktop::control::bus_call(backend, &action) {
                        if let Err(e) = sliver_agent::ops::bus_call(&call) {
                            eprintln!("sliver-agent: {action:?}: {e}");
                        }
                        return;
                    }
                    if let Err(e) = sliver_desktop::control::run(|k| std::env::var(k).ok(), &action)
                    {
                        eprintln!("sliver-agent: {action:?}: {e}");
                    }
                });
            }
            // The driver draws; these never reach here.
            Effect::Redraw | Effect::RedrawPress => {}
            Effect::EmitKeys(keys, mode) => link.emit_keys(&keys, mode)?,
            Effect::Log(msg) => eprintln!("sliver-agent: {msg}"),
            Effect::Spawn(argv) => match spawn_detached(&argv) {
                Ok(child) => children.push(child),
                Err(e) => eprintln!(
                    "sliver-agent: {}: {e}",
                    argv.first().map_or("", |s| s.as_str())
                ),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_unit_names_are_safe() {
        let p = scope_prefix(&["/usr/bin/fire fox".into(), "x".into()], 3);
        assert_eq!(
            &p[..5],
            ["systemd-run", "--user", "--scope", "--collect", "--quiet"]
        );
        assert_eq!(
            p[5],
            format!("--unit=app-sliver-fire_fox-{}-3.scope", std::process::id())
        );
        assert_eq!(p[6], "--");
    }
}
