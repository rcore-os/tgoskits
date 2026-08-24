# Official `dev` equivalence audit

Audited checkout: `/home/huhu/tgoskits-official-dev`

* branch: `dev`
* HEAD: `8e39cbd586a4a34ab9f522931ca4b1e7523709c7`
* pre-existing dirty files: `virtualization/axvm/src/arch/aarch64/vm.rs`,
  `virtualization/axvm/src/machine/mod.rs`, and
  `virtualization/axvm/src/machine/timer.rs`
* dirty diff stat: 3 files, 56 insertions, 12 deletions

The dirty change is a guest timer-frequency selection/override compatibility
patch. It does not add the Task 1 scheduler or diagnostics. This audit did not
overwrite or clean it.

## Missing equivalence requirements

At this HEAD, AxVisor's feature list has no `rr-scheduler`,
`fp-rr-scheduler`, or `sched-prio-rr`. The tree contains ArceOS's generic
`sched-rr`, but AxVisor does not expose the pair of host scheduler policies
required for a single-variable Task 1 A/B. It also lacks Task 1's
`host_sched_priority` path, FP-RR service counters, `rt stat`, the expanded
`vmexit stat`, and `scripts/test/rt-partition` runners.

Official assets do include an eight-vCPU RK3588 Linux placeholder and a
four-vCPU Orange Pi 5 Plus Zephyr guest placeholder. They are not an equivalent
paired workload: there is no ATK-DLRK3588 configuration combining the same
2-vCPU Linux workload and RTOS probe, priorities, core placement,
instrumentation, and two scheduler arms.

## Verdict

An unmodified official-`dev` physical A/B is NOT COMPARABLE, not a failed run.
Porting the experimental schedulers, priority plumbing, commands and workload
would make it comparable, but it would then cease to be unmodified official
`dev`. The archived official Cargo/config snapshots permit independent review
of this conclusion.

