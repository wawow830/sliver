//! sliverd: the daemon that owns the strip.
//!
//! Milestone zero: renders your layout to a preview PNG, no hardware
//! involved. Milestones ahead: DRM master on card1, touch on event2,
//! live layout application over a unix socket.

use anyhow::Result;

fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sliver.toml".into());
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "preview.png".into());

    let cfg = sliver_core::load_config(std::path::Path::new(&config_path))?;
    sliver_core::render_preview(&cfg, std::path::Path::new(&out))?;

    let (w, h) = (sliver_core::STRIP_W as u32, sliver_core::STRIP_H as u32);
    println!("rendered {w}x{h} preview of {config_path} -> {out}");
    Ok(())
}
