mod apply_ipc;
mod drm_out;
mod hardware;
mod lua_canvas;
mod lua_worker;
mod m2_hardware;

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};

/// Send one explicit Lua config to the running per-user supervisor.
#[doc(hidden)]
pub fn apply_config(path: &std::path::Path) -> Result<()> {
    apply_ipc::request_apply(path)
}

/// Run the legacy TOML daemon and developer modes until issue #16 removes them.
pub fn legacy_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--probe") {
        return drm_out::probe();
    }
    if let Some(pos) = args.iter().position(|a| a == "--apply") {
        let path = args
            .get(pos + 1)
            .map(String::as_str)
            .unwrap_or("sliver.toml");
        return apply_legacy(path);
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

fn apply_legacy(path: &str) -> Result<()> {
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
