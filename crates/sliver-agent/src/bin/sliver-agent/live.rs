//! The live world: the feeds (desktop, session, user modules), the public API and the ops
//! thread, and the [`World`] they make for each frame.

use super::*;

/// The live world: the feeds (desktop, session, user modules) plus the ops thread.
pub(super) struct Live {
    pub(super) feeds: Feeds,
    pub(super) agg: Aggregate,
    pub(super) outputs: HashMap<String, sliver_modules::Output>,
    /// logind's raw `LockedHint`, before the plausibility check.
    pub(super) locked_hint: bool,
    pub(super) lock_watch: sliver_agent::context::LockWatch,
    pub(super) ops: Option<std::sync::mpsc::Sender<Op>>,
    /// The public D-Bus API, if the session bus is there.
    pub(super) api: Option<ApiLink>,
    pub(super) published: Published,
    /// The registry changed since `published` was copied: copy it before the next frame
    /// (not on every change, which an app may make hundreds of times a second).
    pub(super) published_stale: bool,
    /// Our own compositor backend (Hyprland/sway/niri IPC) is connected. Without one, what a
    /// GNOME/KDE integration pushes over `Desktop1` is the desktop.
    pub(super) own_desktop: bool,
    /// cpu/memory/... sampler, kept across frames for CPU deltas.
    pub(super) stats: sliver_agent::modules::Stats,
    /// Installed applications (taskbar icons).
    pub(super) apps: std::sync::Arc<sliver_icons::desktop::DesktopEntries>,
    /// Emoji recents, shared with the agent.
    pub(super) emoji: sliver_agent::modules::EmojiHandle,
}

impl Live {
    /// `api`: serve the public API (`Some(replace)`), or not (`--render`).
    pub(super) fn start(config: &Config, api: Option<bool>) -> std::io::Result<Live> {
        // The API first: owning the bus name is what tells systemd (Type=dbus) and D-Bus
        // activation that the agent is up, so it must not wait on slow session sources.
        let api = match api.map(ApiLink::start) {
            None => None,
            Some(Ok(a)) => Some(a),
            // Another agent drives the bar. Two would revoke each other on sliverd forever.
            Some(Err(zbus::Error::NameTaken)) => {
                eprintln!(
                    "sliver-agent: another sliver-agent is running (it owns {}). Stop it \
                     (systemctl --user stop sliver-agent), or pass --replace to take over \
                     until this one exits.",
                    sliver_ipc::dbus::BUS_NAME
                );
                std::process::exit(1);
            }
            Some(Err(e)) => {
                eprintln!("sliver-agent: public D-Bus API unavailable: {e}");
                None
            }
        };
        let mut feeds = Feeds::new()?;
        feeds.start_desktop(|k| std::env::var(k).ok());
        feeds.start_session();
        let ops = feeds.session.clone().and_then(|h| match start_ops(h) {
            Ok(tx) => Some(tx),
            Err(e) => {
                eprintln!("sliver-agent: session controls unavailable: {e}");
                None
            }
        });
        let mut live = Live {
            feeds,
            agg: Aggregate::from_env(),
            outputs: HashMap::new(),
            locked_hint: false,
            lock_watch: Default::default(),
            ops,
            api,
            published: Published::default(),
            published_stale: false,
            own_desktop: false,
            stats: Default::default(),
            emoji: sliver_agent::modules::EmojiHandle::persistent(),
            apps: std::sync::Arc::new(sliver_icons::desktop::DesktopEntries::from_paths(
                &sliver_icons::SearchPaths::from_env(|k| std::env::var(k).ok()),
            )),
        };
        live.follow(config);
        Ok(live)
    }

    /// Takes in everything the feeds sent. Returns whether anything did.
    pub(super) fn pump(&mut self) -> bool {
        let feeds = self.feeds.drain();
        let any = !feeds.is_empty();
        for f in feeds {
            match &f {
                Feed::Module(u) => {
                    self.outputs.insert(u.module.clone(), u.output.clone());
                }
                Feed::Session(s) => self.locked_hint = s.locked,
                Feed::Desktop(d) => {
                    self.own_desktop = d.connected;
                    // Each window has its own input context: a new focus can mean another
                    // fcitx5 input method (a no-op unless the `language` module follows fcitx5).
                    let focus = |d: &sliver_desktop::DesktopState| {
                        d.window.as_ref().map(|w| w.address.clone())
                    };
                    if focus(d) != focus(&self.agg.desktop) {
                        self.feeds.refresh_input_method();
                    }
                }
                Feed::Note(n) => eprintln!("sliver-agent: {n}"),
                Feed::Art { .. } | Feed::Spectrum(_) => {}
            }
            self.agg.apply(f);
        }
        if any {
            self.push_focus();
        }
        any
    }

    /// Our compositor backend's focus decides who fills App controls (GNOME/KDE push theirs
    /// over `Desktop1` instead).
    pub(super) fn push_focus(&mut self) {
        let d = &self.agg.desktop;
        if let Some(api) = &self.api
            && self.own_desktop
            && d.connected
        {
            api.set_focus(Focus {
                window: d.window.clone(),
                workspace: d.workspace.as_ref().map(|w| w.name.clone()),
                locked: self.world_locked(),
            });
            self.published_stale = true;
        }
    }

    /// Without our own backend, adopt what a DE integration pushed over `Desktop1` (focus,
    /// workspaces, windows) as the desktop, so rules, the workspaces and taskbar modules and the
    /// `app_id` context work on GNOME/KDE too. Returns whether it changed.
    pub(super) fn sync_pushed_desktop(&mut self) -> bool {
        if self.own_desktop {
            return false;
        }
        let Some(api) = &self.api else {
            return false;
        };
        let backend = sliver_desktop::Backend::detect(|k| std::env::var(k).ok());
        let pushed = sliver_api::service::pushed_desktop(&api.api.state(), backend);
        match pushed {
            Some(d) if d != self.agg.desktop => {
                self.agg.apply(Feed::Desktop(d));
                true
            }
            _ => false,
        }
    }

    pub(super) fn world_locked(&self) -> bool {
        self.lock_watch.locked(
            self.locked_hint,
            self.agg.session.screensaver,
            self.agg.desktop.backend,
        )
    }

    /// The world for a frame at `now`. Copies the API registry first if it changed since the
    /// last copy.
    pub(super) fn world(&mut self, now: Now) -> World {
        if self.published_stale
            && let Some(api) = &self.api
        {
            self.published = Published::snapshot(&api.api);
            self.published_stale = false;
        }
        // logind's LockedHint can be stale; see context::plausibly_locked.
        let locked = self.world_locked();
        let mut session = self.agg.session.clone();
        session.locked = locked;
        let mut context = self.agg.context();
        context.set("locked", locked);
        World {
            now,
            session,
            desktop: self.agg.desktop.clone(),
            context,
            outputs: self.outputs.clone(),
            published: self.published.clone(),
            preview: false,
            stats: self.stats.clone(),
            apps: self.apps.clone(),
            emoji: self.emoji.clone(),
            media_art: self.agg.media_art().map(std::path::Path::to_path_buf),
            spectrum: self.agg.spectrum.clone(),
        }
    }

    pub(super) fn op(&self, op: Op) {
        if let Some(tx) = &self.ops {
            let _ = tx.send(op);
        }
    }

    pub(super) fn modules(&self) -> Option<&sliver_modules::Runtime> {
        self.feeds.modules.as_ref()
    }

    /// Runs what `config` needs (at start and after a reload): its user modules (unchanged
    /// specs keep the running ones), the watchers its modules use, and the API's policy.
    pub(super) fn follow(&mut self, config: &Config) {
        if let Some(api) = &self.api {
            api.set_policy(sliver_agent::api::policy_from_config(config));
        }
        for e in self.feeds.follow(config) {
            eprintln!("sliver-agent: {e}");
        }
        if let Some(rt) = &self.feeds.modules {
            let keys: std::collections::HashSet<String> = rt.keys().into_iter().collect();
            self.outputs.retain(|k, _| keys.contains(k));
        }
    }
}

/// Whether the desktop asked for no animations (unknown: animate).
pub(super) fn reduce_motion(live: &Live) -> bool {
    live.agg.session.appearance.reduce_motion.unwrap_or(false)
}
