#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 199309L
#include <stdio.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <signal.h>

/* On loongarch64 musl's vfork() degrades to clone(SIGCHLD,0) — no CLONE_VM,
 * no CLONE_VFORK.  Call clone directly with the right flags instead.
 * Syscall 220 = clone; args: flags, stack, parent_tid, tls, child_tid.
 * CLONE_VM=0x100, CLONE_VFORK=0x4000, SIGCHLD=17 → flags=0x4111
 */
#ifdef __loongarch__
static inline pid_t raw_vfork(void) {
    register long a0 __asm__("$a0") = 0x4111; /* CLONE_VM|CLONE_VFORK|SIGCHLD */
    register long a1 __asm__("$a1") = 0;      /* stack */
    register long a2 __asm__("$a2") = 0;      /* parent_tid */
    register long a3 __asm__("$a3") = 0;      /* tls */
    register long a4 __asm__("$a4") = 0;      /* child_tid */
    register long a7 __asm__("$a7") = 220;    /* SYS_clone */
    __asm__ volatile (
        "syscall 0"
        : "+r"(a0)
        : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7)
        : "memory"
    );
    return (pid_t)a0;
}
#define do_vfork() raw_vfork()
#else
#define do_vfork() vfork()
#endif

/* Test 1: Memory Sniff - Check if vfork shares address space */
int test_vfork_memory_sniff(void) {
    volatile int stack_var = 0;
    pid_t ret = do_vfork();

    if (ret < 0) {
        perror("vfork failed");
        return -1;
    }

    if (ret == 0) {
        /* Child: modify shared variable */
        stack_var = 42;
        _exit(0);
    } else {
        /* Parent: check if child modification is visible */
        int result = (stack_var == 42) ? 1 : 0;
        wait(NULL);
        return result;
    }
    return 0;
}

/* Test 2: Execution Order - Check if parent blocks until child exits */
int test_vfork_execution_order(void) {
    struct timespec start, end;

    /* Start the clock BEFORE vfork — the parent should be blocked inside
       vfork() until the child calls _exit(), so the elapsed time measured
       after vfork() returns in the parent reflects the blocking duration. */
    clock_gettime(CLOCK_MONOTONIC, &start);

    pid_t ret = do_vfork();

    if (ret < 0) {
        perror("vfork failed");
        return -1;
    }

    if (ret == 0) {
        /* Child: sleep for 5 seconds then exit */
        sleep(5);
        _exit(0);
    } else {
        /* Parent resumes here only after child exits.
           Measure how long we were blocked inside vfork(). */
        clock_gettime(CLOCK_MONOTONIC, &end);
        wait(NULL);

        long elapsed_ms = (end.tv_sec - start.tv_sec) * 1000 +
                          (end.tv_nsec - start.tv_nsec) / 1000000;

        /* True vfork should block parent for at least 4 seconds */
        return (elapsed_ms >= 4000) ? 1 : 0;
    }
    return 0;
}

int main(void) {
    int vfork_mem_pass = 0, vfork_exec_pass = 0;

    /* Test 1: vfork memory sharing */
    vfork_mem_pass = test_vfork_memory_sniff();

    /* Test 2: vfork execution blocking */
    vfork_exec_pass = test_vfork_execution_order();

    /* Report results */
    if (vfork_mem_pass > 0) {
        printf("VFORK: PASS (Memory shared)\n");
    } else {
        printf("VFORK: FAIL (Memory NOT shared)\n");
    }

    if (vfork_exec_pass > 0) {
        printf("VFORK: PASS (Parent blocked)\n");
    } else {
        printf("VFORK: FAIL (Parent NOT blocked)\n");
    }

    /* Return success only if both vfork tests pass */
    if (vfork_mem_pass > 0 && vfork_exec_pass > 0) {
        printf("VFORK TEST: ALL TESTS PASSED\n");
        return 0;
    } else {
        printf("VFORK TEST: SOME TESTS FAILED\n");
        return 1;
    }
}
