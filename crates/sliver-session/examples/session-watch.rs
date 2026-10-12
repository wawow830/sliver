//! Print the session state (Debug format) on every change.
//! Read-only; safe on a live session. `cargo run -p sliver-session --example session-watch`

use sliver_session::{Session, SessionSource};
use zbus::blocking::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut s = Session::start(Connection::system()?, Connection::session()?);
    s.handle()
        .refresh_brightness(&sliver_session::brightness::Sysfs::default());
    s.handle()
        .start_audio(sliver_session::audio::Tools::default());
    s.handle()
        .start_notifications(Connection::session()?, Connection::session()?)?;
    while let Some(state) = s.next_change()? {
        println!("{state:?}");
    }
    Ok(())
}
