//! A scripted [`DesktopSource`] for tests and the headless agent: replays fixed states.

use crate::{DesktopSource, DesktopState};
use std::collections::VecDeque;
use std::io;

#[derive(Debug, Default)]
pub struct ScriptedSource {
    script: VecDeque<DesktopState>,
}

impl ScriptedSource {
    pub fn new(states: impl IntoIterator<Item = DesktopState>) -> Self {
        Self {
            script: states.into_iter().collect(),
        }
    }
}

impl DesktopSource for ScriptedSource {
    fn next_change(&mut self) -> io::Result<Option<DesktopState>> {
        Ok(self.script.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_then_ends() {
        let a = DesktopState {
            connected: true,
            ..Default::default()
        };
        let mut s = ScriptedSource::new([a.clone()]);
        assert_eq!(s.next_change().unwrap(), Some(a));
        assert_eq!(s.next_change().unwrap(), None);
    }
}
