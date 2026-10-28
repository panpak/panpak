# Reporting vulnerabilities

If you found a security problem in `Panpak` (sandbox bypass,
signature verification, TOFU/pin, Landlock, supply chain), report it
privately: **do not open public issues** until it is fixed.

## How to report

Write to **security@panpak.org**
with:

1. problem description and estimated impact;
2. minimal steps to reproduce (commands, test `.panpak` if useful);
3. version (`panpak --version`, commit hash) and kernel (`uname -r`);
4. if possible, a proposed fix or mitigation.

Do not send working exploits against other people's systems; tests
must run only locally or in dedicated containers.

## What happens next

- **Acknowledgment within 3 working days** (indicative, not a guarantee).
- Assessment and reproduction within about **14 days**; if confirmed,
  fix prioritized by impact:
  - sandbox bypass / unverified execution: immediate fix;
  - partial weakenings or local DoS: fix in the next cycle.
- Coordinated disclosure: the fix ships before the public
  announcement; the reporter is credited (if they wish).

## Rules and expectations

- Send reports by email to the address above. No PGP key is published yet;
  until one exists, do not include working exploit code or secrets in the
  message and ask for a secure channel if details are sensitive.
- Panpak is a volunteer project. Times above are targets, not commitments.
- There is **no bug bounty** and no promise of payment or reward. Reporters
  are credited if they wish.
- Keep the report private until a fix is released or 90 days have passed,
  whichever comes first, unless we agree otherwise.

## Scope

In scope: `panpak`, `panpak-stub`, format/verify (spec
`PANPAK-FORMAT.md`), update/rollback/TOFU, Landlock rules,
build/release scripts. Out of scope: D-Bus/X11/Wayland/PulseAudio
(known surface documented in `THREAT-MODEL.md`, mitigated only by the
future broker), kernel and drivers, phishing/initial TOFU over a
hostile channel.
