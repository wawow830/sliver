//! Resolve icon names or window app ids on this machine:
//! `cargo run -p sliver-icons --example icon-find -- kitty audio-volume-high [size]`

use sliver_icons::desktop::DesktopEntries;
use sliver_icons::{IconLoader, Query, SearchPaths};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let size = args
        .last()
        .and_then(|a| a.parse::<u32>().ok())
        .inspect(|_| {
            args.pop();
        })
        .unwrap_or(40);
    let paths = SearchPaths::from_env(|k| std::env::var(k).ok());
    let loader = IconLoader::from_env();
    let apps = DesktopEntries::from_paths(&paths);
    println!("theme: {}", loader.default_theme());
    for a in args {
        let entry = apps.find_by_app_id(&a);
        let icon_name = entry
            .and_then(|e| e.icon.clone())
            .unwrap_or_else(|| a.clone());
        let found = loader.lookup(&Query::new(&icon_name, size));
        println!(
            "{a}: entry={:?} icon={icon_name:?} -> {:?}",
            entry.map(|e| (&e.id, &e.name)),
            found.map(|i| (i.path, i.kind, i.size))
        );
    }
}
