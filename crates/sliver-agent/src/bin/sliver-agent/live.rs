//! The live world: the feeds (desktop, session, user modules), `Control1` and the ops thread,
//! and the [`World`] they make for each frame.

use super::*;

pub(super) struct Live {
    pub(super) feeds: Feeds,
    pub(super) agg: Aggregate,
    pub(super) outputs: HashMap<String, sliver_agent::scripts::Output>,
    /// logind's raw `LockedHint`, before the plausibility check.
    pub(super) locked_hint: bool,
    pub(super) lock_watch: sliver_agent::context::LockWatch,
    pub(super) ops: Option<std::sync::mpsc::Sender<Op>>,
    pub(super) api: Option<ApiLink>,
    /// cpu/memory/... sampler, kept across frames for CPU deltas, and what the config's modules
    /// read with it.
    pub(super) stats: sliver_agent::stats::Sampler,
    pub(super) wants: sliver_agent::stats::Wants,
    /// Installed applications (taskbar icons).
    pub(super) apps: std::sync::Arc<sliver_icons::desktop::DesktopEntries>,
}

impl Live {
    /// `api`: serve `Control1` (`Some(replace)`), or not (`--render`).
    pub(super) fn start(config: &Config, api: Option<bool>) -> std::io::Result<Live> {
        // `Control1` first: owning the bus name is what tells systemd (Type=dbus) and D-Bus
        // activation that the agent is up, so it must not wait on slow session sources.
        let api = match api.map(ApiLink::start) {
            None => None,
            Some(Ok(a)) => Some(a),
            // Another agent drives the bar; two would revoke each other on sliverd.
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
                eprintln!("sliver-agent: D-Bus interface unavailable: {e}");
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
            stats: Default::default(),
            wants: Default::default(),
            apps: std::sync::Arc::new(sliver_icons::desktop::DesktopEntries::from_paths(
                &sliver_icons::SearchPaths::from_env(|k| std::env::var(k).ok()),
            )),
        };
        live.follow(config);
        Ok(live)
    }

    pub(super) fn pump(&mut self) -> bool {
        let feeds = self.feeds.drain();
        let any = !feeds.is_empty();
        for f in feeds {
            match &f {
                Feed::Module(u) => {
                    self.outputs.insert(u.module.clone(), u.output.clone());
                }
                Feed::Session(s) => self.locked_hint = s.locked,
                Feed::Note(n) => eprintln!("sliver-agent: {n}"),
                Feed::Desktop(_) | Feed::Art { .. } => {}
            }
            self.agg.apply(f);
        }
        any
    }

    pub(super) fn world_locked(&self) -> bool {
        self.lock_watch.locked(
            self.locked_hint,
            self.agg.session.screensaver,
            self.agg.desktop.backend,
        )
    }

    pub(super) fn world(&mut self, now: Now) -> World {
        let wants = self.wants.clone();
        self.world_reading(now, &wants)
    }

    /// Reads the statistics `wants` asks for: a preview's config may place modules the live one
    /// doesn't.
    pub(super) fn world_reading(&mut self, now: Now, wants: &sliver_agent::stats::Wants) -> World {
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
            stats: self.stats.readings(wants, now.instant),
            apps: self.apps.clone(),
            media_art: self.agg.media_art().map(std::path::Path::to_path_buf),
        }
    }

    pub(super) fn op(&self, op: Op) {
        if let Some(tx) = &self.ops {
            let _ = tx.send(op);
        }
    }

    pub(super) fn modules(&self) -> Option<&sliver_agent::scripts::Runtime> {
        self.feeds.modules.as_ref()
    }

    /// Runs what `config` needs: its user modules (unchanged specs keep the running ones) and the
    /// watchers its modules use.
    pub(super) fn follow(&mut self, config: &Config) {
        self.wants = sliver_agent::stats::wants(config);
        self.feeds.follow(config);
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
