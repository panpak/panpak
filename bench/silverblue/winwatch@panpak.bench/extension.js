// SPDX-License-Identifier: MIT OR Apache-2.0
// Writes one JSON line per window event to ~/.cache/panpak-bench/windows.jsonl:
//   {"t": <seconds since 1970, real time>, "event": "created" | "first-frame" | "closed", "wm_class": "...", "app_id": "...", "title": "...", "pid": N}
// "first-frame" is the moment the compositor has the first frame of the window on screen: the time to a usable window is that minus the time the command was started.
// Nothing here changes what is shown. It needs GNOME Shell 45 or later (ES modules).
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class WinWatch extends Extension {
    enable() {
        const dir = GLib.build_filenamev([GLib.get_user_cache_dir(), 'panpak-bench']);
        GLib.mkdir_with_parents(dir, 0o755);
        this._path = GLib.build_filenamev([dir, 'windows.jsonl']);
        this._ids = [];
        this._hooked = new Set();
        this._ids.push([global.display, global.display.connect('window-created', (_d, w) => this._watch(w))]);
        this._ids.push([global.window_manager, global.window_manager.connect('map', (_wm, actor) => {
            const w = actor.meta_window;
            if (w && w.get_window_type() === Meta.WindowType.NORMAL)
                this._hook(w);
        })]);
        // Windows that exist already (the extension was enabled after them) are not reported: the measurements start apps after login.
        this._log({event: 'enabled', wm_class: '', app_id: '', title: '', pid: 0});
    }

    disable() {
        for (const [obj, id] of this._ids)
            obj.disconnect(id);
        this._ids = [];
    }

    _now() {
        return GLib.get_real_time() / 1e6;
    }

    _log(fields) {
        const line = JSON.stringify({t: this._now(), ...fields}) + '\n';
        try {
            const f = Gio.File.new_for_path(this._path);
            const out = f.append_to(Gio.FileCreateFlags.NONE, null);
            out.write_all(new TextEncoder().encode(line), null);
            out.close(null);
        } catch (e) {
            console.error(`winwatch: ${e}`);
        }
    }

    _fields(w, event) {
        return {
            event,
            wm_class: w.get_wm_class() ?? '',
            app_id: w.get_gtk_application_id() ?? '',
            title: w.get_title() ?? '',
            pid: w.get_pid(),
        };
    }

    _hook(w) {
        // The actor of the window may not exist yet when the window is created: it is looked for again when the window is mapped.
        const id = w.get_id();
        if (this._hooked.has(id))
            return;
        const actor = w.get_compositor_private();
        if (!actor)
            return;
        this._hooked.add(id);
        const first = actor.connect('first-frame', () => {
            actor.disconnect(first);
            this._log(this._fields(w, 'first-frame'));
        });
    }

    _watch(w) {
        if (w.get_window_type() !== Meta.WindowType.NORMAL)
            return;
        this._log(this._fields(w, 'created'));
        this._hook(w);
        const closed = w.connect('unmanaged', () => {
            this._log(this._fields(w, 'closed'));
            w.disconnect(closed);
            this._hooked.delete(w.get_id());
        });
    }
}
