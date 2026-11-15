#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A second proof for the end of step 8, with the writes themselves: `dm-log-writes` inside a VM records every write that install, update and apply make to a
test disk, in order, with the flush and FUA marks; then the disk is rebuilt at every flush point and at random points between two flushes (what a loss of
power at that instant would have left), and the store on each rebuilt disk is checked. A MANUAL test, to be run in a VM, as root. Results: docs/POWER-LOSS.md.

Needs in the guest: root; `dmsetup`, `losetup`, `mkfs.ext4`, `mount`; the kernel target `log-writes` (`modprobe dm-log-writes`); `replay-log` of xfstests
(`git clone https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git`, then in `src/log-writes`: `gcc -O2 -D_GNU_SOURCE -o replay-log replay-log.c log-writes.c`, which needs no configure; pass it with --replay-log);
the release binaries of panpak and `range_server.py`; `<work>/packages/v1.panpak` and `v2.panpak` made by `tests/power_loss_packs.py` (signed for --port).

Two phases, one command each:

  record  : makes two sparse files (the test disk and its log), maps the disk through `log-writes`, makes an ext4 file system on it, mounts it, and runs the
            workload as --user with its home on that disk: install, update, apply, rollback, in rounds; `dmsetup message ... mark <step>` is sent between steps
            so that the log says where each began; then unmounts and removes the mapping. What is kept: the log file.
  replay  : for every flush point of the log (`replay-log --check flush`) and for --random points chosen between flushes (a seeded number of entries),
            the disk is made again from zeros by replaying that many entries, a copy-on-write snapshot of it is mounted (the journal of ext4 is replayed
            into the snapshot, not into the disk that the next point needs), and the check runs on it as --user: `panpak list`, `panpak doctor`, a start of
            the app. It must be OK, or give an error that names its repair command, and then that command must make it OK. The snapshot is thrown away.

What it shows and does not: an order of writes that a disk with a volatile cache is allowed to make is a prefix of the log plus any subset of the writes
since the last flush; this replays prefixes (every flush, and a random number of entries after it), not subsets. A prefix is what a clean loss of power gives; a subset is the harder
case and replay-log does not make it.

Usage (as root, in the guest):
  power_loss_dmlog.py record --user USER --bin DIR --work DIR [--rounds 3]
  power_loss_dmlog.py replay --user USER --bin DIR --work DIR --replay-log PATH [--random 100] [--seed N]
"""
import argparse
import os
from pathlib import Path
import random
import re
import shutil
import subprocess
import sys
import time

SIZE = 3 << 30           # the test disk
LOG_SIZE = 12 << 30      # the log of its writes (a sparse file)
ID = 'org.example.incremental'


def sh(cmd, check=True, **kw):
    p = subprocess.run(cmd, shell=isinstance(cmd, str), capture_output=True, text=True, **kw)
    if check and p.returncode:
        sys.exit(f'failed: {cmd}\n{p.stdout}{p.stderr}')
    return p


def losetup(path):
    return sh(['losetup', '--find', '--show', str(path)]).stdout.strip()


def env_of(user, home):
    return (f'env -i PATH=/usr/bin:/bin HOME={home} XDG_DATA_HOME={home}/data XDG_CACHE_HOME={home}/cache XDG_CONFIG_HOME={home}/config '
            f'XDG_RUNTIME_DIR={home}/run PANPAK_NO_BACKGROUND=1 LC_ALL=C')


def as_user(user, home, command, check=False, timeout=300):
    full = f'mkdir -p {home}/run && chmod 700 {home}/run && {env_of(user, home)} {command}'
    return sh(['runuser', '-u', user, '--', 'sh', '-c', full], check=check, timeout=timeout)


def mapped(name):
    return f'/dev/mapper/{name}'


def record(a):
    work = Path(a.work)
    work.mkdir(parents=True, exist_ok=True)
    disk, log = work / 'disk.img', work / 'log.img'
    for f, n in ((disk, SIZE), (log, LOG_SIZE)):
        f.unlink(missing_ok=True)
        with open(f, 'wb') as fh:
            fh.truncate(n)
    ld, ll = losetup(disk), losetup(log)
    sectors = SIZE // 512
    sh('modprobe dm-log-writes', check=False)
    sh(['dmsetup', 'create', 'logw', '--table', f'0 {sectors} log-writes {ld} {ll}'])
    mnt = work / 'mnt'
    mnt.mkdir(exist_ok=True)
    try:
        sh(['mkfs.ext4', '-q', '-F', mapped('logw')])
        sh(['dmsetup', 'message', 'logw', '0', 'mark', 'mkfs'])
        sh(['mount', mapped('logw'), str(mnt)])
        home = mnt / 'h'
        home.mkdir()
        shutil.chown(home, a.user)
        packages = work / 'packages'
        # the packages come from the host side (see tests/power_loss_vm.py: pack of v1 and v2 with the same key); here they are expected in --work
        for v in ('v1.panpak', 'v2.panpak'):
            if not (packages / v).exists():
                sys.exit(f'missing {packages / v}: build them as tests/power_loss_vm.py does and copy them there')
        # The server of the update (its port is the one that the packages were signed for); it lives on the disk of the guest, not on the test disk.
        serve = work / 'serve'
        serve.mkdir(exist_ok=True)
        shutil.copy(packages / 'v2.panpak', serve / 'remote.panpak')
        shutil.chown(serve, a.user)
        for q in serve.iterdir():
            shutil.chown(q, a.user)
        server = subprocess.Popen(['runuser', '-u', a.user, '--', sys.executable, str(a.range_server), str(serve), str(a.port), 'normal'],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.0)
        steps = []
        for r in range(a.rounds):
            steps += [('install', f'{a.bin}/panpak install --yes --runtime {a.bin}/panpak-stub {packages}/v1.panpak'),
                      ('stage', f'{a.bin}/panpak update --stage --allow-insecure-http {ID}'),
                      ('apply', f'{a.bin}/panpak apply --yes {ID}'),
                      ('rollback', f'{a.bin}/panpak rollback {ID}'),
                      ('remove', f'{a.bin}/panpak remove --yes {ID}')]
        for i, (name, cmd) in enumerate(steps):
            sh(['dmsetup', 'message', 'logw', '0', 'mark', f'{i:03d}-{name}-begin'])
            p = as_user(a.user, str(home), cmd)
            print(f'{i:03d} {name}: rc={p.returncode}')
        sh(['dmsetup', 'message', 'logw', '0', 'mark', 'end'])
        sh('sync')
    finally:
        if 'server' in locals():
            server.terminate()
            server.wait(timeout=10)
        sh(['umount', str(mnt)], check=False)
        sh(['dmsetup', 'remove', 'logw'], check=False)
        sh(['losetup', '-d', ld], check=False)
        sh(['losetup', '-d', ll], check=False)
    print(f'log written: {log} ({log.stat().st_size >> 20} MiB sparse)')


CHECK = r'''
B=@BIN@; ID=@ID@; store=$XDG_DATA_HOME/panpak
[ -L "$store/apps/$ID/current" ] || { echo "OK not installed"; exit 0; }
out=$($B/panpak list 2>&1) || { echo "FAIL list: $out"; exit 0; }
d=$($B/panpak doctor "$store/apps/$ID/current" 2>&1)
s=$($B/panpak-stub run "$store/apps/$ID/current" 2>&1)
case "$d$s" in *PANPAK-E2007*) echo "REPAIRABLE panpak repair $ID"; exit 0;; esac
echo "$s" | grep -q "^version " || { echo "FAIL start: $s $d"; exit 0; }
echo OK
'''


def check_point(a, work, label):
    """Mount a snapshot of the rebuilt disk, check, and throw the snapshot away. Returns (verdict, detail)."""
    replay_dev = work / 'replay.img'
    cow = work / 'cow.img'
    cow.unlink(missing_ok=True)
    with open(cow, 'wb') as fh:
        fh.truncate(1 << 30)
    lr, lc = losetup(replay_dev), losetup(cow)
    sectors = SIZE // 512
    mnt = work / 'mnt'   # the same path as at the recording: the links of the store are absolute
    mnt.mkdir(exist_ok=True)
    try:
        sh(['dmsetup', 'create', 'snap', '--table', f'0 {sectors} snapshot {lr} {lc} N 8'])
        r = sh(['mount', '-o', 'rw', mapped('snap'), str(mnt)], check=False)
        if r.returncode:
            return 'FAIL', f'{label}: the file system does not mount: {r.stderr.strip()}'
        home = mnt / 'h'
        if not home.exists():
            return 'OK', f'{label}: the file system is there, the home is not yet (before the first step)'
        script = work / 'check.sh'
        script.write_text(CHECK.replace('@BIN@', a.bin).replace('@ID@', ID))
        script.chmod(0o755)
        p = as_user(a.user, str(home), f'sh {script}')
        line = (p.stdout.strip().splitlines() or ['FAIL no output'])[-1]
        if line.startswith('REPAIRABLE '):
            cmd = line[len('REPAIRABLE '):].replace('panpak ', f'{a.bin}/panpak ', 1)
            as_user(a.user, str(home), f'{cmd} --allow-insecure-http')
            line = (as_user(a.user, str(home), f'sh {script}').stdout.strip().splitlines() or ['FAIL no output'])[-1]
            return ('REPAIRED' if line.startswith('OK') else 'FAIL'), f'{label}: {line}'
        return ('OK' if line.startswith('OK') else 'FAIL'), f'{label}: {line}'
    finally:
        sh(['umount', str(mnt)], check=False)
        sh(['dmsetup', 'remove', 'snap'], check=False)
        sh(['losetup', '-d', lr], check=False)
        sh(['losetup', '-d', lc], check=False)


def rebuild(a, work, limit):
    """The test disk as it was after `limit` entries of the log (None: the whole log up to the next flush that the replay stops at)."""
    img = work / 'replay.img'
    img.unlink(missing_ok=True)
    with open(img, 'wb') as fh:
        fh.truncate(SIZE)
    ll, lr = losetup(work / 'log.img'), losetup(img)
    try:
        sh([a.replay_log, '--log', ll, '--replay', lr, '--limit', str(limit)])
    finally:
        sh(['losetup', '-d', ll], check=False)
        sh(['losetup', '-d', lr], check=False)


def flush_points(a, work):
    """The entry numbers of the flush marks, from the verbose output of replay-log."""
    img = work / 'scan.img'
    img.unlink(missing_ok=True)
    with open(img, 'wb') as fh:
        fh.truncate(SIZE)
    ll, lr = losetup(work / 'log.img'), losetup(img)
    try:
        p = sh([a.replay_log, '--log', ll, '--replay', lr, '--verbose'], check=False)
    finally:
        sh(['losetup', '-d', ll], check=False)
        sh(['losetup', '-d', lr], check=False)
    # Lines look like `replaying 681@136755: sector 0, size 0, flags 0x8(MARK)`: the entry number, then the flag names.
    found = [(int(m.group(1)), m.group(2)) for m in re.finditer(r'replaying (\d+)@\d+:[^\n]*flags 0x[0-9a-f]+\(([^)]*)\)', p.stdout + p.stderr)]
    total = max((n for n, _ in found), default=-1) + 1
    first_mark = min((n for n, fl in found if 'MARK' in fl.split('|')), default=0)   # the mark `mkfs`: before it there is no file system to check
    # A point is a number of entries to replay: after the flush at entry n, n + 1 entries are on the disk.
    return sorted({n + 1 for n, fl in found if n > first_mark and ('FLUSH' in fl.split('|') or 'FUA' in fl.split('|'))}), total


def replay(a):
    work = Path(a.work)
    if not (work / 'log.img').exists():
        sys.exit('no log: run `record` first')
    flushes, total = flush_points(a, work)
    if a.max_flush and len(flushes) > a.max_flush:
        step = len(flushes) / a.max_flush
        flushes = [flushes[int(i * step)] for i in range(a.max_flush)]
    if not flushes:
        sys.exit('no flush point found in the output of replay-log --verbose: check its version and its output format, then adapt flush_points()')
    rnd = random.Random(a.seed)
    points = [(f, f'flush at entry {f}') for f in flushes]
    for _ in range(a.random):
        lo = rnd.choice(flushes)
        nxt = [f for f in flushes if f > lo]
        hi = nxt[0] if nxt else total
        points.append((rnd.randint(lo, max(lo, hi - 1)), f'random entry {{}} after the flush at {lo}'))
    print(f'seed {a.seed}: {len(points)} points ({len(flushes)} flushes, {a.random} random) of {total} entries')
    counts = {'OK': 0, 'REPAIRED': 0, 'FAIL': 0}
    for n, (limit, label) in enumerate(points, 1):
        rebuild(a, work, limit)
        verdict, detail = check_point(a, work, label.format(limit))
        counts[verdict] += 1
        print(f'{n:4d}/{len(points)} {verdict}: {detail}')
        sys.stdout.flush()
    print(f'{len(points)} points: {counts["OK"]} ok, {counts["REPAIRED"]} ok after the repair that the error named, {counts["FAIL"]} failed (seed {a.seed})')
    sys.exit(1 if counts['FAIL'] else 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('phase', choices=['record', 'replay'])
    ap.add_argument('--user', required=True)
    ap.add_argument('--bin', required=True)
    ap.add_argument('--work', required=True)
    ap.add_argument('--rounds', type=int, default=3)
    ap.add_argument('--port', type=int, default=18765, help='the port that the packages were signed for (the server of the update)')
    ap.add_argument('--range-server', dest='range_server', default='range_server.py')
    ap.add_argument('--replay-log', default='replay-log')
    ap.add_argument('--random', type=int, default=100)
    ap.add_argument('--max-flush', type=int, default=0, help='check this many flush points, evenly spaced (0: all of them)')
    ap.add_argument('--seed', type=int, default=int(time.time()))
    a = ap.parse_args()
    if os.geteuid() != 0:
        sys.exit('run as root, in a VM: this makes block devices and mounts them')
    record(a) if a.phase == 'record' else replay(a)


if __name__ == '__main__':
    main()
