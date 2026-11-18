# Incremental updates — 2026-10-26

**Update, 2026-11-18 (step 9, PR 1 and 2).** The package has changed in two places that an update downloads: the signature is now a
signature block of 72 bytes (a 64-byte signature in one entry of 8 bytes of header: **+8 bytes**), and the manifest carries the `[format]`
table (**+187 bytes** for QOwnNotes, where `critical` lists `abi.allow_missing`, `abi.glibc_min` and `abi.needs`; measured on a package made by
the current `panpak convert`). The same two payloads therefore take 5,280,528 + 8 + 187 = **5,280,723 bytes** to update (the full package,
67,613,228 + 195 = **67,613,423**), the saving stays 92.19 percent, and the analytic formula of `tests/incremental.py` is now
`80 + manifest + 72 + index + missing chunks`. These two numbers are computed from the table above and the sizes just given; they are **not
re-measured**: the QOwnNotes 26.10.8 payload is not kept, so the run below cannot be repeated on it. The test `tests/incremental.py` (in CI)
still asserts that the bytes the server sent equal the analytic number, on a synthetic pair. The table below is the measurement of 2026-10-26, as it was.

Reproducible command (rebuild musl release first):

```
python3 tests/incremental.py old.panpak new.panpak
```

Measured on **QOwnNotes 26.10.8 → 26.10.9** payloads, converted from the
previous pilot's official AppImages. The test only re-signs the metadata
(id/version/localhost URL/test key); original payload, index and stub stay
unchanged. Isolated store. The server counts the lengths of the bodies
actually sent, independently of the client counter.

| measure | bytes |
|---|---:|
| full new package | 67,613,228 |
| analytic: footer + manifest + signature + compressed index + missing chunks | 5,280,528 |
| actually downloaded | **5,280,528** |
| difference from analytic | **0** |

151 new chunks out of 3,195, 6 requests, identical stub reused. 92.19%
saving vs the full package. The old analytic measure in docs/BENCHMARKS-history.md
needlessly included the stub: here the identical signed stub is
reused. Comparison uses compressed/raw hashes and lengths.

Coalescing: adjacent/overlapping chunks in a single Range (4 MiB max);
intervals separated by already-present chunks stay separate, even with
small gaps, to avoid downloading available data. No multipart Range
needed. Different stub: fetch of the stub only, segmented.

Before the flip: remote signature verified, root/index, stub and
**all** assembled chunks, including reused ones. Errors do not change
`current`. Regressions: 200/no-Range, lying Content-Range, redirect
loop, slowloris, huge length, corrupt signature/index/chunks,
truncated body, damaged local chunk. Synthetic test with two
non-contiguous changes: 1,013 bytes out of 2,296,935, 6 requests;
analytic = actual.
