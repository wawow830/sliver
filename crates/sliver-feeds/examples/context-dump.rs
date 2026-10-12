//! Start the live feeds, wait a moment, print the rule context. Read-only.
//! `cargo run -p sliver-feeds --example context-dump`

use sliver_feeds::{Aggregate, Feed, Feeds};
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    let mut feeds = Feeds::new()?;
    feeds.start_desktop(|k| std::env::var(k).ok());
    feeds.start_session();
    let mut agg = Aggregate::from_env();
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        for f in feeds.drain() {
            if let Feed::Note(n) = &f {
                eprintln!("note: {n}");
            }
            agg.apply(f);
        }
    }
    println!("{:#?}", agg.context());
    Ok(())
}
