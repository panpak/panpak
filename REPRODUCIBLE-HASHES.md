# Reproducible build hashes (tracked reference)

Produced by `scripts/repro-build.sh` (double-build with pinned toolchain:
`rust-toolchain.toml` 1.99.1, musl.cc verified via SHA-256,
`SOURCE_DATE_EPOCH=1700000000`, path remapping) and confirmed
bit-identical between host and clean container (`Dockerfile.repro`),
check of 2026-10-27. The container's toolchain-install layers were
cached; all four Rust builds ran in fresh source/target directories.

Hashes changed vs the previous reference (stub 84ed4722..., CLI 7fd79a43...)
because of the release-preparation changes: stable `PANPAK-Exxxx` error codes
and secret-file handling (new code and strings), and the SPDX header line on
every source file (shifts line numbers embedded in panic locations). The CLI
also embeds NOTICE, the license texts and THIRD-PARTY-LICENSES. No change to
the file format or the security rules. Check of 2026-10-27: host double-build
and clean-container (`Dockerfile.repro`) double-build both IDENTICAL and equal
to each other.

Updated on 2026-10-28 (stub 2f374281..., CLI 713f0f7b... before): the only
source change since then is renaming Italian test functions to English. Test
code is not compiled into the release binaries, but rustc's LLVM symbol
suffixes (e.g. `anon.<hash>.0.llvm.<n>`) derive from a hash of the crate
sources, so any source text change alters the binaries; the previous commit
was rebuilt and still gives the old hashes. Host and clean-container
double-builds are again both IDENTICAL and equal to each other.

Updated again after the zstd 0.13 -> 0.14.3 / zstd-safe 7.3.0 -> 8.2.0
update (RUSTSEC-2026-0391, -0398, -0403; previous stub 69987ba6..., CLI
15ef6574...). The linked zstd code and the compressed output are unchanged
in behaviour (a package packed with the old and the new CLI is byte-identical),
but the binaries differ because the dependency versions and the sources
changed. The CLI also links the zstd 0.13 copy that backhand still needs.
Host and clean-container double-builds are both IDENTICAL and equal.

Updated again (stub d479e99b..., CLI e480f0c2... before) for the English
runtime messages, the English test names and sample data, the `<package>`
help texts and the new `ext_alias` test. No change to the file format or the
security rules. Host and clean-container double-builds are again both
IDENTICAL and equal to each other.

Updated again (stub 15da43b7..., CLI unchanged) for the rename of the
footer magic test and its comments in `panpak-core`. Only comments, a test name
and test data changed: a change limited to comments or tests can still change
the hashes. Host and clean-container double-builds are both IDENTICAL and
equal to each other.

```
3dfa9a5c7d78d929f3dd3adaa53eb3469220bab4473a5102718c952b1cbd32ec  panpak-stub
0475f2ac7554cd392af19ae49bc0ef0775c05ae71863740cc495854f1740b397  panpak
```

Check: the `repro` job in `.github/workflows/ci.yml` re-runs the
double-build and compares the hashes obtained with these lines
(`diff`). If sources change legitimately, regenerate the double-build,
verify host-vs-container and update THIS file in the same commit.
