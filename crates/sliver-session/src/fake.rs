//! A scripted [`SessionSource`] for tests and the headless agent.

use crate::{SessionSource, SessionState};
use std::collections::VecDeque;
use std::io;

#[derive(Debug, Default)]
pub struct ScriptedSession {
    script: VecDeque<SessionState>,
}

impl ScriptedSession {
    pub fn new(states: impl IntoIterator<Item = SessionState>) -> Self {
        Self {
            script: states.into_iter().collect(),
        }
    }
}

impl SessionSource for ScriptedSession {
    fn next_change(&mut self) -> io::Result<Option<SessionState>> {
        Ok(self.script.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_then_ends() {
        let a = SessionState {
            locked: true,
            ..Default::default()
        };
        let mut s = ScriptedSession::new([a.clone()]);
        assert_eq!(s.next_change().unwrap(), Some(a));
        assert_eq!(s.next_change().unwrap(), None);
    }
}
