use anyhow::{bail, ensure, Context, Result};

use crate::logind::Logind;
use crate::peer_credentials::PeerCredentials;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizationGrant {
    session_id: String,
    seat: String,
    uid: libc::uid_t,
    generation: u64,
}

impl AuthorizationGrant {
    pub(crate) fn uid(&self) -> libc::uid_t {
        self.uid
    }
}

#[derive(Clone)]
pub(crate) struct SessionAuthorizer<L> {
    logind: L,
}

impl<L: Logind> SessionAuthorizer<L> {
    pub(crate) fn new(logind: L) -> Self {
        Self { logind }
    }

    pub(crate) fn generation(&self) -> Result<u64> {
        self.logind
            .generation()
            .context("reading the logind session generation")
    }

    pub(crate) fn active_session(
        &self,
        seat: &str,
    ) -> Result<Option<crate::logind::ActiveSession>> {
        self.logind
            .active_session(seat)
            .with_context(|| format!("looking up the active logind session on {seat}"))
    }

    pub(crate) fn authorize_active_uid(
        &self,
        uid: libc::uid_t,
        seat: &str,
    ) -> Result<AuthorizationGrant> {
        ensure!(uid != 0, "root is not authorized to own Sliver workers");
        let generation_before = self
            .logind
            .generation()
            .context("reading the logind session generation")?;
        let session = self
            .logind
            .active_session(seat)
            .with_context(|| format!("looking up the active logind session on {seat}"))?
            .context("no local user session is active")?;
        ensure!(session.uid == uid, "worker is not owned by the active user");
        let generation_after = self
            .logind
            .generation()
            .context("reading the logind session generation")?;
        ensure!(
            generation_before == generation_after,
            "session changed while checking worker ownership"
        );
        Ok(AuthorizationGrant {
            session_id: session.id,
            seat: seat.to_owned(),
            uid,
            generation: generation_after,
        })
    }

    pub(crate) fn recheck_active_uid(
        &self,
        uid: libc::uid_t,
        expected: &AuthorizationGrant,
    ) -> Result<()> {
        let current = self.authorize_active_uid(uid, &expected.seat)?;
        ensure!(
            current == *expected,
            "worker session changed during broker request"
        );
        Ok(())
    }

    pub(crate) fn authorize(&self, peer: PeerCredentials) -> Result<AuthorizationGrant> {
        if peer.uid == 0 {
            bail!("root is not authorized to apply Sliver configurations");
        }

        let generation_before = self
            .logind
            .generation()
            .context("reading the logind session generation")?;
        let session = self
            .logind
            .session_for_pid(peer.pid)
            .context("looking up the caller's logind session")?
            .context("caller does not belong to a logind session")?;

        ensure!(
            session.uid == peer.uid,
            "kernel peer UID does not own the caller's logind session"
        );
        ensure!(
            !session.remote,
            "remote sessions are not authorized to apply Sliver configurations"
        );
        let seat = session
            .seat
            .as_deref()
            .context("caller session is not attached to a local seat")?;
        ensure!(session.active, "caller logind session is inactive");

        let active = self
            .logind
            .active_session(seat)
            .with_context(|| format!("looking up the active logind session on {seat}"))?;
        match active {
            Some(active) if active.id == session.id && active.uid == peer.uid => {}
            _ => bail!("caller session is not the active session on seat {seat}"),
        }
        let generation_after = self
            .logind
            .generation()
            .context("reading the logind session generation")?;
        ensure!(
            generation_before == generation_after,
            "session changed while checking authorization"
        );

        Ok(AuthorizationGrant {
            session_id: session.id,
            seat: seat.to_owned(),
            uid: peer.uid,
            generation: generation_after,
        })
    }

    pub(crate) fn recheck(
        &self,
        peer: PeerCredentials,
        expected: &AuthorizationGrant,
    ) -> Result<()> {
        let current = self.authorize(peer)?;
        ensure!(
            current == *expected,
            "caller session changed during config apply"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logind::{ActiveSession, FakeLogind, Session};

    fn peer(uid: libc::uid_t) -> PeerCredentials {
        PeerCredentials {
            pid: std::process::id() as libc::pid_t,
            uid,
            gid: 1000,
        }
    }

    #[test]
    fn root_is_rejected_even_when_it_has_the_active_local_session() -> Result<()> {
        let logind = FakeLogind::new();
        logind.set_session(
            peer(0).pid,
            Some(Session {
                id: "root-session".into(),
                uid: 0,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "root-session".into(),
                uid: 0,
            }),
        );

        let error = SessionAuthorizer::new(logind)
            .authorize(peer(0))
            .expect_err("root bypassed session authorization");

        assert!(format!("{error:#}").contains("root is not authorized"));
        Ok(())
    }

    #[test]
    fn a_generation_change_invalidates_an_unchanged_session_snapshot() -> Result<()> {
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: "same-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "same-session".into(),
                uid,
            }),
        );
        let authorizer = SessionAuthorizer::new(logind.clone());
        let grant = authorizer.authorize(peer(uid))?;

        logind.bump_generation();

        let error = authorizer
            .recheck(peer(uid), &grant)
            .expect_err("an unchanged session snapshot hid a generation change");
        assert!(format!("{error:#}").contains("changed during config apply"));
        Ok(())
    }

    #[test]
    fn a_user_manager_is_authorized_by_its_active_uid_and_seat() -> Result<()> {
        let uid = unsafe { libc::getuid() };
        let logind = FakeLogind::new();
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "user-session".into(),
                uid,
            }),
        );
        let authorizer = SessionAuthorizer::new(logind.clone());

        let grant = authorizer.authorize_active_uid(uid, "seat0")?;
        authorizer.recheck_active_uid(uid, &grant)?;
        Ok(())
    }

    #[test]
    fn non_qualifying_logind_callers_are_rejected() -> Result<()> {
        let uid = unsafe { libc::getuid() };
        let cases = [
            (
                "inactive",
                Some(Session {
                    id: "inactive".into(),
                    uid,
                    seat: Some("seat0".into()),
                    remote: false,
                    active: false,
                }),
                Some(ActiveSession {
                    id: "inactive".into(),
                    uid,
                }),
                "session is inactive",
            ),
            (
                "remote",
                Some(Session {
                    id: "ssh".into(),
                    uid,
                    seat: Some("seat0".into()),
                    remote: true,
                    active: true,
                }),
                Some(ActiveSession {
                    id: "ssh".into(),
                    uid,
                }),
                "remote sessions",
            ),
            (
                "uid-mismatch",
                Some(Session {
                    id: "wrong-owner".into(),
                    uid: uid.wrapping_add(1),
                    seat: Some("seat0".into()),
                    remote: false,
                    active: true,
                }),
                Some(ActiveSession {
                    id: "wrong-owner".into(),
                    uid: uid.wrapping_add(1),
                }),
                "kernel peer UID",
            ),
            ("cron", None, None, "does not belong to a logind session"),
            (
                "user-service",
                None,
                None,
                "does not belong to a logind session",
            ),
            (
                "no-seat",
                Some(Session {
                    id: "background".into(),
                    uid,
                    seat: None,
                    remote: false,
                    active: true,
                }),
                None,
                "not attached to a local seat",
            ),
            (
                "background-seat-session",
                Some(Session {
                    id: "background".into(),
                    uid,
                    seat: Some("seat0".into()),
                    remote: false,
                    active: true,
                }),
                Some(ActiveSession {
                    id: "other-active".into(),
                    uid,
                }),
                "not the active session",
            ),
        ];

        for (name, session, active, expected) in cases {
            let logind = FakeLogind::new();
            logind.set_session(peer(uid).pid, session);
            if let Some(active) = active {
                logind.set_active("seat0", Some(active));
            }

            let error = match SessionAuthorizer::new(logind).authorize(peer(uid)) {
                Ok(grant) => panic!("{name} caller was accepted: {grant:?}"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains(expected),
                "{name} caller returned the wrong rejection: {error:#}"
            );
        }
        Ok(())
    }
}
