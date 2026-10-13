//! Prints fcitx5's current input method once (read-only). `cargo run -p sliver-session --example fcitx-read`
fn main() {
    let conn = sliver_bus::session().expect("session bus");
    println!("{:#?}", sliver_session::fcitx::read(&conn));
}
