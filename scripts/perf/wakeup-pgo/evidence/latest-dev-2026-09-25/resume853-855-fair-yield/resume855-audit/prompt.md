You are a read-only exploration subagent for TGOSKits issue #2308.
Workspace: /home/zhourui/.codex/worktrees/03ae/tgoskits-dev
Exact HEAD: 69a33650763538692fafea27c869870ed0313642
Do not edit, build, test, run board commands, commit, or push.

New evidence: /home/zhourui/.codex/artifacts/issue2308-perf/resume854-yield-stages/decision.md and protocol.md. On one current-head qperf image, paired FIFO/OTHER yield handoff had six valid process rounds; OTHER-FIFO put-prev+pick stage means differed by 3156ns/event, versus 1432ns/event for rq commit+selection tail. These are probe-inclusive means, not native removable costs.

Task: audit whether a narrowly conditional, semantics-preserving Fair two-peer yield/current-switch path can materially reduce put-prev and pick work. Trace current source ownership and EEVDF selection. Compare old rejected global retained-node attempts /home/zhourui/.codex/artifacts/issue2308-perf/resume135-change.patch and resume136-change.patch, plus ledger attempts resume135/136/777 and the BY-DIRECTION.md no-repeat notes. Consider how to prove a two-peer fast path without changing yield fairness, lag, slice protection, membership, weighted sums, rq load publication, migration, PI, or higher-class selection. Distinguish an actual avoidable operation from merely moving it elsewhere or narrowing a tree search. Quantify plausible upper bound from existing stage/leaf data, with probe overhead caveat.

Deliver one of:
1. A concrete, minimal source change design with exact file:function references, preconditions, fallback, proof obligations, unit/system test locations, and a native A/B gate that could plausibly move the 90% target; OR
2. A clear no-go with the precise invariant or cost bound that rules it out, and one different next mechanism with source-based rationale.

The agent is an independent scout. The main agent owns any implementation, verification, and PR update. Do not claim 90% was achieved or that instrumented latency is production performance.
