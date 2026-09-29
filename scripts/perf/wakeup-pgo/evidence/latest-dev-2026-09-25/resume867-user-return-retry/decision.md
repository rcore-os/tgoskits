# resume867: user-return pending observation is not the large gap

The same qperf-only image as resume866 (SHA256
`54ee6a87398275a571f917ac55526947ea5eb2f440c75e6aad55fbf8af39537f`)
ran on OrangePi-5-Plus-1 session
`22efc5e2-3ea1-4e00-9efa-07369c8d3596`. The frozen benchmark SHA256
was `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.
The boot passed the expected PLL check and used eight CPUs. Switching the
guest's file-transfer endpoint to `192.168.1.2:2999` allowed the script
and benchmark to run; the board session was deleted and a subsequent GET
returned 404.

One boot ran OTHER/FIFO/OTHER/FIFO/OTHER as five independent processes.
The first OTHER round had 19998/20000 samples and `not_parked=2`; it is
invalid and excluded. The remaining four rounds each had 20000/20000
samples, `not_parked=0`, `missed_deadlines=0` and exit zero. Thus the
pre-registered five-valid-round protocol did **not** complete. The four
valid rounds may still be read individually as qperf-only diagnostic
evidence, not spliced into a full acceptance run.

| Round | Valid | Pending observations | Pending observe mean | Pending unmask mean | Clear observe mean |
| --- | --- | ---: | ---: | ---: | ---: |
| OTHER-1 | no: not_parked=2 | 21202 | 179.24 ns | 115.32 ns | 164.20 ns |
| FIFO-1 | yes | 3 | 291.33 ns | 97.33 ns | 163.34 ns |
| OTHER-2 | yes | 21208 | 178.69 ns | 122.00 ns | 166.96 ns |
| FIFO-2 | yes | 3 | 291.67 ns | 97.33 ns | 161.77 ns |
| OTHER-3 | yes | 21214 | 179.05 ns | 118.01 ns | 164.96 ns |

The counters are global and include background user returns. They are
probe-inclusive means, not paired per-benchmark-event timings or tail
latencies. The valid OTHER rounds have about 21.2k pending observations,
consistent with one Lazy request per benchmark handoff plus warmup and
background. The measured loop-entry-to-observation mean is about 179 ns,
and observation-to-schedule-entry (including IRQ unmask) about 118-122 ns.
Neither includes the duration of `schedule_current_cpu()` after entry.
These data do not support removing guard validation as a multi-microsecond
optimization. There is no native full20 gain or 90% acceptance claim.

`python3 check.py` independently verifies the raw serial marker, recorded image
SHA, probe-source and benchmark hashes, five process outputs, histogram/sample counts,
validity and before/after counter deltas. The temporary qperf source was
archived under resume866 and removed from the main worktree; the main
worktree remains clean. Next optimization work should inspect the actual
post-observation schedule and receiver resume transaction, while avoiding
unsafe IRQ or scheduler-request shortcutting.

In the PR archive, the 17 MB image is omitted. `check.py` verifies its
recorded digest and checks image bytes only if `../resume866-user-return/image.bin`
is supplied; it always verifies the archived probe-source tarball. The
archived board scripts are provenance from the local board service, not a
standalone replay command.
