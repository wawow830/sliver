//! `sliverd`: the system daemon. Owns the Touch Bar panel, touch input, backlight and uinput keys, and always keeps a built-in safe layer (Esc + F-keys) available.

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliverd {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!("sliverd {}: not implemented yet", env!("CARGO_PKG_VERSION"));
}
