//! List the tray items and their menus (read-only): `cargo run -p sliver-session --example tray-dump`

use sliver_session::{Session, SessionSource};
use zbus::blocking::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session_bus = Connection::session()?;
    let mut s = Session::start(Connection::system()?, session_bus.clone());
    s.handle().start_tray(Connection::session()?);
    s.next_change()?; // initial
    let state = s.next_change()?.unwrap_or_default();
    for item in &state.tray {
        println!(
            "{} id={} title={:?} status={:?} icon={:?} pixmap={}",
            item.key,
            item.id,
            item.title,
            item.status,
            item.icon_name,
            item.icon.is_some()
        );
        for m in sliver_session::menu::layout(&session_bus, item)? {
            println!("  [{}] {:?} children={}", m.id, m.label, m.children.len());
        }
    }
    Ok(())
}
