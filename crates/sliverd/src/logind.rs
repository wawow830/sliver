use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::{Arc, Mutex};

use anyhow::{bail, ensure, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) uid: libc::uid_t,
    pub(crate) seat: Option<String>,
    pub(crate) remote: bool,
    pub(crate) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveSession {
    pub(crate) id: String,
    pub(crate) uid: libc::uid_t,
}

pub(crate) trait Logind: Clone + Send + Sync + 'static {
    fn generation(&self) -> Result<u64>;
    fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>>;
    fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>>;
}

#[derive(Clone, Default)]
pub(crate) struct RealLogind {
    state: Arc<Mutex<RealState>>,
}

#[derive(Default)]
struct RealState {
    monitor: Option<LoginMonitor>,
    generation: u64,
}

impl RealLogind {
    fn monitor_generation(&self) -> Result<u64> {
        let mut state = self.state.lock().expect("real logind mutex poisoned");
        if state.monitor.is_none() {
            state.monitor = Some(LoginMonitor::new()?);
        }
        if state
            .monitor
            .as_mut()
            .expect("logind monitor was just initialized")
            .changed()?
        {
            state.generation = state
                .generation
                .checked_add(1)
                .context("logind generation overflow")?;
        }
        Ok(state.generation)
    }
}

struct LoginMonitor {
    raw: std::ptr::NonNull<ffi::SdLoginMonitor>,
}

// LoginMonitor is accessed only while RealState's mutex is held.
unsafe impl Send for LoginMonitor {}

impl LoginMonitor {
    fn new() -> Result<Self> {
        let mut raw = ptr::null_mut();
        let code = unsafe { ffi::sd_login_monitor_new(ptr::null(), &mut raw) };
        ensure!(
            code >= 0,
            "sd_login_monitor_new failed: {}",
            std::io::Error::from_raw_os_error(-code)
        );
        let raw = std::ptr::NonNull::new(raw).context("systemd returned a null login monitor")?;
        let monitor = Self { raw };
        monitor.flush()?;
        Ok(monitor)
    }

    fn changed(&mut self) -> Result<bool> {
        let fd = unsafe { ffi::sd_login_monitor_get_fd(self.raw.as_ptr()) };
        ensure!(fd >= 0, "sd_login_monitor_get_fd failed: {fd}");
        let events = unsafe { ffi::sd_login_monitor_get_events(self.raw.as_ptr()) };
        ensure!(events >= 0, "sd_login_monitor_get_events failed: {events}");
        let mut descriptor = libc::pollfd {
            fd,
            events: events as libc::c_short,
            revents: 0,
        };
        let result = loop {
            let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break result;
        };
        ensure!(
            result >= 0,
            "polling the logind monitor: {}",
            std::io::Error::last_os_error()
        );
        if result == 0 {
            return Ok(false);
        }
        self.flush()?;
        Ok(true)
    }

    fn flush(&self) -> Result<()> {
        let code = unsafe { ffi::sd_login_monitor_flush(self.raw.as_ptr()) };
        ensure!(
            code >= 0,
            "sd_login_monitor_flush failed: {}",
            std::io::Error::from_raw_os_error(-code)
        );
        Ok(())
    }
}

impl Drop for LoginMonitor {
    fn drop(&mut self) {
        unsafe {
            let _ = ffi::sd_login_monitor_unref(self.raw.as_ptr());
        }
    }
}

impl Logind for RealLogind {
    fn generation(&self) -> Result<u64> {
        self.monitor_generation()
    }

    fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>> {
        let id = match systemd_string("sd_pid_get_session", |output| unsafe {
            ffi::sd_pid_get_session(pid, output)
        })? {
            Some(id) => id,
            None => return Ok(None),
        };
        let c_id = CString::new(id.as_str()).context("logind returned an invalid session ID")?;
        let uid = match systemd_uid(&c_id, |session, output| unsafe {
            ffi::sd_session_get_uid(session, output)
        })? {
            Some(uid) => uid,
            None => return Ok(None),
        };
        let seat = systemd_string("sd_session_get_seat", |output| unsafe {
            ffi::sd_session_get_seat(c_id.as_ptr(), output)
        })?;
        let remote = match systemd_bool(&c_id, "sd_session_is_remote", |session| unsafe {
            ffi::sd_session_is_remote(session)
        })? {
            Some(remote) => remote,
            None => return Ok(None),
        };
        let active = match systemd_bool(&c_id, "sd_session_is_active", |session| unsafe {
            ffi::sd_session_is_active(session)
        })? {
            Some(active) => active,
            None => return Ok(None),
        };

        Ok(Some(Session {
            id,
            uid,
            seat,
            remote,
            active,
        }))
    }

    fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>> {
        let seat = CString::new(seat).context("logind seat contains a NUL byte")?;
        let mut uid = 0;
        let id = systemd_string("sd_seat_get_active", |output| unsafe {
            ffi::sd_seat_get_active(seat.as_ptr(), output, &mut uid)
        })?;
        Ok(id.map(|id| ActiveSession { id, uid }))
    }
}

fn systemd_string<F>(name: &str, call: F) -> Result<Option<String>>
where
    F: FnOnce(*mut *mut c_char) -> libc::c_int,
{
    let mut output = ptr::null_mut();
    let code = call(&mut output);
    if code < 0 {
        free_string(output);
        return missing_or_error(name, code);
    }
    if output.is_null() {
        return Ok(None);
    }
    Ok(Some(unsafe { take_string(output) }?))
}

fn systemd_uid<F>(session: &CString, call: F) -> Result<Option<libc::uid_t>>
where
    F: FnOnce(*const c_char, *mut libc::uid_t) -> libc::c_int,
{
    let mut uid = 0;
    let code = call(session.as_ptr(), &mut uid);
    if code < 0 {
        return missing_or_error("sd_session_get_uid", code);
    }
    Ok(Some(uid))
}

fn systemd_bool<F>(session: &CString, name: &str, call: F) -> Result<Option<bool>>
where
    F: FnOnce(*const c_char) -> libc::c_int,
{
    let code = call(session.as_ptr());
    if code < 0 {
        return missing_or_error(name, code);
    }
    Ok(Some(code > 0))
}

fn missing_or_error<T>(name: &str, code: libc::c_int) -> Result<Option<T>> {
    let error = -code;
    if matches!(
        error,
        libc::ENODATA | libc::ENOENT | libc::ENXIO | libc::ESRCH
    ) {
        return Ok(None);
    }
    bail!(
        "{name} failed: {}",
        std::io::Error::from_raw_os_error(error)
    );
}

unsafe fn take_string(output: *mut c_char) -> Result<String> {
    if output.is_null() {
        bail!("systemd returned a null string");
    }
    let value = CStr::from_ptr(output).to_string_lossy().into_owned();
    libc::free(output.cast());
    Ok(value)
}

fn free_string(output: *mut c_char) {
    if !output.is_null() {
        unsafe { libc::free(output.cast()) };
    }
}

#[cfg(target_os = "linux")]
mod ffi {
    use std::os::raw::c_char;

    #[repr(C)]
    pub(super) struct SdLoginMonitor {
        _private: [u8; 0],
    }

    extern "C" {
        pub(super) fn sd_pid_get_session(
            pid: libc::pid_t,
            ret_session: *mut *mut c_char,
        ) -> libc::c_int;
        pub(super) fn sd_session_get_uid(
            session: *const c_char,
            ret_uid: *mut libc::uid_t,
        ) -> libc::c_int;
        pub(super) fn sd_session_get_seat(
            session: *const c_char,
            ret_seat: *mut *mut c_char,
        ) -> libc::c_int;
        pub(super) fn sd_session_is_remote(session: *const c_char) -> libc::c_int;
        pub(super) fn sd_session_is_active(session: *const c_char) -> libc::c_int;
        pub(super) fn sd_seat_get_active(
            seat: *const c_char,
            ret_session: *mut *mut c_char,
            ret_uid: *mut libc::uid_t,
        ) -> libc::c_int;
        pub(super) fn sd_login_monitor_new(
            category: *const c_char,
            ret: *mut *mut SdLoginMonitor,
        ) -> libc::c_int;
        pub(super) fn sd_login_monitor_unref(monitor: *mut SdLoginMonitor) -> *mut SdLoginMonitor;
        pub(super) fn sd_login_monitor_flush(monitor: *mut SdLoginMonitor) -> libc::c_int;
        pub(super) fn sd_login_monitor_get_fd(monitor: *mut SdLoginMonitor) -> libc::c_int;
        pub(super) fn sd_login_monitor_get_events(monitor: *mut SdLoginMonitor) -> libc::c_int;
    }
}

#[cfg(test)]
pub(crate) use fake::FakeLogind;

#[cfg(test)]
mod fake {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::{ActiveSession, Logind, Session};
    use anyhow::Result;

    #[derive(Clone, Default)]
    pub(crate) struct FakeLogind {
        state: Arc<(Mutex<State>, Condvar)>,
    }

    #[derive(Default)]
    struct State {
        sessions: std::collections::HashMap<libc::pid_t, Session>,
        active: std::collections::HashMap<String, ActiveSession>,
        generation: u64,
        generation_reads: usize,
        generation_hook: Option<(usize, String, ActiveSession)>,
        clear_active_hook: Option<(usize, String)>,
    }

    impl FakeLogind {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn set_session(&self, pid: libc::pid_t, session: Option<Session>) {
            let (lock, _) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            if let Some(session) = session {
                state.sessions.insert(pid, session);
            } else {
                state.sessions.remove(&pid);
            }
            state.generation = state
                .generation
                .checked_add(1)
                .expect("fake logind generation overflow");
        }

        pub(crate) fn set_active(&self, seat: &str, session: Option<ActiveSession>) {
            let (lock, _) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            if let Some(session) = session {
                state.active.insert(seat.to_owned(), session);
            } else {
                state.active.remove(seat);
            }
            state.generation = state
                .generation
                .checked_add(1)
                .expect("fake logind generation overflow");
        }

        pub(crate) fn bump_generation(&self) {
            let (lock, _) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            state.generation = state
                .generation
                .checked_add(1)
                .expect("fake logind generation overflow");
        }

        pub(crate) fn switch_active_on_generation_read(
            &self,
            read: usize,
            seat: &str,
            active: ActiveSession,
        ) {
            let (lock, _) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            state.generation_hook = Some((read, seat.to_owned(), active));
        }

        pub(crate) fn clear_active_on_generation_read(&self, read: usize, seat: &str) {
            let (lock, _) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            state.clear_active_hook = Some((read, seat.to_owned()));
        }

        pub(crate) fn wait_for_generation_reads(&self, expected: usize, timeout: Duration) -> bool {
            let (lock, condition) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            let deadline = Instant::now() + timeout;
            while state.generation_reads < expected {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (next, result) = condition
                    .wait_timeout(state, remaining)
                    .expect("fake logind mutex poisoned");
                state = next;
                if result.timed_out() && state.generation_reads < expected {
                    return false;
                }
            }
            true
        }
    }

    impl Logind for FakeLogind {
        fn generation(&self) -> Result<u64> {
            let (lock, condition) = &*self.state;
            let mut state = lock.lock().expect("fake logind mutex poisoned");
            state.generation_reads += 1;
            if state
                .generation_hook
                .as_ref()
                .is_some_and(|(read, _, _)| *read == state.generation_reads)
            {
                let (_, seat, active) = state
                    .generation_hook
                    .take()
                    .expect("generation hook was just checked");
                state.active.insert(seat, active);
                state.generation = state
                    .generation
                    .checked_add(1)
                    .expect("fake logind generation overflow");
            }
            if state
                .clear_active_hook
                .as_ref()
                .is_some_and(|(read, _)| *read == state.generation_reads)
            {
                let (_, seat) = state
                    .clear_active_hook
                    .take()
                    .expect("fake logind clear hook was just checked");
                state.active.remove(&seat);
            }
            condition.notify_all();
            Ok(state.generation)
        }

        fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>> {
            let (lock, _) = &*self.state;
            Ok(lock
                .lock()
                .expect("fake logind mutex poisoned")
                .sessions
                .get(&pid)
                .cloned())
        }

        fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>> {
            let (lock, _) = &*self.state;
            Ok(lock
                .lock()
                .expect("fake logind mutex poisoned")
                .active
                .get(seat)
                .cloned())
        }
    }
}
