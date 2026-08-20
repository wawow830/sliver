//! sliverd: the daemon that owns the strip.
//!
//! Usage:
//!   sliverd [config.toml] [out.png]   render a preview PNG (no hardware)
//!   sliverd [config.toml] --drm       claim the real touchbar panel
//!
//! Milestone one: modeset, rotate, paint, hold. A heartbeat (re-renders,
//! touch input, socket apply) arrives with milestone two.

mod drm_out;

use anyhow::Result;

fn main() -> Result<()> {
    let mut config_path = "sliver.toml".to_string();
    let mut out = "preview.png".to_string();
    let mut drm = false;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--drm" => drm = true,
            a if config_path == "sliver.toml" => config_path = a.to_string(),
            a => out = a.to_string(),
        }
    }

    let cfg = sliver_core::load_config(std::path::Path::new(&config_path))?;

    if drm {
        drm_out::run(&cfg)
    } else {
        sliver_core::render_preview(&cfg, std::path::Path::new(&out))?;
        let (w, h) = (sliver_core::STRIP_W as u32, sliver_core::STRIP_H as u32);
        println!("rendered {w}x{h} preview of {config_path} -> {out}");
        Ok(())
    }
}
