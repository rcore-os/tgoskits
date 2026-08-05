#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
#endif
#ifndef PTRACE_INTERRUPT
#define PTRACE_INTERRUPT 0x4207
#endif
#ifndef PTRACE_EVENT_EXEC
#define PTRACE_EVENT_EXEC 4
#endif
#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif
#ifndef PTRACE_O_TRACESYSGOOD
#define PTRACE_O_TRACESYSGOOD 0x00000001
#endif
#ifndef PTRACE_O_TRACEEXEC
#define PTRACE_O_TRACEEXEC 0x00000010
#endif
#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif

enum {
    WAIT_TIMEOUT_MS = 3000,
    WAIT_POLL_INTERVAL_MS = 10,
};

struct x86_64_user_regs {
    uint64_t r15;
    uint64_t r14;
    uint64_t r13;
    uint64_t r12;
    uint64_t rbp;
    uint64_t rbx;
    uint64_t r11;
    uint64_t r10;
    uint64_t r9;
    uint64_t r8;
    uint64_t rax;
    uint64_t rcx;
    uint64_t rdx;
    uint64_t rsi;
    uint64_t rdi;
    uint64_t orig_rax;
    uint64_t rip;
    uint64_t cs;
    uint64_t eflags;
    uint64_t rsp;
    uint64_t ss;
    uint64_t fs_base;
    uint64_t gs_base;
    uint64_t ds;
    uint64_t es;
    uint64_t fs;
    uint64_t gs;
};

static int fail(const char *message)
{
    printf("FAIL: %s: errno=%d (%s)\n", message, errno, strerror(errno));
    return 1;
}

static pid_t wait_for_tracee_event(pid_t pid, int *status)
{
    struct timespec delay = {
        .tv_sec = 0,
        .tv_nsec = WAIT_POLL_INTERVAL_MS * 1000 * 1000,
    };

    for (int waited_ms = 0; waited_ms < WAIT_TIMEOUT_MS; waited_ms += WAIT_POLL_INTERVAL_MS) {
        pid_t waited = waitpid(pid, status, WUNTRACED | WNOHANG);
        if (waited != 0) {
            return waited;
        }
        (void)nanosleep(&delay, NULL);
    }

    errno = ETIMEDOUT;
    return 0;
}

static void print_stop(const char *phase, int status)
{
    printf("PTRACE_STOP: %s status=%#x signal=%d event=%u\n", phase, status,
           WIFSTOPPED(status) ? WSTOPSIG(status) : -1, (unsigned int)status >> 16);
}

static int expect_stop(pid_t pid, int expected_signal, unsigned int expected_event,
                       const char *description)
{
    int status = 0;
    pid_t waited = wait_for_tracee_event(pid, &status);
    if (waited != pid) {
        return fail(description);
    }
    print_stop(description, status);
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != expected_signal
        || ((unsigned int)status >> 16) != expected_event) {
        printf("FAIL: %s: status=%#x signal=%d event=%u\n", description, status,
               WIFSTOPPED(status) ? WSTOPSIG(status) : -1, (unsigned int)status >> 16);
        return 1;
    }
    return 0;
}

static void kill_tracee(pid_t pid)
{
    int status = 0;
    (void)kill(pid, SIGKILL);
    (void)wait_for_tracee_event(pid, &status);
}

static int read_tracee_regs(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {
        .iov_base = regs,
        .iov_len = sizeof(*regs),
    };

    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("PTRACE_GETREGSET for syscall stop");
    }
    if (iov.iov_len != sizeof(*regs)) {
        errno = EINVAL;
        return fail("PTRACE_GETREGSET returned a complete x86_64 register set");
    }
    return 0;
}

static int resume_until_execve_entry(pid_t pid)
{
    int signal_to_deliver = SIGCONT;

    for (int stop_count = 0; stop_count < 8; stop_count++) {
        int status = 0;
        if (ptrace(PTRACE_SYSCALL, pid, NULL, (void *)(uintptr_t)signal_to_deliver) != 0) {
            return fail("PTRACE_SYSCALL resumes the seized tracee");
        }

        pid_t waited = wait_for_tracee_event(pid, &status);
        if (waited != pid || !WIFSTOPPED(status)) {
            return fail("wait for a ptrace stop while resuming toward execve");
        }

        print_stop("resume toward execve", status);
        unsigned int event = (unsigned int)status >> 16;
        if (WSTOPSIG(status) == SIGSTOP && event == PTRACE_EVENT_STOP) {
            signal_to_deliver = 0;
            continue;
        }
        if (WSTOPSIG(status) != (SIGTRAP | 0x80) || event != 0) {
            printf("FAIL: expected a syscall or group-stop before execve entry: status=%#x "
                   "signal=%d event=%u\n",
                   status, WSTOPSIG(status), event);
            return 1;
        }

        struct x86_64_user_regs regs = {0};
        if (read_tracee_regs(pid, &regs) != 0) {
            return 1;
        }
        printf("PTRACE_SYSCALL_REGS: orig_rax=%lld rax=%lld\n",
               (long long)(int64_t)regs.orig_rax, (long long)(int64_t)regs.rax);
        if (regs.orig_rax == SYS_execve && (int64_t)regs.rax == -ENOSYS) {
            return 0;
        }

        signal_to_deliver = 0;
    }

    errno = ETIMEDOUT;
    return fail("PTRACE_SYSCALL reaches the execve entry stop");
}

static int run_traceexec_sequence(pid_t pid)
{
    unsigned long old_tid = 0;
    int status = 0;

    if (ptrace(PTRACE_SEIZE, pid, NULL,
               (void *)(uintptr_t)(PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACEEXEC))
        != 0) {
        return fail("PTRACE_SEIZE with PTRACE_O_TRACEEXEC");
    }
    if (ptrace(PTRACE_INTERRUPT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_INTERRUPT");
    }
    if (expect_stop(pid, SIGSTOP, PTRACE_EVENT_STOP,
                    "PTRACE_INTERRUPT reports PTRACE_EVENT_STOP")
        != 0) {
        return 1;
    }
    if (resume_until_execve_entry(pid) != 0) {
        return 1;
    }
    if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) != 0) {
        return fail("PTRACE_SYSCALL resumes execve entry");
    }
    if (expect_stop(pid, SIGTRAP, PTRACE_EVENT_EXEC,
                    "PTRACE_O_TRACEEXEC replaces execve syscall-exit stop")
        != 0) {
        return 1;
    }
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &old_tid) != 0) {
        return fail("PTRACE_GETEVENTMSG for PTRACE_EVENT_EXEC");
    }
    if ((pid_t)old_tid != pid) {
        printf("FAIL: PTRACE_EVENT_EXEC former tid=%lu expected %d\n", old_tid, pid);
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("PTRACE_CONT resumes PTRACE_EVENT_EXEC");
    }
    if (wait_for_tracee_event(pid, &status) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        printf("FAIL: tracee did not exit after PTRACE_EVENT_EXEC: status=%#x\n", status);
        return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--after-exec") == 0) {
        return 0;
    }

    printf("PTRACE_SEIZE TRACEEXEC syscall-stop regression\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork tracee");
    }
    if (pid == 0) {
        (void)raise(SIGSTOP);
        execl("/proc/self/exe", "test-ptrace-seize-traceexec", "--after-exec", NULL);
        _exit(127);
    }

    if (expect_stop(pid, SIGSTOP, 0, "tracee enters its initial group stop") != 0) {
        kill_tracee(pid);
        return 1;
    }
    if (run_traceexec_sequence(pid) != 0) {
        kill_tracee(pid);
        return 1;
    }

    printf("PASS: PTRACE_EVENT_EXEC replaces the execve syscall-exit stop\n");
    return 0;
}
