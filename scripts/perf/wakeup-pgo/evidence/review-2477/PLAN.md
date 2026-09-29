# #2477 tail-latency replication, 2026-09-22

The first complete, valid four-boot block (`/tmp/review2477-board-retry/`,
`resume653`) failed the fixed p999 guardrail for FIFO absolute timer: A median
29500 ns, B median 32812 ns, a regression of 11.23%. The earlier
`/tmp/review2477-board/` block is invalid because its A2 run had one
`not_parked` sample; it must not be included.

Collect exactly one more independent A1-B1-B2-A2 block on the same board,
using identical images, profile, benchmark and per-run validity gates. Stop
after that block even if it fails. Report each block's original verdict.
Additionally calculate a joint comparison using all eight valid boots (four
A, four B): for every scenario take the median of each side's four p50, p99
and p999 values and apply the unchanged >=10% improvement and <=3% regression
limits. Any invalid new run makes the joint comparison inconclusive. The joint
result is an exploratory stability check, not a retroactive PASS for the
first failed block.
