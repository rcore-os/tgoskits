#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
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
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif
#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
#endif
#ifndef PTRACE_INTERRUPT
#define PTRACE_INTERRUPT 0x4207
#endif
#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif
#ifndef PTRACE_O_TRACESYSGOOD
#define PTRACE_O_TRACESYSGOOD 0x00000001
#endif
#ifndef __WALL
#define __WALL 0x40000000
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
        pid_t waited = waitpid(pid, status, __WALL | WNOHANG);
        if (waited != 0) {
            return waited;
        }
        (void)nanosleep(&delay, NULL);
    }

    errno = ETIMEDOUT;
    return 0;
}

static int expect_ptrace_stop(pid_t pid, int expected_signal, int expected_event,
                              const char *description)
{
    int status = 0;
    pid_t waited = wait_for_tracee_event(pid, &status);
    if (waited != pid) {
        return fail(description);
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != expected_signal
        || ((unsigned int)status >> 16) != (unsigned int)expected_event) {
        printf("FAIL: %s: status=%#x signal=%d event=%u\n", description, status,
               WIFSTOPPED(status) ? WSTOPSIG(status) : -1, (unsigned int)status >> 16);
        return 1;
    }
    return 0;
}

static int expect_ptrace_event(pid_t pid, int expected_event, const char *description)
{
    int status = 0;
    pid_t waited = wait_for_tracee_event(pid, &status);
    if (waited != pid) {
        return fail(description);
    }
    if (!WIFSTOPPED(status)
        || ((unsigned int)status >> 16) != (unsigned int)expected_event) {
        printf("FAIL: %s: status=%#x signal=%d event=%u\n", description, status,
               WIFSTOPPED(status) ? WSTOPSIG(status) : -1, (unsigned int)status >> 16);
        return 1;
    }
    return 0;
}

static int expect_getsiginfo_signal(pid_t pid, int expected_signal, const char *description)
{
    siginfo_t siginfo;
    memset(&siginfo, 0, sizeof(siginfo));
    errno = 0;
    long result = ptrace(PTRACE_GETSIGINFO, pid, NULL, &siginfo);
    if (result != 0 || siginfo.si_signo != expected_signal) {
        printf("FAIL: %s: result=%ld signo=%d errno=%d (%s)\n", description,
               result, siginfo.si_signo, errno, strerror(errno));
        return 1;
    }
    return 0;
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
        printf("FAIL: PTRACE_GETREGSET returned register length %zu, expected %zu\n",
               iov.iov_len, sizeof(*regs));
        return 1;
    }
    return 0;
}

static int write_tracee_regs(pid_t pid, struct x86_64_user_regs *regs)
{
    struct iovec iov = {
        .iov_base = regs,
        .iov_len = sizeof(*regs),
    };

    if (ptrace(PTRACE_SETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("PTRACE_SETREGSET replaces the syscall number");
    }
    return 0;
}

static int resume_to_getpid_entry(pid_t pid, struct x86_64_user_regs *regs)
{
    int signal_to_deliver = SIGCONT;

    for (int stop_count = 0; stop_count < 32; stop_count++) {
        if (ptrace(PTRACE_SYSCALL, pid, NULL,
                   (void *)(uintptr_t)signal_to_deliver)
            != 0) {
            return fail("PTRACE_SYSCALL resumes toward a getpid entry stop");
        }
        if (expect_ptrace_stop(pid, SIGTRAP | 0x80, 0,
                               "PTRACE_SYSCALL reports a TRACESYSGOOD syscall stop")
            != 0) {
            return 1;
        }
        if (expect_getsiginfo_signal(pid, SIGTRAP,
                                     "syscall stop PTRACE_GETSIGINFO returns SIGTRAP")
            != 0) {
            return 1;
        }
        if (read_tracee_regs(pid, regs) != 0) {
            return 1;
        }

        printf("PTRACE_SYSCALL_REGS: orig_rax=%lld rax=%lld\n",
               (long long)(int64_t)regs->orig_rax, (long long)(int64_t)regs->rax);
        if (regs->orig_rax == SYS_getpid && (int64_t)regs->rax == -ENOSYS) {
            return 0;
        }
        signal_to_deliver = 0;
    }

    errno = ETIMEDOUT;
    return fail("PTRACE_SYSCALL reaches a getpid entry stop");
}

static void kill_tracee(pid_t pid)
{
    int status = 0;
    (void)kill(pid, SIGKILL);
    (void)wait_for_tracee_event(pid, &status);
}

static int expect_ptrace_errno(long request, pid_t pid, void *addr, void *data,
                               int expected_errno, const char *description)
{
    errno = 0;
    long result = syscall(SYS_ptrace, request, pid, addr, data);
    if (result != -1 || errno != expected_errno) {
        printf("FAIL: %s: result=%ld errno=%d (%s), expected errno=%d (%s)\n",
               description, result, errno, strerror(errno), expected_errno,
               strerror(expected_errno));
        return 1;
    }
    return 0;
}

static pid_t fork_spinning_tracee(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        for (;;) {
            (void)syscall(SYS_getpid);
        }
    }
    return pid;
}

static int wait_for_job_stop(pid_t pid)
{
    struct timespec delay = {
        .tv_sec = 0,
        .tv_nsec = WAIT_POLL_INTERVAL_MS * 1000 * 1000,
    };

    for (int waited_ms = 0; waited_ms < WAIT_TIMEOUT_MS; waited_ms += WAIT_POLL_INTERVAL_MS) {
        int status = 0;
        pid_t waited = waitpid(pid, &status, WUNTRACED | WNOHANG);
        if (waited == pid) {
            if (WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) {
                return 0;
            }
            printf("FAIL: tracee job stop: status=%#x\n", status);
            return 1;
        }
        if (waited < 0) {
            return fail("waitpid for tracee job stop");
        }
        (void)nanosleep(&delay, NULL);
    }

    errno = ETIMEDOUT;
    return fail("tracee enters a job stop");
}

static int wait_for_task_sleeping(_Atomic pid_t *tid_slot)
{
    struct timespec delay = {
        .tv_sec = 0,
        .tv_nsec = WAIT_POLL_INTERVAL_MS * 1000 * 1000,
    };

    for (int waited_ms = 0; waited_ms < WAIT_TIMEOUT_MS; waited_ms += WAIT_POLL_INTERVAL_MS) {
        pid_t tid = atomic_load_explicit(tid_slot, memory_order_acquire);
        if (tid > 0) {
            char path[128];
            snprintf(path, sizeof(path), "/proc/self/task/%ld/status", (long)tid);
            FILE *file = fopen(path, "r");
            if (file != NULL) {
                char line[128];
                while (fgets(line, sizeof(line), file) != NULL) {
                    if (strncmp(line, "State:\tS ", strlen("State:\tS ")) == 0) {
                        fclose(file);
                        return 0;
                    }
                }
                fclose(file);
            }
        }
        (void)nanosleep(&delay, NULL);
    }

    return -1;
}

struct sibling_thread_args {
    int tid_fd;
    int block_fd;
    _Atomic pid_t *tid_slot;
};

static void *block_sibling_thread(void *opaque)
{
    struct sibling_thread_args *args = opaque;
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (write(args->tid_fd, &tid, sizeof(tid)) != (ssize_t)sizeof(tid)) {
        _exit(2);
    }
    atomic_store_explicit(args->tid_slot, tid, memory_order_release);

    char byte;
    if (read(args->block_fd, &byte, sizeof(byte)) >= 0) {
        _exit(2);
    }
    _exit(2);
}

static pid_t fork_job_stopped_multithreaded_tracee(pid_t *sibling_tid, int *block_write_fd)
{
    int tid_pipe[2];
    int block_pipe[2];
    if (pipe(tid_pipe) != 0) {
        return -1;
    }
    if (pipe(block_pipe) != 0) {
        close(tid_pipe[0]);
        close(tid_pipe[1]);
        return -1;
    }

    pid_t pid = fork();
    if (pid == 0) {
        close(tid_pipe[0]);
        close(block_pipe[1]);
        _Atomic pid_t sibling_tid_slot = 0;
        struct sibling_thread_args args = {
            .tid_fd = tid_pipe[1],
            .block_fd = block_pipe[0],
            .tid_slot = &sibling_tid_slot,
        };
        pthread_t sibling;
        if (pthread_create(&sibling, NULL, block_sibling_thread, &args) != 0) {
            _exit(2);
        }
        if (wait_for_task_sleeping(&sibling_tid_slot) != 0) {
            _exit(2);
        }
        if (raise(SIGSTOP) != 0) {
            _exit(2);
        }
        for (;;) {
            (void)syscall(SYS_getpid);
        }
    }

    close(tid_pipe[1]);
    close(block_pipe[0]);
    if (pid < 0) {
        close(tid_pipe[0]);
        close(block_pipe[1]);
        return -1;
    }
    struct pollfd tid_poll = {
        .fd = tid_pipe[0],
        .events = POLLIN,
    };
    int poll_result = poll(&tid_poll, 1, WAIT_TIMEOUT_MS);
    if (poll_result <= 0) {
        int saved_errno = poll_result == 0 ? ETIMEDOUT : errno;
        close(tid_pipe[0]);
        close(block_pipe[1]);
        kill_tracee(pid);
        errno = saved_errno;
        return -1;
    }
    ssize_t bytes = read(tid_pipe[0], sibling_tid, sizeof(*sibling_tid));
    close(tid_pipe[0]);
    if (bytes != (ssize_t)sizeof(*sibling_tid)) {
        close(block_pipe[1]);
        kill_tracee(pid);
        errno = EIO;
        return -1;
    }
    *block_write_fd = block_pipe[1];
    return pid;
}

static int test_seize_rejects_nonzero_addr(void)
{
    pid_t pid = fork_spinning_tracee();
    if (pid < 0) {
        return fail("fork tracee for nonzero PTRACE_SEIZE addr");
    }

    int result = expect_ptrace_errno(
        PTRACE_SEIZE, pid, (void *)(uintptr_t)1, NULL, EIO,
        "PTRACE_SEIZE rejects a nonzero addr");
    kill_tracee(pid);
    return result;
}

static int test_interrupt_rejects_attach_tracee(void)
{
    pid_t pid = fork_spinning_tracee();
    if (pid < 0) {
        return fail("fork tracee for PTRACE_ATTACH interrupt");
    }

    if (ptrace(PTRACE_ATTACH, pid, NULL, NULL) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_ATTACH before PTRACE_INTERRUPT");
    }
    if (expect_ptrace_stop(pid, SIGSTOP, 0, "PTRACE_ATTACH reports SIGSTOP") != 0) {
        kill_tracee(pid);
        return 1;
    }

    int result = expect_ptrace_errno(
        PTRACE_INTERRUPT, pid, NULL, NULL, EIO,
        "PTRACE_INTERRUPT rejects a PTRACE_ATTACH tracee");
    kill_tracee(pid);
    return result;
}

static int test_interrupt_job_stopped_sibling(void)
{
    pid_t sibling_tid = -1;
    int block_write_fd = -1;
    pid_t pid = fork_job_stopped_multithreaded_tracee(&sibling_tid, &block_write_fd);
    if (pid < 0) {
        return fail("fork multithreaded tracee for sibling PTRACE_INTERRUPT");
    }
    if (wait_for_job_stop(pid) != 0) {
        close(block_write_fd);
        kill_tracee(pid);
        return 1;
    }

    if (ptrace(PTRACE_SEIZE, sibling_tid, NULL, NULL) != 0) {
        close(block_write_fd);
        kill_tracee(pid);
        return fail("PTRACE_SEIZE job-stopped sibling");
    }
    if (ptrace(PTRACE_INTERRUPT, sibling_tid, NULL, NULL) != 0) {
        close(block_write_fd);
        kill_tracee(pid);
        return fail("PTRACE_INTERRUPT job-stopped sibling");
    }
    int result = expect_ptrace_event(
        sibling_tid, PTRACE_EVENT_STOP,
        "PTRACE_INTERRUPT reports EVENT_STOP for a running sibling of a job-stop waiter");
    close(block_write_fd);
    kill_tracee(pid);
    return result;
}

int main(void)
{
    printf("PTRACE_SEIZE syscall-stop regression\n");

    if (test_seize_rejects_nonzero_addr() != 0
        || test_interrupt_rejects_attach_tracee() != 0
        || test_interrupt_job_stopped_sibling() != 0) {
        return 1;
    }

    pid_t pid = fork_spinning_tracee();
    if (pid < 0) {
        return fail("fork tracee");
    }

    if (ptrace(PTRACE_SEIZE, pid, NULL, (void *)(uintptr_t)PTRACE_O_TRACESYSGOOD) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SEIZE with PTRACE_O_TRACESYSGOOD");
    }
    if (ptrace(PTRACE_INTERRUPT, pid, NULL, NULL) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_INTERRUPT");
    }
    if (expect_ptrace_stop(pid, SIGTRAP, PTRACE_EVENT_STOP,
                            "PTRACE_INTERRUPT reports PTRACE_EVENT_STOP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }
    if (expect_getsiginfo_signal(pid, SIGTRAP,
                                 "PTRACE_EVENT_STOP PTRACE_GETSIGINFO returns SIGTRAP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }

    struct x86_64_user_regs entry_regs = {0};
    if (resume_to_getpid_entry(pid, &entry_regs) != 0) {
        kill_tracee(pid);
        return 1;
    }
    entry_regs.orig_rax = SYS_getppid;
    if (write_tracee_regs(pid, &entry_regs) != 0) {
        kill_tracee(pid);
        return 1;
    }
    struct x86_64_user_regs replaced_entry_regs = {0};
    if (read_tracee_regs(pid, &replaced_entry_regs) != 0) {
        kill_tracee(pid);
        return 1;
    }
    printf("PTRACE_REPLACED_ENTRY_REGS: orig_rax=%lld rax=%lld\n",
           (long long)(int64_t)replaced_entry_regs.orig_rax,
           (long long)(int64_t)replaced_entry_regs.rax);
    if (replaced_entry_regs.orig_rax != SYS_getppid
        || (int64_t)replaced_entry_regs.rax != -ENOSYS) {
        printf("FAIL: replacement syscall entry view: orig_rax=%lld rax=%lld expected "
               "orig_rax=%ld rax=%d\n",
               (long long)(int64_t)replaced_entry_regs.orig_rax,
               (long long)(int64_t)replaced_entry_regs.rax, (long)SYS_getppid, -ENOSYS);
        kill_tracee(pid);
        return 1;
    }

    if (ptrace(PTRACE_SYSCALL, pid, NULL, (void *)(uintptr_t)SIGSTOP) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SYSCALL resumes syscall stop with SIGSTOP");
    }
    if (expect_ptrace_stop(pid, SIGTRAP | 0x80, 0,
                            "PTRACE_SYSCALL preserves the syscall-exit stop before SIGSTOP")
        != 0) {
        kill_tracee(pid);
        return 1;
    }
    struct x86_64_user_regs exit_regs = {0};
    if (read_tracee_regs(pid, &exit_regs) != 0) {
        kill_tracee(pid);
        return 1;
    }
    printf("PTRACE_REPLACED_SYSCALL_REGS: orig_rax=%lld rax=%lld\n",
           (long long)(int64_t)exit_regs.orig_rax, (long long)(int64_t)exit_regs.rax);
    if (exit_regs.orig_rax != SYS_getppid || (pid_t)exit_regs.rax != getpid()) {
        printf("FAIL: replacement syscall result: orig_rax=%lld rax=%lld expected "
               "orig_rax=%ld rax=%d\n",
               (long long)(int64_t)exit_regs.orig_rax,
               (long long)(int64_t)exit_regs.rax, (long)SYS_getppid, getpid());
        kill_tracee(pid);
        return 1;
    }
    if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) != 0) {
        kill_tracee(pid);
        return fail("PTRACE_SYSCALL resumes syscall-exit stop");
    }
    if (expect_ptrace_stop(pid, SIGSTOP, 0,
                            "PTRACE_SYSCALL signal injection reports SIGSTOP delivery stop")
        != 0) {
        kill_tracee(pid);
        return 1;
    }

    printf("PASS: PTRACE_SEIZE arguments, interrupt mode, and syscall-stop sequence\n");
    kill_tracee(pid);
    return 0;
}
