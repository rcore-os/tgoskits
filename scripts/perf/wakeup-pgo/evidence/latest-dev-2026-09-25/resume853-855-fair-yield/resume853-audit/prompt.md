You are a read-only exploration subagent for TGOSKits issue #2308.

Workspace: /home/zhourui/.codex/worktrees/03ae/tgoskits-dev
Exact HEAD: 69a33650763538692fafea27c869870ed0313642
Objective: find a specific semantics-preserving transaction-level optimization candidate, or a narrowly falsifiable next diagnostic, for the unchanged-head same-CPU futex / yield handoff 90% deficit. Do not edit source or artifacts, run tests/builds/board tools, commit, or push. Read only.

First read these existing conclusions to avoid repetition:
- /home/zhourui/.codex/artifacts/issue2308-perf/audit850-hit-only/decision.md
- /home/zhourui/.codex/artifacts/issue2308-perf/resume852-policy-axis-settle/decision.md
- /home/zhourui/.codex/artifacts/issue2308-perf/experiment-ledger/attempts/resume805.md
- /home/zhourui/.codex/artifacts/issue2308-perf/experiment-ledger/attempts/resume807.md
- /home/zhourui/.codex/artifacts/issue2308-perf/experiment-ledger/attempts/resume628.md
- /home/zhourui/.codex/artifacts/issue2308-perf/experiment-ledger/attempts/resume630.md

Then trace exact current path from private FUTEX_WAIT/BITSET park preparation through schedule-out/switch-in and return to user for FIFO and OTHER same CPU. Include sender wake/preemption only where it affects that handoff. Identify whether state/accounting/locking work is duplicated and could safely be merged across the wake and schedule pass. Consider races (remote wake, signal, timeout, PI/policy changes, migration, ownership, interrupt) before proposing any skip or cached state. If no candidate with plausible microsecond-scale effect exists, say so plainly and propose one sharply scoped discriminator with predicted outcomes, not a broad probe.

Output concise evidence: source file:line references; mechanism and required invariants; old experiment IDs that overlap; estimated upper bound from existing measurements where available; exact A/B and correctness gates for a candidate. Distinguish inference from measurement. Do not claim diagnostic p50 as full20 benefit. Your work is an independent audit only; the main agent owns validation and integration.
