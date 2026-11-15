#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A real loss of power, on a virtual machine: install, update and rollback run in the guest while the host stops it dead (`virsh destroy`) at a random moment,
at least 30 times; after each restart the store must be whole, or the error must be clear and its command must repair it. A MANUAL test (it needs a VM that the
user starts and agrees to have killed; it is not in the CI). Written for the end of step 8 (after the garbage collection of 8e), not run yet.

What it does, per cycle:
  1. the guest starts a workload in the background (install of version 1, update to version 2, rollback, update again, remove, again and again), in a store
     on the disk of the guest (`~/panpak-power` there: not a RAM disk, not /tmp);
  2. after a random time (seeded, printed) the host runs `virsh destroy <domain>` (the guest loses what it had not written to its disk, and nothing is shut down);
  3. the host starts the guest again and waits for ssh;
  4. the check, in the guest: `panpak list`, `panpak doctor <id>`, a start of the app. Each must be OK, or give a clear error that names its command
     (`PANPAK-E2007` and `panpak repair <id>`, `PANPAK-E2004` not installed, ...); then that command is run, and the check must be OK. Anything else is a failure
     of the cycle: the cycle, its seed and the output are kept in `<work>/failures/`.
At the end: how many cycles ended OK at once, how many after a repair, how many failed; the exit status is 1 if one failed.

What this does and does not show: `virsh destroy` kills QEMU, so everything the guest had sent to its virtual disk is kept (the host keeps it in its cache or on
its disk, depending on the `cache=` mode of the disk), and everything that was still in the memory of the guest is lost. It shows that the guest never relied on a
page that was not written back. It does not reorder writes the way a disk with a volatile cache can; for that, a machine with its power cord is needed.

Usage (the VM must be running, ssh must work without a password, python3 must be in the guest):
  tests/power_loss_vm.py --domain "Bluefin #1" --ssh panpak-vm --yes-destroy "Bluefin #1" [--cycles 30] [--seed N] [--work DIR]
`--yes-destroy` must repeat the name of the domain: the test kills it, again and again. Build the release first (`target/x86_64-unknown-linux-musl/release`).
"""
import argparse
import os
from pathlib import Path
import random
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import PUBLIC, resign  # noqa: E402

BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
ID = 'org.example.incremental'
GUEST = '~/panpak-power'   # on the disk of the guest

# The workload that runs in the guest. It never stops by itself until it is killed: install, update, rollback, update, remove, over and over, each step
# checked for its exit status only (a step may fail after a restart that left an old state; the next cycle's check is what counts).
WORKLOAD = r"""#!/bin/sh
cd "$HOME/panpak-power" || exit 1
export HOME="$HOME/panpak-power/h" XDG_DATA_HOME="$HOME/panpak-power/h/data" XDG_CACHE_HOME="$HOME/panpak-power/h/cache" XDG_CONFIG_HOME="$HOME/panpak-power/h/config"
export XDG_RUNTIME_DIR="$HOME/panpak-power/h/run"; mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
B="$HOME/../bin"
n=0
while :; do
  n=$((n+1))
  echo "round $n" >> "$HOME/../workload.log"
  "$B/panpak" install --yes --runtime "$B/panpak-stub" "$HOME/../v1.panpak" >> "$HOME/../workload.log" 2>&1
  cp "$HOME/../v2.panpak" "$HOME/../serve/remote.panpak"
  "$B/panpak" update --allow-insecure-http --timeout 20 org.example.incremental >> "$HOME/../workload.log" 2>&1
  "$B/panpak" rollback org.example.incremental >> "$HOME/../workload.log" 2>&1
  "$B/panpak" update --stage --allow-insecure-http --timeout 20 org.example.incremental >> "$HOME/../workload.log" 2>&1
  "$B/panpak" apply org.example.incremental >> "$HOME/../workload.log" 2>&1
  "$B/panpak" remove --yes org.example.incremental >> "$HOME/../workload.log" 2>&1
done
"""

# The check, in the guest, after a restart. Prints `OK`, or `REPAIRABLE <command>`, or `FAIL <why>`.
CHECK = r"""#!/bin/sh
export HOME="$HOME/panpak-power/h" XDG_DATA_HOME="$HOME/panpak-power/h/data" XDG_CACHE_HOME="$HOME/panpak-power/h/cache" XDG_CONFIG_HOME="$HOME/panpak-power/h/config"
export XDG_RUNTIME_DIR="$HOME/panpak-power/h/run"; mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
B="$HOME/../bin"; ID=org.example.incremental
store="$XDG_DATA_HOME/panpak"
if [ ! -e "$store/apps/$ID/current" ] && [ ! -L "$store/apps/$ID/current" ]; then echo "OK not installed"; exit 0; fi
out=$("$B/panpak" list 2>&1) || { echo "FAIL list: $out"; exit 0; }
d=$("$B/panpak" doctor "$ID" 2>&1); drc=$?
s=$("$B/panpak-stub" run "$store/apps/$ID/current" 2>&1); src=$?
case "$d$s" in
  *PANPAK-E2007*) echo "REPAIRABLE panpak repair $ID"; exit 0;;
esac
echo "$d" | grep -q "verified package" || { echo "FAIL doctor: $d"; exit 0; }
echo "$s" | grep -q "^version " || { echo "FAIL start: $s"; exit 0; }
echo OK
"""


def sh(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def ssh(host, command, timeout=120):
    return sh(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=8', host, command], timeout=timeout)


def wait_ssh(host, seconds=240):
    t = time.time()
    while time.time() - t < seconds:
        if ssh(host, 'true', timeout=20).returncode == 0:
            return True
        time.sleep(3)
    return False


def pack(tmp, version, change, port):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(7)
    for i in range(400):
        data = bytearray(rng.randbytes(60_000))   # about 24 MB in 400 files: an install is long enough to be caught in the middle
        if version == '2' and i % 9 == 0:
            data[5:15] = b'version two'
        (tree / f'payload{i}').write_bytes(bytes(data))
    out = tmp / f'packed{version}.panpak'
    subprocess.run([str(BIN / 'panpak'), 'pack', str(tree), '--stub', str(BIN / 'panpak-stub'), '--secret-file', str(tmp / 'secret.key'), '-o', str(out)], check=True, capture_output=True)
    return resign(out.read_bytes(), f'1.{int(version) - 1}', port)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--domain', required=True)
    ap.add_argument('--ssh', required=True)
    ap.add_argument('--yes-destroy', required=True, help='the name of the domain again: this test kills it, again and again')
    ap.add_argument('--cycles', type=int, default=30)
    ap.add_argument('--seed', type=int, default=int(time.time()))
    ap.add_argument('--work', default=None)
    a = ap.parse_args()
    if a.yes_destroy != a.domain:
        sys.exit('--yes-destroy must repeat the name of the domain')
    if a.cycles < 30:
        sys.exit('at least 30 cycles')
    for b in ('panpak', 'panpak-stub'):
        if not (BIN / b).exists():
            sys.exit(f'build the release first: {BIN / b} is missing')
    state = sh(['virsh', 'domstate', a.domain]).stdout.strip()
    if state not in ('in esecuzione', 'running'):
        sys.exit(f'the domain {a.domain!r} is {state!r}: start it first (this script does not start the first time)')
    rnd = random.Random(a.seed)
    print('seed', a.seed)
    work = Path(a.work) if a.work else Path(tempfile.mkdtemp(prefix='panpak-power-', dir=os.path.expanduser('~/panpak-dogfood')))
    work.mkdir(parents=True, exist_ok=True)
    (work / 'failures').mkdir(exist_ok=True)
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
    sock.close()
    (work / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
    (work / 'secret.key').chmod(0o600)
    for v in ('1', '2'):
        (work / f'v{v}.panpak').write_bytes(pack(work, v, v == '2', port))
    (work / 'workload.sh').write_text(WORKLOAD)
    (work / 'check.sh').write_text(CHECK)
    # Put everything on the disk of the guest.
    ssh(a.ssh, f'mkdir -p {GUEST}/bin {GUEST}/serve {GUEST}/h')
    for local, remote in ((BIN / 'panpak', 'bin/panpak'), (BIN / 'panpak-stub', 'bin/panpak-stub'), (work / 'v1.panpak', 'v1.panpak'), (work / 'v2.panpak', 'v2.panpak'),
                          (work / 'workload.sh', 'workload.sh'), (work / 'check.sh', 'check.sh'), (ROOT / 'tests/range_server.py', 'range_server.py')):
        r = sh(['scp', '-q', '-o', 'BatchMode=yes', str(local), f'{a.ssh}:{GUEST}/{remote}'])
        if r.returncode:
            sys.exit(f'scp {local}: {r.stderr}')
    ssh(a.ssh, f'chmod +x {GUEST}/bin/* {GUEST}/workload.sh {GUEST}/check.sh; sync')
    ok = repaired = failed = 0
    for cycle in range(1, a.cycles + 1):
        # 1. the guest works (the server of the update is part of the workload: a small one, in the background)
        start = (f'cd {GUEST} && cp v1.panpak serve/remote.panpak 2>/dev/null; '
                 f'(nohup python3 range_server.py {GUEST}/serve {port} normal >/dev/null 2>&1 &) ; '
                 f'(nohup sh {GUEST}/workload.sh >/dev/null 2>&1 &) ; true')
        ssh(a.ssh, start)
        delay = rnd.uniform(0.3, 7.0)
        time.sleep(delay)
        # 2. the power goes
        sh(['virsh', 'destroy', a.domain])
        # 3. and comes back
        time.sleep(1.5)
        sh(['virsh', 'start', a.domain])
        if not wait_ssh(a.ssh):
            print(f'cycle {cycle}: the guest did not come back (ssh) in time')
            failed += 1
            (work / 'failures' / f'cycle{cycle}.txt').write_text('no ssh after the restart\n')
            continue
        # 4. the check
        r = ssh(a.ssh, f'sh {GUEST}/check.sh')
        line = (r.stdout.strip().splitlines() or ['FAIL no output'])[-1]
        if line.startswith('OK'):
            ok += 1
            verdict = 'ok'
        elif line.startswith('REPAIRABLE '):
            cmd = line[len('REPAIRABLE '):]
            fix = ssh(a.ssh, f'cd {GUEST} && HOME={GUEST}/h XDG_DATA_HOME={GUEST}/h/data XDG_CACHE_HOME={GUEST}/h/cache XDG_RUNTIME_DIR={GUEST}/h/run {GUEST}/bin/{cmd} --allow-insecure-http 2>&1')
            again = ssh(a.ssh, f'sh {GUEST}/check.sh')
            if again.stdout.strip().endswith('OK') or again.stdout.strip().splitlines()[-1].startswith('OK'):
                repaired += 1
                verdict = f'repaired by `{cmd}`'
            else:
                failed += 1
                verdict = 'FAILED after the repair'
                (work / 'failures' / f'cycle{cycle}.txt').write_text(f'seed {a.seed} delay {delay:.2f}\n{line}\n{fix.stdout}{fix.stderr}\n{again.stdout}{again.stderr}\n')
        else:
            failed += 1
            verdict = 'FAILED: ' + line[:160]
            (work / 'failures' / f'cycle{cycle}.txt').write_text(f'seed {a.seed} delay {delay:.2f}\n{r.stdout}{r.stderr}\n')
        print(f'cycle {cycle:3d}: killed after {delay:4.1f} s: {verdict}')
        sys.stdout.flush()
    print(f'{a.cycles} cycles: {ok} ok at once, {repaired} ok after a repair, {failed} failed (seed {a.seed}, files in {work})')
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
