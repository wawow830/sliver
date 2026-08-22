use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use anyhow::{bail, Context, Result};

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

pub(crate) trait Logind {
    fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>>;
    fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RealLogind;

impl Logind for RealLogind {
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
        let seat =
            systemd_string_for_session(&c_id, "sd_session_get_seat", |session, output| unsafe {
                ffi::sd_session_get_seat(session, output)
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
        let mut id = ptr::null_mut();
        let mut uid = 0;
        let code = unsafe { ffi::sd_seat_get_active(seat.as_ptr(), &mut id, &mut uid) };
        if code < 0 {
            free_string(id);
            return missing_or_error("sd_seat_get_active", code);
        }
        if id.is_null() {
            return Ok(None);
        }

        Ok(Some(ActiveSession {
            id: unsafe { take_string(id) }?,
            uid,
        }))
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

fn systemd_string_for_session<F>(session: &CString, name: &str, call: F) -> Result<Option<String>>
where
    F: FnOnce(*const c_char, *mut *mut c_char) -> libc::c_int,
{
    let mut output = ptr::null_mut();
    let code = call(session.as_ptr(), &mut output);
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
    }
}

#[cfg(test)]
pub(crate) use fake::FakeLogind;

#[cfg(test)]
mod fake {
    use std::sync::{Arc, Mutex};

    use super::{ActiveSession, Logind, Session};
    use anyhow::Result;

    #[derive(Clone, Default)]
    pub(crate) struct FakeLogind {
        state: Arc<Mutex<State>>,
    }

    #[derive(Default)]
    struct State {
        sessions: std::collections::HashMap<libc::pid_t, Session>,
        active: std::collections::HashMap<String, ActiveSession>,
    }

    impl FakeLogind {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn set_session(&self, pid: libc::pid_t, session: Option<Session>) {
            let mut state = self.state.lock().expect("fake logind mutex poisoned");
            if let Some(session) = session {
                state.sessions.insert(pid, session);
            } else {
                state.sessions.remove(&pid);
            }
        }

        pub(crate) fn set_active(&self, seat: &str, session: Option<ActiveSession>) {
            let mut state = self.state.lock().expect("fake logind mutex poisoned");
            if let Some(session) = session {
                state.active.insert(seat.to_owned(), session);
            } else {
                state.active.remove(seat);
            }
        }
    }

    impl Logind for FakeLogind {
        fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>> {
            Ok(self
                .state
                .lock()
                .expect("fake logind mutex poisoned")
                .sessions
                .get(&pid)
                .cloned())
        }

        fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>> {
            Ok(self
                .state
                .lock()
                .expect("fake logind mutex poisoned")
                .active
                .get(seat)
                .cloned())
        }
    }
}
