use anyhow::{bail, ensure, Context, Result};

use crate::logind::Logind;
use crate::peer_credentials::PeerCredentials;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizationGrant {
    session_id: String,
    seat: String,
    uid: libc::uid_t,
}

pub(crate) struct SessionAuthorizer<L> {
    logind: L,
}

impl<L: Logind> SessionAuthorizer<L> {
    pub(crate) fn new(logind: L) -> Self {
        Self { logind }
    }

    pub(crate) fn authorize(&self, peer: PeerCredentials) -> Result<AuthorizationGrant> {
        if peer.uid == 0 {
            bail!("root is not authorized to apply Sliver configurations");
        }

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

        Ok(AuthorizationGrant {
            session_id: session.id,
            seat: seat.to_owned(),
            uid: peer.uid,
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
}
