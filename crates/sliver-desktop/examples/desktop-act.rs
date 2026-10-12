//! One desktop action through `control::run`, like the agent's taskbar and workspaces taps.
//!
//!     desktop-act focus ADDRESS | close ADDRESS | workspace ID NAME | layout-next
//!
//! On the generic Wayland backend the action goes through the running watcher in the same
//! process, so this example only works there with `--watch` (it runs the watcher in a thread).
use sliver_desktop::control::{Action, run};
use sliver_desktop::{Backend, WorkspaceInfo};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let watch = args.iter().any(|a| a == "--watch");
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--watch")
        .collect();
    let action = match args.as_slice() {
        ["focus", a] => Action::FocusWindow {
            address: (*a).into(),
        },
        ["close", a] => Action::CloseWindow {
            address: (*a).into(),
        },
        ["workspace", id, name] => Action::FocusWorkspace(WorkspaceInfo {
            id: id.parse().expect("workspace id"),
            name: (*name).into(),
            ..Default::default()
        }),
        ["layout-next"] => Action::NextLayout,
        _ => {
            eprintln!(
                "usage: desktop-act focus ADDR | close ADDR | workspace ID NAME | layout-next"
            );
            std::process::exit(2);
        }
    };
    let env = |k: &str| std::env::var(k).ok();
    if watch && Backend::detect(env) == Backend::Wlroots {
        // The generic backend's actions are requests on the watcher's own connection.
        std::thread::spawn(|| {
            let _ = sliver_desktop::wlr::watch(|_| std::ops::ControlFlow::Continue(()));
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    match run(env, &action) {
        Ok(()) => println!("ok"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
