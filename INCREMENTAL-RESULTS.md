# Incremental updates — 2026-10-26

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
saving vs the full package. The old analytic measure in BENCHMARKS.md
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
