# resume843: current G ELF memset reachability audit

This is a read-only static audit of source `69a33650763538692fafea27c869870ed0313642`
and G ELF SHA-256 `31313ba5988b7aa21165d97907435807eb29db7e406a81613ae43185e1599bac`
(.bin SHA-256 `9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`).
No source, image or board state was changed.

The AArch64 naked `memset` is a byte loop at `ffffffff8007e2f8`, needed so
profile-generation images can clear early memory without touching unmapped
LLVM counters. The uninstrumented G ELF has many global callers, but a direct
callsite count is not a hotness measurement. `ResolvedFutex::wake`,
`wake_thread_source`, `activate_waking_thread_locked`,
`ThreadWakeBatch::wake_all`, `execute_switch_plan` and the runtime
`switch_context` entry contain no direct call to it. The calls found in
`finish_owner_selection` (`ffffffff800c6080`) and preemption scheduling
(`ffffffff800d3f0c`) are in the idle-pull preparation branch: they clear a
`CpuSet` of one 64-bit word on the eight-CPU board. The measured same-CPU
futex handoff selects a runnable receiver, not idle. The sender's preparatory
`sched_yield` is outside the timing window.

The subagent report maps 672 static direct callsites, including large
allocation/VM paths, but cannot prove every inlined arm of the large syscall
dispatcher unreachable without runtime evidence. Its 2.4 GHz cycle estimate
is inapplicable to this cpufreq-off ~816 MHz experiment and is not used for
this decision. Even at 816 MHz, one 8-byte clear cannot account for a
multi-microsecond p50 gap; there is no source- or ELF-backed hot, multi-KB
clear in the futex/yield handoff. The 8-byte idle branch also cannot explain
the OTHER timer deficit. This is a no-go for a `memset` optimization as the
next 90% candidate, not a claim that `memset` has zero cost globally.

If later evidence contradicts the static reachability conclusion, use a
bounded, diagnostic-only callsite/length sample tied to the measured window;
do not treat a global count or instrumented p50 as acceptance. Full details
and source addresses are in `subagent/final.txt`.
