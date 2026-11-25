# 0003. `network = true` without ports means the full network

- Status: accepted (pull request 112, merged 2026-11-24)
- Date: 2026-11-24
- Pull requests: #112

## Context
The screen of permissions said, for `network = true` with no ports: "It can use the network. The ports are not listed by the package." The sandbox did something else: on a kernel with Landlock ABI 4 or more, with no port rule, all TCP connect and bind were denied; on an older kernel the app was refused (`E3000`). UDP and name resolution worked (tests/view.py). The converted apps of GIMP, Kdenlive and ONLYOFFICE declare `network = true` and no ports. (`panpak-stub/src/main.rs` network part; PANPAK-FORMAT.md §9; PILOT.md for the measure of `bind`.)

## Decision
`network = true` with no list of ports means the network without limits: no TCP filter and no refusal on old kernels. With a list of ports (`network_allow_tcp`, `network_bind_tcp`) the filter stays. Texts approved by the project owner: "Can use the network without limits." and, once, as its own line under the rows with ports, "Other network traffic, such as UDP, is not limited."

## Alternatives that were not chosen
Changing the screen to say that nothing is allowed: it would make the permission useless for apps that need any port (a browser, cloud connectors).

## Consequences
This widens the sandbox for apps that declared `network = true` without ports (a person who accepted "can use the network" now gets what the text says). The threat model and the specification say it.

The filter is applied by the stub of the installed runtime, so apps that are already installed with `network = true` and no ports get TCP as soon as the runtime is updated, with no new question: their signed manifest did not change. The project owner accepted this on 2026-11-24. Reason: the person agreed to "Use the network" and the sandbox now does what the screen promised; Panpak is before the beta and has no real users, so nobody holds an installation that relied on the old, stricter behaviour. An update of an app from ports to the whole network still asks for consent (tested). A person who wants ports only can use an app that lists them.

Rule for the future (a decision of the project owner, not yet in the specification or the threat model; it goes there before the beta): after the beta, a change of the runtime that widens what an already installed app can do has to be shown to the person at least (for example with a notification or in the log of updates), even when the declared permission does not change.

## Sources
PANPAK-FORMAT.md §3.1, §9; THREAT-MODEL.md; the pull request.
