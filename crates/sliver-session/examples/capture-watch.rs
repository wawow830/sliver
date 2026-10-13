//! Prints who is capturing the microphone, camera or screen (`Capture { mic: [..], .. }`), as it
//! changes. Starts the session sources too, so run it on a private bus (integration/pipewire).
//! `capture-watch [SOCKET]` (default: PipeWire's own socket). For integration/pipewire/test.sh.

use sliver_session::SessionSource;

fn main() {
    let socket = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .or_else(|| sliver_session::capture::socket_path(|k| std::env::var(k).ok()))
        .expect("no PipeWire socket: set XDG_RUNTIME_DIR or pass a path");
    let mut session = sliver_session::Session::start(
        sliver_bus::system().expect("system bus"),
        sliver_bus::session().expect("session bus"),
    );
    session.handle().start_capture(socket);
    let mut last = None;
    while let Ok(Some(state)) = session.next_change() {
        if last.as_ref() != Some(&state.capture) {
            println!("{:?}", state.capture);
            last = Some(state.capture);
        }
    }
}
