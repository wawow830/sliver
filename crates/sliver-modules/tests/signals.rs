//! Real-time signals reach modules through `forward_realtime_signals`. Separate test binary:
//! signal handlers are process-wide.

use sliver_modules::{Runtime, Spec, forward_realtime_signals};
use std::time::Duration;

#[test]
fn rtmin_n_reruns_the_module() {
    let d = std::env::temp_dir().join(format!("sliver-mod-rt-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let c = d.join("n");
    let script = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; echo $n",
        c = c.display()
    );
    let mut s = Spec::new("custom/rt", &script);
    s.signal = Some(3);
    let (rt, rx) = Runtime::start(vec![s]);
    forward_realtime_signals(rt.clone()).unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .output
            .text,
        "1"
    );
    // SAFETY: raising a signal that the handler above now owns.
    unsafe {
        libc::raise(libc::SIGRTMIN() + 3);
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .output
            .text,
        "2"
    );
    let _ = std::fs::remove_dir_all(d);
}
