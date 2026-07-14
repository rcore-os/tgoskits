#!/bin/sh
# Non-aarch64 skip stub for perf-tool-smoke.
#
# The upstream `perf` test asset is a static aarch64 musl ELF, so it can only run
# on aarch64 targets. On every other architecture the CMakeLists installs THIS
# stub (renamed perf-tool-smoke) instead of the real binary + runner, so the
# grouped system runner records an explicit, visible SKIP — never silently
# vanishing and never SIGSEGV-ing a foreign ELF. Exits 0 (a legitimate N/A).
echo "PERF_SMOKE_BEGIN"
echo "PERF_SMOKE_SKIPPED: upstream perf is an aarch64-only binary; not applicable to this target arch"
echo "PERF_SMOKE_END"
exit 0
