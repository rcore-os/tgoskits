You are a read-only exploration subagent. Work in
`/home/zhourui/.codex/worktrees/03ae/tgoskits-dev` at exact HEAD
`b292a098bb60ef604e7677c37cd95d926ff08200` on dev 714accd8f6.
Do not edit files, build, test, run boards, submit PRs, or launch agents.

Goal: design the smallest reliable qperf-only, per-target paired diagnostic
for the COMMON same-CPU OTHER gate futex handoff, measuring (1) gate wake
publication -> target actual switch plan/low-level switch, and (2) switch
entry -> receiver first user-space timestamp, without changing the frozen
benchmark's native acceptance data. It is acceptable to conclude that
exact pairing requires a separate diagnostic benchmark or is infeasible
without excessive perturbation. In that case specify the smallest
falsifiable alternative and its limits.

Current evidence to incorporate:
- resume862 `/home/zhourui/.codex/artifacts/issue2308-perf/resume862-stage-probe/decision.md`:
  >=97.645% conservative off-rq target gate activations.
- resume864 `/home/zhourui/.codex/artifacts/issue2308-perf/resume864-wake-window/decision.md`:
  only 71-77 of about 21000 OTHER gate wake_batch windows have any global
  switch; dominant switch is after wake_batch.
- resume867 `/home/zhourui/.codex/artifacts/issue2308-perf/resume867-user-return-retry/decision.md`:
  valid OTHER qperf pending-observation mean ~179 ns plus unmask ~118-122 ns;
  one invalid round excluded. Do not infer native gain.
- resume865 `/home/zhourui/.codex/artifacts/issue2308-perf/resume865-lazy-handoff-audit/decision.md`:
  no safe multi-us shortcut found, runtime deadline omission unsafe.
- benchmark is `apps/starry/wakeup-latency-bench/handoff.c`; frozen binary
  SHA256 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

Read relevant producer, request/claim, schedule and user-entry code, plus
existing qperf instrumentation and historical attempts `resume801`, `resume854`
and `resume862` as needed. Specifically answer:
1. How to tag ONE gate wake with target thread ID and generation with no
   allocation, lock inversion, unsafe data race, or effect on production.
2. Exact source insertion points for producer publish, request claim,
   switch decision, architecture switch, and receiver user-return, and which
   points are actually observable. Distinguish switch-away and switch-in.
3. Buffer/slot ownership and memory ordering; how overwrites, competing
   switches, migration, spurious wakes, lost observations and background
   syscalls are counted. Do not silently select only successful pairs.
4. Whether A0 user wake timestamp and A5 receiver timestamp can be paired
   through the frozen binary, or require a separate diagnostic binary;
   say what this does to comparability.
5. A focused measurement protocol and pass/fail thresholds that would
   decide whether a multi-microsecond cost is in publish->claim,
   claim->switch, or switch->receiver return. qperf-only results are not
   full20 acceptance.
6. If code audit alone reveals a semantically equivalent multi-us candidate
   not already rejected in the ledger, cite exact source and invariant;
   otherwise say no candidate rather than speculate.

Return concise Chinese findings with exact file:line references, clear
uncertainty and a recommendation. Do not claim optimization benefit.
