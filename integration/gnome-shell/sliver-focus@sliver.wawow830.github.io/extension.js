// Sliver focus: a GNOME Shell extension (ESM, GNOME 45+) that feeds the focused window and
// workspace to the Sliver session agent over D-Bus (io.github.wawow830.Sliver.Desktop1,
// docs/api.md section 5).
//
// Mutter lets no client ask "what is focused?" (org.gnome.Shell.Eval is locked down), so the
// shell side has to push. The agent is optional: calls use NO_AUTO_START and ignore errors,
// so nothing happens while it is not running. The first caller of SetFocus becomes the agent's
// focus source.
//
// NOT yet run against a real GNOME Shell (none on the development machine); written from the
// GNOME Shell 45+ extension API. See integration/README.md.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const NAME = 'io.github.wawow830.Sliver';
const PATH = '/io/github/wawow830/Sliver';
const IFACE = `${NAME}.Desktop1`;

// What the agent calls back (workspace switching); the agent never starts us, we own the name
// only while enabled.
const SHELL_NAME = `${NAME}.Shell`;
const SHELL_PATH = `${PATH}/Shell`;
const SHELL_XML = `<node><interface name="${NAME}.Shell1">
  <method name="FocusWorkspace"><arg type="i" direction="in" name="number"/></method>
</interface></node>`;

function str(v) {
    return GLib.Variant.new_string(String(v ?? ''));
}

// Windows that are chrome, not something the user is "in".
function isPlain(w) {
    if (!w)
        return false;
    switch (w.get_window_type()) {
    case Meta.WindowType.DESKTOP:
    case Meta.WindowType.DOCK:
    case Meta.WindowType.MENU:
    case Meta.WindowType.DROPDOWN_MENU:
    case Meta.WindowType.POPUP_MENU:
    case Meta.WindowType.TOOLTIP:
    case Meta.WindowType.NOTIFICATION:
    case Meta.WindowType.COMBO:
    case Meta.WindowType.DND:
    case Meta.WindowType.OVERRIDE_OTHER:
        return false;
    default:
        return true;
    }
}

function describe(w) {
    const ws = w.get_workspace();
    const d = {
        'app-id': str(w.get_gtk_application_id() || w.get_sandboxed_app_id?.() || w.get_wm_class()),
        'title': str(w.get_title()),
        'window': str(`mutter:${w.get_stable_sequence()}`),
        'workspace': str(ws ? ws.index() + 1 : ''),
        'fullscreen': GLib.Variant.new_uint32(w.is_fullscreen() ? 2 : 0),
        'floating': GLib.Variant.new_boolean(w.is_above()),
        'xwayland': GLib.Variant.new_boolean(w.get_client_type() === Meta.WindowClientType.X11),
        'monitor': str(w.get_monitor()),
    };
    const pid = w.get_pid();
    if (pid > 0)
        d['pid'] = GLib.Variant.new_uint32(pid);
    return d;
}

function call(method, params) {
    Gio.DBus.session.call(
        NAME, PATH, IFACE, method, params, null,
        Gio.DBusCallFlags.NO_AUTO_START, -1, null,
        (conn, res) => {
            try {
                conn.call_finish(res);
            } catch (_e) {
                // The agent is not running or is not our focus source: nothing to do.
            }
        });
}

export default class SliverFocus extends Extension {
    enable() {
        this._window = null;
        this._windowSignals = [];
        this._display = global.display;
        this._wm = global.workspace_manager;
        this._displayId = this._display.connect('notify::focus-window', () => this._focusChanged());
        this._wsId = this._wm.connect('active-workspace-changed', () => {
            this._pushWorkspaces();
            this._pushFocus();
        });
        this._wsCountIds = ['workspace-added', 'workspace-removed'].map(
            s => this._wm.connect(s, () => this._pushWorkspaces()));
        this._focusChanged();
        this._pushWorkspaces();
        // The agent's "switch to workspace N" (1-based, as reported in SetWorkspaces).
        this._exported = Gio.DBusExportedObject.wrapJSObject(SHELL_XML, this);
        this._exported.export(Gio.DBus.session, SHELL_PATH);
        this._ownerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, SHELL_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    FocusWorkspace(number) {
        const ws = this._wm?.get_workspace_by_index(number - 1);
        ws?.activate(global.get_current_time());
    }

    disable() {
        if (this._ownerId)
            Gio.bus_unown_name(this._ownerId);
        this._exported?.unexport();
        this._ownerId = 0;
        this._exported = null;
        this._unwatchWindow();
        this._display?.disconnect(this._displayId);
        this._wm?.disconnect(this._wsId);
        this._wsCountIds?.forEach(id => this._wm.disconnect(id));
        // Tell the agent we're gone so it can fall back to another source.
        call('SetFocus', new GLib.Variant('(a{sv})', [{}]));
        this._display = this._wm = null;
        this._wsCountIds = null;
    }

    _unwatchWindow() {
        this._windowSignals.forEach(id => this._window?.disconnect(id));
        this._windowSignals = [];
        this._window = null;
    }

    _focusChanged() {
        this._unwatchWindow();
        const w = this._display.focus_window;
        if (isPlain(w)) {
            this._window = w;
            // Titles and fullscreen state change under a focused window.
            for (const prop of ['title', 'fullscreen'])
                this._windowSignals.push(w.connect(`notify::${prop}`, () => this._pushFocus()));
        }
        this._pushFocus();
    }

    _pushFocus() {
        const dict = this._window ? describe(this._window) : {};
        call('SetFocus', new GLib.Variant('(a{sv})', [dict]));
    }

    _pushWorkspaces() {
        const n = this._wm.get_n_workspaces();
        const active = this._wm.get_active_workspace_index();
        const list = [];
        for (let i = 0; i < n; i++) {
            list.push({
                'id': str(i + 1),
                'name': str(Meta.prefs_get_workspace_name?.(i) ?? i + 1),
                'active': GLib.Variant.new_boolean(i === active),
                'urgent': GLib.Variant.new_boolean(false),
            });
        }
        call('SetWorkspaces', new GLib.Variant('(aa{sv})', [list]));
    }
}
