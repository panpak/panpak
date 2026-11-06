import Shell from 'gi://Shell';
// SPDX-License-Identifier: MIT OR Apache-2.0
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class Probe extends Extension {
    enable() {
        this._log = GLib.build_filenamev([GLib.get_home_dir(), 'panpak-probe.log']);
        this._n = 0;
        const sys = Shell.AppSystem.get_default();
        this._sig = sys.connect('installed-changed', () => { this._n++; this._w({event: 'installed-changed', n: this._n}); });
        this._w({event: 'enabled'});
        this._t = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 500, () => { this._poll(); return GLib.SOURCE_CONTINUE; });
    }
    _w(o) {
        o.t = GLib.get_monotonic_time() / 1e6;
        const f = Gio.File.new_for_path(this._log);
        const s = f.append_to(Gio.FileCreateFlags.NONE, null);
        s.write_all(JSON.stringify(o) + '\n', null);
        s.close(null);
    }
    _poll() {
        const f = Gio.File.new_for_path(GLib.build_filenamev([GLib.get_home_dir(), 'panpak-probe.watch']));
        let ids = [];
        try { ids = new TextDecoder().decode(f.load_contents(null)[1]).split('\n').filter(x => x); } catch (e) { return; }
        const sys = Shell.AppSystem.get_default();
        for (const id of ids) {
            const app = sys.lookup_app(id);
            let o = {event: 'state', id, found: !!app};
            if (app) {
                const ai = app.get_app_info();
                o.actions = ai ? ai.list_actions() : null;
                o.name = ai ? ai.get_name() : null;
                const ic = app.get_icon();
                o.icon = ic ? ic.to_string() : null;
                o.running = app.state !== Shell.AppState.STOPPED;
            }
            const k = JSON.stringify(o);
            if (this._last?.[id] !== k) { (this._last ??= {})[id] = k; this._w(o); }
        }
    }
    disable() {
        GLib.source_remove(this._t);
        Shell.AppSystem.get_default().disconnect(this._sig);
    }
}
