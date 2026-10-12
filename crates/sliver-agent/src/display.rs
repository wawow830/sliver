//! The bar's backlight and idle policy, sent to sliverd as `display_policy` (api.md §6).
//!
//! It comes from the user's `[display]` (tiny-dfr's 0–255 brightness scale), with
//! `[display.on-battery]` replacing values while UPower says we're on battery. The agent sends it
//! on connect and whenever it changes (config reload, AC ↔ battery). sliverd keeps its own
//! defaults until then.

use sliver_config::Display;
use sliver_ipc::proto::DisplayPolicy;

/// The policy for `display` (as configured), the power state, and the panel's backlight range.
/// `on_battery` is `None` when UPower is unavailable (or hasn't answered yet, so the AC values
/// may show for a moment at startup on battery); that counts as AC.
pub fn policy(display: &Display, on_battery: Option<bool>, panel_max: u32) -> DisplayPolicy {
    let d = display.effective(on_battery == Some(true));
    let ms = |t: Option<std::time::Duration>| t.map_or(0, |t| t.as_millis() as u64);
    DisplayPolicy {
        brightness: Some(scale(d.max_brightness, panel_max)),
        dim_after_ms: Some(ms(d.dim_after)),
        // Dim level stays sliverd's (1, or 1 of 2 on a T2): there's no config key for it.
        dim_brightness: None,
        off_after_ms: Some(ms(d.off_after)),
        follow_screen: Some(d.adaptive),
        // Always sent, so removing `curve` from the config goes back to sqrt on reload.
        curve: Some(
            d.curve
                .iter()
                .map(|&(s, b)| [permille(s), permille(b)])
                .collect(),
        ),
        pixel_shift: Some(d.pixel_shift),
    }
}

/// While pixel shift is on, sliverd moves the picture up to this far either way in x, so content
/// is laid out this far in from both ends (tiny-dfr renders into `width - 22` for the same reason).
pub const PIXEL_SHIFT_INSET: f32 = 11.0;

/// The renderer's extra `[top, right, bottom, left]` inset for this config.
pub fn content_inset(display: &Display) -> [f32; 4] {
    if display.pixel_shift {
        [0.0, PIXEL_SHIFT_INSET, 0.0, PIXEL_SHIFT_INSET]
    } else {
        [0.0; 4]
    }
}

/// Percent (0–100, validated by sliver-config) → per-mille for the wire.
fn permille(pct: f32) -> u16 {
    (pct.clamp(0.0, 100.0) * 10.0).round() as u16
}

/// tiny-dfr 0–255 → panel units, rounded, but never 0 for a non-zero setting (a T2 bar has only
/// off/dim/full, max 2, and "dim but on" must not turn it off).
fn scale(v: u8, panel_max: u32) -> u32 {
    if v == 0 || panel_max == 0 {
        return 0;
    }
    ((v as u32 * panel_max + 127) / 255).clamp(1, panel_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_config::DisplayOverride;
    use std::time::Duration;

    #[test]
    fn config_becomes_policy() {
        let p = policy(&Display::default(), Some(false), 255);
        assert_eq!(
            p,
            DisplayPolicy {
                brightness: Some(128),
                dim_after_ms: Some(30_000),
                dim_brightness: None,
                off_after_ms: Some(60_000),
                follow_screen: Some(true),
                curve: Some(vec![]),
                pixel_shift: Some(false),
            }
        );
        let shifted = Display {
            pixel_shift: true,
            ..Display::default()
        };
        assert_eq!(policy(&shifted, None, 255).pixel_shift, Some(true));
        assert_eq!(content_inset(&shifted), [0.0, 11.0, 0.0, 11.0]);
        assert_eq!(content_inset(&Display::default()), [0.0; 4]);
        let never = Display {
            dim_after: None,
            off_after: None,
            adaptive: false,
            ..Display::default()
        };
        let p = policy(&never, None, 255);
        assert_eq!(
            (p.dim_after_ms, p.off_after_ms),
            (Some(0), Some(0)),
            "0 = never"
        );
        assert_eq!(p.follow_screen, Some(false));
        let curved = Display {
            curve: vec![(0.0, 5.0), (33.33, 40.0), (100.0, 100.0)],
            ..Display::default()
        };
        assert_eq!(
            policy(&curved, None, 255).curve,
            Some(vec![[0, 50], [333, 400], [1000, 1000]])
        );
    }

    #[test]
    fn battery_override_only_on_battery() {
        let d = Display {
            max_brightness: 200,
            on_battery: DisplayOverride {
                max_brightness: Some(60),
                dim_after: Some(Some(Duration::from_secs(10))),
                off_after: None,
            },
            ..Display::default()
        };
        let ac = policy(&d, Some(false), 255);
        let bat = policy(&d, Some(true), 255);
        let unknown = policy(&d, None, 255);
        assert_eq!((ac.brightness, ac.dim_after_ms), (Some(200), Some(30_000)));
        assert_eq!((bat.brightness, bat.dim_after_ms), (Some(60), Some(10_000)));
        assert_eq!(
            bat.off_after_ms,
            Some(60_000),
            "unset override keeps [display]"
        );
        assert_eq!(unknown, ac, "no UPower = AC");
    }

    #[test]
    fn scales_to_the_panel() {
        assert_eq!(scale(128, 255), 128);
        assert_eq!(scale(255, 255), 255);
        assert_eq!(scale(0, 255), 0);
        // T2: off/dim/full.
        assert_eq!(scale(255, 2), 2);
        assert_eq!(scale(128, 2), 1);
        assert_eq!(scale(1, 2), 1, "on stays on");
        assert_eq!(scale(0, 2), 0);
        assert_eq!(scale(128, 0), 0);
    }
}
