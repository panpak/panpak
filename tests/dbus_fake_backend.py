#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A fake portal backend for tests/dbus_filter.py: no display, no desktop. It owns
org.freedesktop.impl.portal.desktop.fake on the session bus it is started on and implements the
backend side of Settings, Notification and Inhibit. Every call it gets is appended to the log
file (argv[1]) so that a test can see what reached it, with the application id the portal passed.

Control: the method test.Fake.SetColorScheme(u) on /test/fake changes the color scheme and emits
SettingChanged, as a desktop would when the user switches the theme.
"""
import sys

import gi

gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib  # noqa: E402

LOG = sys.argv[1]
scheme = [1]  # 1 = prefer dark


def log(line):
    with open(LOG, 'a') as f:
        f.write(line + '\n')


XML = '''<node>
 <interface name="org.freedesktop.impl.portal.Settings">
  <property name="version" type="u" access="read"/>
  <method name="ReadAll"><arg type="as" direction="in"/><arg type="a{sa{sv}}" direction="out"/></method>
  <method name="Read"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
  <signal name="SettingChanged"><arg type="s"/><arg type="s"/><arg type="v"/></signal>
 </interface>
 <interface name="org.freedesktop.impl.portal.Notification">
  <property name="version" type="u" access="read"/>
  <property name="SupportedOptions" type="a{sv}" access="read"/>
  <method name="AddNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
  <method name="RemoveNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
  <signal name="ActionInvoked"><arg type="s"/><arg type="s"/><arg type="s"/><arg type="av"/></signal>
 </interface>
 <interface name="org.freedesktop.impl.portal.Inhibit">
  <property name="version" type="u" access="read"/>
  <method name="Inhibit"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="a{sv}" direction="in"/></method>
  <method name="CreateMonitor"><arg type="o" direction="in"/><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="u" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.impl.portal.FileChooser">
  <property name="version" type="u" access="read"/>
  <method name="OpenFile"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="u" direction="out"/><arg type="a{sv}" direction="out"/></method>
  <method name="SaveFile"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="u" direction="out"/><arg type="a{sv}" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.impl.portal.Request">
  <method name="Close"/>
 </interface>
 <interface name="org.freedesktop.impl.portal.Session">
  <method name="Close"/>
  <property name="version" type="u" access="read"/>
 </interface>
 <interface name="test.Fake">
  <method name="SetColorScheme"><arg type="u" direction="in"/></method>
 </interface>
</node>'''

# Services of the session that an app must never reach. The fake owns their names and answers any
# call with "reached", so a refusal cannot be mistaken for a service that is not there.
DECOYS = ['org.freedesktop.secrets', 'org.freedesktop.systemd1', 'org.freedesktop.Notifications', 'org.gnome.keyring', 'ca.desrt.dconf',
          'org.gnome.Shell', 'org.kde.StatusNotifierWatcher', 'org.a11y.Bus', 'org.freedesktop.login1', 'org.freedesktop.PolicyKit1']
filtered = [False]


def decoy_filter(conn, message, incoming):
    if incoming and message.get_message_type() == Gio.DBusMessageType.METHOD_CALL and message.get_destination() in DECOYS:
        log(f'decoy {message.get_destination()} {message.get_interface()}.{message.get_member()}')
        reply = Gio.DBusMessage.new_method_reply(message)
        reply.set_body(GLib.Variant('(s)', ('reached',)))
        conn.send_message(reply, Gio.DBusSendMessageFlags.NONE)
        return None
    return message


def method_call(conn, sender, path, iface, method, params, inv):
    a = params.unpack()
    if iface == 'org.freedesktop.impl.portal.Settings':
        if method == 'ReadAll':
            inv.return_value(GLib.Variant('(a{sa{sv}})', ({'org.freedesktop.appearance': {'color-scheme': GLib.Variant('u', scheme[0])}},)))
        elif method == 'Read':
            if a == ('org.freedesktop.appearance', 'color-scheme'):
                inv.return_value(GLib.Variant('(v)', (GLib.Variant('u', scheme[0]),)))
            else:
                inv.return_dbus_error('org.freedesktop.portal.Error.NotFound', 'no such setting')
    elif iface == 'org.freedesktop.impl.portal.Notification':
        log(f'{method} app_id={a[0]} id={a[1]}')
        inv.return_value(None)
    elif iface == 'org.freedesktop.impl.portal.Inhibit':
        log(f'{method} app_id={a[1] if method == "Inhibit" else a[2]}')
        inv.return_value(GLib.Variant('(u)', (0,)) if method == 'CreateMonitor' else None)
    elif iface == 'org.freedesktop.impl.portal.FileChooser':
        # The user's choice is scripted: the file `<log>.chooser` holds the addresses (one per line); an empty or missing file is a dialog the user closed.
        log(f'{method} app_id={a[1]} parent={a[2]} keys={",".join(sorted(a[4]))}')
        try:
            uris = [u for u in open(LOG + '.chooser').read().split() if u]
        except OSError:
            uris = []
        if uris:
            inv.return_value(GLib.Variant('(ua{sv})', (0, {'uris': GLib.Variant('as', uris)})))
        else:
            inv.return_value(GLib.Variant('(ua{sv})', (1, {})))
    elif iface == 'test.Fake':
        scheme[0] = a[0]
        conn.emit_signal(None, '/org/freedesktop/portal/desktop', 'org.freedesktop.impl.portal.Settings', 'SettingChanged',
                         GLib.Variant('(ssv)', ('org.freedesktop.appearance', 'color-scheme', GLib.Variant('u', scheme[0]))))
        inv.return_value(None)
    else:
        inv.return_value(None)


def get_prop(conn, sender, path, iface, prop):
    if prop == 'version':
        return GLib.Variant('u', 2)
    if prop == 'SupportedOptions':
        return GLib.Variant('a{sv}', {})
    return None


def on_bus(conn, name):
    if not filtered[0]:
        filtered[0] = True
        conn.add_filter(decoy_filter)
    if name != 'org.freedesktop.impl.portal.desktop.fake':
        return
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    for i in node.interfaces:
        for p in ('/org/freedesktop/portal/desktop', '/test/fake'):
            if (i.name == 'test.Fake') != (p == '/test/fake'):
                continue
            conn.register_object(p, i, method_call, get_prop, None)


loop = GLib.MainLoop()
Gio.bus_own_name(Gio.BusType.SESSION, 'org.freedesktop.impl.portal.desktop.fake', Gio.BusNameOwnerFlags.NONE, on_bus, lambda c, n: log('ready'), lambda c, n: loop.quit())
for n in DECOYS:
    Gio.bus_own_name(Gio.BusType.SESSION, n, Gio.BusNameOwnerFlags.NONE, on_bus, None, None)
loop.run()
