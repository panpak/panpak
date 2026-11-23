# Contributing to `Panpak`

Read the [guiding principles](docs/PRINCIPLES.md) first: they are what a change is judged by.

## Principles (non negotiable)

- **No weakened checks**: every change must keep or strengthen the
  `THREAT-MODEL.md` guarantees. When in doubt, fail-closed.
- **One commit per goal**, clear message (what + why); never commit
  with broken tests or warnings.
- **A test that fails once and then passes is written down** in `tests/UNSTABLE.md`; if it fails again it is investigated before anything else is merged.
- **Green tests, zero warnings** (clean `cargo build/test`; `clippy`
  with no new warnings in the touched areas).
- **Spec before code** for the format: format changes require updating
  `PANPAK-FORMAT.md` + fixtures in `conformance/` (independent
  verifier + Rust cross-check).

## Workflow

1. Read `PANPAK-FORMAT.md`, `THREAT-MODEL.md`, `QUICKSTART.md`.
2. Open/edit the needed code (workspace: `panpak-core`,
   `panpak-stub`, `panpak`).
3. Add or update tests:
   - Rust unit tests where possible;
   - Landlock fork tests for new sandbox rules;
   - conformance fixtures (`conformance/gen.py` + `run.py`)
     for any format/verify change.
4. Run before committing:
   `cargo test --locked --target x86_64-unknown-linux-musl`,
   `cargo clippy --locked --all-targets --target x86_64-unknown-linux-musl -- -D warnings`
   (CI runs clippy on this target with warnings as errors),
   `python3 conformance/run.py --rust`,
   `python3 -m unittest discover -s tests/apps -p 'test_*.py'` (the app harness),
   `./scripts/check-spdx.sh` and `./scripts/repro-build.sh` (if you touch build or toolchain files; a change
   to the binaries needs new hashes in `REPRODUCIBLE-HASHES.md`, in a separate commit: when the pull request touches the code, run
   `./scripts/repro-build.sh --update` on your machine and commit the file it writes; the CI only checks it, it never writes it).
5. Update the docs touched by the change
   (`QUICKSTART.md`, `RELEASE.md`, `THREAT-MODEL.md`, etc.).

## Documents go with the change

A change that adds or changes a behavior updates, in the same pull request, the documents that speak of it: the specification (`PANPAK-FORMAT.md`), `docs/`, `THREAT-MODEL.md`, `docs/PRINCIPLES.md` when it applies, and the README if what a user or an author reads first changes. A pull request that changes a behavior without updating its documents is not finished.

In the description of the pull request, say which documents you updated, or why none needed to be. If the change affects what the website says, name the page.

## Rules for tests and builds

- A test that creates or deletes files runs in an isolated HOME inside its own work folder and uses
  `tests/isolation.py`. It never touches your real home or store.
- Graphical tests run on a virtual X server or a headless Sway, never on your own session. Tests of
  D-Bus use a private bus (`tests/dbus_lab.py`), never your session bus.
- **No test can reach your desktop, even by mistake.** The runners take it out of the environment: `scripts/test-runner.sh` (the `runner` of cargo, set in `.cargo/config.toml`, for every
  test binary), `tests/with_timeout.py` (the CI) and `tests/isolation.py` (imported by every script of `tests/`, so a script run by hand is covered too) remove `DISPLAY`,
  `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` and `XAUTHORITY`, give `DBUS_SESSION_BUS_ADDRESS` an address that leads nowhere (or the canary of the CI) and `XDG_RUNTIME_DIR` a folder of the test's own.
  A test that needs a display, a bus or a runtime folder makes its own (Xvfb, headless Sway, `tests/dbus_lab.py`) and puts it in the environment it hands to the program. A test of a window runs
  with the window tool stand-in (`PANPAK_TEST_TOOLS_DIR`, debug builds only), never with the real `zenity` or `kdialog`. `tests_run_without_the_desktop_of_the_person` fails when the runner did not run.
- A test ends the processes it started by their process id, never by name.
- Builds of programs from source (`panpak build`, `tests/apps/run.py`) are heavy: run one at a time, and
  give the run a memory cap (`systemd-run --user --scope -p MemoryMax=10G -p MemorySwapMax=6G -- ...`;
  the harness does it by itself unless `--memory-cap none`). See `docs/APP-HARNESS.md`.
- Test data is made for the repository and declared in `docs/TEST-DATA.md`. Do not add files of third
  parties, secrets, personal paths or user names.
- Do not publish packages made with this code: there is no registry yet (see the status in `README.md`).

## Style

- Clear error messages, in English like the rest of the project,
  telling what to do (`reinstall`, `update`, `use --flag`).
- Never silent fallbacks: every degradation requires explicit consent
  or a hard error.
- No new dependencies without a reason documented in the commit.

## License of contributions

The code is licensed under `MIT OR Apache-2.0` and the documentation under
CC BY 4.0 (see `README.md`). By contributing you agree that your contribution
is provided under those licenses. Every new source file must start with the
SPDX header (`// SPDX-License-Identifier: MIT OR Apache-2.0`, or `#` for
Python, shell, YAML and TOML); `./scripts/check-spdx.sh --fix` adds it.

## Developer Certificate of Origin (DCO)

Every commit MUST carry a `Signed-off-by` line with the name you are known by
in the project (a legal name is not required) and an email address you can be
reached at:

```
Signed-off-by: Jane Developer <jane@example.org>
```

Add it with `git commit -s`. The line certifies the Developer Certificate of
Origin, version 1.1:

> By making a contribution to this project, I certify that:
>
> (a) The contribution was created in whole or in part by me and I have the
> right to submit it under the open source license indicated in the file; or
>
> (b) The contribution is based upon previous work that, to the best of my
> knowledge, is covered under an appropriate open source license and I have
> the right under that license to submit that work with modifications,
> whether created in whole or in part by me, under the same open source
> license (unless I am permitted to submit under a different license), as
> indicated in the file; or
>
> (c) The contribution was provided directly to me by some other person who
> certified (a), (b) or (c) and I have not modified it.
>
> (d) I understand and agree that this project and the contribution are
> public and that a record of the contribution (including all personal
> information I submit with it, including my sign-off) is maintained
> indefinitely and may be redistributed consistent with this project or the
> open source license(s) involved.

CI checks the sign-off on every commit of a pull request
(`scripts/check-dco.sh`). Commits authored by Dependabot are exempt from the
sign-off. To fix missing sign-offs:
`git rebase --signoff <base>` and force-push your branch.

## Name and trademark

The code is free; the name "Panpak" is covered by `TRADEMARKS.md`. Do not
use it for modified forks or products in a way that suggests endorsement.
