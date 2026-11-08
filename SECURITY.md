# Security policy

Panpak is pre-beta software. The format may change and nothing here is a
promise of security for important data. This file says how to report a
problem and what to expect.

## Supported versions

Only the `main` branch. There are no stable releases yet, so older commits
and tags get no fixes: a report is checked against the current `main`.

## How to report

Use GitHub's private vulnerability reporting: open the repository, choose
**Security**, then **Report a vulnerability**. The report is visible only to
the maintainers. Please do not open a public issue or pull request for a
vulnerability before it is fixed.

A useful report has:

1. what the problem is and what an attacker gains;
2. the steps to reproduce it (commands, and a test `.panpak` if that helps);
3. the commit (`git rev-parse HEAD`), `panpak doctor` output and `uname -r`;
4. a proposed fix or mitigation, if you have one.

Test only on your own machines or in containers and virtual machines you
control. Do not send working exploits against other people's systems.

## What to expect

Panpak is maintained by volunteers. These are targets, not commitments:

- an answer to the report within **7 days**;
- a first assessment (confirmed, not a vulnerability, or needs more
  information) within **30 days**;
- a fix, or a documented decision not to fix, within **90 days** for a
  confirmed problem. A problem that lets a package run unverified code or
  leave the sandbox is handled first.

Disclosure is coordinated: the fix is published before the details, and the
reporter is credited unless they prefer not to be. If no fix exists after 90
days, the reporter may publish the report.

There is **no bug bounty** and no payment of any kind.

## In scope

- Verification before execution: the signature, the stub hash, the index and
  chunk hashes, trust on first use and the key pin, key rotation.
- `panpak update` and `panpak rollback`: the download, the checks before the
  switch, the rules for new permissions.
- The stub and the sandbox it builds: the Landlock rules, the isolated view
  (bubblewrap), the files and folders it creates, the launcher and `.desktop`
  files written by `install`, the channel that opens addresses, the removal of
  stale lock files.
- The parsers of untrusted input: the package format, the manifest, and
  `panpak convert` reading an AppImage.
- `panpak build` and its recipes (the fetch of sources and images, the build sandbox, the package it makes).
- The build and release scripts, and the reproducibility of the binaries.

Known limits that are already written down (`THREAT-MODEL.md` section 5, for example KL-31, the D-Bus
names shared with a native program until step 13p) are not new findings; a way around them is.

## Out of scope

- The limits listed in `THREAT-MODEL.md`, section "Known limitations". They
  are known and documented. A way to make one of them worse, or to get past a
  defence that the document says holds, is in scope.
- Bugs in the Linux kernel, Landlock, bubblewrap, the C library or the desktop
  (report those to their projects), unless Panpak uses them in an unsafe way.
- Vulnerabilities of the applications inside a package, and the content of
  third-party AppImages. A flaw in the way `panpak convert` handles them is in
  scope.
- An attacker who already runs code as the same user outside the sandbox: the
  local store is trusted (`THREAT-MODEL.md`, section 1).
- A hostile channel for the first install: the first key is pinned without an
  out-of-band check.
- Denial of service by an app against its own resources.
- Social engineering, and reports from automated scanners with no
  demonstration of impact.
