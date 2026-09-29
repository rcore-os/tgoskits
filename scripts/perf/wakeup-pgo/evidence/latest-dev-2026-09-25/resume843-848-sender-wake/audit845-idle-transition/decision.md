# resume845: idle-to-ktimer transition audit

Read-only source audit at `69a33650763538692fafea27c869870ed0313642`,
base `dev@05175ca38823b631a73777b0130226ddfa558439`. The independent
exploration is in `subagent/final.txt`. No source, build, board, or native
performance result changed.

The post-WFI IRQ-return path remains nested under the idle preemption guards;
the scheduler cannot switch away from idle until the tick is restarted. The
later idle-to-non-idle restart is a race fallback and is a no-op after the
first restart. The clockevent firing transaction does not make a second
effective physical comparator commit. These observations do not identify a
removable multi-microsecond operation. The corresponding idle/IRQ/selection
machinery also appears in the FIFO timer path; only OTHER requires the
ParkSoft worker hop. Reclassifying OTHER timeouts as ParkHard violates the
PREEMPT_RT split and is not a candidate.

The agent's phrase "byte-identical" should be read only as *shared source
machinery*, not identical dynamic instructions or cost; policy-dependent work
and cache state can differ. Its five-stamp proposed diagnostic would be
instrumented and cannot count as native acceptance. Further timer idle-exit
probing needs new evidence of a policy-specific cost within that interval;
the existing generation-paired stages do not provide it. Close this subpath
for now rather than repeating a board trace without a falsifiable candidate.

The latest valid uninstrumented G1/G2 full20 remains 11/20 at 90%, worst
OTHER same-CPU futex 58.00%. Its OTHER timer p50 is 40625.5 ns versus Linux
RT 23920 ns; FIFO timer is 19875 ns versus Linux RT 13211 ns. The frozen
benchmark and same-frequency acceptance protocol were not rerun here.
