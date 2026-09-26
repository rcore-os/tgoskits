# 2026-09-22: selective ax-task profile diagnostic

The source baseline remains dev@7fb26597a8; the archived ordinary A image
from 9d06d500 is not rerun at the user's request. Its measurements serve as
historical, cross-source comparison only, never same-source acceptance.

First candidate resume655 excluded ax-runtime from PGO: two complete B boots
still improved OTHER same-CPU futex p50 by 32.54%, but FIFO absolute-timer
p999 regressed 50.7% against four archived A boots. The candidate is rejected.

Next candidate: train/load the existing full-ten-feature profile from 9d06d500
only on crates other than ax-task (plus prebuilt sysroot); leave ax-runtime and
the root crate profiled. Because timer deadline queue and scheduling live in
ax-task, this tests whether their PGO code layout contributes to rare timer
tail stalls. A local build must have no CFG mismatch and must prove the root,
ax-task and ax-runtime profile flags before any board use. If it passes, run
exactly two independent B-only full20 boots on OrangePi-5-Plus-1, retain both
complete logs, and stop even if one fails. Evaluate focus p50 improvement
strictly >10% and every p50/p99/p999 regression <3% against the archived
A medians. Never convert this cross-source comparison into a production READY.
