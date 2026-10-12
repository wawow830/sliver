use std::time::Instant;

use sliver_ipc::proto::{Layer, RegionKind, TouchPhase};

fn main() {
    let l = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = sliver_agent::Agent::new(
        l.config,
        sliver_render::Stylesheet::sliver(),
        sliver_render::Renderer::new(sliver_render::Fonts::system()),
        sliver_agent::icons::Icons::system(),
        2008,
        60,
    );
    let w = sliver_agent::modules::World::sample(chrono::Local::now());
    a.render(&w);
    let t = Instant::now();
    for _ in 0..20 {
        a.render(&w);
    }
    println!(
        "full render (2 layers + pressed twins): {:?}",
        t.elapsed() / 20
    );
    let r = a
        .frame(Layer::Base)
        .unwrap()
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Agent)
        .expect("an agent region")
        .id;
    let t = Instant::now();
    for i in 0..20u64 {
        a.touch(r, 0, TouchPhase::Down, 1700.0, i * 1_000_000);
        a.render_press();
        a.touch(r, 0, TouchPhase::Up, 1700.0, i * 1_000_000 + 50_000);
        a.render_press();
    }
    println!(
        "press fast path (per press or release): {:?}",
        t.elapsed() / 40
    );
}
