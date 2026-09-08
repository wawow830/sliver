/// Seconds on Linux's shared monotonic clock. Unlike an Instant measured from
/// process/device construction, this origin survives broker/supervisor start
/// differences and touch-device reopen. Touch timestamps cross process seams;
/// worker scheduling and presentation keep their existing supervisor clock.
pub(crate) fn touch_seconds() -> f64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is always supported on the Linux runtime target, and the
    // pointer is valid writable storage. A failure cannot yield a valid time.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    assert_eq!(result, 0, "reading the monotonic touch clock failed");
    time.tv_sec as f64 + time.tv_nsec as f64 / 1_000_000_000.0
}
