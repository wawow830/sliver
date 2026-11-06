//! Who fills the bar: the focused app's context for App controls, and the status contexts, under
//! the user's policy.

use super::*;

/// What the agent knows about the desktop right now.
#[derive(Debug, Clone, Default)]
pub struct Focus {
    pub window: Option<Window>,
    pub workspace: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AppSource {
    #[default]
    Auto,
    /// No app controls for this app.
    Disabled,
    /// Only this provider may fill App controls for this app.
    Provider(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPolicy {
    pub source: AppSource,
    /// "May take the whole bar". Default on.
    pub allow_modal: bool,
}

impl Default for AppPolicy {
    fn default() -> Self {
        AppPolicy {
            source: AppSource::Auto,
            allow_modal: true,
        }
    }
}

/// The user's preferences that affect published items (from config).
#[derive(Debug, Clone)]
pub struct Policy {
    pub allow_app: bool,
    pub allow_target: bool,
    /// Keyed by window class / app id, matched case-insensitively. `"*"` sets the default for
    /// apps without their own entry (config `[apps."*"]`).
    pub per_app: HashMap<String, AppPolicy>,
    pub hidden_providers: HashSet<String>,
    /// Status providers in the user's order. Unlisted ones follow, sorted by id.
    pub status_order: Vec<String>,
    /// Sandboxed app ids allowed to publish `status` contexts.
    pub sandbox_status_allowed: HashSet<String>,
}

impl Policy {
    /// The policy for a focused app: its own entry (case-insensitive), else `"*"`, else defaults.
    pub fn for_app(&self, class: &str) -> AppPolicy {
        self.per_app
            .get(class)
            .or_else(|| {
                self.per_app
                    .iter()
                    .find(|(k, _)| k.as_str() != "*" && k.eq_ignore_ascii_case(class))
                    .map(|(_, v)| v)
            })
            .or_else(|| self.per_app.get("*"))
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            allow_app: true,
            allow_target: true,
            per_app: HashMap::new(),
            hidden_providers: HashSet::new(),
            status_order: Vec::new(),
            sandbox_status_allowed: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    /// Fills App controls.
    pub app: Option<ContextId>,
    /// `app` is modal and allowed to cover Main + Controls.
    pub modal: bool,
    /// Status items, in order.
    pub status: Vec<ContextId>,
}

impl Registry {
    /// Who fills App controls and Status items.
    pub fn resolve(&self, focus: &Focus, policy: &Policy, proc: &dyn ProcInfo) -> Resolved {
        if focus.locked {
            return Resolved::default();
        }
        let live = |c: &&Context| c.visible && !policy.hidden_providers.contains(&c.provider);

        // Status items.
        let mut status: Vec<&Context> = self
            .contexts
            .values()
            .filter(live)
            .filter(|c| matches!(c.opts.scope, Scope::Status))
            .filter(|c| {
                let id = &self
                    .clients
                    .get(&c.owner)
                    .map(|cl| cl.identity.clone())
                    .unwrap_or_default();
                !id.sandboxed()
                    || id
                        .verified_app_id()
                        .is_some_and(|a| policy.sandbox_status_allowed.contains(a))
            })
            .collect();
        let rank = |p: &str| {
            policy
                .status_order
                .iter()
                .position(|x| x == p)
                .unwrap_or(usize::MAX)
        };
        status.sort_by(|a, b| {
            (rank(&a.provider), &a.provider, a.id).cmp(&(rank(&b.provider), &b.provider, b.id))
        });
        let status = status.into_iter().map(|c| c.id).collect();

        // App controls.
        let Some(win) = &focus.window else {
            return Resolved {
                app: None,
                modal: false,
                status,
            };
        };
        let app_policy = policy.for_app(&win.class);
        if !policy.allow_app || app_policy.source == AppSource::Disabled {
            return Resolved {
                app: None,
                modal: false,
                status,
            };
        }
        let win_identity = win.pid.map(|p| proc.identity(p));
        let mut best: Option<(u8, u64, &Context)> = None;
        for c in self.contexts.values().filter(live) {
            if let AppSource::Provider(p) = &app_policy.source
                && &c.provider != p
            {
                continue;
            }
            let Some(rank) = self.app_rank(c, win, win_identity.as_ref(), focus, policy, proc)
            else {
                continue;
            };
            if best.is_none_or(|(r, s, _)| {
                (rank, std::cmp::Reverse(c.seq)) < (r, std::cmp::Reverse(s))
            }) {
                best = Some((rank, c.seq, c));
            }
        }
        let app = best.map(|b| b.2);
        Resolved {
            app: app.map(|c| c.id),
            modal: app.is_some_and(|c| {
                c.opts.modal && matches!(c.opts.scope, Scope::App) && app_policy.allow_modal
            }),
            status,
        }
    }

    /// Lower is better: 0 window-bound, 1 same app, 2 include-children, 3 target. `None` = no
    /// match.
    fn app_rank(
        &self,
        c: &Context,
        win: &Window,
        win_identity: Option<&ProcIdentity>,
        focus: &Focus,
        policy: &Policy,
        proc: &dyn ProcInfo,
    ) -> Option<u8> {
        let client = self.clients.get(&c.owner)?;
        match &c.opts.scope {
            Scope::Status => None,
            Scope::Target(t) => {
                if !policy.allow_target || client.identity.sandboxed() {
                    return None;
                }
                let ok = t
                    .app_id
                    .as_ref()
                    .is_none_or(|a| a.eq_ignore_ascii_case(&win.class))
                    && t.workspace
                        .as_ref()
                        .is_none_or(|ws| focus.workspace.as_ref() == Some(ws))
                    && t.title.as_ref().is_none_or(|re| re.is_match(&win.title));
                ok.then_some(3)
            }
            Scope::App => {
                let (cpid, wpid) = (client.pid?, win.pid?);
                let same_app = cpid == wpid
                    || win_identity.is_some_and(|wi| wi.same_instance(&client.identity));
                if same_app {
                    return match c.opts.window.as_deref().and_then(hyprland_address) {
                        Some(addr) if addr == hyprland_norm(&win.address) => Some(0),
                        Some(_) => None, // bound to another window of this app
                        None => Some(1), // unbound, or a handle we can't resolve: app-level
                    };
                }
                (c.opts.include_children && proc.is_ancestor(wpid, cpid)).then_some(2)
            }
        }
    }
}

fn hyprland_norm(a: &str) -> String {
    a.trim_start_matches("0x").to_ascii_lowercase()
}

/// `hyprland:0xabc` → `abc`. Other handle kinds aren't resolvable yet.
fn hyprland_address(handle: &str) -> Option<String> {
    handle.strip_prefix("hyprland:").map(hyprland_norm)
}
