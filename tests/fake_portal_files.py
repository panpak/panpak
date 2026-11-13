#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A fake desktop portal for the file dialog tests: no display, no desktop. On the session bus it is started on, it owns
org.freedesktop.portal.Desktop (FileChooser, version 4), org.freedesktop.portal.Documents (version 5) and
org.freedesktop.impl.portal.PermissionStore. Usage: fake_portal_files.py <log file> <answers file>

The answers file is JSON: a list, one entry for each dialog in the order they are asked (each is taken off the file when the dialog is asked):
  {"code": 0, "uris": ["file:///..."], "delay": 0.0, "extra": true}
`extra` adds a `current_filter` and a `choices` entry to the answer, as GTK does. Every call is appended to the log as one JSON line
(the method, the arguments that matter, and for the document calls the path each descriptor points to).
"""
import json
import os
import sys

import gi

gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib  # noqa: E402

LOG, ANSWERS = sys.argv[1], sys.argv[2]
docs = {}      # id -> {'path': str, 'apps': {app: [perms]}}
by_key = {}    # (path) -> id
counter = [0]


def log(**kw):
    with open(LOG, 'a') as f:
        f.write(json.dumps(kw, sort_keys=True) + '\n')


XML = '''<node>
 <interface name="org.freedesktop.portal.FileChooser">
  <property name="version" type="u" access="read"/>
  <method name="OpenFile"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
  <method name="SaveFile"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
  <method name="SaveFiles"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.portal.Documents">
  <property name="version" type="u" access="read"/>
  <method name="AddFull"><arg type="ah" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
    <arg type="as" direction="out"/><arg type="a{sv}" direction="out"/></method>
  <method name="AddNamedFull"><arg type="h" direction="in"/><arg type="ay" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
    <arg type="s" direction="out"/><arg type="a{sv}" direction="out"/></method>
  <method name="List"><arg type="s" direction="in"/><arg type="a{say}" direction="out"/></method>
  <method name="RevokePermissions"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/></method>
  <method name="Delete"><arg type="s" direction="in"/></method>
 </interface>
 <interface name="org.freedesktop.impl.portal.PermissionStore">
  <method name="Lookup"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sas}" direction="out"/><arg type="v" direction="out"/></method>
 </interface>
</node>'''
INFO = Gio.DBusNodeInfo.new_for_xml(XML)


def fd_path(fdlist, idx):
    fd = fdlist.get(idx)
    try:
        return os.readlink(f'/proc/self/fd/{fd}')
    finally:
        os.close(fd)


def new_doc(path, app, perms):
    if path in by_key:
        did = by_key[path]
    else:
        counter[0] += 1
        did = f'd{counter[0]:03d}'
        by_key[path] = did
        docs[did] = {'path': path, 'apps': {}}
    docs[did]['apps'][app] = list(perms)
    return did


def on_chooser(conn, sender, path, iface, method, params, inv):
    args = params.unpack()
    parent, title, opts = args
    log(call=method, parent=parent, title=title, keys=sorted(opts.keys()), opts={k: v for k, v in opts.items() if isinstance(v, (str, bool))})
    # The first entry of the file is this dialog's answer; it is taken off, so that a test can write a new list whenever it likes.
    try:
        queue = json.load(open(ANSWERS))
        answer = queue.pop(0)
        json.dump(queue, open(ANSWERS, 'w'))
    except Exception:
        answer = {'code': 1, 'uris': []}
    token = opts.get('handle_token', 'none')
    handle = '/org/freedesktop/portal/desktop/request/%s/%s' % (inv.get_sender().lstrip(':').replace('.', '_'), token)
    inv.return_value(GLib.Variant('(o)', (handle,)))
    results = {}
    if answer['code'] == 0:
        results['uris'] = GLib.Variant('as', answer['uris'])
        if answer.get('extra'):
            results['current_filter'] = GLib.Variant('(sa(us))', ('Text', [(0, '*.txt')]))
            results['choices'] = GLib.Variant('a(ss)', [('encoding', 'utf8')])
            results['junk'] = GLib.Variant('s', 'not for the app')
    target = inv.get_sender()

    def emit():
        conn.emit_signal(target, handle, 'org.freedesktop.portal.Request', 'Response', GLib.Variant('(ua{sv})', (answer['code'], results)))
        return False
    GLib.timeout_add(int(answer.get('delay', 0) * 1000), emit)


def on_documents(conn, sender, path, iface, method, params, inv):
    args = params.unpack()
    msg = inv.get_message()
    fds = msg.get_unix_fd_list()
    if method == 'AddFull':
        idxs, flags, app, perms = args
        paths = [fd_path(fds, i) for i in idxs]
        ids = [new_doc(p, app, perms) for p in paths]
        log(call=method, flags=flags, app=app, perms=perms, paths=paths, ids=ids)
        inv.return_value(GLib.Variant('(asa{sv})', (ids, {})))
    elif method == 'AddNamedFull':
        idx, name, flags, app, perms = args
        parent = fd_path(fds, idx)
        full = parent + '/' + bytes(name).rstrip(b'\0').decode()
        did = new_doc(full, app, perms)
        log(call=method, flags=flags, app=app, perms=perms, paths=[full], ids=[did])
        inv.return_value(GLib.Variant('(sa{sv})', (did, {})))
    elif method == 'List':
        app = args[0]
        out = {i: list(d['path'].encode() + b'\0') for i, d in docs.items() if d['apps'].get(app)}
        log(call=method, app=app, ids=sorted(out))
        inv.return_value(GLib.Variant('(a{say})', (out,)))
    elif method == 'RevokePermissions':
        did, app, perms = args
        docs[did]['apps'][app] = [p for p in docs[did]['apps'].get(app, []) if p not in perms]
        log(call=method, id=did, app=app, perms=perms)
        inv.return_value(None)
    elif method == 'Delete':
        log(call=method, id=args[0])
        d = docs.pop(args[0], None)
        if d:
            by_key.pop(d['path'], None)
        inv.return_value(None)


def on_store(conn, sender, path, iface, method, params, inv):
    table, did = params.unpack()
    d = docs.get(did)
    perms = {a: p for a, p in (d['apps'].items() if d else []) if p}
    log(call='Lookup', id=did)
    inv.return_value(GLib.Variant('(a{sas}v)', (perms, GLib.Variant('s', ''))))


def get_prop(conn, sender, path, iface, name):
    return GLib.Variant('u', 4 if iface.endswith('FileChooser') else 5)


def bus_acquired(conn, name):
    for i, handler in (('org.freedesktop.portal.FileChooser', on_chooser), ('org.freedesktop.portal.Documents', on_documents)):
        p = '/org/freedesktop/portal/desktop' if i.endswith('FileChooser') else '/org/freedesktop/portal/documents'
        conn.register_object(p, INFO.lookup_interface(i), lambda *a, h=handler: h(*a), get_prop, None)
    conn.register_object('/org/freedesktop/impl/portal/PermissionStore', INFO.lookup_interface('org.freedesktop.impl.portal.PermissionStore'),
                         lambda *a: on_store(*a), None, None)


loop = GLib.MainLoop()
owned = [0]


def acquired(conn, name):
    owned[0] += 1
    if owned[0] == 3:
        open(LOG + '.ready', 'w').write('ready')


for n in ('org.freedesktop.portal.Desktop', 'org.freedesktop.portal.Documents', 'org.freedesktop.impl.portal.PermissionStore'):
    Gio.bus_own_name(Gio.BusType.SESSION, n, Gio.BusNameOwnerFlags.NONE, bus_acquired if n.endswith('Desktop') else None, acquired, None)
loop.run()
