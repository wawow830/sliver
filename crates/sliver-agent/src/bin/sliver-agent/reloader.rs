//! The config and stylesheet as the running agent holds them: hot reload, profiles and the
//! wallpaper accent colours.

use super::*;

/// The current config and stylesheet, hot-reloaded by [`sliver_agent::reload`] (sliver-config(5)):
/// file changes (inotify, debounced), SIGUSR2 and `Control1.ReloadConfig`. A config with errors is
/// never applied; the last good one stays and `errors` says why.
pub(super) struct Reloader {
    pub(super) inner: Option<sliver_agent::reload::Reloader>,
    pub(super) config: Config,
    pub(super) sheet: Stylesheet,
    /// Why the newest config on disk isn't live. Empty = it is.
    pub(super) errors: Vec<String>,
    /// SIGUSR2 arrives here (signal-hook's self-pipe).
    pub(super) usr2: Option<UnixStream>,
    /// The active profile and all profiles, read on (re)load rather than every frame. Empty with
    /// `--config FILE`.
    pub(super) profile: (String, Vec<String>),
    /// The live config file (`None`: built-in defaults); relative icon paths start here.
    pub(super) config_path: Option<PathBuf>,
}

impl Reloader {
    /// Config text checked and loaded as if saved over the live config (`sliver_agent::reload::load_text`).
    pub(super) fn load_text(
        &self,
        text: &str,
    ) -> Result<sliver_agent::reload::Applied, Vec<String>> {
        match &self.inner {
            Some(r) => r.load_text(text),
            None => sliver_agent::reload::load_text(text, self.config_path.as_deref(), None),
        }
    }

    pub(super) fn start(explicit: Option<PathBuf>) -> Reloader {
        let (inner, applied) = match sliver_agent::reload::Reloader::start(explicit.as_deref()) {
            Ok((r, a)) => (Some(r), a),
            Err(e) => {
                eprintln!(
                    "sliver-agent: no hot reload (inotify: {e}); using the built-in defaults"
                );
                let config = sliver_config::load_str("", Path::new(".")).config;
                let a = sliver_agent::reload::Applied {
                    config,
                    sheet: Stylesheet::sliver(),
                    config_path: None,
                    warnings: Vec::new(),
                    errors: vec![format!("hot reload unavailable: {e}")],
                };
                (None, a)
            }
        };
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
        if !applied.errors.is_empty() {
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
            config: applied.config,
            sheet: applied.sheet,
            errors: applied.errors,
            usr2,
            profile: Default::default(),
            config_path: applied.config_path,
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
        self.inner.as_ref().is_some_and(|r| r.explicit().is_some())
    }

    pub(super) fn watch_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.inner
            .as_ref()
            .map(sliver_agent::reload::Reloader::as_fd)
    }

    /// How long poll may sleep before a debounced change is due.
    pub(super) fn timeout_ms(&self) -> Option<i64> {
        let d = self.inner.as_ref()?.deadline()?;
        Some(d.saturating_duration_since(Instant::now()).as_millis() as i64 + 1)
    }

    /// After poll: feeds file events and SIGUSR2, reloads when due. Returns whether to redraw.
    pub(super) fn on_wake(
        &mut self,
        watch_ready: bool,
        usr2_ready: bool,
        agent: &mut Agent,
        live: &mut Live,
    ) -> bool {
        let Some(inner) = &mut self.inner else {
            return false;
        };
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
        outcome.is_some_and(|o| self.handle(o, Some(agent), live))
    }

    /// The desktop's accent colour changed (portal or KDE): rebuild the stylesheet with it.
    /// Returns whether something changed on screen.
    pub(super) fn sync_accent(&mut self, agent: &mut Agent, live: &mut Live) -> bool {
        let accent = live
            .agg
            .session
            .appearance
            .accent
            .map(|[r, g, b]| sliver_render::Color::rgb(r, g, b));
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        if !inner.set_accent(accent) {
            return false;
        }
        let outcome = inner.reload_now();
        self.handle(outcome, Some(agent), live)
    }

    /// Reloads now (`sliverctl reload`). Returns the errors (empty = the file's config is live).
    pub(super) fn reload(&mut self, agent: Option<&mut Agent>, live: &mut Live) -> Vec<String> {
        match self
            .inner
            .as_mut()
            .map(sliver_agent::reload::Reloader::reload_now)
        {
            Some(o) => {
                self.handle(o, agent, live);
            }
            None => self.errors = vec!["hot reload unavailable (no inotify)".into()],
        }
        self.errors.clone()
    }

    pub(super) fn handle(
        &mut self,
        o: sliver_agent::reload::Outcome,
        agent: Option<&mut Agent>,
        live: &mut Live,
    ) -> bool {
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
                live.restart_modules(&a.config);
                if let Some(api) = &live.api {
                    api.set_policy(sliver_agent::api::policy_from_config(&a.config));
                }
                if let Some(agent) = agent {
                    agent.config = a.config.clone();
                    agent.sheet = a.sheet.clone();
                    agent.set_config_dir(a.config_path.as_deref().and_then(Path::parent));
                    let layers = &agent.config.layers;
                    agent.layers.retain(|l| layers.contains_key(l));
                }
                self.config = a.config;
                self.sheet = a.sheet;
                self.config_path = a.config_path;
                self.errors.clear();
                eprintln!("sliver-agent: reloaded {src}");
                true
            }
            Outcome::Rejected(errors) => {
                errors
                    .iter()
                    .for_each(|e| eprintln!("sliver-agent: config error: {e}"));
                eprintln!("sliver-agent: keeping the last good config");
                self.errors = errors;
                false
            }
            // Back to what's live (e.g. a broken save reverted): nothing to redraw, no errors.
            Outcome::Unchanged => {
                self.errors.clear();
                false
            }
        }
    }
}
