//! The config and stylesheet as the running agent holds them: hot reload, profiles and the
//! wallpaper accent colours.

use super::*;

/// The current config and stylesheet, hot-reloaded by [`sliver_agent::reload`] (sliver-config(5)):
/// file changes (inotify, debounced), SIGUSR2 and `Control1.ReloadConfig`. A config with errors is
/// never applied; the last good one stays and `errors` says why.
pub(super) struct Reloader {
    inner: sliver_agent::reload::Reloader,
    /// Why the newest config on disk isn't live. Empty = it is.
    pub(super) errors: Vec<String>,
    /// SIGUSR2 arrives here (signal-hook's self-pipe).
    pub(super) usr2: Option<UnixStream>,
    /// The active profile and all profiles, read on (re)load rather than every frame. Empty with
    /// `--config FILE`.
    pub(super) profile: (String, Vec<String>),
}

impl Reloader {
    /// What's live: the last good config and stylesheet.
    pub(super) fn applied(&self) -> &Applied {
        self.inner.current()
    }

    /// Config text checked and loaded as if saved over the live config
    /// (`sliver_agent::reload::load_text`).
    pub(super) fn load_text(&self, text: &str) -> Result<Applied, Vec<String>> {
        self.inner.load_text(text)
    }

    pub(super) fn start(explicit: Option<PathBuf>) -> Reloader {
        let (inner, applied) = sliver_agent::reload::Reloader::start(explicit.as_deref());
        let src = applied
            .config_path
            .as_ref()
            .map_or("built-in defaults".into(), |p| p.display().to_string());
        applied
            .warnings
            .iter()
            .for_each(|w| eprintln!("sliver-agent: {src}: warning: {w}"));
        applied
            .errors
            .iter()
            .for_each(|e| eprintln!("sliver-agent: {src}: error: {e}"));
        if !applied.errors.is_empty() && applied.config_path.is_some() {
            eprintln!("sliver-agent: using the built-in defaults until the config is fixed");
        }
        let usr2 = UnixStream::pair().ok().and_then(|(r, w)| {
            r.set_nonblocking(true).ok()?;
            signal_hook::low_level::pipe::register(signal_hook::consts::SIGUSR2, w)
                .inspect_err(|e| eprintln!("sliver-agent: SIGUSR2: {e}"))
                .ok()?;
            Some(r)
        });
        let mut me = Reloader {
            inner,
            errors: applied.errors,
            usr2,
            profile: Default::default(),
        };
        me.read_profiles();
        me
    }

    pub(super) fn read_profiles(&mut self) {
        self.profile = if self.explicit() {
            Default::default()
        } else {
            (
                sliver_config::profile::active(),
                sliver_config::profile::list(),
            )
        };
    }

    /// Running with `--config FILE` (no profiles).
    pub(super) fn explicit(&self) -> bool {
        self.inner.explicit().is_some()
    }

    pub(super) fn watch_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.inner.as_fd()
    }

    /// How long poll may sleep before a debounced change is due.
    pub(super) fn timeout_ms(&self) -> Option<i64> {
        let d = self.inner.deadline()?;
        Some(d.saturating_duration_since(Instant::now()).as_millis() as i64 + 1)
    }

    /// After poll: feeds file events and SIGUSR2, reloads when due. Returns a config to show.
    pub(super) fn on_wake(
        &mut self,
        watch_ready: bool,
        usr2_ready: bool,
        live: &mut Live,
    ) -> Option<Box<Applied>> {
        let inner = &mut self.inner;
        let now = Instant::now();
        if watch_ready {
            inner.on_readable(now);
        }
        let mut forced = false;
        if usr2_ready && let Some(r) = &mut self.usr2 {
            let mut buf = [0u8; 64];
            while matches!(r.read(&mut buf), Ok(n) if n > 0) {}
            forced = true;
        }
        let outcome = if forced {
            Some(inner.reload_now())
        } else {
            inner.poll_due(now)
        };
        outcome.and_then(|o| self.handle(o, live))
    }

    /// The desktop's accent colour changed (portal or KDE): rebuild the stylesheet with it.
    /// Returns a config to show.
    pub(super) fn sync_accent(&mut self, live: &mut Live) -> Option<Box<Applied>> {
        let accent = live
            .agg
            .session
            .appearance
            .accent
            .map(|[r, g, b]| sliver_render::Color::rgb(r, g, b));
        if !self.inner.set_accent(accent) {
            return None;
        }
        let outcome = self.inner.reload_now();
        self.handle(outcome, live)
    }

    /// Reloads now (`sliverctl reload`): a config to show, and the errors (empty = the file's
    /// config is live).
    pub(super) fn reload(&mut self, live: &mut Live) -> (Option<Box<Applied>>, Vec<String>) {
        let o = self.inner.reload_now();
        let applied = self.handle(o, live);
        (applied, self.errors.clone())
    }

    pub(super) fn handle(
        &mut self,
        o: sliver_agent::reload::Outcome,
        live: &mut Live,
    ) -> Option<Box<Applied>> {
        use sliver_agent::reload::Outcome;
        self.read_profiles();
        match o {
            Outcome::Applied(a) => {
                let src = a
                    .config_path
                    .as_ref()
                    .map_or("built-in defaults".into(), |p| p.display().to_string());
                a.warnings
                    .iter()
                    .for_each(|w| eprintln!("sliver-agent: {src}: warning: {w}"));
                live.follow(&a.config);
                self.errors.clear();
                eprintln!("sliver-agent: reloaded {src}");
                Some(a)
            }
            Outcome::Rejected(errors) => {
                errors
                    .iter()
                    .for_each(|e| eprintln!("sliver-agent: config error: {e}"));
                eprintln!("sliver-agent: keeping the last good config");
                self.errors = errors;
                None
            }
            // Back to what's live (e.g. a broken save reverted): nothing to redraw, no errors.
            Outcome::Unchanged => {
                self.errors.clear();
                None
            }
        }
    }
}
