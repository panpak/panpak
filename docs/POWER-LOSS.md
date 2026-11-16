# Loss of power

What the store is expected to survive, and the two tests that were run to check it. The order of the writes is in
[PANPAK-FORMAT.md](../PANPAK-FORMAT.md) section 6.2; the stops at every step of every command are in section 6.5 (`tests/failpoints.py`). Those test
the commands. The two tests here stop the whole machine, or replay what a disk would hold.

**Guarantee that was tested.** After a loss of power at any moment of `install`, `update`, `update --stage`, `apply`, `rollback` and `remove`, the
store is whole: `panpak list`, `panpak doctor` and a start of the app work, or the app is not installed. A damaged tree, if there ever is one, is
named with an error that gives its repair command. This holds for a clean loss of power; see the limits.

Pre-beta. One VM, one small test app.

## Test 1: the writes themselves (`tests/power_loss_dmlog.py`)

In a Fedora 45 VM (kernel 7.2), `dm-log-writes` records every write that the commands make to a 3 GiB test disk, with the flush and FUA marks, in order.
The workload: 3 rounds of `install`, `update --stage`, `apply`, `rollback`, `remove` (15 steps, all exited 0), run as a normal user with the home on
that disk. The log has 6230 entries, 1568 of them flush or FUA points after the file system was made.

Then, for each point, the disk is rebuilt from zeros with `replay-log --limit N` (xfstests), a copy-on-write snapshot is mounted (ext4 replays its
journal into the snapshot), and as the user: `panpak list`, `panpak doctor`, a start of the app. The result must be OK, or an error that names
its repair command and then OK after it.

| Run | Points | OK | OK after a repair | Failed |
|---|---|---|---|---|
| every flush or FUA point, and 200 random points between two flushes | 1768 | 1768 | 0 | 0 |

In 296 of the points the app was installed and started (`version N`); in 1472 it was not installed (inside an install before `current`
exists, or after a `remove`). No point needed `panpak repair`.

## Test 2: the VM is killed (`tests/power_loss_vm.py`)

The guest runs `install`, `update`, `rollback`, `update --stage`, `apply`, `remove` in a loop on its own disk. The host runs `virsh destroy` after a random 0.3 to
7 s, starts the VM, waits for ssh, and the same check runs.

| Cycles | OK at once | OK after a repair | Failed |
|---|---|---|---|
| 30 | 30 | 0 | 0 |

## Limits

- Test 1 replays prefixes of the log: what a clean loss of power leaves. A disk with a volatile cache may keep any subset of the writes since the last
  flush; `replay-log` does not make subsets, so that harder case was not tested.
- Test 2 uses `virsh destroy`: it kills QEMU, and what the guest had already sent to its virtual disk is kept. It does not reorder writes. It does show that nothing
  relied on data that was still only in the memory of the guest.
- In test 2 the log of the workload in the guest was not synced and was lost at the kills, so the step that each kill hit is not recorded. The store at the end held
  `current`, `previous` and `update-state`, so the install, update and rollback steps were reached. The 0.3 to 7 s window covers the first seconds of each
  round more than the later steps.
- 83% of the points of test 1 are inside an install or after a `remove`, because installs are most of the writes.
- The check is the store and a start of a small app (400 files, 24 MB), not every byte of a large one.
- No point and no cycle needed a repair, so the path "error names its repair command" was not exercised by a real loss of power here (the stops of
  `tests/failpoints.py` do exercise it).
- Not tested: a real machine with a real power cut, other file systems (the VM test was run on the file system of the guest as it was), other disks.

## Run them again

In the guest, as root, with a kernel that has `dm-log-writes` (`modprobe dm-log-writes`), `dmsetup`, `losetup`, `mkfs.ext4`, `gcc`, `git`:

```sh
git clone https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git
gcc -O2 -D_GNU_SOURCE -o replay-log xfstests-dev/src/log-writes/replay-log.c xfstests-dev/src/log-writes/log-writes.c
python3 tests/power_loss_packs.py WORK/packages                  # on any machine, then copy
python3 tests/power_loss_dmlog.py record --user USER --bin BIN --work WORK --range-server tests/range_server.py
python3 tests/power_loss_dmlog.py replay --user USER --bin BIN --work WORK --replay-log ./replay-log --random 200 [--max-flush N]
```

For test 2, on the host with the VM running: `tests/power_loss_vm.py --domain NAME --ssh ALIAS --yes-destroy NAME --cycles 30`. The test kills and
starts the VM by itself; run it only on a VM that can be killed.
