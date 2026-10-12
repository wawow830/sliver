//! `cargo run -p sliver-waybar --example waybar-import -- ~/.config/waybar/config.jsonc [style.css]`
//! Prints the sliver config on stdout and the report on stderr.

use sliver_waybar::{Level, Options, import_path};
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: waybar-import CONFIG [STYLE.CSS]");
        std::process::exit(2);
    };
    let opts = Options {
        stylesheet: args.next(),
    };
    match import_path(Path::new(&path), &opts) {
        Ok(i) => {
            print!("{}", i.toml);
            for n in &i.notes {
                let tag = if n.level == Level::Warn {
                    "warn"
                } else {
                    "info"
                };
                eprintln!("{tag}: {}: {}", n.path, n.message);
            }
            eprintln!(
                "{} modules placed, {} dropped",
                i.modules.len(),
                i.dropped.len()
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
