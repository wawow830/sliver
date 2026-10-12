//! Shared test helper: a private `dbus-daemon`.
#![allow(dead_code)]

use zbus::blocking::{Connection, connection::Builder};

pub struct Bus {
    _tb: sliver_testbus::TestBus,
    pub addr: String,
}

impl Bus {
    pub fn start() -> Option<Bus> {
        // Isolated: no service dirs (nothing real gets D-Bus-activated) and dies with the test.
        let tb = sliver_testbus::TestBus::start()?;
        let addr = tb.address().to_owned();
        Some(Bus { _tb: tb, addr })
    }

    pub fn connect(&self) -> Connection {
        Builder::address(self.addr.as_str())
            .unwrap()
            .build()
            .unwrap()
    }

    pub fn service(&self, name: &str) -> Connection {
        Builder::address(self.addr.as_str())
            .unwrap()
            .name(name.to_string())
            .unwrap()
            .build()
            .unwrap()
    }
}
