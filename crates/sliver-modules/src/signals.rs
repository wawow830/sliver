//! `pkill -RTMIN+N sliver-agent`: real-time signals re-run modules with `signal = N`.

use crate::Runtime;
use crate::spec::MAX_SIGNAL;
use signal_hook::iterator::Signals;
use std::io;

/// Route `SIGRTMIN+1 ..= SIGRTMIN+30` to [`Runtime::signal`] on a dedicated thread (blocked in
/// `sigwait`-style iteration, so it costs nothing while idle). Call once, early, from the
/// process that should receive the signals. The thread keeps `rt` alive; to replace runtimes
/// (config reload), use [`forward_realtime_signals_to`] instead.
pub fn forward_realtime_signals(rt: Runtime) -> io::Result<()> {
    forward_realtime_signals_to(move |n| {
        rt.signal(n);
    })
}

/// Like [`forward_realtime_signals`], but calls `f(n)` for `SIGRTMIN+n`, so the target can change.
pub fn forward_realtime_signals_to(f: impl Fn(u8) + Send + 'static) -> io::Result<()> {
    let min = libc::SIGRTMIN();
    let mut signals = Signals::new((1..=i32::from(MAX_SIGNAL)).map(|n| min + n))?;
    std::thread::Builder::new()
        .name("sliver-rt-signals".into())
        .spawn(move || {
            for sig in signals.forever() {
                if let Ok(n) = u8::try_from(sig - min) {
                    f(n);
                }
            }
        })?;
    Ok(())
}
