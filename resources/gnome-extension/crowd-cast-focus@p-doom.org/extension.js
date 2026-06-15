// crowd-cast focus provider — minimal, read-only GNOME Shell extension.
//
// Exposes the focused window's (pid, wm_class, title) on a private session-bus name
// `org.crowdcast.FocusProvider` at `/org/crowdcast/FocusProvider`, plus a `FocusChanged`
// signal. crowd-cast uses this only to gate input capture (record input only while a
// configured target app is focused). It performs NO actions on windows, no network, no UI.
//
// Defensive by design: every shell callback is wrapped so a fault here can never throw
// into gnome-shell (a crash would take down the whole Wayland session). GNOME 45+ (ESM).

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'org.crowdcast.FocusProvider';
const OBJ_PATH = '/org/crowdcast/FocusProvider';
const IFACE = `
<node>
  <interface name="org.crowdcast.FocusProvider">
    <method name="GetFocused">
      <arg type="t" name="window_id" direction="out"/>
      <arg type="i" name="pid" direction="out"/>
      <arg type="s" name="wm_class" direction="out"/>
      <arg type="s" name="title" direction="out"/>
    </method>
    <method name="ListWindows">
      <arg type="a(tiss)" name="windows" direction="out"/>
    </method>
    <method name="GetWindowGeometry">
      <arg type="t" name="window_id" direction="in"/>
      <arg type="b" name="found" direction="out"/>
      <arg type="i" name="win_x" direction="out"/>
      <arg type="i" name="win_y" direction="out"/>
      <arg type="i" name="win_w" direction="out"/>
      <arg type="i" name="win_h" direction="out"/>
      <arg type="i" name="mon_x" direction="out"/>
      <arg type="i" name="mon_y" direction="out"/>
      <arg type="i" name="mon_w" direction="out"/>
      <arg type="i" name="mon_h" direction="out"/>
      <arg type="d" name="scale" direction="out"/>
    </method>
    <signal name="FocusChanged">
      <arg type="t" name="window_id"/>
      <arg type="i" name="pid"/>
      <arg type="s" name="wm_class"/>
      <arg type="s" name="title"/>
    </signal>
  </interface>
</node>`;

export default class CrowdCastFocusExtension extends Extension {
    enable() {
        try {
            this._impl = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
            this._impl.export(Gio.DBus.session, OBJ_PATH);
            this._nameId = Gio.bus_own_name(
                Gio.BusType.SESSION, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
                null, null, null);
            this._focusId = global.display.connect(
                'notify::focus-window', () => this._emit());
            this._emit();
        } catch (e) {
            logError(e, 'crowd-cast-focus: enable failed');
            this.disable();
        }
    }

    disable() {
        try {
            if (this._focusId) {
                global.display.disconnect(this._focusId);
                this._focusId = null;
            }
            if (this._nameId) {
                Gio.bus_unown_name(this._nameId);
                this._nameId = 0;
            }
            if (this._impl) {
                this._impl.unexport();
                this._impl = null;
            }
        } catch (e) {
            logError(e, 'crowd-cast-focus: disable failed');
        }
    }

    _focused() {
        const w = global.display.focus_window;
        if (!w)
            return [0, 0, '', ''];
        // Mutter window stamp — the SAME id ListWindows reports and RecordWindow expects, so
        // crowd-cast can pin/capture the exact focused window (not just the app).
        let id = 0;
        try { id = w.get_id() || 0; } catch (e) {}
        let pid = 0;
        try { pid = w.get_pid() || 0; } catch (e) {}
        let cls = '';
        try { cls = w.get_wm_class() || ''; } catch (e) {}
        let title = '';
        try { title = w.get_title() || ''; } catch (e) {}
        return [id, pid, cls, title];
    }

    // D-Bus method: current focused window (0/'' when no window is focused).
    GetFocused() {
        return this._focused();
    }

    // D-Bus method: enumerate all current toplevel windows as (window_id, pid, wm_class,
    // title). window_id is the Mutter window stamp (Meta.Window.get_id()) — the same id
    // org.gnome.Mutter.ScreenCast.RecordWindow expects. crowd-cast filters these by app
    // identity (wm_class / pid->comm) to bind every window of a target app for capture,
    // since an external client cannot enumerate windows itself (Shell.Introspect.GetWindows
    // is access-denied). Read-only; never throws into the shell.
    ListWindows() {
        const out = [];
        try {
            for (const actor of global.get_window_actors()) {
                let w = null;
                try { w = actor.meta_window; } catch (e) {}
                if (!w)
                    continue;
                let id = 0;
                try { id = w.get_id() || 0; } catch (e) {}
                if (!id)
                    continue;
                let pid = 0;
                try { pid = w.get_pid() || 0; } catch (e) {}
                let cls = '';
                try { cls = w.get_wm_class() || ''; } catch (e) {}
                let title = '';
                try { title = w.get_title() || ''; } catch (e) {}
                out.push([id, pid, cls, title]);
            }
        } catch (e) {
            logError(e, 'crowd-cast-focus: ListWindows failed');
        }
        return out;
    }

    // D-Bus method: geometry of the window with Mutter id `window_id`, for the multi-monitor
    // per-app capture fit (crowd-cast::capture::monitor_layout). Returns the window frame rect
    // and the geometry of the monitor it sits on, all in GNOME *logical* (stage) coordinates,
    // plus that monitor's scale. An external client cannot read window/monitor geometry on
    // Wayland (no protocol exposes another client's placement), so the extension supplies it —
    // same reason ListWindows lives here. crowd-cast derives the on-canvas scale from the actual
    // captured PipeWire buffer size, so `scale` is advisory only. Read-only; never throws into
    // the shell. `found` is false (and the rest zeroed) when the window is gone.
    GetWindowGeometry(window_id) {
        const miss = [false, 0, 0, 0, 0, 0, 0, 0, 0, 1.0];
        try {
            const wid = Number(window_id);
            let target = null;
            for (const actor of global.get_window_actors()) {
                let w = null;
                try { w = actor.meta_window; } catch (e) {}
                if (w && w.get_id() === wid) {
                    target = w;
                    break;
                }
            }
            if (!target)
                return miss;
            const r = target.get_frame_rect();
            const mi = target.get_monitor();
            if (mi < 0)
                return [true, r.x, r.y, r.width, r.height, 0, 0, 0, 0, 1.0];
            const mg = global.display.get_monitor_geometry(mi);
            let scale = 1.0;
            try { scale = global.display.get_monitor_scale(mi); } catch (e) {}
            return [true, r.x, r.y, r.width, r.height, mg.x, mg.y, mg.width, mg.height, scale];
        } catch (e) {
            logError(e, 'crowd-cast-focus: GetWindowGeometry failed');
            return miss;
        }
    }

    _emit() {
        try {
            const [id, pid, cls, title] = this._focused();
            this._impl?.emit_signal(
                'FocusChanged', new GLib.Variant('(tiss)', [id, pid, cls, title]));
        } catch (e) {
            logError(e, 'crowd-cast-focus: emit failed');
        }
    }
}
