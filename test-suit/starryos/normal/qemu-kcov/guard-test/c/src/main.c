/*
 * Regression test: guard address register clobbered after call.
 *
 * ## The bug
 *
 * Each architecture's naked assembly trampoline in `__sanitizer_cov_trace_pc`
 * (os/StarryOS/kernel/src/kcov/mod.rs) must:
 *   1. check and set   IN_KCOV_TRACE  (recursion guard)
 *   2. call            kcov_trace_pc_impl(pc)
 *   3. clear           IN_KCOV_TRACE
 *
 * On aarch64, riscv64, and loongarch64 the guard address was loaded into a
 * caller-saved register (x16 / t0 / $t0) before step 2, then used again
 * *after* step 2 to clear the guard.  However, the C-ABI call in step 2
 * may clobber that register — the store in step 3 then writes 0 to a
 * garbage address instead of clearing IN_KCOV_TRACE.
 *
 * Effect: IN_KCOV_TRACE stays 1 permanently.  After the very first traced
 * basic block, every subsequent __sanitizer_cov_trace_pc invocation skips
 * the trace handler — coverage recording stops.  Additionally, a random
 * byte of memory is corrupted by the spurious write.
 *
 * x86_64 was NOT affected because it uses RIP-relative addressing (the
 * address is recomputed from the instruction pointer each time, not cached
 * in a register).
 *
 * ## The fix
 *
 * Re-derive the guard address after the call, before clearing the flag:
 *
 *   aarch64:     adrp x16, {guard}    // added after "bl {impl}"
 *   riscv64:     la   t0,  {guard}    // added after "call {impl}"
 *   loongarch64: la.local $t0, {guard} // added after "bl {impl}"
 *
 * ## How this test catches the bug
 *
 * Staircase pattern — 10 enable→burst→disable cycles.  Each cycle calls
 * a small, fixed set of syscalls, then reads the coverage count from the
 * mmap'd buffer and asserts it is *strictly greater* than the previous
 * cycle's count.
 *
 *   Cycle 1:  enable → syscalls → disable  →  count = N₁  (N₁ > 0)  ✓
 *   Cycle 2:  enable → syscalls → disable  →  count = N₂
 *                    assert N₂ > N₁   ← catches the stall
 *
 * If the guard was not cleared, cycle 2 records zero new edges, N₂ = N₁,
 * and the assertion fails.
 *
 * A larger buffer size (51200 entries) is used to ensure we never hit the
 * "buffer full" path, which would also cause the count to stall.
 */

#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_TRACE_PC 0

#define STAIR_STEPS 10

/* A modest burst: enough syscalls to generate multiple trace edges per
 * step but not so many that we risk filling the buffer. */
static void burst(int n) {
	for (volatile int i = 0; i < n; i++) {
		getpid();
		getuid();
		getppid();
	}
}

int main(void) {
	TEST_START("KCOV guard: staircase coverage increment");

	int fd = open("/dev/kcov", O_RDWR);
	CHECK(fd >= 0, "open /dev/kcov");
	if (fd < 0) {
		TEST_DONE();
	}

	CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 51200), 0,
		  "KCOV_INIT_TRACE size=51200");
	size_t sz = 51200 * sizeof(uint64_t);
	uint64_t *buf =
	    mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	CHECK_PTR(buf, 1, "mmap kcov buffer");
	if (buf == MAP_FAILED) {
		close(fd);
		TEST_DONE();
	}

	uint64_t prev = buf[0];
	CHECK(prev == 0, "initial count is 0");

	/*
	 * Staircase loop — each iteration must record new coverage edges.
	 *
	 * Per-step flow (matching the buggy code path in kcov/mod.rs):
	 *   KCOV_ENABLE        → sets mode = KCOV_TRACE_PC in thread state
	 *   syscalls (burst)   → each triggers __sanitizer_cov_trace_pc
	 *                          → naked trampoline sets/clears IN_KCOV_TRACE
	 *                          → kcov_trace_pc_impl records the PC
	 *   KCOV_DISABLE       → sets mode = KCOV_MODE_DISABLED
	 *   read buf[0]        → coverage count from the shared buffer
	 *
	 * If the trampoline's guard-clear step writes to a garbage address
	 * (the bug), IN_KCOV_TRACE remains 1, every subsequent trampoline
	 * invocation takes the "skip" branch, and curr == prev.
	 */
	for (int step = 1; step <= STAIR_STEPS; step++) {
		CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0,
			  "KCOV_ENABLE");

		/*
		 * Each step does `step` iterations of getpid/getuid/getppid
		 * so the amount of work monotonically increases.  If the guard
		 * is working correctly each step adds non-zero coverage.
		 */
		burst(step);
		CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "KCOV_DISABLE");

		uint64_t curr = buf[0];
		printf("  STAIR step %d: count %lu → %lu (delta %lu)\n", step,
		       prev, curr, curr - prev);

		/*
		 * Core assertion: coverage must grow across cycles.
		 * A failed assertion here (on any step ≥ 2) is the signal
		 * that the guard-address-register bug has regressed.
		 */
		CHECK(curr > prev, "coverage count increased this step");
		if (curr <= prev) {
			printf("  FAIL: guard likely not cleared — trace "
			       "stalled at step %d\n",
			       step);
		}

		prev = curr;
	}

	munmap(buf, sz);
	close(fd);
	TEST_DONE();
}
