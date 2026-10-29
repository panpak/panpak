# SPDX-License-Identifier: MIT OR Apache-2.0
"""Guards for the test and bench scripts that create, move or delete files.

Those scripts hand a HOME to panpak and to the applications they start. Every
file they create or delete MUST stay inside their own work directory:

- `require_isolated_home(home, work)`: the script stops unless `home` is inside
  `work`, `home` is not the real home of the user (from the password database,
  not from $HOME) and `work` is neither the real home nor a directory that
  contains it. Only this check can be skipped, on purpose, with the environment
  variable PANPAK_ALLOW_REAL_HOME=1 (a warning is printed).
- `remove_owned`: `safe_rmtree` of a runtime folder, repeated until it is gone.
- `safe_rmtree` and `safe_unlink`: delete only inside the work directory, or a
  runtime directory made by `owned_mkdtemp`. There is no way to turn this off.
- `require_inside(path, work, what)`: stop unless `path` is inside `work`.
- `stop_group(pgid, proc, grace)`: end a process group started by a script.
  It sends SIGTERM, waits for the group to exit and sends SIGKILL only to
  what is left after the grace time, so applications can flush their data.
  Note: Joplin keeps a `lock` file in its profile and does not remove it on
  SIGTERM either. A start after such an end quits at once, with or without
  panpak. Scripts that start Joplin again on the same profile MUST remove
  that file first.
"""
import os
import pwd
import shutil
import sys
import signal
import tempfile
import time
from pathlib import Path

OVERRIDE = 'PANPAK_ALLOW_REAL_HOME'
OWNED_MARK = '.panpak-test-owned'


def _real(p):
    return Path(os.path.realpath(p))


def real_home():
    return _real(pwd.getpwuid(os.getuid()).pw_dir)


def _inside(path, root):
    path, root = _real(path), _real(root)
    return path == root or root in path.parents


def require_isolated_home(home, work):
    if os.environ.get(OVERRIDE) == '1':
        print(f'warning: {OVERRIDE}=1: HOME isolation check skipped for {home}', file=sys.stderr)
        return
    home, work, real = _real(home), _real(work), real_home()
    problems = []
    if home == real:
        problems.append('HOME is your real home')
    if work == real or work in real.parents:
        problems.append('the work directory is, or contains, your real home')
    if not _inside(home, work):
        problems.append(f'HOME {home} is not inside the work directory {work}')
    if problems:
        raise SystemExit(f'refusing to start: {"; ".join(problems)} (override: {OVERRIDE}=1)')


def require_inside(path, work, what='path'):
    if not _inside(path, work):
        raise SystemExit(f'refusing to start: {what} {path} is not inside the work directory {work}')


def owned_mkdtemp(prefix):
    """A short-path temporary directory (Unix socket paths are limited to 108
    bytes) that `safe_rmtree` may delete from any process."""
    d = tempfile.mkdtemp(prefix=prefix)
    (Path(d) / OWNED_MARK).write_text(str(os.getpid()))
    return d


def _may_delete(path, work):
    p = Path(path)
    parent, work = _real(p.parent), _real(work)
    if parent == work or work in parent.parents:  # below the work directory, final link not followed
        return not (parent == work and p.name in ('', '.', '..'))
    if parent == _real(tempfile.gettempdir()):  # a runtime directory made by owned_mkdtemp
        return not p.is_symlink() and (p / OWNED_MARK).is_file()
    return False


def safe_rmtree(path, work, ignore_errors=True):
    if not _may_delete(path, work):
        raise SystemExit(f'refusing to delete {path}: outside the work directory {work}')
    shutil.rmtree(path, ignore_errors=ignore_errors)


def remove_owned(path, work, wait=5.0):
    """Delete a runtime folder made by `owned_mkdtemp`, and keep trying while it comes back: a helper
    of the session (dconf, for one) may create files in it just after the first attempt. Returns True
    when the folder is gone; otherwise prints what is left and returns False."""
    if not Path(path).exists():
        return True
    # Checked once, while the marker file is still there: a first attempt may remove it and leave the rest.
    if not _may_delete(path, work):
        raise SystemExit(f'refusing to delete {path}: outside the work directory {work}')
    deadline = time.monotonic() + wait
    while True:
        shutil.rmtree(path, ignore_errors=True)
        if not Path(path).exists():
            return True
        if time.monotonic() > deadline:
            left = sorted(p.name for p in Path(path).iterdir()) if Path(path).is_dir() else ['(not a folder)']
            print(f'warning: {path} was not removed; left: {left}', file=sys.stderr)
            return False
        time.sleep(0.2)


def safe_unlink(path, work):
    if not _may_delete(path, work):
        raise SystemExit(f'refusing to delete {path}: outside the work directory {work}')
    Path(path).unlink(missing_ok=True)


def stop_group(pgid, proc=None, grace=15.0):
    """SIGTERM the process group, wait up to `grace` seconds, then SIGKILL
    what is still there. `proc` is the Popen of the group leader, if any: it
    is reaped so the leader does not stay a zombie."""
    def alive():
        if proc is not None:
            proc.poll()
        try:
            os.killpg(pgid, 0)
            return True
        except (ProcessLookupError, PermissionError):
            return False

    try:
        os.killpg(pgid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    deadline = time.monotonic() + grace
    while alive() and time.monotonic() < deadline:
        time.sleep(0.1)
    if alive():
        try:
            os.killpg(pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    if proc is not None:
        proc.wait(timeout=10)
