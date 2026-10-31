# Contributing to `Panpak`

## Principles (non negotiable)

- **No weakened checks**: every change must keep or strengthen the
  `THREAT-MODEL.md` guarantees. When in doubt, fail-closed.
- **One commit per goal**, clear message (what + why); never commit
  with broken tests or warnings.
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
   `cargo test --target x86_64-unknown-linux-musl`,
   `python3 conformance/run.py --rust`,
   `./scripts/repro-build.sh` (if you touch build/toolchain).
5. Update the docs touched by the change
   (`QUICKSTART.md`, `RELEASE.md`, `THREAT-MODEL.md`, etc.).

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
