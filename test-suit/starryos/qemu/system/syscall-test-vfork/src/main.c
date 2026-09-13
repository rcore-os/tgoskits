#define _GNU_SOURCE
#define _DEFAULT_SOURCE
#define _POSIX_C_SOURCE 199309L
#include <sched.h>
#include <errno.h>
#include <stdio.h>
#include <unistd.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <sys/ipc.h>
#include <sys/shm.h>
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

/* musl clone() rejects CLONE_THREAD before entering the kernel. This raw
 * CLONE_VFORK call keeps the parent stack protected until the child exits;
 * always inline it so the child cannot consume a shared helper return slot. */
static __attribute__((always_inline)) inline long raw_vfork_clone(long flags)
{
#if defined(__riscv)
    register long a0 __asm__("a0") = flags;
    register long a1 __asm__("a1") = 0;
    register long a2 __asm__("a2") = 0;
    register long a3 __asm__("a3") = 0;
    register long a4 __asm__("a4") = 0;
    register long a7 __asm__("a7") = SYS_clone;

    __asm__ volatile(
        "ecall"
        : "+r"(a0)
        : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7)
        : "memory");

    return a0;
#elif defined(__x86_64__)
    register long r10 __asm__("r10") = 0;
    register long r8 __asm__("r8") = 0;
    long result;
    __asm__ volatile("syscall" : "=a"(result)
                 : "a"(SYS_clone), "D"((long)flags), "S"(0L), "d"(0L), "r"(r10), "r"(r8)
                 : "rcx", "r11", "memory");
    return result;
#elif defined(__aarch64__)
    register long x0 __asm__("x0") = flags;
    register long x1 __asm__("x1") = 0;
    register long x2 __asm__("x2") = 0;
    register long x3 __asm__("x3") = 0;
    register long x4 __asm__("x4") = 0;
    register long x8 __asm__("x8") = SYS_clone;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8) : "memory");
    return x0;
#elif defined(__loongarch64)
    register long a0 __asm__("$a0") = flags;
    register long a1 __asm__("$a1") = 0;
    register long a2 __asm__("$a2") = 0;
    register long a3 __asm__("$a3") = 0;
    register long a4 __asm__("$a4") = 0;
    register long a7 __asm__("$a7") = SYS_clone;
    __asm__ volatile("syscall 0" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7) : "memory");
    return a0;
#else
#error Unsupported clone register ABI
#endif
}

static int clone_child_sleep(void *arg) {
    (void)arg;
    sleep(2);
    _exit(0);
}

static int clone_child_attach_shm(void *arg) {
    int shmid = *(int *)arg;

    if (shmat(shmid, NULL, 0) == (void *)-1) {
        _exit(1);
    }
    _exit(0);
}

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

/* Test 3: Linux-compatible clone semantics. CLONE_VFORK blocks the parent
   until the child exits or execs even when the caller passes a private child
   stack. BusyBox shell/timeout code depends on this ordering. */
int test_vfork_clone_child_stack_blocking(void) {
    static char child_stack[16384];
    char *stack_top = child_stack + sizeof(child_stack);
    struct timespec start, end;

    clock_gettime(CLOCK_MONOTONIC, &start);
    int pid = clone(clone_child_sleep, stack_top, CLONE_VM | CLONE_VFORK | SIGCHLD, NULL);
    if (pid < 0) {
        perror("clone(CLONE_VM|CLONE_VFORK) failed");
        return -1;
    }

    clock_gettime(CLOCK_MONOTONIC, &end);

    int status = 0;
    waitpid(pid, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        return 0;
    }

    long elapsed_ms = (end.tv_sec - start.tv_sec) * 1000 +
                      (end.tv_nsec - start.tv_nsec) / 1000000;

    return (elapsed_ms >= 1500) ? 1 : 0;
}

/* Test 4: do_exit releases a CLONE_VFORK child's SysV SHM attachment before
 * the blocked parent is allowed to return. The private child stack permits
 * this test to attach SHM without entering the restricted vfork child path. */
int test_clone_vfork_child_shm_cleanup(void) {
    static char child_stack[16384];
    char *stack_top = child_stack + sizeof(child_stack);
    int shmid = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    if (shmid < 0) {
        perror("shmget failed");
        return -1;
    }

    void *parent_addr = shmat(shmid, NULL, 0);
    if (parent_addr == (void *)-1) {
        perror("parent shmat failed");
        shmctl(shmid, IPC_RMID, NULL);
        return -1;
    }

    int child = clone(clone_child_attach_shm, stack_top,
                      CLONE_VM | CLONE_VFORK | SIGCHLD, &shmid);
    if (child < 0) {
        perror("clone(CLONE_VM|CLONE_VFORK) failed");
        shmdt(parent_addr);
        shmctl(shmid, IPC_RMID, NULL);
        return -1;
    }

    struct shmid_ds segment;
    int status = 0;
    int cleanup_observed = shmctl(shmid, IPC_STAT, &segment) == 0
        && (segment.shm_nattch & 0xffffUL) == 1UL;
    int child_reaped = waitpid(child, &status, 0) == child
        && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int resources_removed = 1;

    if (shmdt(parent_addr) != 0 || shmctl(shmid, IPC_RMID, NULL) != 0) {
        resources_removed = 0;
    }
    return cleanup_observed && child_reaped && resources_removed;
}

static volatile sig_atomic_t kill_wait_expired;

static void expire_kill_wait(int signal_number) {
    (void)signal_number;
    kill_wait_expired = 1;
}

enum signal_route {
    SEND_KILL, SEND_TKILL, SEND_TGKILL, SEND_SIGQUEUE, SEND_TGSIGQUEUE, SEND_PIDFD
};

static int send_observed_signal(pid_t target, int signo, enum signal_route route, int value) {
    siginfo_t info = {0};
    info.si_signo = signo;
    info.si_code = SI_QUEUE;
    info.si_pid = getpid();
    info.si_uid = getuid();
    info.si_value.sival_int = value;
    switch (route) {
    case SEND_KILL:
        return syscall(SYS_kill, target, signo);
    case SEND_TKILL:
        return syscall(SYS_tkill, target, signo);
    case SEND_TGKILL:
        return syscall(SYS_tgkill, target, target, signo);
    case SEND_SIGQUEUE:
        return syscall(SYS_rt_sigqueueinfo, target, signo, &info);
    case SEND_TGSIGQUEUE:
        return syscall(SYS_rt_tgsigqueueinfo, target, target, signo, &info);
    case SEND_PIDFD: {
        int fd = syscall(SYS_pidfd_open, target, 0);
        if (fd < 0) return -1;
        int result = syscall(SYS_pidfd_send_signal, fd, signo, NULL, 0);
        int saved_errno = errno;
        close(fd);
        errno = saved_errno;
        return result;
    }
    }
    errno = EINVAL;
    return -1;
}

/* Queued instances remain distinct while blocked, for both pending queues. */
static int test_realtime_fifo(void) {
    int signo = SIGRTMIN;
    sigset_t set, previous;
    sigemptyset(&set);
    sigaddset(&set, signo);
    if (sigprocmask(SIG_BLOCK, &set, &previous) != 0) return 0;
    const struct timespec immediate = {0};
    int passed = 1;
    for (enum signal_route route = SEND_SIGQUEUE; route <= SEND_TGSIGQUEUE; route++) {
        for (int i = 0; i < 4; i++) {
            if (send_observed_signal(getpid(), signo, route, 100 + i) != 0) passed = 0;
        }
        for (int i = 0; i < 4; i++) {
            siginfo_t info = {0};
            long received = syscall(SYS_rt_sigtimedwait, &set, &info, &immediate, sizeof(unsigned long));
            if (received != signo || info.si_value.sival_int != 100 + i || info.si_code != SI_QUEUE) {
                passed = 0;
            }
        }
    }
    siginfo_t remaining;
    if (syscall(SYS_rt_sigtimedwait, &set, &remaining, &immediate, sizeof(unsigned long)) != -1
        || errno != EAGAIN) passed = 0;
    /* Drain on failure before restoring the default-fatal signal's mask. */
    while (syscall(SYS_rt_sigtimedwait, &set, &remaining, &immediate, sizeof(unsigned long)) >= 0) {}
    if (sigprocmask(SIG_SETMASK, &previous, NULL) != 0) passed = 0;
    return passed;
}

struct blocked_child_channels {
    int ready;
    int release;
    int wait_for_release;
};

static int clone_child_wait_for_release(void *argument) {
    struct blocked_child_channels *channels = argument;
    pid_t self = getpid();
    if (write(channels->ready, &self, sizeof(self)) != sizeof(self)) {
        _exit(2);
    }
    close(channels->ready);
    int result = 0;
    if (channels->wait_for_release) {
        char release;
        result = read(channels->release, &release, 1) == 1 ? 0 : 3;
    }
    /* CLONE_THREAD must exit only this thread, not its waiting parent. */
    syscall(SYS_exit, result);
    _exit(3);
}

/* Linux waits for vfork completion in TASK_KILLABLE. The observer releases
 * the child only AFTER reaping its killed parent, so child completion cannot
 * accidentally make a broken, unkillable parent wait appear correct. */
static int test_clone_vfork_parent_wait(int extra_flags, int parent_signal, enum signal_route route) {
    int ready[2], release[2];
    if (pipe(ready) != 0) {
        return -1;
    }
    if (pipe(release) != 0) {
        close(ready[0]);
        close(ready[1]);
        return -1;
    }
    pid_t parent = fork();
    if (parent == 0) {
        static char child_stack[16384];
        close(ready[0]);
        close(release[1]);
        struct blocked_child_channels channels = {ready[1], release[0], parent_signal};
        int flags = CLONE_VM | CLONE_VFORK | SIGCHLD | extra_flags;
        long child;
        if (extra_flags & CLONE_THREAD) {
            child = raw_vfork_clone(flags);
            if (child == 0) {
                clone_child_wait_for_release(&channels);
                _exit(3);
            }
            if (child < 0) {
                errno = (int)-child;
            }
        } else {
            child = clone(clone_child_wait_for_release,
                          child_stack + sizeof(child_stack), flags, &channels);
        }
        if (child < 0) {
            perror("CLONE_VFORK child creation");
        }
        _exit(child < 0 ? 2 : 0);
    }
    close(ready[1]);
    close(release[0]);
    if (parent < 0) {
        close(ready[0]);
        close(release[1]);
        return -1;
    }

    pid_t child;
    int status = 0, passed = 0;
    pid_t reaped = -1;
    struct sigaction action = {0}, previous;
    action.sa_handler = expire_kill_wait;
    sigemptyset(&action.sa_mask);
    int handler_installed = sigaction(SIGALRM, &action, &previous) == 0;
    if (handler_installed && read(ready[0], &child, sizeof(child)) == sizeof(child)
        && (!parent_signal || send_observed_signal(parent, parent_signal, route, 0) == 0)) {
        kill_wait_expired = 0;
        /* Watchdog only: readiness and release pipes define the ordering. */
        alarm(10);
        do {
            reaped = waitpid(parent, &status, 0);
        } while (reaped < 0 && errno == EINTR && !kill_wait_expired);
        alarm(0);
        passed = reaped == parent && !kill_wait_expired
            && (parent_signal ? WIFSIGNALED(status) && WTERMSIG(status) == parent_signal
                            : WIFEXITED(status) && WEXITSTATUS(status) == 0);
    }
    /* Also unblock the old implementation after the watchdog, so a red test
     * reports a failure without leaving an unkillable vfork family behind. */
    char byte = 1;
    if (parent_signal && write(release[1], &byte, 1) != 1) {
        passed = 0;
    }
    close(release[1]);
    close(ready[0]);
    if (reaped != parent) {
        kill(parent, SIGKILL);
        while (waitpid(parent, &status, 0) < 0 && errno == EINTR) {}
    }
    if (handler_installed) {
        sigaction(SIGALRM, &previous, NULL);
    }
    if (!passed) {
        printf("CLONE_VFORK wait diagnostic: flags=%#x reaped=%d status=%#x timeout=%d\n",
               extra_flags, (int)reaped, status, (int)kill_wait_expired);
    }
    return passed;
}

int main(void) {
    int vfork_mem_pass = 0, vfork_exec_pass = 0, clone_stack_pass = 0,
        clone_vfork_shm_cleanup_pass = 0;

    /* Test 1: vfork memory sharing */
    vfork_mem_pass = test_vfork_memory_sniff();

    /* Test 2: vfork execution blocking */
    vfork_exec_pass = test_vfork_execution_order();

    /* Test 3: CLONE_VFORK with a private child stack still blocks parent */
    clone_stack_pass = test_vfork_clone_child_stack_blocking();

    /* Test 4: CLONE_VFORK return observes do_exit resource cleanup. */
    clone_vfork_shm_cleanup_pass = test_clone_vfork_child_shm_cleanup();

    int parent_sigkill_pass = test_clone_vfork_parent_wait(0, SIGKILL, SEND_KILL);
    printf("CLONE_VFORK: %s (SIGKILL releases parent before child completion)\n",
           parent_sigkill_pass > 0 ? "PASS" : "FAIL");

    int parent_sigterm_pass = test_clone_vfork_parent_wait(0, SIGTERM, SEND_KILL);
    printf("CLONE_VFORK: %s (Default SIGTERM releases parent before child completion)\n",
           parent_sigterm_pass > 0 ? "PASS" : "FAIL");

    int parent_realtime_pass = test_clone_vfork_parent_wait(0, SIGRTMIN, SEND_KILL);
    printf("CLONE_VFORK: %s (Default realtime signal releases parent before child completion)\n",
           parent_realtime_pass > 0 ? "PASS" : "FAIL");

    int thread_completion_pass = test_clone_vfork_parent_wait(CLONE_THREAD | CLONE_SIGHAND, 0, SEND_KILL);
    printf("CLONE_VFORK: %s (Child thread exit releases parent)\n",
           thread_completion_pass > 0 ? "PASS" : "FAIL");

    int routed_fatal_pass = 1;
    const char *route_names[] = {"kill", "tkill", "tgkill", "rt_sigqueueinfo", "rt_tgsigqueueinfo", "pidfd_send_signal"};
    for (enum signal_route route = SEND_TKILL; route <= SEND_PIDFD; route++) {
        int signo = (route == SEND_SIGQUEUE || route == SEND_TGSIGQUEUE) ? SIGRTMIN : SIGTERM;
        int passed = test_clone_vfork_parent_wait(0, signo, route);
        printf("CLONE_VFORK: %s (Fatal parent signal via %s)\n", passed > 0 ? "PASS" : "FAIL", route_names[route]);
        if (passed <= 0) routed_fatal_pass = 0;
    }
    int realtime_fifo_pass = test_realtime_fifo();
    printf("SIGNAL_QUEUE: %s (Process and thread realtime FIFO preserves siginfo)\n",
           realtime_fifo_pass ? "PASS" : "FAIL");

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

    if (clone_stack_pass > 0) {
        printf("VFORK: PASS (Child stack clone blocked parent)\n");
    } else {
        printf("VFORK: FAIL (Child stack clone did NOT block parent)\n");
    }

    if (clone_vfork_shm_cleanup_pass > 0) {
        printf("CLONE_VFORK: PASS (Child SHM cleanup precedes parent resume)\n");
    } else {
        printf("CLONE_VFORK: FAIL (Child SHM remained attached after parent resume)\n");
    }

    /* Return success only if all vfork-related tests pass */
    if (vfork_mem_pass > 0 && vfork_exec_pass > 0 && clone_stack_pass > 0
        && clone_vfork_shm_cleanup_pass > 0 && parent_sigkill_pass > 0
        && thread_completion_pass > 0 && parent_sigterm_pass > 0
        && parent_realtime_pass > 0 && routed_fatal_pass && realtime_fifo_pass) {
        printf("VFORK TEST: ALL TESTS PASSED\n");
        return 0;
    } else {
        printf("VFORK TEST: SOME TESTS FAILED\n");
        return 1;
    }
}
