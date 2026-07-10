// Regression test for the si_addr POSIX fix.
//
// POSIX requires that a synchronous SIGSEGV/SIGBUS carry the faulting address
// in siginfo->si_addr. StarryOS used to deliver si_addr == 0 for kernel-raised
// SIGSEGV (SignalInfo::new_kernel), which broke runtimes (notably the HotSpot
// JVM) that read si_addr in their own handler to classify and recover from
// guard-page / implicit-null-check faults.
//
// Before the fix: si_addr is 0 for both cases below  -> prints SIADDR_FAIL.
// After  the fix: si_addr equals the faulting address -> prints SIADDR_OK.
// On real Linux (reference): always passes.
//
// Build: cc -O0 -o si_addr_regression si_addr_regression.c
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <setjmp.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jb;
static volatile void *g_expected;
static volatile void *g_got;
static volatile int   g_got_signo;

static void segv_handler(int signo, siginfo_t *info, void *uctx) {
    (void)uctx;
    g_got_signo = signo;
    g_got = info ? info->si_addr : (void *)-1;
    siglongjmp(jb, 1);
}

static int check(const char *name, volatile void *expected,
                 void (*trigger)(void)) {
    g_expected = expected;
    g_got = (void *)-1;
    g_got_signo = 0;
    if (sigsetjmp(jb, 1) == 0) {
        trigger();
        printf("  %s: NO FAULT (unexpected)\n", name);
        return 1;
    }
    int ok = (g_got == expected);
    printf("  %s: signo=%d si_addr=%p expected=%p -> %s\n",
           name, g_got_signo, (void *)g_got, (void *)expected,
           ok ? "OK" : "MISMATCH");
    return ok ? 0 : 1;
}

// Case 1: implicit-null-check style — read a near-null field offset (0x34),
// exactly the access pattern that made the JVM loop on the buggy kernel.
static void trig_null(void) {
    volatile int *p = (volatile int *)(uintptr_t)0x34;
    volatile int v = *p;
    (void)v;
}

// Case 2: write to a present, read-only page (guard-page / stack-bang style).
static volatile int *g_ro;
static void trig_ro_write(void) { *g_ro = 1; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = segv_handler;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGSEGV, &sa, NULL) != 0 ||
        sigaction(SIGBUS, &sa, NULL) != 0) {
        perror("sigaction");
        return 2;
    }

    long pg = sysconf(_SC_PAGESIZE);
    void *ro = mmap(NULL, (size_t)pg, PROT_READ,
                    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (ro == MAP_FAILED) { perror("mmap"); return 2; }
    g_ro = (volatile int *)ro;

    int fails = 0;
    printf("si_addr regression test\n");
    fails += check("null-deref@0x34", (void *)(uintptr_t)0x34, trig_null);
    fails += check("ro-page-write", (void *)ro, trig_ro_write);

    if (fails == 0) {
        printf("SIADDR_OK 2/2\n");
        return 0;
    }
    printf("SIADDR_FAIL %d/2\n", fails);
    return 1;
}
