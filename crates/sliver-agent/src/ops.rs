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
            Op::Dnd(on) => sliver_session::notifications::set_dnd(&ses, on),
        };
        if let Err(e) = r {
            eprintln!("sliver-agent: {e}");
        }
    }
}
