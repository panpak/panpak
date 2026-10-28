# SPDX-License-Identifier: MIT OR Apache-2.0
"""Guards for the test and bench scripts that create, move or delete files.

Those scripts hand a HOME to panpak and to the applications they start. Every
file they create or delete MUST stay inside their own work directory:

- `require_isolated_home(home, work)`: the script stops unless `home` is inside
  `work`, `home` is not the real home of the user (from the password database,
  not from $HOME) and `work` is neither the real home nor a directory that
  contains it. Only this check can be skipped, on purpose, with the environment
  variable PANPAK_ALLOW_REAL_HOME=1 (a warning is printed).
- `safe_rmtree` and `safe_unlink`: delete only inside the work directory, or a
  runtime directory made by `owned_mkdtemp`. There is no way to turn this off.
- `require_inside(path, work, what)`: stop unless `path` is inside `work`.
"""
import os
import pwd
import shutil
import sys
import tempfile
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


def safe_unlink(path, work):
    if not _may_delete(path, work):
        raise SystemExit(f'refusing to delete {path}: outside the work directory {work}')
    Path(path).unlink(missing_ok=True)
