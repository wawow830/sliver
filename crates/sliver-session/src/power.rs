//! UPower (`OnBattery`, `LidIsClosed`, DisplayDevice battery) and power-profiles-daemon.

use crate::{BatteryState, Power, props};
use zbus::blocking::Connection;

const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// Re-read everything. Absent services leave their fields untouched/`None`.
pub fn read_into(conn: &Connection, power: &mut Power) {
    if let Some(p) = props::get_all(conn, UPOWER, UPOWER_PATH, UPOWER) {
        power.on_battery = props::boolean(&p, "OnBattery").or(power.on_battery);
        power.lid_closed = props::boolean(&p, "LidIsClosed").or(power.lid_closed);
    }
    if let Some(p) = props::get_all(
        conn,
        UPOWER,
        DISPLAY_DEVICE,
        "org.freedesktop.UPower.Device",
    ) {
        if props::boolean(&p, "IsPresent") == Some(false) {
            power.battery_pct = None;
            power.battery_state = BatteryState::Unknown;
        } else {
            power.battery_pct = props::float(&p, "Percentage").map(pct);
            power.battery_state = props::uint(&p, "State")
                .map(BatteryState::from_upower)
                .unwrap_or_default();
        }
    }
    // Newer PPD owns org.freedesktop.UPower.PowerProfiles; older ones net.hadess.PowerProfiles.
    let profile = [
        (
            "org.freedesktop.UPower.PowerProfiles",
            "/org/freedesktop/UPower/PowerProfiles",
        ),
        ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
    ]
    .iter()
    .find_map(|(dest, path)| {
        let p = props::get_all(conn, dest, path, dest)?;
        props::string(&p, "ActiveProfile")
    });
    if profile.is_some() {
        power.profile = profile;
    }
}

fn pct(f: f64) -> u8 {
    f.round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    #[test]
    fn percentage_is_clamped_and_rounded() {
        assert_eq!(super::pct(99.6), 100);
        assert_eq!(super::pct(-3.0), 0);
        assert_eq!(super::pct(250.0), 100);
        assert_eq!(super::pct(41.4), 41);
    }
}
