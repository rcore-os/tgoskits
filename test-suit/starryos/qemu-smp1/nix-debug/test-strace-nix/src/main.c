/*
 * test-strace-nix — minimal ptrace-based syscall tracer for Nix builder.
 *
 * Usage: test-strace-nix <command> [args...]
 *
 * Forks a child, execs the command under PTRACE_SYSCALL, and logs every
 * syscall to stdout. On child crash (SIGSEGV/SIGABRT/SIGBUS), prints the
 * faulting signal and last 50 syscalls before returning success (the crash
 * is the diagnostic signal, not a test failure).
 */

#include <elf.h>
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

#define TRACE_BUF_SIZE 256

typedef struct {
    int valid;
    unsigned long nr;
    unsigned long rax;
} trace_entry_t;

static trace_entry_t trace_buf[TRACE_BUF_SIZE];
static int trace_head = 0;

static void trace_push(unsigned long nr) {
    trace_buf[trace_head].valid = 1;
    trace_buf[trace_head].nr    = nr;
    trace_buf[trace_head].rax   = 0;
    trace_head = (trace_head + 1) % TRACE_BUF_SIZE;
}

static void trace_set_ret(unsigned long rax) {
    int idx = (trace_head - 1 + TRACE_BUF_SIZE) % TRACE_BUF_SIZE;
    trace_buf[idx].rax = rax;
}

static void trace_dump(int count) {
    int start = (trace_head - count + TRACE_BUF_SIZE) % TRACE_BUF_SIZE;
    for (int i = 0; i < count; i++) {
        int idx = (start + i) % TRACE_BUF_SIZE;
        if (trace_buf[idx].valid)
            printf("  STRACE[%d]: nr=%-4lu ret=%ld\n",
                   i, trace_buf[idx].nr, (long)trace_buf[idx].rax);
    }
}

static const char *signame(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";
        case SIGBUS:  return "SIGBUS";
        case SIGILL:  return "SIGILL";
        case SIGFPE:  return "SIGFPE";
        case SIGKILL: return "SIGKILL";
        case SIGTERM: return "SIGTERM";
        case SIGCHLD: return "SIGCHLD";
        case SIGTRAP: return "SIGTRAP";
        default:      return "SIGNAL?";
    }
}

int main(int argc, char *argv[]) {
    printf("NIX_STRACE_BEGIN\n");

    if (argc < 2) {
        printf("NIX_STRACE_SKIP: no command\n");
        printf("NIX_STRACE_PASSED\n");
        return 0;
    }

    /* Print the command being traced */
    printf("NIX_STRACE_CMD:");
    for (int i = 1; i < argc; i++) printf(" %s", argv[i]);
    printf("\n");

    pid_t child = fork();
    if (child < 0) {
        printf("NIX_STRACE_ERROR: fork failed: %s\n", strerror(errno));
        return 1;
    }

    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0) {
            perror("PTRACE_TRACEME");
            _exit(1);
        }
        raise(SIGSTOP);
        execvp(argv[1], &argv[1]);
        perror("execvp");
        _exit(127);
    }

    int status;
    int in_syscall = 0;
    unsigned long g_trace_count = 0;
    unsigned long log_interval = 5000;

    /* wait for initial SIGSTOP */
    if (waitpid(child, &status, 0) < 0) {
        printf("NIX_STRACE_ERROR: waitpid: %s\n", strerror(errno));
        return 1;
    }
    if (!WIFSTOPPED(status)) {
        printf("NIX_STRACE_ERROR: child not stopped (status=0x%x)\n", status);
        return 1;
    }

    ptrace(PTRACE_SETOPTIONS, child, 0, PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACEEXIT);
    ptrace(PTRACE_SYSCALL, child, NULL, NULL);

    while (1) {
        if (waitpid(child, &status, 0) < 0) {
            printf("NIX_STRACE_ERROR: waitpid loop: %s\n", strerror(errno));
            break;
        }

        if (WIFEXITED(status)) {
            printf("NIX_STRACE_EXITED: status=%d syscalls=%lu\n",
                   WEXITSTATUS(status), g_trace_count);
            trace_dump(10);
            printf("NIX_STRACE_PASSED\n");
            return 0;
        }

        if (WIFSIGNALED(status)) {
            int sig = WTERMSIG(status);
            printf("NIX_STRACE_SIGNALED: %s (sig=%d) syscalls=%lu\n",
                   signame(sig), sig, g_trace_count);
            printf("NIX_STRACE_LAST_SYSCALLS_BEGIN\n");
            trace_dump(50);
            printf("NIX_STRACE_LAST_SYSCALLS_END\n");
            printf("NIX_STRACE_PASSED\n");
            return 0;
        }

        if (!WIFSTOPPED(status)) continue;

        int stopsig = WSTOPSIG(status);

        if (stopsig == (SIGTRAP | 0x80)) {
            struct user_regs_struct regs;
            if (ptrace(PTRACE_GETREGS, child, NULL, &regs) < 0) {
                ptrace(PTRACE_SYSCALL, child, NULL, NULL);
                continue;
            }

            if (!in_syscall) {
                trace_push(regs.orig_rax);
            } else {
                trace_set_ret(regs.rax);
                g_trace_count++;
                if (g_trace_count % log_interval == 0)
                    printf("  STRACE_PROGRESS: %lu syscalls\n", g_trace_count);
            }
            in_syscall = !in_syscall;
            ptrace(PTRACE_SYSCALL, child, NULL, NULL);
            continue;
        }

        /* Forward other signals */
        ptrace(PTRACE_SYSCALL, child, NULL, (void *)(long)stopsig);
    }

    printf("NIX_STRACE_PASSED\n");
    return 0;
}
