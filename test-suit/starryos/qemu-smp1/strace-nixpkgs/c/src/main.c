/*
 * test-strace-nix — multi-process ptrace tracer for Nix builder.
 *
 * Uses waitpid(-1) event loop to trace nix-build AND all child processes
 * (builders, downloaders, etc.) simultaneously.
 *
 * Only records ~30 interesting syscall types. On any child crash
 * (SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE), dumps last 50 syscalls
 * from the ring buffer and exits.
 */

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

/* ── ring buffer ── */
#define BUF_SIZE 256

typedef struct {
    pid_t pid;
    unsigned long nr;
    long rax;
    int is_exit;
} entry_t;

static entry_t buf[BUF_SIZE];
static int head = 0;
static unsigned long total = 0, traced = 0;

static void push(pid_t pid, unsigned long nr, long rax, int is_exit) {
    buf[head].pid = pid;
    buf[head].nr = nr;
    buf[head].rax = rax;
    buf[head].is_exit = is_exit;
    head = (head + 1) % BUF_SIZE;
}

static void dump(int n) {
    int s = (head - n + BUF_SIZE) % BUF_SIZE;
    for (int i = 0; i < n; i++) {
        int idx = (s + i) % BUF_SIZE;
        if (buf[idx].nr)
            printf("  TRACE[%d]: pid=%-6d nr=%-4lu ret=%ld %s\n",
                   i, buf[idx].pid, buf[idx].nr, buf[idx].rax,
                   buf[idx].is_exit ? "(exit)" : "(entry)");
    }
}

/* ── syscall filter ── */
static int interesting(unsigned long nr) {
    if (nr <= 3 || nr == 5 || nr == 6) return 1;
    if (nr >= 9 && nr <= 14) return 1;
    if (nr == 25 || nr == 28) return 1;
    if (nr >= 56 && nr <= 61) return 1;
    if (nr == 89 || nr == 97 || nr == 157 || nr == 158) return 1;
    if (nr == 160 || nr == 186 || nr == 200 || nr == 202) return 1;
    if (nr == 217 || nr == 218 || nr == 231 || nr == 233) return 1;
    if (nr == 234 || nr == 257 || nr == 262 || nr == 273) return 1;
    if (nr == 281 || nr == 291 || nr == 293 || nr == 334) return 1;
    if (nr >= 41 && nr <= 45) return 1;
    if (nr == 49 || nr == 50) return 1;
    if (nr == 35 || nr == 24) return 1;
    return 0;
}

/* ── process table ── */
#define MAX_PROCS 64

typedef struct {
    pid_t pid;
    int in_syscall;
    unsigned long last_nr;
} proc_t;

static proc_t procs[MAX_PROCS];
static int nprocs = 0;

static proc_t *find_proc(pid_t pid) {
    for (int i = 0; i < nprocs; i++)
        if (procs[i].pid == pid) return &procs[i];
    return NULL;
}

static proc_t *add_proc(pid_t pid) {
    if (nprocs >= MAX_PROCS) return NULL;
    procs[nprocs].pid = pid;
    procs[nprocs].in_syscall = 0;
    procs[nprocs].last_nr = 0;
    return &procs[nprocs++];
}

static void remove_proc(pid_t pid) {
    for (int i = 0; i < nprocs; i++) {
        if (procs[i].pid == pid) {
            procs[i] = procs[--nprocs];
            return;
        }
    }
}

/* ── signal names ── */
static const char *signame(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";
        case SIGBUS:  return "SIGBUS";
        case SIGILL:  return "SIGILL";
        case SIGFPE:  return "SIGFPE";
        case SIGKILL: return "SIGKILL";
        default:      return "SIGNAL?";
    }
}

int main(int argc, char *argv[]) {
    printf("NIX_STRACE_BEGIN\n");
    if (argc < 2) { printf("NIX_STRACE_PASSED\n"); return 0; }

    printf("NIX_STRACE_CMD:");
    for (int i = 1; i < argc; i++) printf(" %s", argv[i]);
    printf("\n");

    /* ── fork the main child (nix-build) ── */
    pid_t main_child = fork();
    if (main_child < 0) { perror("fork"); return 1; }
    if (main_child == 0) {
        ptrace(PTRACE_TRACEME, 0, NULL, NULL);
        raise(SIGSTOP);
        execvp(argv[1], &argv[1]);
        _exit(127);
    }

    /* wait for initial SIGSTOP */
    int st;
    if (waitpid(main_child, &st, 0) < 0) { perror("waitpid"); return 1; }

    /* add main child to process table */
    add_proc(main_child);

    /* set options: trace syscalls + clone/fork/vfork events */
    ptrace(PTRACE_SETOPTIONS, main_child, 0,
           PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACECLONE |
           PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK | PTRACE_O_TRACEEXIT);
    ptrace(PTRACE_SYSCALL, main_child, NULL, NULL);

    /* ── main event loop ── */
    int main_exited = 0;
    unsigned long loop_count = 0;

    while (!main_exited && nprocs > 0) {
        pid_t w = waitpid(-1, &st, 0);
        loop_count++;
        if (loop_count % 50000 == 0)
            printf("  HEARTBEAT: loop=%lu procs=%d total=%lu\n",
                   loop_count, nprocs, total);
        if (w < 0) {
            if (errno == ECHILD) break;
            if (errno == EINTR) continue;
            break;
        }

        proc_t *p = find_proc(w);
        if (!p) {
            /* unknown child — shouldn't happen, but handle gracefully */
            if (WIFSTOPPED(st))
                ptrace(PTRACE_SYSCALL, w, NULL, (void*)(long)WSTOPSIG(st));
            continue;
        }

        /* ── child exited ── */
        if (WIFEXITED(st)) {
            printf("  CHILD_EXIT: pid=%d status=%d\n", w, WEXITSTATUS(st));
            remove_proc(w);
            if (w == main_child) main_exited = 1;
            continue;
        }

        /* ── child killed by signal ── */
        if (WIFSIGNALED(st)) {
            int sig = WTERMSIG(st);
            printf("NIX_STRACE_SIGNALED: pid=%d %s (sig=%d)\n", w, signame(sig), sig);
            /* dump trace on crash signals */
            if (sig == SIGSEGV || sig == SIGABRT || sig == SIGBUS ||
                sig == SIGILL || sig == SIGFPE) {
                printf("NIX_STRACE_LAST_BEGIN\n");
                dump(50);
                printf("NIX_STRACE_LAST_END\n");
            }
            remove_proc(w);
            if (w == main_child) main_exited = 1;
            continue;
        }

        /* ── child stopped ── */
        if (!WIFSTOPPED(st)) continue;
        int stopsig = WSTOPSIG(st);

        /* PTRACE_EVENT_CLONE/FORK/VFORK */
        if (stopsig == (SIGTRAP | (PTRACE_EVENT_CLONE << 8)) ||
            stopsig == (SIGTRAP | (PTRACE_EVENT_FORK << 8)) ||
            stopsig == (SIGTRAP | (PTRACE_EVENT_VFORK << 8))) {

            unsigned long new_pid;
            if (ptrace(PTRACE_GETEVENTMSG, w, NULL, &new_pid) == 0) {
                proc_t *np = add_proc((pid_t)new_pid);
                if (np) {
                    /* set same options on new child */
                    ptrace(PTRACE_SETOPTIONS, (pid_t)new_pid, 0,
                           PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACECLONE |
                           PTRACE_O_TRACEFORK | PTRACE_O_TRACEVFORK |
                           PTRACE_O_TRACEEXIT);
                    printf("  CHILD_FORK: pid=%d parent=%d\n",
                           (int)new_pid, w);
                }
            }
            ptrace(PTRACE_SYSCALL, w, NULL, NULL);
            continue;
        }

        /* PTRACE_EVENT_EXIT */
        if (stopsig == (SIGTRAP | (PTRACE_EVENT_EXIT << 8))) {
            unsigned long exit_status;
            ptrace(PTRACE_GETEVENTMSG, w, NULL, &exit_status);
            ptrace(PTRACE_SYSCALL, w, NULL, NULL);
            continue;
        }

        /* Syscall stop (SIGTRAP | 0x80 from PTRACE_O_TRACESYSGOOD) */
        if (stopsig == (SIGTRAP | 0x80)) {
            struct user_regs_struct r;
            if (ptrace(PTRACE_GETREGS, w, NULL, &r) < 0) {
                ptrace(PTRACE_SYSCALL, w, NULL, NULL);
                continue;
            }

            total++;
            if (!p->in_syscall) {
                /* syscall entry */
                p->last_nr = r.orig_rax;
                if (interesting(p->last_nr)) {
                    push(w, p->last_nr, 0, 0);
                    traced++;
                }
            } else {
                /* syscall exit */
                if (interesting(p->last_nr)) {
                    push(w, p->last_nr, (long)r.rax, 1);
                }
            }
            p->in_syscall = !p->in_syscall;

            if (total % 50000 == 0)
                printf("  PROGRESS: %lu total (%lu traced) procs=%d\n",
                       total, traced, nprocs);

            ptrace(PTRACE_SYSCALL, w, NULL, NULL);
            continue;
        }

        /* Forward other signals to the child */
        ptrace(PTRACE_SYSCALL, w, NULL, (void*)(long)stopsig);
    }

    printf("NIX_STRACE_EXITED: total=%lu traced=%lu\n", total, traced);
    dump(20);
    printf("NIX_STRACE_PASSED\n");
    return 0;
}
