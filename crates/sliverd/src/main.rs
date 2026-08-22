//! sliverd: the daemon that owns the strip.
//!
//! Usage:
//!   sliverd [config.toml] [out.png]   render a preview PNG (no hardware)
//!   sliverd [config.toml] --drm       claim the real touchbar panel
//!   sliverd --probe                   paint calibration bands on the panel
//!   sliverd --apply [config.toml]     push a new config to a running daemon
//!
//! Milestone three: the strip listens. Touch, heartbeat, live apply.

mod drm_out;
mod hardware;
mod lua_canvas;
mod lua_worker;
mod m2_hardware;

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--probe") {
        return drm_out::probe();
    }
    if let Some(pos) = args.iter().position(|a| a == "--apply") {
        let path = args
            .get(pos + 1)
            .map(String::as_str)
            .unwrap_or("sliver.toml");
        return apply(path);
    }

    let mut config_path = "sliver.toml".to_string();
    let mut out = "preview.png".to_string();
    let drm = args.iter().any(|a| a == "--drm");
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    if let Some(path) = positional.next() {
        config_path = path.clone();
    }
    if let Some(path) = positional.next() {
        out = path.clone();
    }

    let cfg = sliver_core::load_config(std::path::Path::new(&config_path))?;

    if drm {
        drm_out::run(cfg)
    } else {
        sliver_core::render_preview(&cfg, std::path::Path::new(&out))?;
        let (w, h) = (sliver_core::STRIP_W as u32, sliver_core::STRIP_H as u32);
        println!("rendered {w}x{h} preview of {config_path} -> {out}");
        Ok(())
    }
}

/// Push a config to the running daemon: one connection, one document,
/// one-word reply.
fn apply(path: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut stream = std::os::unix::net::UnixStream::connect(sliver_core::socket_path())
        .context("connecting to sliverd (is the daemon running?)")?;
    stream.write_all(text.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    print!("{reply}");
    if reply.trim() == "ok" {
        Ok(())
    } else {
        bail!("the daemon rejected that config")
    }
}
