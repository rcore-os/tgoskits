#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

/* Same-CPU FIFO priorities force the waiting parent to run before the signal
 * sender can exit. This distinguishes a restarted wait from an exit that
 * happened to beat signal delivery, without sleeps or repeated signals. */
static volatile sig_atomic_t handler_called;

static void record_handler(int signo)
{
    handler_called = signo == SIGUSR1;
}

static int wait_after_signal(int mode)
{
    cpu_set_t available, selected;
    CPU_ZERO(&available);
    if (syscall(SYS_sched_getaffinity, 0, sizeof(available), &available) < 0) {
        perror("sched_getaffinity");
        return 1;
    }
    int cpu;
    for (cpu = 0; cpu < CPU_SETSIZE && !CPU_ISSET(cpu, &available); ++cpu) {}
    if (cpu == CPU_SETSIZE) {
        fprintf(stderr, "FAIL: no available CPU\n");
        return 1;
    }
    CPU_ZERO(&selected);
    CPU_SET(cpu, &selected);
    struct sched_param priority = { .sched_priority = 80 };
    if (syscall(SYS_sched_setaffinity, 0, sizeof(selected), &selected) != 0
        || syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &priority) != 0) {
        perror("configure FIFO waiter");
        return 1;
    }
    int default_continue = mode == 0 || mode == 3;
    int signo = default_continue ? SIGCONT : SIGUSR1;
    struct sigaction action = {
        .sa_handler = default_continue ? SIG_DFL : record_handler,
        .sa_flags = mode == 2 ? SA_RESTART : 0,
    };
    sigemptyset(&action.sa_mask);
    if (sigaction(signo, &action, NULL) != 0) {
        perror("sigaction FIFO waiter");
        return 1;
    }
    pid_t parent = getpid();
    pid_t child = fork();
    if (child < 0) {
        perror("fork FIFO sender");
        return 1;
    }
    if (child == 0) {
        priority.sched_priority = 70;
        if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &priority) != 0) {
            _exit(101);
        }
        long sent = mode == 3 ? syscall(SYS_tgkill, parent, parent, signo)
                              : syscall(SYS_kill, parent, signo);
        if (sent != 0) {
            _exit(101);
        }
        _exit(42);
    }
    int status = 0;
    errno = 0;
    long result = syscall(SYS_wait4, child, &status, 0, NULL);
    int error = errno;
    if (mode == 1 && result == -1 && error == EINTR && handler_called) {
        result = syscall(SYS_wait4, child, &status, 0, NULL);
    } else if (mode == 1 || (mode == 2 && !handler_called)) {
        fprintf(stderr, "FAIL: wait4 handler decision: mode=%d ret=%ld errno=%d handler=%d\n",
                mode, result, error, handler_called);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    if (result != child || !WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        fprintf(stderr, "FAIL: wait4 after signal: mode=%d ret=%ld errno=%d status=%#x\n",
                mode, result, error, status);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    return 0;
}

static int run_wait_restart_case(int mode)
{
    /* Keep realtime policy local to this one regression process. */
    pid_t waiter = fork();
    if (waiter < 0) {
        perror("fork FIFO waiter");
        return 1;
    }
    if (waiter == 0) {
        _exit(wait_after_signal(mode));
    }
    int status = 0;
    if (waitpid(waiter, &status, 0) != waiter
        || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "FAIL: FIFO wait restart regression status=%#x\n", status);
        return 1;
    }
    return 0;
}

static int continue_stopped_thread(int ignored)
{
    pid_t child = fork();
    if (child < 0) {
        perror("fork stopped child");
        return 1;
    }
    if (child == 0) {
        struct sigaction action = { .sa_handler = ignored ? SIG_IGN : SIG_DFL };
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGCONT, &action, NULL) != 0) {
            _exit(101);
        }
        if (!ignored) {
            sigset_t mask;
            sigemptyset(&mask);
            sigaddset(&mask, SIGCONT);
            if (sigprocmask(SIG_BLOCK, &mask, NULL) != 0) {
                _exit(102);
            }
        }
        if (syscall(SYS_kill, getpid(), SIGSTOP) != 0) {
            _exit(103);
        }
        _exit(42);
    }
    int status = 0;
    if (syscall(SYS_wait4, child, &status, WUNTRACED, NULL) != child
        || !WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        fprintf(stderr, "FAIL: child did not report SIGSTOP: status=%#x\n", status);
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    printf("checking tgkill SIGCONT resumes stopped thread: ignored=%d\n", ignored);
    fflush(stdout);
    if (syscall(SYS_tgkill, child, child, SIGCONT) != 0) {
        perror("tgkill SIGCONT");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        return 1;
    }
    if (syscall(SYS_wait4, child, &status, 0, NULL) != child
        || !WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        fprintf(stderr, "FAIL: resumed child did not exit: status=%#x\n", status);
        return 1;
    }
    return 0;
}

int test_wait_signal_delivery(void)
{
    for (int mode = 0; mode < 4; ++mode) {
        if (run_wait_restart_case(mode) != 0) {
            return 1;
        }
    }
    puts("PASS: default SIGCONT preserves wait4 and handlers obey SA_RESTART");
    for (int ignored = 0; ignored < 2; ++ignored) {
        if (continue_stopped_thread(ignored) != 0) {
            return 1;
        }
    }
    puts("PASS: thread SIGCONT resumes despite blocking or ignoring delivery");
    return 0;
}
