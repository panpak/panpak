#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Dedicated Xvfb session for document-save automation. Never uses user display.

Usage: gui_session.py work-dir start APP [args...]
       gui_session.py work-dir key ctrl+s
       gui_session.py work-dir type /absolute/output
       gui_session.py work-dir screenshot
       gui_session.py work-dir stop
Requires prepared work-dir from pilot_cycle.py and xdotool in PATH.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pilot_cycle import environment
import isolation


def main():

    work = Path(sys.argv[1]).resolve()
    action = sys.argv[2]
    statefile = work / 'gui-session.json'
    if action == 'start':
        app = sys.argv[3]
        home = work / 'home'
        isolation.require_isolated_home(home, work)
        appdata = home / '.local/share' / f'org.example.{app}'
        fd = os.open(work / 'gui-display', os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
        xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
        server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x900x24'],
                                  pass_fds=[fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  start_new_session=True)
        os.close(fd)
        for _ in range(100):
            text = (work / 'gui-display').read_text().strip()
            if text: break
            time.sleep(0.1)
        assert text
        env = environment(home, app, ':' + text)
        log = (work / f'{app}-save.log').open('w')
        flags = sys.argv[4:]
        p = subprocess.Popen([home / '.local/bin' / f'org.example.{app}', *flags], env=env,
                             stdout=log, stderr=log, start_new_session=True, cwd=appdata)
        state = {'app': app, 'pid': p.pid, 'server': server.pid, 'display': ':' + text,
                 'runtime': env['XDG_RUNTIME_DIR']}
        statefile.write_text(json.dumps(state))
        print(state)
    else:
        state = json.loads(statefile.read_text())
        env = dict(os.environ, DISPLAY=state['display'])
        if action == 'stop':
            for pid in (state['pid'], state['server']):
                try: os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError: pass
            isolation.safe_rmtree(state.get('runtime', '/nonexistent'), work)
        elif action == 'screenshot':
            subprocess.run(['import', '-window', 'root', work / 'gui.png'], env=env, check=True)
            print(work / 'gui.png')
        elif action == 'windows':
            subprocess.run(['xwininfo', '-root', '-tree'], env=env, check=True)
        else:
            subprocess.run(['xdotool', action, *sys.argv[3:]], env=env, check=True)


if __name__ == "__main__":
    main()
