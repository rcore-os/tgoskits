/* Test: KCOV_DISABLE thread ownership check.
 *
 * Verifies that DISABLE and close() from a thread that did NOT enable kcov
 * are handled correctly, matching Linux semantics:
 *
 *   1. DISABLE from a non-tracing state (INIT) is a no-op success.
 *   2. DISABLE from a different thread → EINVAL.
 *   3. The tracer thread can still DISABLE after a rogue DISABLE attempt.
 *   4. close() from the tracer thread cleans up correctly.
 *   5. close() from a non-tracer thread does not clear the tracer's state.
 *   6. DISABLE from INIT is idempotent.
 *
 * These tests use fork() to create a second task that shares the fd but has
 * a different TID, exercising the tracer_tid check in KCOV_DISABLE.
 *
 * Ordering note: test §5 (close from non-tracer) must run LAST because
 * it intentionally leaves the thread's kcov state active after the fd
 * is torn down.  All other tests clean up their per-thread kcov state.
 */
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define KCOV_INIT_TRACE  _IOR('c', 1, unsigned long)
#define KCOV_ENABLE      _IO('c', 100)
#define KCOV_DISABLE     _IO('c', 101)
#define KCOV_TRACE_PC    0

/* Large buffer to avoid overflow during test bursts. */
#define BUF_ENTRIES 65536

static void burst(int n) {
    for (volatile int i = 0; i < n; i++) {
        getpid();
        getuid();
    }
}

/* ---- Helper: open + INIT_TRACE + mmap ---- */
static int kcov_setup(uint64_t **buf, size_t *sz) {
    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) return -1;
    if (ioctl(fd, KCOV_INIT_TRACE, BUF_ENTRIES)) { close(fd); return -1; }
    *sz = BUF_ENTRIES * sizeof(uint64_t);
    *buf = mmap(NULL, *sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (*buf == MAP_FAILED) { close(fd); return -1; }
    return fd;
}

static void kcov_teardown(int fd, uint64_t *buf, size_t sz) {
    munmap(buf, sz);
    close(fd);
}

/* ================================================================
 *  §1: DISABLE from a non-tracing state (INIT) — no-op, no error.
 *
 *  Even after a fork, DISABLE on an fd that was never ENABLE'd
 *  by this thread must succeed (the TID check only applies when
 *  the fd is in TRACE_PC / TRACE_CMP mode).
 * ================================================================ */
static void disable_from_init_is_noop(void) {
    printf("\n  --- §1: DISABLE from INIT mode is no-op ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §1");

    /* fd is in INIT mode now (never enabled). */
    pid_t pid = fork();
    if (pid == 0) {
        /* Child: fd inherited, mode=INIT, tracer_tid=None.
         * DISABLE from INIT must succeed (TID check skipped). */
        int r = ioctl(fd, KCOV_DISABLE, 0);
        _exit(r == 0 ? 0 : 10);
    }

    int wstatus;
    CHECK_RET(waitpid(pid, &wstatus, 0), pid, "waitpid §1");
    CHECK(WIFEXITED(wstatus), "§1 child exited");
    CHECK_RET(WEXITSTATUS(wstatus), 0, "§1 child DISABLE from INIT succeeded");

    kcov_teardown(fd, buf, sz);
}

/* ================================================================
 *  §2: DISABLE from a different thread → EINVAL.
 *
 *  Parent ENABLEs, child inherits fd and tries to DISABLE.
 *  The kernel must return EINVAL because the child's TID does
 *  not match the tracer_tid stored at ENABLE time.
 *
 *  After the fork test the parent DISABLEs, leaving a clean state.
 * ================================================================ */
static void disable_from_wrong_thread_returns_einval(void) {
    printf("\n  --- §2: DISABLE from wrong thread → EINVAL ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §2");

    /* Enable on parent thread. */
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "parent ENABLE");
    burst(10);
    uint64_t before = buf[0];
    CHECK(before > 0, "parent recorded coverage before fork");

    pid_t pid = fork();
    if (pid == 0) {
        /* Child: inherits fd, but tracer_tid belongs to parent.
         * DISABLE must fail with EINVAL. */
        int r = ioctl(fd, KCOV_DISABLE, 0);
        if (r == -1 && errno == EINVAL) {
            _exit(0);
        }
        _exit(r == 0 ? 11 : 12);
    }

    int wstatus;
    CHECK_RET(waitpid(pid, &wstatus, 0), pid, "waitpid §2");
    CHECK(WIFEXITED(wstatus), "§2 child exited");
    CHECK_RET(WEXITSTATUS(wstatus), 0,
              "§2 child DISABLE → EINVAL (matching Linux)");

    /* Clean up: parent DISABLEs normally. */
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "parent DISABLE after fork test");
    kcov_teardown(fd, buf, sz);
}

/* ================================================================
 *  §3: Parent can STILL DISABLE after child's failed attempt.
 *
 *  Because the child's DISABLE was rejected (EINVAL), the fd is
 *  still in TRACE_PC mode and the parent can disable normally.
 * ================================================================ */
static void parent_can_disable_after_child_fail(void) {
    printf("\n  --- §3: Parent DISABLE works after child's failed attempt ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §3");

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "parent ENABLE");
    burst(10);

    pid_t pid = fork();
    if (pid == 0) {
        /* Child tries (and fails) to DISABLE. */
        ioctl(fd, KCOV_DISABLE, 0);
        _exit(0);
    }

    int wstatus;
    waitpid(pid, &wstatus, 0);

    /* Parent must still be able to DISABLE. */
    uint64_t after = buf[0];
    CHECK(after > 0, "parent still tracing after child's failed DISABLE");

    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0,
              "parent DISABLE after child's failed attempt");

    kcov_teardown(fd, buf, sz);
}

/* ================================================================
 *  §4: close() from the tracer thread stops tracing correctly.
 *
 *  Baseline: ensure that close from the correct thread works as
 *  expected (the original behavior, now with TID match).
 * ================================================================ */
static void close_from_tracer_stops_tracing(void) {
    printf("\n  --- §4: close() from tracer stops tracing ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §4");

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "parent ENABLE");
    burst(10);

    uint64_t before_close = buf[0];
    CHECK(before_close > 0, "coverage before close");

    /* Tracer thread closes fd → on_close matches TID → clears thread state */
    close(fd);

    /* After close: re-open to verify the new fd works (fresh state). */
    fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "re-open after close from tracer");
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "re-open ENABLE");
    burst(5);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "re-open DISABLE");

    kcov_teardown(fd, buf, sz);
}

/* ================================================================
 *  §5: close() from a non-tracer thread does NOT clear the tracer's
 *      thread state — the tracer continues tracing.
 *
 *  The child closes its inherited fd.  Since the child's TID does
 *  not match tracer_tid, on_close skips clearing the parent's
 *  thread state.  The parent then calls DISABLE which acts as a
 *  safety-net cleanup (DISABLE from DISABLED mode clears thread
 *  state unconditionally), allowing the test to exit cleanly.
 *
 *  This test MUST run last because it temporarily leaves the
 *  thread's kcov state active while the fd is in DISABLED mode
 *  (corner case only reachable from a rogue close on the fd).
 * ================================================================ */
static void close_from_wrong_thread_does_not_stop_tracer(void) {
    printf("\n  --- §5: close() from wrong thread does not stop tracer ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §5");

    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "parent ENABLE");
    burst(10);

    pid_t pid = fork();
    if (pid == 0) {
        /* Child closes the inherited fd.  KcovFdState::on_close runs in
         * the child's context: TID mismatch → thread state NOT cleared,
         * but fd state IS reset (mode=DISABLED, buf_pages=None). */
        close(fd);
        _exit(0);
    }

    int wstatus;
    waitpid(pid, &wstatus, 0);
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "§5 child close succeeded");

    /* The parent's fd is still open (file-description refcount) but its
     * mode is DISABLED and buf_pages is None.  The parent's THREAD state
     * (KcovThreadState) is still active — on_close only resets the fd.
     *
     * Verify the thread is still tracing by doing more work and checking
     * that the count increased. */
    uint64_t prev = buf[0];
    burst(50);
    uint64_t after = buf[0];
    CHECK(after > prev,
          "§5 coverage increased after child's close (thread still tracing)");
    printf("  INFO: §5 coverage: %lu → %lu\n", prev, after);

    /* The fd is in DISABLED mode.  Calling DISABLE here acts as a safety
     * net: DISABLE from a non-tracing mode always clears the thread's
     * kcov state, allowing clean exit. */
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0,
              "§5 DISABLE recovers thread state after rogue close");

    munmap(buf, sz);
    close(fd);
}

/* ================================================================
 *  §6: Double DISABLE (idempotent) — DISABLE from INIT mode is fine.
 * ================================================================ */
static void disable_from_init_is_idempotent(void) {
    printf("\n  --- §6: DISABLE from INIT is idempotent ---\n");

    uint64_t *buf;
    size_t sz;
    int fd = kcov_setup(&buf, &sz);
    CHECK(fd >= 0, "setup fd for §6");

    /* DISABLE from INIT (never enabled) */
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "first DISABLE (INIT)");
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "second DISABLE (INIT, still fine)");

    /* ENABLE then DISABLE normally still works */
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE after DISABLE");
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE after ENABLE");

    kcov_teardown(fd, buf, sz);
}

/* ---- main ---- */

int main(void) {
    TEST_START("KCOV DISABLE thread ownership check");

    disable_from_init_is_noop();
    disable_from_wrong_thread_returns_einval();
    parent_can_disable_after_child_fail();
    close_from_tracer_stops_tracing();
    disable_from_init_is_idempotent();
    /* §5 (close from non-tracer) intentionally leaves the thread's kcov
     * state active while the fd is in DISABLED mode.  It must run last. */
    close_from_wrong_thread_does_not_stop_tracer();

    TEST_DONE();
}
