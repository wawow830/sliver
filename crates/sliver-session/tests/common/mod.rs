//! Shared test helper: a private `dbus-daemon`.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use zbus::blocking::{Connection, connection::Builder};

pub struct Bus {
    child: Child,
    addr: String,
}

impl Bus {
    pub fn start() -> Option<Bus> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut addr = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut addr)
            .ok()?;
        Some(Bus {
            child,
            addr: addr.trim().to_string(),
        })
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

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
