#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Who may sign an app, over HTTP (PANPAK-FORMAT.md 3.2): `panpak key new`, the first install that has to say whom it trusts, the succession of the signing key
(by the old key, and by the recovery key whose 24 words the author kept), what a thief of the signing key can and cannot do, and the note for a change of channel
that takes the same revision with other bytes. Real binaries (build the release first), an isolated store, a local server with Range support.
"""
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN  # noqa: E402

ID = 'org.example.trust'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok is True:
        assert p.returncode == 0, p.stdout + p.stderr
    elif ok is False:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-trust-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('PANPAK_RECOVERY', None)  # the keys here come from `key new`, with their own recovery commitment
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()

        def new_key(name):
            """`panpak key new -o`: the key, its commitment beside it, and the 24 words that are shown once (kept in a file here, as a person keeps them on paper)."""
            p = run([BIN / 'panpak', 'key', 'new', '-o', tmp / f'{name}.key'], env)
            words = [w for l in p.stdout.splitlines() if l.startswith('    ') for w in l.split()]
            assert len(words) == 24, p.stdout
            (tmp / f'{name}.words').write_text(' '.join(words) + '\n')
            pub = next(l.split()[-1] for l in p.stdout.splitlines() if l.startswith('key:'))
            commit = (tmp / f'{name}.key.recovery').read_text().strip()
            assert commit.startswith('blake3:') and (tmp / f'{name}.key').stat().st_mode & 0o777 == 0o600
            return pub, commit

        pub_a, rec_a = new_key('a')   # the author's key and recovery key
        pub_b, _ = new_key('b')       # the next signing key, with the recovery commitment of a (given by --recovery)
        pub_t, rec_t = new_key('t')   # a thief: his own key, his own recovery key
        pub_c, rec_c = new_key('c')   # the author's new key after a recovery, with a new recovery commitment

        def pack(name, key, release='', extra=(), domain=''):
            tree = tmp / f'tree-{name}'
            tree.mkdir()
            (tree / 'hello').write_text(f'#!/bin/sh\necho "{name}"\n')
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(
                f'[app]\nid="{ID}"\nname="Trust"\nversion="{name}"\nentry="hello"\n{release}{domain}[update]\nurl="http://127.0.0.1:{port}/remote.panpak"\n')
            out = tmp / f'{name}.panpak'
            run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / f'{key}.key', '-o', out, *extra], env)
            return out

        serve = tmp / 'serve'
        serve.mkdir()
        serving = lambda p: (serve / 'remote.panpak').write_bytes(Path(p).read_bytes())
        store = tmp / 'data/panpak'
        appdir = store / 'apps' / ID

        def state():
            return dict(l.split('=', 1) for l in (appdir / 'trust').read_text().splitlines() if '=' in l)

        def update(flags=(), ok=True):
            return run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', *flags, ID], env, ok=ok)

        # 0. Packing without a key is an error that names the command, and a key is never made by itself.
        bare = {**env, 'HOME': str(tmp / 'nokey'), 'XDG_CONFIG_HOME': str(tmp / 'nokey/config')}
        (tmp / 'nokey').mkdir()
        p = run([BIN / 'panpak', 'pack', tmp / 'nokey', '--stub', BIN / 'panpak-stub', '-o', tmp / 'x.panpak'], bare, ok=False)
        assert 'PANPAK-E1000' in p.stderr and 'panpak key new' in p.stderr, p.stderr
        assert not (tmp / 'nokey/config').exists(), 'a key or a folder was made'
        # A key made without its recovery commitment (an old key, a key from elsewhere) is refused too, and the way out is named.
        (tmp / 'old.key').write_text((bytes([77]) * 32).hex() + '\n')
        (tmp / 'old.key').chmod(0o600)
        (tmp / 'tree-old').mkdir()
        (tmp / 'tree-old/hello').write_text('x')
        (tmp / 'tree-old/panpak.toml').write_text(f'[app]\nid="{ID}"\nname="T"\nversion="1"\nentry="hello"\n')
        p = run([BIN / 'panpak', 'pack', tmp / 'tree-old', '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'old.key', '-o', tmp / 'x.panpak'], env, ok=False)
        assert 'recovery commitment' in p.stderr and 'panpak key new' in p.stderr, p.stderr
        print('pack without a key, or without the recovery commitment, says which command makes them PASS')

        v1 = pack('v1', 'a')
        # 1. The first install says whom it trusts. --yes alone is not enough, a wrong key is refused even with --yes, and nothing is pinned or made ready.
        install = ['install', '--yes', '--runtime', BIN / 'panpak-stub']
        p = run([BIN / 'panpak', *install, v1], env, ok=False)
        assert 'PANPAK-E1003' in p.stderr and '--expect-key' in p.stderr and '--trust-first-key' in p.stderr, p.stderr
        assert not appdir.exists() or not (appdir / 'trust').exists()
        p = run([BIN / 'panpak', *install, '--expect-key', pub_t, v1], env, ok=False)
        assert 'PANPAK-E1004' in p.stderr and pub_a in p.stderr, p.stderr
        assert not (appdir / 'trust').exists() and not (appdir / 'current').exists() and not (tmp / '.local/bin' / ID).exists(), 'a wrong key left something behind'
        # No terminal and no flag: the question cannot be asked, and the message says what to give instead.
        p = run([BIN / 'panpak', 'install', '--runtime', BIN / 'panpak-stub', v1], env, ok=False)
        assert 'PANPAK-E5004' in p.stderr and '--expect-key' in p.stderr, p.stderr
        run([BIN / 'panpak', *install, '--trust-first-key', v1], env)
        print('the first install: --yes alone refused, a wrong --expect-key refused with nothing left, --trust-first-key accepted PASS')
        out = run([BIN / 'panpak', 'list'], env).stdout
        assert '127.0.0.1 (author)' in out, out
        st = state()
        assert st['root'] == pub_a and st['recovery'] == rec_a and st['source'] == 'author' and st['floor'] == '0', st
        doctor = run([BIN / 'panpak', 'doctor'], env).stdout
        assert 'Updates: from 127.0.0.1, named by the author.' in doctor, doctor
        print('the trust record has the root, the recovery commitment and the source; list and doctor say where updates come from PASS')

        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)
            # 2. The same key: an ordinary update.
            v2 = pack('v2', 'a', extra=['--previous', v1])
            serving(v2)
            assert 'updated' in update().stdout
            # 3. The author changes the signing key: the old key signs the new one, and the recovery commitment stays.
            # Without the succession the new key is refused (made first: once a succession is made, the chain is kept beside the key and every package carries it).
            v3bare = pack('v3bare', 'b', extra=['--recovery', rec_a], release='[release]\nrevision = 3\n')
            serving(v3bare)
            p = update(ok=False)
            assert 'PANPAK-E1005' in p.stderr and 'succession' in p.stderr, p.stderr
            v3 = pack('v3', 'b', extra=['--previous', v2, '--rotate-from-file', tmp / 'a.key', '--recovery', rec_a])
            assert (tmp / 'b.key.chain').exists(), 'the chain is kept beside the new key'
            # Two more releases, with no option about the succession: they carry the chain by themselves. The client has not seen v3 or v3b: it stays on the old key
            # until v3c, and follows the chain from there.
            v3b = pack('v3b', 'b', extra=['--previous', v3])
            v3c = pack('v3c', 'b', extra=['--previous', v3b])
            serving(v3c)
            p = update()
            assert 'changed their signing key in the regular way' in p.stderr + p.stdout, p.stdout + p.stderr
            st = state()
            assert st['root'] == pub_b and st['recovery'] == rec_a, st
            print('a succession by the old key moves the pin, the recovery commitment stays, and later releases carry the chain with no option PASS')

            # 4. A thief of the signing key b: he can sign a succession to his own key that keeps the commitment (that is what the key is for), but never one that
            # changes the commitment.
            greedy = pack('v4greedy', 't', extra=['--previous', v3c, '--rotate-from-file', tmp / 'b.key', '--recovery', rec_t])
            serving(greedy)
            p = update(ok=False)
            assert 'PANPAK-E1005' in p.stderr and 'recovery commitment' in p.stderr, p.stderr
            assert state()['root'] == pub_b
            thief = pack('v4thief', 't', extra=['--previous', v3c, '--rotate-from-file', tmp / 'b.key', '--recovery', rec_a])
            serving(thief)
            update()
            assert state()['root'] == pub_t and state()['recovery'] == rec_a
            print('a thief cannot change the recovery commitment; with the signing key he can move the pin (until the author recovers) PASS')

            # 5. The author uses the 24 words: a new root and a new commitment, accepted from a pin that the thief moved.
            back = pack('v5', 'c', extra=['--previous', thief, '--recover-with', tmp / 'a.words'])
            serving(back)
            p = update()
            assert 'took control of the app back' in p.stderr + p.stdout, p.stdout + p.stderr
            st = state()
            assert st['root'] == pub_c and st['recovery'] == rec_c, st
            # The thief is out: his next package has no succession from c, and his old key is not the pin.
            serving(pack('v6thief', 't', extra=['--previous', back, '--recovery', rec_a]))
            p = update(ok=False)
            assert 'PANPAK-E1005' in p.stderr, p.stderr
            # And an install over the installed app with another key is refused as before.
            p = run([BIN / 'panpak', *install, '--trust-first-key', tmp / 'v6thief.panpak'], env, ok=False)
            assert 'PANPAK-E1004' in p.stderr, p.stderr
            # A wrong word is caught before anything is signed.
            (tmp / 'wrong.words').write_text((tmp / 'a.words').read_text().replace((tmp / 'a.words').read_text().split()[0], 'xxxx', 1))
            tree = tmp / 'tree-v5'
            p = run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'c.key', '--recover-with', tmp / 'wrong.words', '-o', tmp / 'x2.panpak'], env, ok=False)
            assert 'not a word of the recovery phrase' in p.stderr, p.stderr
            print('the recovery key (the 24 words) takes the app back from a thief, and a wrong word is caught PASS')

            # 6. A change of channel that takes the same revision with other bytes is told by `doctor`, with the note kept in the trust record.
            serving(pack('v7', 'c', extra=['--previous', back]))
            update()
            installed_rev = 8  # v1 to v3c are revisions 1 to 5, v4thief 6, v5 7 (`--previous`); v6 was refused; v7 is revision 8
            same = pack('v7beta', 'c', release=f'[release]\nrevision = {installed_rev}\nchannel = "beta"\n')
            serving(same)
            update(['--switch-channel'])
            assert 'switch_note=stable to beta at revision 8' in (appdir / 'trust').read_text(), (appdir / 'trust').read_text()
            doctor = run([BIN / 'panpak', 'doctor'], env).stdout
            assert 'with the same number as the one you had' in doctor and 'error of the author' in doctor and 'stable to beta at revision' in doctor, doctor
            print('a change of channel with the same revision and other bytes is noted and doctor says so PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)
    print('trust update PASS')


if __name__ == '__main__':
    main()
