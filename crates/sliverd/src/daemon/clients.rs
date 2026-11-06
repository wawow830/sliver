//! Agent connections: hello, roles, liveness pings, packets, and the seat's active user.

use super::*;

impl Daemon {
    /// Pings the active agent unless a ping is outstanding. Without `force`, at most every
    /// [`PING_MIN_INTERVAL_MS`].
    pub(super) fn ping(&mut self, now: u64, force: bool) {
        let Some(c) = self.active else { return };
        if self.liveness.outstanding.is_some() {
            return;
        }
        if !force
            && self
                .liveness
                .last_ping_ms
                .is_some_and(|t| now.saturating_sub(t) < PING_MIN_INTERVAL_MS)
        {
            return;
        }
        self.next_nonce += 1;
        let nonce = self.next_nonce;
        self.liveness.outstanding = Some((nonce, now + PONG_TIMEOUT_MS));
        self.liveness.last_ping_ms = Some(now);
        self.send(c, DaemonMsg::Ping { nonce });
    }

    // ---- connections --------------------------------------------------------------------------

    pub(super) fn connected(&mut self, client: ClientId, uid: u32) {
        if self.clients.len() >= MAX_CLIENTS {
            self.out.push(Out::Close(client));
            return;
        }
        self.clients.insert(client, Client::new(uid));
    }

    pub(super) fn send(&mut self, client: ClientId, msg: DaemonMsg) {
        self.out.push(Out::Send(client, msg));
    }

    pub(super) fn error(
        &mut self,
        client: ClientId,
        code: ErrorCode,
        msg: String,
        serial: Option<u32>,
    ) {
        self.send(client, DaemonMsg::Error { code, msg, serial });
    }

    pub(super) fn close(&mut self, client: ClientId) {
        if self.active == Some(client) {
            self.drop_agent();
        }
        if self.clients.remove(&client).is_some() {
            self.out.push(Out::Close(client));
        }
    }

    /// The active agent is gone or demoted: release what it held and show the safe layer.
    pub(super) fn drop_agent(&mut self) {
        let Some(old) = self.active.take() else {
            return;
        };
        self.liveness = Liveness::default();
        if let Some(c) = self.clients.get_mut(&old) {
            // Demoted, still connected: frames don't carry over to its next promotion.
            c.base = None;
            c.fn_layer = None;
            let held = std::mem::take(&mut c.held_keys);
            self.keys.release(&held, &mut *self.hw.keys);
        }
        self.recompose();
    }

    pub(super) fn promote(&mut self, client: ClientId) {
        self.active = Some(client);
        self.liveness = Liveness::default();
        let state = self.idle.state();
        if state != DisplayState::On {
            self.send(client, DaemonMsg::Display { state });
        }
        if self.hatch.active() {
            self.send(client, DaemonMsg::Hatch { active: true });
        }
        if self.fn_held {
            self.send(client, DaemonMsg::FnKey { held: true });
        }
    }

    pub(super) fn session_changed(&mut self, now: u64, uid: Option<u32>) {
        let _ = now;
        self.active_uid = uid;
        let demote = self
            .active
            .filter(|c| self.clients.get(c).map(|c| c.uid) != uid);
        if let Some(c) = demote {
            self.send(
                c,
                DaemonMsg::Role {
                    role: Role::Standby,
                },
            );
            self.drop_agent();
        }
        if self.active.is_none() {
            // The newest eligible connection takes over.
            let next = self
                .clients
                .iter()
                .rev()
                .find(|(_, c)| c.said_hello && Some(c.uid) == uid)
                .map(|(id, _)| *id);
            if let Some(next) = next {
                self.send(next, DaemonMsg::Role { role: Role::Active });
                self.promote(next);
            }
        }
    }

    pub(super) fn packet(
        &mut self,
        now: u64,
        client: ClientId,
        msg: Result<AgentMsg, CodecError>,
        fd: Attachment,
    ) {
        let Some(c) = self.clients.get(&client) else {
            return;
        };
        let msg = match msg {
            Ok(m) => m,
            Err(e @ CodecError::UnknownMessage(_)) => {
                return self.error(client, e.code(), e.to_string(), None);
            }
            Err(e) => {
                // Garbage on the wire: don't try to follow it.
                self.error(client, e.code(), e.to_string(), None);
                return self.close(client);
            }
        };
        if matches!(fd, Attachment::WrongCount) {
            self.error(
                client,
                ErrorCode::Malformed,
                "wrong number of fds".into(),
                None,
            );
            return self.close(client);
        }
        if let (false, AgentMsg::Report { proto, client: who }) = (c.said_hello, &msg) {
            // Read-only (sliverctl doctor, any uid): never an agent, so roles stay as they are.
            if PROTO.negotiate(*proto).is_none() {
                self.error(
                    client,
                    ErrorCode::Proto,
                    format!("sliverd speaks {PROTO:?}"),
                    None,
                );
                return self.close(client);
            }
            crate::debug!("report for {who} (uid {})", c.uid);
            self.send(client, DaemonMsg::HwReport(Box::new(self.hw_report())));
            return self.close(client);
        }
        if !c.said_hello && !matches!(msg, AgentMsg::Hello { .. }) {
            self.error(client, ErrorCode::Malformed, "hello first".into(), None);
            return self.close(client);
        }
        let is_active = self.active == Some(client);
        match msg {
            AgentMsg::Hello { proto, .. } => self.hello(client, proto),
            AgentMsg::AttachBuffer {
                id,
                width,
                height,
                stride,
                format,
            } => self.attach(client, id, width, height, stride, format, fd),
            AgentMsg::DetachBuffer { id } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.buffers.remove(&id);
                }
            }
            AgentMsg::Present(p) if is_active => self.present(client, p),
            AgentMsg::Present(p) => self.error(
                client,
                ErrorCode::NotActive,
                "standby".into(),
                Some(p.serial),
            ),
            AgentMsg::ClearLayer { layer: Layer::Fn } if is_active => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.fn_layer = None;
                }
                self.recompose();
            }
            AgentMsg::ClearLayer { layer: Layer::Base } => self.error(
                client,
                ErrorCode::InvalidFrame,
                "only the fn layer can be cleared".into(),
                None,
            ),
            AgentMsg::DisplayPolicy(p) if is_active => {
                if let Some(on) = p.pixel_shift {
                    self.set_pixel_shift(on, now);
                }
                let before = self.idle.state();
                let after = self.idle.set_policy(&p, now);
                if after != before {
                    self.display_changed(now, after);
                } else {
                    self.apply_backlight();
                }
            }
            AgentMsg::Wake if is_active => {
                let c = self.clients.get_mut(&client).expect("checked");
                if c.last_wake_ms
                    .is_some_and(|t| now.saturating_sub(t) < WAKE_MIN_INTERVAL_MS)
                {
                    self.error(client, ErrorCode::RateLimited, "wake".into(), None);
                } else {
                    c.last_wake_ms = Some(now);
                    self.activity(now);
                }
            }
            AgentMsg::EmitKeys { keys, mode } if is_active => {
                self.emit_keys(now, client, keys, mode)
            }
            AgentMsg::Pong { nonce } => {
                if is_active && self.liveness.outstanding.is_some_and(|(n, _)| n == nonce) {
                    self.liveness.outstanding = None;
                }
            }
            AgentMsg::Bye => self.close(client),
            // From an agent (after hello): `report` is for non-agent connections.
            AgentMsg::Report { .. } => self.error(
                client,
                ErrorCode::Malformed,
                "report replaces hello; it isn't for agents".into(),
                None,
            ),
            AgentMsg::ClearLayer { .. }
            | AgentMsg::DisplayPolicy(_)
            | AgentMsg::Wake
            | AgentMsg::EmitKeys { .. } => {
                self.error(client, ErrorCode::NotActive, "standby".into(), None)
            }
        }
    }

    pub(super) fn hello(&mut self, client: ClientId, proto: sliver_ipc::proto::ProtoVersion) {
        let c = self.clients.get_mut(&client).expect("checked");
        if c.said_hello {
            return self.error(client, ErrorCode::Malformed, "duplicate hello".into(), None);
        }
        let Some(version) = PROTO.negotiate(proto) else {
            self.error(
                client,
                ErrorCode::Proto,
                format!("sliverd speaks {:?}", PROTO),
                None,
            );
            return self.close(client);
        };
        c.said_hello = true;
        let eligible = Some(c.uid) == self.active_uid;
        let role = if eligible {
            Role::Active
        } else {
            Role::Standby
        };
        if let (true, Some(old)) = (eligible, self.active) {
            self.send(
                old,
                DaemonMsg::Role {
                    role: Role::Revoked,
                },
            );
            self.close(old);
        }
        self.send(
            client,
            DaemonMsg::Welcome {
                proto: version,
                panel: self.panel.clone(),
                role,
                features: Vec::new(),
            },
        );
        if eligible {
            self.promote(client);
        }
    }
}
