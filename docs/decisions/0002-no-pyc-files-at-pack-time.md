# 0002. The `.pyc` files are not made by `pack`

- Status: accepted
- Date: 2026-11-24
- Pull requests: #102 (added), #105 (removed)

## Context
`pack` ran the Python of a package in a sandbox to make `__pycache__` files, in the hope of a faster first start of apps with their own Python (GIMP). Measured on the VM (Silverblue, external SSD with TRIM, 2026-11-23, GIMP 3.2.8, series of three): the first start took 15.7 s with the `.pyc` files and 15.9 s without. A Python plug-in started 0.15 to 0.2 s faster in isolation (0.10 s against 0.25 to 0.34 s). The package was 15 MB larger (+5.4% at level 3) and took longer to make (6.0 s to 7.5 s).

## Decision
`pack`, `convert` and `build` do not run the Python of a package. A package that carries its own `.pyc` files keeps them: they are files of the package.

## Alternatives that were not chosen
Keeping the precompilation: it adds code and a sandbox that runs a package's Python on the machine of whoever packs, for a gain that does not show in the first start.

## Consequences
The paragraph of the specification about it and the matching entry of the threat model are removed. If a later measure shows a real gain, the question can be opened again.

## Sources
Pull requests #102 and #105; CHANGELOG.md (the entry of the removal).
