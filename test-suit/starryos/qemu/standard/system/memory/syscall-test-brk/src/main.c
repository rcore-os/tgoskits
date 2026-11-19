/*
 * test_brk.c - Test cases for brk(2) syscall
 *
 * Covers raw syscall semantics and libc wrapper behavior.
 * Uses CHECK macros (not assert) to work correctly in Release builds.
 *
 * Note: Static musl binaries may reject brk() with ENOMEM (known quirk).
 * We follow the pattern from linux-compatible-testsuit/tests/test_brk.c:
 * - Allow brk() to fail with ENOMEM for static musl
 * - But verify sbrk(0) confirms break unchanged
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE 1
#endif

#include "test_framework.h"
#include <limits.h>
#include <stdint.h>
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/resource.h>
#include <sys/wait.h>

/* ============================================================
 * RAW SYSCALL TESTS - Verify Linux brk syscall ABI directly
 * ============================================================ */

static void test_raw_brk_query(void)
{
    unsigned long break1 = syscall(SYS_brk, 0);
    CHECK(break1 > 0, "raw brk(0) returns valid address");

    unsigned long break2 = syscall(SYS_brk, 0);
    CHECK(break2 == break1, "raw brk(0) returns consistent address");

    printf("  raw brk(0) = 0x%lx\n", break1);
}

static void test_raw_brk_expand_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for expand test");

    unsigned long new_addr = current + 4096;
    unsigned long ret = syscall(SYS_brk, new_addr);

    /* MUST return new_addr on success */
    CHECK(ret == new_addr, "raw brk(expand) returns new address");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == new_addr, "raw brk(0) confirms new break");

    /* Write to memory */
    memset((void *)current, 0xAA, 4096);
    CHECK(((unsigned char *)current)[0] == 0xAA, "memory write/read succeeds");

    /* Restore */
    syscall(SYS_brk, current);

    after = syscall(SYS_brk, 0);
    CHECK(after == current, "raw brk(shrink) restores original break");
}

static void test_raw_brk_shrink_success(void)
{
    unsigned long current = syscall(SYS_brk, 0);

    unsigned long expanded = current + 8192;
    unsigned long ret = syscall(SYS_brk, expanded);
    CHECK(ret == expanded, "raw brk(expand 8K) returns new address");

    ret = syscall(SYS_brk, current);
    CHECK(ret == current, "raw brk(shrink) returns original address");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "raw brk(0) confirms shrink");
}

static void test_raw_brk_failure(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for failure test");

    unsigned long absurd = 1UL << 50;
    errno = 0;
    unsigned long ret = syscall(SYS_brk, absurd);

    CHECK(ret == current, "raw brk(absurd) returns current break");
    CHECK(errno == 0, "raw brk(absurd) does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after failure");
}

static void test_raw_brk_below_base(void)
{
    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for below-base test");

    errno = 0;
    unsigned long ret = syscall(SYS_brk, 0x1000);

    CHECK(ret == current, "raw brk(below base) returns current break");
    CHECK(errno == 0, "raw brk(below base) does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after below-base attempt");
}

static void test_raw_brk_roundtrip(void)
{
    unsigned long base = syscall(SYS_brk, 0);
    CHECK(base > 0, "get base for roundtrip test");

    /* Expand by multiple pages */
    unsigned long expanded1 = syscall(SYS_brk, base + 4096);
    CHECK(expanded1 == base + 4096, "raw brk(expand 4K) succeeds");

    unsigned long expanded2 = syscall(SYS_brk, base + 8192);
    CHECK(expanded2 == base + 8192, "raw brk(expand 8K) succeeds");

    /* Write to memory */
    memset((void *)base, 0xDD, 8192);
    CHECK(((unsigned char *)base)[0] == 0xDD, "memory write succeeds");
    CHECK(((unsigned char *)base)[4095] == 0xDD, "page 1 read succeeds");
    CHECK(((unsigned char *)base)[8191] == 0xDD, "page 2 read succeeds");

    /* Shrink back to base */
    unsigned long shrunk = syscall(SYS_brk, base);
    CHECK(shrunk == base, "raw brk(shrink to base) succeeds");

    unsigned long final = syscall(SYS_brk, 0);
    CHECK(final == base, "raw brk(0) confirms base restored");
}

struct clone_vm_brk_args {
    int result_fd;
    unsigned long target;
};

static int clone_vm_brk_child(void *opaque)
{
    struct clone_vm_brk_args *args = opaque;
    unsigned long result = syscall(SYS_brk, args->target);
    unsigned char success = result == args->target;
    (void)write(args->result_fd, &success, sizeof(success));
    return success ? 0 : 1;
}

static void test_clone_vm_shares_brk_state(void)
{
    enum { CLONE_STACK_SIZE = 64 * 1024 };
    unsigned long original = syscall(SYS_brk, 0);
    CHECK(original > 0, "get current break for CLONE_VM test");

    void *stack = mmap(NULL, CLONE_STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(stack != MAP_FAILED, "allocate CLONE_VM child stack");
    if (stack == MAP_FAILED) {
        return;
    }

    int result_pipe[2];
    int pipe_result = pipe(result_pipe);
    CHECK(pipe_result == 0, "create CLONE_VM result pipe");
    if (pipe_result != 0) {
        munmap(stack, CLONE_STACK_SIZE);
        return;
    }

    struct clone_vm_brk_args args = {
        .result_fd = result_pipe[1],
        .target = original + 4096,
    };
    pid_t child = clone(clone_vm_brk_child,
                        (char *)stack + CLONE_STACK_SIZE,
                        CLONE_VM | SIGCHLD, &args);
    CHECK(child > 0, "clone(CLONE_VM|SIGCHLD) succeeds");
    close(result_pipe[1]);

    unsigned char child_success = 0;
    ssize_t received = read(result_pipe[0], &child_success,
                            sizeof(child_success));
    close(result_pipe[0]);
    int status = 0;
    if (child > 0) {
        CHECK(waitpid(child, &status, 0) == child,
              "wait for CLONE_VM child");
    }
    CHECK(received == (ssize_t)sizeof(child_success) && child_success == 1,
          "CLONE_VM child publishes requested break");
    CHECK(child > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "CLONE_VM child exits successfully");

    unsigned long observed = syscall(SYS_brk, 0);
    CHECK(observed == args.target,
          "parent observes brk stored in shared MM, not process-local mirror");
    CHECK(syscall(SYS_brk, original) == original,
          "restore break after CLONE_VM sharing test");
    munmap(stack, CLONE_STACK_SIZE);
}

/* ============================================================
 * LIBC WRAPPER TESTS - Test brk/sbrk via libc functions
 *
 * Note: Static musl binaries may reject brk() with ENOMEM.
 * We follow the pattern from linux-compatible-testsuit/test_brk.c:
 * - Allow brk() to fail with ENOMEM
 * - But verify sbrk(0) confirms break unchanged
 * ============================================================ */

static void test_libc_brk_current_break(void)
{
    void *cur = sbrk(0);
    CHECK(cur != (void *)-1, "sbrk(0) returns valid address");

    errno = 0;
    int ret = brk(cur);
    if (ret == 0) {
        CHECK(sbrk(0) == cur, "brk(current) succeeded, break unchanged");
    } else {
        /* Static musl may reject no-op brk() with ENOMEM */
        CHECK(ret == -1 && errno == ENOMEM, "brk(current) failed with ENOMEM (static musl quirk)");
        CHECK(sbrk(0) == cur, "break unchanged despite brk() failure");
    }
}

static void test_libc_sbrk_zero(void)
{
    void *p1 = sbrk(0);
    CHECK(p1 != (void *)-1, "sbrk(0) first call succeeds");

    void *p2 = sbrk(0);
    CHECK(p2 != (void *)-1, "sbrk(0) second call succeeds");

    CHECK(p1 == p2, "two consecutive sbrk(0) return same value");
}

static void test_libc_sbrk_allocate(void)
{
    void *old_break = sbrk(0);
    CHECK(old_break != (void *)-1, "sbrk(0) returns current break");

    errno = 0;
    void *returned = sbrk(4096);
    if (returned == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(+4K) failed with ENOMEM");
        return;
    }

    CHECK(returned == old_break, "sbrk(+4K) returns old break (success)");

    void *new_break = sbrk(0);
    CHECK(new_break == (char *)old_break + 4096, "sbrk(0) confirms expansion");

    /* Write to allocated memory */
    memset(returned, 0x55, 4096);
    CHECK(((unsigned char *)returned)[0] == 0x55, "memory write succeeds");

    /* Restore */
    errno = 0;
    void *after_free = sbrk(-4096);
    CHECK(after_free != (void *)-1, "sbrk(-4K) succeeds");

    CHECK(sbrk(0) == old_break, "break restored to original");
}

static void test_libc_sbrk_sequential(void)
{
    void *base = sbrk(0);
    CHECK(base != (void *)-1, "sbrk(0) returns base");

    errno = 0;
    void *p1 = sbrk(4096);
    if (p1 == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(+4K) failed with ENOMEM");
        return;
    }
    CHECK(p1 == base, "first sbrk(+4K) returns base");

    errno = 0;
    void *p2 = sbrk(4096);
    if (p2 == (void *)-1) {
        CHECK(errno == ENOMEM, "second sbrk(+4K) failed with ENOMEM");
        brk(base);
        return;
    }
    CHECK(p2 == (char *)base + 4096, "second sbrk(+4K) returns base+4K");

    errno = 0;
    void *p3 = sbrk(4096);
    if (p3 == (void *)-1) {
        CHECK(errno == ENOMEM, "third sbrk(+4K) failed with ENOMEM");
        brk(base);
        return;
    }
    CHECK(p3 == (char *)base + 2 * 4096, "third sbrk(+4K) returns base+8K");

    /* Write across full 12K region */
    memset(base, 0xBB, 3 * 4096);
    CHECK(((unsigned char *)base)[0] == 0xBB, "memory write succeeds");

    /* Restore */
    errno = 0;
    int ret = brk(base);
    CHECK(ret == 0, "brk(base) restores original");
}

static void test_libc_sbrk_huge_negative(void)
{
    void *before = sbrk(0);
    CHECK(before != (void *)-1, "sbrk(0) returns current break");

    errno = 0;
    void *after = sbrk(-((intptr_t)1 << 40));

    if (after == (void *)-1) {
        CHECK(errno == ENOMEM, "sbrk(huge negative) returns -1 with ENOMEM");
    } else {
        /* Some implementations clamp; verify break stays sane */
        void *current = sbrk(0);
        CHECK(current != (void *)-1, "sbrk(0) still valid");
        CHECK((uintptr_t)current <= (uintptr_t)before, "break not increased");
    }
}

static void test_libc_brk_enomem_huge(void)
{
    void *absurd = (void *)(1UL << 50);

    errno = 0;
    int ret = brk(absurd);
    CHECK(ret == -1, "brk(absurd address) returns -1");
    CHECK(errno == ENOMEM, "brk(absurd address) sets ENOMEM");

    /* Break unchanged */
    void *current = sbrk(0);
    CHECK(current != (void *)-1, "sbrk(0) still valid");
    CHECK(current != absurd, "break not moved to absurd address");
}

/* ============================================================
 * RLIMIT_DATA TESTS
 * ============================================================ */

static void test_raw_brk_rlimit_data(void)
{
    struct rlimit old_rlim;
    CHECK(getrlimit(RLIMIT_DATA, &old_rlim) == 0, "getrlimit(RLIMIT_DATA) succeeds");

    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current > 0, "get current break for rlimit test");

    /* Set tight limit: current + 4K */
    struct rlimit new_rlim = {
        .rlim_cur = 4096,
        .rlim_max = old_rlim.rlim_max
    };
    CHECK(setrlimit(RLIMIT_DATA, &new_rlim) == 0, "setrlimit(RLIMIT_DATA) succeeds");

    /* Try to allocate well beyond limit */
    errno = 0;
    unsigned long beyond_limit = current + 64 * 1024;
    unsigned long ret = syscall(SYS_brk, beyond_limit);

    CHECK(ret == current, "brk beyond RLIMIT_DATA returns current break");
    CHECK(errno == 0, "raw syscall does not set errno");

    unsigned long after = syscall(SYS_brk, 0);
    CHECK(after == current, "break unchanged after rlimit rejection");

    /* Restore original limit */
    CHECK(setrlimit(RLIMIT_DATA, &old_rlim) == 0, "restore RLIMIT_DATA");
}

static int read_proc_stat_data_layout(unsigned long *start_data,
                                      unsigned long *end_data,
                                      unsigned long *start_brk)
{
    char line[4096];
    FILE *file = fopen("/proc/self/stat", "r");
    if (file == NULL) return -1;
    if (fgets(line, sizeof(line), file) == NULL) {
        fclose(file);
        return -1;
    }
    fclose(file);

    /* comm may contain spaces and parentheses; fields after its final ')' are
     * unambiguous.  The first token there is field 3 (state). */
    char *cursor = strrchr(line, ')');
    if (cursor == NULL || cursor[1] != ' ') return -1;
    cursor += 2;

    char *save = NULL;
    char *token = strtok_r(cursor, " \n", &save);
    unsigned int field = 3;
    int found = 0;
    while (token != NULL) {
        if (field == 45) {
            *start_data = strtoul(token, NULL, 10);
            found |= 1;
        } else if (field == 46) {
            *end_data = strtoul(token, NULL, 10);
            found |= 2;
        } else if (field == 47) {
            *start_brk = strtoul(token, NULL, 10);
            found |= 4;
            break;
        }
        token = strtok_r(NULL, " \n", &save);
        field++;
    }
    return found == 7 ? 0 : -1;
}

static void test_raw_brk_rlimit_includes_elf_data(void)
{
    unsigned long start_data = 0;
    unsigned long end_data = 0;
    unsigned long start_brk = 0;
    int parsed = read_proc_stat_data_layout(&start_data, &end_data, &start_brk);
    CHECK(parsed == 0, "read ELF data layout from /proc/self/stat");
    if (parsed != 0) return;

    CHECK(start_data != 0 && end_data >= start_data && start_brk != 0,
          "/proc/self/stat publishes ELF data and brk bounds");
    if (start_data == 0 || end_data < start_data || start_brk == 0) return;

    unsigned long current = syscall(SYS_brk, 0);
    CHECK(current >= start_brk, "current break is not below start_brk");
    if (current < start_brk) return;

    unsigned long data_span = end_data - start_data;
    unsigned long heap_delta = current - start_brk;
    CHECK(data_span <= ULONG_MAX - heap_delta - 1,
          "RLIMIT_DATA boundary arithmetic is representable");
    if (data_span > ULONG_MAX - heap_delta - 1) return;

    struct rlimit old_rlim;
    CHECK(getrlimit(RLIMIT_DATA, &old_rlim) == 0,
          "get RLIMIT_DATA for ELF data boundary");
    unsigned long exact_current = data_span + heap_delta;
    CHECK(old_rlim.rlim_max == RLIM_INFINITY ||
          exact_current + 1 <= old_rlim.rlim_max,
          "RLIMIT_DATA hard limit admits boundary test");
    if (old_rlim.rlim_max != RLIM_INFINITY &&
        exact_current + 1 > old_rlim.rlim_max) return;

    struct rlimit limit = {
        .rlim_cur = exact_current,
        .rlim_max = old_rlim.rlim_max,
    };
    CHECK(setrlimit(RLIMIT_DATA, &limit) == 0,
          "set RLIMIT_DATA to ELF data plus current heap");
    unsigned long ret = syscall(SYS_brk, current + 1);
    CHECK(ret == current,
          "RLIMIT_DATA counts ELF data before one-byte heap growth");

    limit.rlim_cur = exact_current + 1;
    CHECK(setrlimit(RLIMIT_DATA, &limit) == 0,
          "raise RLIMIT_DATA by one byte");
    ret = syscall(SYS_brk, current + 1);
    CHECK(ret == current + 1,
          "one-byte RLIMIT_DATA increase admits one-byte heap growth");
    CHECK(syscall(SYS_brk, current) == current,
          "restore break after ELF data boundary test");
    CHECK(setrlimit(RLIMIT_DATA, &old_rlim) == 0,
          "restore RLIMIT_DATA after ELF data boundary test");
}

static void test_libc_brk_rlimit_data(void)
{
    struct rlimit old_rlim;
    CHECK(getrlimit(RLIMIT_DATA, &old_rlim) == 0, "getrlimit(RLIMIT_DATA) succeeds");

    void *original = sbrk(0);
    CHECK(original != (void *)-1, "sbrk(0) returns current break");

    /* Set tight limit: 4K */
    struct rlimit new_rlim = {
        .rlim_cur = 4096,
        .rlim_max = old_rlim.rlim_max
    };
    CHECK(setrlimit(RLIMIT_DATA, &new_rlim) == 0, "setrlimit(RLIMIT_DATA) succeeds");

    /* Try to allocate beyond limit via libc brk() */
    errno = 0;
    int ret = brk((char *)original + 64 * 1024);

    CHECK(ret == -1, "brk beyond RLIMIT_DATA returns -1");
    CHECK(errno == ENOMEM, "brk beyond RLIMIT_DATA sets ENOMEM");

    /* Break unchanged */
    CHECK(sbrk(0) == original, "break unchanged after rlimit rejection");

    /* Restore original limit */
    CHECK(setrlimit(RLIMIT_DATA, &old_rlim) == 0, "restore RLIMIT_DATA");
}

/* ============================================================
 * MAIN
 * ============================================================ */

int main(void)
{
    TEST_START("brk syscall");

    printf("--- raw syscall tests ---\n\n");

    test_raw_brk_query();
    test_raw_brk_expand_success();
    test_raw_brk_shrink_success();
    test_raw_brk_failure();
    test_raw_brk_below_base();
    test_raw_brk_roundtrip();
    test_clone_vm_shares_brk_state();
    test_raw_brk_rlimit_data();
    test_raw_brk_rlimit_includes_elf_data();

    printf("\n--- libc wrapper tests ---\n\n");

    test_libc_brk_current_break();
    test_libc_sbrk_zero();
    test_libc_sbrk_allocate();
    test_libc_sbrk_sequential();
    test_libc_sbrk_huge_negative();
    test_libc_brk_enomem_huge();
    test_libc_brk_rlimit_data();

    TEST_DONE();
}
