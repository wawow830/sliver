//! `sliverctl`: command-line client of the public sliver session D-Bus API.

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliverctl {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "sliverctl {}: not implemented yet",
        env!("CARGO_PKG_VERSION")
    );
}
