//! `pkill -RTMIN+N sliver-agent`: real-time signals re-run modules with `signal = N`.

use crate::Runtime;
use crate::spec::MAX_SIGNAL;
use signal_hook::iterator::Signals;
use std::io;

/// Route `SIGRTMIN+1 ..= SIGRTMIN+30` to [`Runtime::signal`] on a dedicated thread (blocked in
/// `sigwait`-style iteration, so it costs nothing while idle). Call once, early, from the
/// process that should receive the signals.
pub fn forward_realtime_signals(rt: Runtime) -> io::Result<()> {
    let min = libc::SIGRTMIN();
    let mut signals = Signals::new((1..=i32::from(MAX_SIGNAL)).map(|n| min + n))?;
    std::thread::Builder::new()
        .name("sliver-rt-signals".into())
        .spawn(move || {
            for sig in signals.forever() {
                if let Ok(n) = u8::try_from(sig - min) {
                    rt.signal(n);
                }
            }
        })?;
    Ok(())
}
