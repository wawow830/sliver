//! `sliver-settings`: the settings app and WYSIWYG Touch Bar editor (GTK4 + libadwaita).

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliver-settings {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "sliver-settings {}: not implemented yet",
        env!("CARGO_PKG_VERSION")
    );
}
