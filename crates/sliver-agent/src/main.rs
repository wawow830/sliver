//! `sliver-agent`: the per-user session agent. Tracks the compositor, serves the public session D-Bus API and runs widgets.

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliver-agent {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "sliver-agent {}: not implemented yet",
        env!("CARGO_PKG_VERSION")
    );
}
