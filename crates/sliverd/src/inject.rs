//! `sliverd --fake-hardware --touch-socket PATH`: inject hardware input into a fake-hardware
//! sliverd, so headless runs can test real touches end to end (regions, the key fast path, the
//! hatch, agent touch events) without a bar.
//!
//! PATH is a Unix **datagram** socket. Each datagram holds one or more lines:
//!
//! ```text
//! tap X              down + up on slot 0 at X
//! down X [SLOT]      touch-down (slot defaults to 0)
//! move X [SLOT]
//! up [SLOT]          touch-up where that slot last was
//! cancel [SLOT]      the touch was cancelled (palm rejection on real hardware)
//! fn down|up         KEY_FN on a seat0 keyboard (the hatch is Fn double-press-hold)
//! activity           other seat0 input (keeps the bar awake)
//! lid open|closed
//! screen-backlight   the main display's brightness changed
//! ```
//!
//! X is in logical bar pixels (0 at the left edge, up to the panel width), the same space as
//! regions. Touches land at the vertical middle of the bar. Bad lines are reported on stderr and
//! skipped. Send with e.g. `printf 'tap 100\n' | socat - UNIX-SENDTO:PATH`.
//!
//! Only `--fake-hardware` accepts it: real hardware has a real digitizer.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sliver_ipc::proto::TouchPhase;

use crate::backend::{HwEvent, InputSource, TouchEvent};

/// Turns command lines into hardware events. Remembers where each slot is so `up` needs no X.
#[derive(Debug, Default)]
pub struct Parser {
    height: f32,
    slots: HashMap<u8, f32>,
}

impl Parser {
    pub fn new(panel_height: u32) -> Parser {
        Parser {
            height: panel_height as f32,
            slots: HashMap::new(),
        }
    }

    /// One line. Blank lines and `#` comments give nothing.
    pub fn line(&mut self, line: &str, time_us: u64) -> Result<Vec<HwEvent>, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((&cmd, rest)) = words.split_first() else {
            return Ok(Vec::new());
        };
        if cmd.starts_with('#') {
            return Ok(Vec::new());
        }
        let x = |i: usize| -> Result<f32, String> {
            let w = rest.get(i).ok_or(format!("{cmd}: needs X"))?;
            w.parse::<f32>()
                .ok()
                .filter(|x| x.is_finite())
                .ok_or(format!("{cmd}: bad X {w:?}"))
        };
        let slot = |i: usize| -> Result<u8, String> {
            rest.get(i).map_or(Ok(0), |w| {
                w.parse().map_err(|_| format!("{cmd}: bad slot {w:?}"))
            })
        };
        let nargs = |max: usize| -> Result<(), String> {
            if rest.len() > max {
                Err(format!("{cmd}: too many arguments"))
            } else {
                Ok(())
            }
        };
        let ev = match cmd {
            "tap" => {
                nargs(1)?;
                let x = x(0)?;
                let mut evs = self.touch(0, TouchPhase::Down, x, time_us);
                evs.extend(self.touch(0, TouchPhase::Up, x, time_us));
                return Ok(evs);
            }
            "down" | "move" => {
                nargs(2)?;
                let phase = if cmd == "down" {
                    TouchPhase::Down
                } else {
                    TouchPhase::Move
                };
                let (x, slot) = (x(0)?, slot(1)?);
                if phase == TouchPhase::Move && !self.slots.contains_key(&slot) {
                    return Err(format!("move: slot {slot} is not down"));
                }
                return Ok(self.touch(slot, phase, x, time_us));
            }
            "up" | "cancel" => {
                nargs(1)?;
                let slot = slot(0)?;
                let x = *self
                    .slots
                    .get(&slot)
                    .ok_or(format!("{cmd}: slot {slot} is not down"))?;
                let phase = if cmd == "up" {
                    TouchPhase::Up
                } else {
                    TouchPhase::Cancel
                };
                return Ok(self.touch(slot, phase, x, time_us));
            }
            "fn" => {
                nargs(1)?;
                match rest.first() {
                    Some(&"down") => HwEvent::FnKey { held: true },
                    Some(&"up") => HwEvent::FnKey { held: false },
                    _ => return Err("fn: down or up".into()),
                }
            }
            "lid" => {
                nargs(1)?;
                match rest.first() {
                    Some(&"open") => HwEvent::Lid { closed: false },
                    Some(&"closed") => HwEvent::Lid { closed: true },
                    _ => return Err("lid: open or closed".into()),
                }
            }
            "activity" => {
                nargs(0)?;
                HwEvent::Activity
            }
            "screen-backlight" => {
                nargs(0)?;
                HwEvent::ScreenBacklight
            }
            _ => return Err(format!("unknown command {cmd:?}")),
        };
        Ok(vec![ev])
    }

    fn touch(&mut self, slot: u8, phase: TouchPhase, x: f32, time_us: u64) -> Vec<HwEvent> {
        match phase {
            TouchPhase::Down | TouchPhase::Move => {
                self.slots.insert(slot, x);
            }
            TouchPhase::Up | TouchPhase::Cancel => {
                self.slots.remove(&slot);
            }
        }
        vec![HwEvent::Touch(TouchEvent {
            slot,
            phase,
            x,
            y: self.height / 2.0,
            time_us,
        })]
    }
}

/// The socket as an [`InputSource`] for the event loop. Removes its path when dropped.
pub struct TouchSocket {
    sock: UnixDatagram,
    path: PathBuf,
    parser: Parser,
    clock: Instant,
}

impl TouchSocket {
    /// Binds `path` (replacing a stale socket file left there).
    pub fn bind(path: &Path, panel_height: u32) -> io::Result<TouchSocket> {
        match std::fs::symlink_metadata(path) {
            Ok(m) if std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()) => {
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Err(_) => {}
        }
        let sock = UnixDatagram::bind(path)?;
        sock.set_nonblocking(true)?;
        Ok(TouchSocket {
            sock,
            path: path.to_owned(),
            parser: Parser::new(panel_height),
            clock: Instant::now(),
        })
    }
}

impl Drop for TouchSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl AsFd for TouchSocket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.sock.as_fd()
    }
}

impl InputSource for TouchSocket {
    fn drain(&mut self) -> Vec<HwEvent> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = self.sock.recv(&mut buf) {
            let time_us = self.clock.elapsed().as_micros() as u64;
            for line in String::from_utf8_lossy(&buf[..n]).lines() {
                match self.parser.line(line, time_us) {
                    Ok(evs) => out.extend(evs),
                    Err(e) => eprintln!("sliverd: --touch-socket: {e}"),
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(e: &HwEvent) -> (u8, TouchPhase, f32) {
        match e {
            HwEvent::Touch(t) => (t.slot, t.phase, t.x),
            e => panic!("not a touch: {e:?}"),
        }
    }

    #[test]
    fn parses_touches_with_slots() {
        let mut p = Parser::new(60);
        let tap = p.line("tap 100", 7).unwrap();
        assert_eq!(
            tap.iter().map(touch).collect::<Vec<_>>(),
            [(0, TouchPhase::Down, 100.0), (0, TouchPhase::Up, 100.0)]
        );
        let HwEvent::Touch(t) = tap[0] else { panic!() };
        assert_eq!((t.y, t.time_us), (30.0, 7));

        assert_eq!(
            touch(&p.line("down 10 1", 0).unwrap()[0]),
            (1, TouchPhase::Down, 10.0)
        );
        assert_eq!(
            touch(&p.line("move 20.5 1", 0).unwrap()[0]),
            (1, TouchPhase::Move, 20.5)
        );
        assert_eq!(
            touch(&p.line("up 1", 0).unwrap()[0]),
            (1, TouchPhase::Up, 20.5)
        );
        assert_eq!(
            touch(&p.line("down 5", 0).unwrap()[0]),
            (0, TouchPhase::Down, 5.0)
        );
        assert_eq!(
            touch(&p.line("cancel", 0).unwrap()[0]),
            (0, TouchPhase::Cancel, 5.0)
        );
    }

    #[test]
    fn parses_seat_events() {
        let mut p = Parser::new(60);
        assert_eq!(
            p.line("fn down", 0),
            Ok(vec![HwEvent::FnKey { held: true }])
        );
        assert_eq!(p.line("fn up", 0), Ok(vec![HwEvent::FnKey { held: false }]));
        assert_eq!(
            p.line("lid closed", 0),
            Ok(vec![HwEvent::Lid { closed: true }])
        );
        assert_eq!(
            p.line("lid open", 0),
            Ok(vec![HwEvent::Lid { closed: false }])
        );
        assert_eq!(p.line("activity", 0), Ok(vec![HwEvent::Activity]));
        assert_eq!(
            p.line("screen-backlight", 0),
            Ok(vec![HwEvent::ScreenBacklight])
        );
        assert_eq!(p.line("   ", 0), Ok(vec![]));
        assert_eq!(p.line("# a comment", 0), Ok(vec![]));
    }

    #[test]
    fn rejects_bad_lines() {
        let mut p = Parser::new(60);
        for bad in [
            "tap",
            "tap x",
            "tap NaN",
            "tap 1 2",
            "down 1 300",
            "move 5",
            "up",
            "up 3",
            "fn sideways",
            "lid ajar",
            "activity now",
            "swipe 1 2",
        ] {
            assert!(p.line(bad, 0).is_err(), "{bad:?} should be refused");
        }
    }
}
