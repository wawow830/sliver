use std::fmt::Display;
use std::os::unix::net::UnixDatagram;

const SYSTEM_JOURNAL: &str = "/run/systemd/journal/socket";

fn clean_message(error: impl Display) -> String {
    error.to_string().replace('\0', "")
}

pub(crate) fn broker_error(error: impl Display) {
    let message = clean_message(error);
    let packet = format!("MESSAGE={message}\0SYSLOG_IDENTIFIER=sliver-broker\0PRIORITY=3\0");
    let sent = UnixDatagram::unbound()
        .and_then(|socket| socket.send_to(packet.as_bytes(), SYSTEM_JOURNAL))
        .is_ok();
    if !sent {
        eprintln!("broker: {message}");
    }
}

/// User-service failures must stay in the user journal. Unlike the broker,
/// the supervisor has no reason to bypass the user manager's journal routing.
pub(crate) fn supervisor_error(error: impl Display) {
    eprintln!("supervisor: {}", clean_message(error));
}
