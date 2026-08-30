use std::fmt::Display;
use std::os::unix::net::UnixDatagram;

const SYSTEM_JOURNAL: &str = "/run/systemd/journal/socket";

pub(crate) fn broker_error(error: impl Display) {
    let message = error.to_string().replace('\0', "");
    let packet = format!("MESSAGE={message}\0SYSLOG_IDENTIFIER=sliver-broker\0PRIORITY=3\0");
    let sent = UnixDatagram::unbound()
        .and_then(|socket| socket.send_to(packet.as_bytes(), SYSTEM_JOURNAL))
        .is_ok();
    if !sent {
        eprintln!("broker: {message}");
    }
}
