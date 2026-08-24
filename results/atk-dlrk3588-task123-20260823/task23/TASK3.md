# Task 3 physical-board completion report

> This is the compact repository snapshot. Generated images are identified by
> the parent `artifact-hashes.txt` and remain in the authenticated full archive.

Status: **PASS** for real ncnn/YOLO inference and the observable closed loop on
ATK-DLRK3588.

## Real in-Guest model

StarryOS ran the AArch64 `task2-net` application and ncnn inside VM1. The
runtime marker pins:

- ncnn revision: `946fe3fb14a8dff8c06df763f67be522167b2f00`
- parameter SHA256: `d2c0adf8939dc9ce02964ce8ada104447768ffd8e3bffad8fa11e2e61e709c1f`
- weight SHA256: `0ae562447923999779b12b4f91f96b9ef263add8c9902d10e22e6dd6a2932c12`
- input SHA256: `608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f`
- input path: `/usr/share/task3-yolo`
- preprocessing: RGB resize to 640x640, normalize by 1/255
- inference threads: 1

The three assets extracted directly from the archived 27,400,910-byte initrd
match these hashes and the host-side pinned assets. The initrd SHA256 is
`e61b388e1abbe872ab401ec305329ae144c2c5660bcc664a180c21ab9f67e5b8`;
`gzip -t` passed on the live Guest. The independent host extraction and hash
comparison is retained in `task3-asset-hashes.log`.

## Detection and safety policy

Every retained inference produced the same pinned detection:

```text
class=75 confidence_milli=843 center_x_milli=421 area_milli=63 target=421
```

The deployed policy was:

```text
min_confidence_milli=600 min_area_milli=10 max_target_step=100
```

Thus confidence and area were checked before control, and the accepted target
was bounded to a maximum step of 100 rather than applied without validation.
The 11/11 `task3-model` tests include low-confidence/small-area rejection,
malformed/non-finite output rejection, range validation, and step clamping.

## Closed-loop result

The first uninterrupted FIFO run completed 137 matched inference, CONTROL,
ACK, and STATUS events in 329.188 seconds with no business-path ESR or host IRQ
26 error. CONTROL-to-STATUS RTT was:

| metric | ms |
|---|---:|
| min | 45 |
| p50 | 243 |
| p90 | 250 |
| p95 | 252 |
| p99 | 261 |
| max | 278 |
| mean | 239.78 |

The independent short pcap proves 17 CONTROL and 17 STATUS frames at both
switch boundaries. In the later blackout log, 29 additional complete status
cycles have min/median/mean/max RTT 235/248/246.79/256 ms.

After a deliberate network blackout, inference resumed without restarting the
Guest. Request 165 produced a fresh detection and CONTROL after recovery,
received ACK, and completed STATUS in 236 ms; at least 12 additional StarryOS
closed-loop completions are retained after `virtnet drop off`.

The repository's exhaustive acceptance set remains
`results/starryos-task123-final-db42f6168-20260822/`: it contains the
fixed-parameter/model comparison and model-rejection scenario. This physical
addendum supplies the previously missing real-board execution proof without
replacing those controlled comparison results.

## Reproduction and limits

This is physical-board implementation/liveness and observed latency evidence,
not a hard WCET proof. The exact RAM-only FIT, VM configs, model-bearing initrd
identity, logs, pcap files, and verifier/test outputs are authenticated by
`MANIFEST.sha256` in the full external archive and by the snapshot-root
`SHA256SUMS.txt` in the compact repository copy.
