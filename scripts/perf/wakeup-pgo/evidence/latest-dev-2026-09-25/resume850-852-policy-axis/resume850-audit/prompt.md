You are a read-only exploration subagent. Do not edit files, build, test,
use board sessions, or delegate. Worktree:
/home/zhourui/.codex/worktrees/03ae/tgoskits-dev at HEAD
69a33650763538692fafea27c869870ed0313642, dev 05175ca388.

Goal remains issue #2308: all frozen full20 p50 ratios >90% at matched
frequency, source-matched release p50/p99/p99.9 regressions <3%, three valid
candidate boots and wake correctness. Current G1/G2 uninstrumented full20 is
11/20, worst 58%; do not treat a diagnostic as acceptance. Read
/home/zhourui/.codex/artifacts/issue2308-perf/resume849-bitset-wake/decision.md
and protocol.md. Same static binary and Plus-2, unchanged G/Linux RT, three
valid process rounds each: empty/miss/hit p50 Starry 2042/2042/7000ns and
Linux 2333/2333/4084ns, every round. This only localizes excess to the
COMBINED hit-specific domain/batch/scheduler interval. It does not prove
activation alone costs 3207ns or that nonmatching bucket work costs zero.

Trace exact source for matched waiter only: VecDeque::remove, pending count,
mark_woken, UserTaskRef::into_wake_handle, ThreadWakeBatch push/pop/wake_all,
wake_thread_source, task/rq activation and commit. Compare correctness and
ownership requirements with Linux wake_q/futex only if source already in
local workspace; no web search needed. Inspect experiment-ledger/BY-DIRECTION
and relevant old attempts, especially resume604/605/742/848, exp47,
resume597/598. Do not repeat singleton fast path, fence weakening, wrapper
inline, entity snapshot, membership skip, or Fair tree micro-optimizations
without a genuinely new mechanism and evidence. Do not infer removable cost
from old instrumented means or a single leaf.

Deliver ONE: (A) a concrete source candidate with exact lines, removed work,
frequency, task/rq/waiter ownership and memory-order proof, negative cases,
and plausible native impact; (B) a new falsifiable minimal diagnostic that
separates hit-specific domain/batch from scheduler transaction more sharply
than resume849 and old broad probes, with validity criteria; or (C) a clear
closure if there is no remaining safe direction. Explicitly identify which
substeps current evidence can/cannot isolate. No PR action.
