//! Runs a one-shot module's tap action (`screenshot`, `lock`) on the session bus, the way the
//! agent does on a tap, and prints the result. For integration/portal/test.sh.
//!
//! `oneshot NAME [WAIT_MS]`: stays connected for up to WAIT_MS and prints the portal's `Response`
//! signal if one arrives (`response 0 uri=…`). The agent keeps its connection for good; a portal
//! drops a request whose caller has gone, answer or not.

use std::time::{Duration, Instant};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_default();
    let wait: u64 = std::env::args()
        .nth(2)
        .and_then(|w| w.parse().ok())
        .unwrap_or(0);
    let Some(action) = sliver_agent::modules::oneshot_action(&name) else {
        eprintln!("usage: oneshot screenshot|lock [WAIT_MS]");
        std::process::exit(2);
    };
    let sliver_config::Action::DBus(call) = action else {
        eprintln!("{name} is not a D-Bus action");
        std::process::exit(2);
    };
    let conn = match sliver_bus::session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no session bus: {e}");
            std::process::exit(1);
        }
    };
    // Subscribe before calling: the Response can come right behind the reply.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|b| b.member("Response"))
        .map(|b| b.build());
    let mut responses = rule
        .ok()
        .and_then(|r| zbus::blocking::MessageIterator::for_match_rule(r, &conn, None).ok());
    match sliver_agent::ops::dbus_call(&conn, &call) {
        Ok(()) => println!("ok"),
        Err(e) => {
            println!("error: {e}");
            std::process::exit(1);
        }
    }
    let deadline = Instant::now() + Duration::from_millis(wait);
    while wait > 0 && Instant::now() < deadline {
        let Some(it) = responses.as_mut() else { break };
        // `next` blocks; a Response normally arrives within milliseconds, and the harness kills
        // a hung run.
        let Some(Ok(msg)) = it.next() else { break };
        if let Ok((code, results)) = msg.body().deserialize::<(
            u32,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        )>() {
            let mut keys: Vec<_> = results.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
            keys.sort();
            println!("response {code} {}", keys.join(" "));
            break;
        }
    }
}
