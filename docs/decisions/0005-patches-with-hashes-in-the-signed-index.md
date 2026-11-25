# 0005. Patches: the hash of each patch is in the signed index

- Status: accepted
- Date: 2026-11-23
- Pull requests: #96

## Context
When every chunk of a large file changed (a program built again), an update downloaded the file whole. Measured on the host (2026-11-23; external SSD, level 19, loopback server): Kdenlive 26.08.2 to 26.08.3 needed 112.2 MB with chunks and 45.8 MB with patches; ONLYOFFICE 9.4.0 to 10.0.0 115.9 MB and 64.4 MB.

## Decision
For such a file the update downloads a zstd frame that uses the installed file as its dictionary. The index (signed) holds, for each such file, the hash of the base (the previous release's file) and the offset, length and hash of the patch. The patches are in a file `<package>.patches` beside the package, with no index and no signature of its own. The client checks the patch hash before the decompressor sees a byte, bounds the window (2^28) and the output, and checks the result chunk by chunk; anything wrong falls back to the chunks.

## Alternatives that were not chosen
A list of patches that nobody signs (a server could swap or add patches), and a separate signed list.

## Consequences
A server can keep patches back but not change them. The decompressor reads bytes from the network with a second input: THREAT-MODEL.md §4 says it. The `.patches` file must be hosted beside the package.

## Sources
PANPAK-FORMAT.md §4, §4.1; THREAT-MODEL.md §4 (patches); pull request #96.
