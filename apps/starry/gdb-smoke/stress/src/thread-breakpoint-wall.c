#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif
#ifndef PTRACE_SETOPTIONS
#define PTRACE_SETOPTIONS 0x4200
#endif
#ifndef PTRACE_GETEVENTMSG
#define PTRACE_GETEVENTMSG 0x4201
#endif
#ifndef PTRACE_GETSIGINFO
#define PTRACE_GETSIGINFO 0x4202
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif
#ifndef PTRACE_SINGLESTEP
#define PTRACE_SINGLESTEP 9
#endif
#ifndef PTRACE_O_TRACECLONE
#define PTRACE_O_TRACECLONE 0x00000008
#endif
#ifndef PTRACE_EVENT_CLONE
#define PTRACE_EVENT_CLONE 3
#endif
#ifndef __WALL
#define __WALL 0x40000000
#endif

#define RISCV_EBREAK_INSN 0x00100073UL

struct trace_addrs {
    uintptr_t text_addr;
};

struct riscv_user_regs {
    unsigned long pc;
    unsigned long ra;
    unsigned long sp;
    unsigned long gp;
    unsigned long tp;
    unsigned long t0;
    unsigned long t1;
    unsigned long t2;
    unsigned long s0;
    unsigned long s1;
    unsigned long a0;
    unsigned long a1;
    unsigned long a2;
    unsigned long a3;
    unsigned long a4;
    unsigned long a5;
    unsigned long a6;
    unsigned long a7;
    unsigned long s2;
    unsigned long s3;
    unsigned long s4;
    unsigned long s5;
    unsigned long s6;
    unsigned long s7;
    unsigned long s8;
    unsigned long s9;
    unsigned long s10;
    unsigned long s11;
    unsigned long t3;
    unsigned long t4;
    unsigned long t5;
    unsigned long t6;
};

struct worker_arg {
    int id;
    int result;
};

__attribute__((noinline, aligned(8))) static int thread_marker(int id)
{
    volatile int marker_id = id;
    return marker_id + 100;
}

static void *worker_main(void *arg)
{
    struct worker_arg *worker = arg;
    worker->result = thread_marker(worker->id);
    return NULL;
}

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int mem_read_word(int fd, uintptr_t addr, unsigned long *out)
{
    ssize_t bytes = pread(fd, out, sizeof(*out), (off_t)addr);
    if (bytes != (ssize_t)sizeof(*out)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

static int mem_write_word(int fd, uintptr_t addr, unsigned long word)
{
    ssize_t bytes = pwrite(fd, &word, sizeof(word), (off_t)addr);
    if (bytes != (ssize_t)sizeof(word)) {
        errno = bytes < 0 ? errno : EIO;
        return -1;
    }
    return 0;
}

static int get_regs(pid_t tid, struct riscv_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_GETREGSET, tid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("getregset stopped tid");
    }
    if (iov.iov_len == 0 || regs->pc == 0) {
        printf("FAIL: invalid stopped tid regs iov_len=%ld pc=%#lx\n", (long)iov.iov_len,
               regs->pc);
        return 1;
    }
    return 0;
}

static int set_regs(pid_t tid, struct riscv_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_SETREGSET, tid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("setregset stopped tid");
    }
    return 0;
}

static int inspect_and_continue_stopped_thread(pid_t new_tid)
{
    siginfo_t siginfo;
    if (ptrace(PTRACE_GETSIGINFO, new_tid, NULL, &siginfo) != 0) {
        return fail("getsiginfo new tid");
    }
    if (siginfo.si_signo != SIGSTOP) {
        printf("FAIL: expected new tid SIGSTOP siginfo, got signo=%d\n", siginfo.si_signo);
        return 1;
    }

    struct riscv_user_regs regs = {0};
    if (get_regs(new_tid, &regs) != 0) {
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, new_tid, NULL, (void *)PTRACE_O_TRACECLONE) != 0) {
        return fail("setoptions new tid");
    }
    if (ptrace(PTRACE_CONT, new_tid, NULL, NULL) != 0) {
        return fail("cont new tid");
    }

    printf("INFO: inspected and continued new tid=%ld pc=%#lx\n", (long)new_tid, regs.pc);
    return 0;
}

static int inspect_and_continue_new_thread(unsigned long new_tid)
{
    int status = 0;
    pid_t stopped = -1;
    for (int tries = 0; tries < 50; tries++) {
        stopped = waitpid((pid_t)new_tid, &status, __WALL | WNOHANG);
        if (stopped != 0) {
            break;
        }
        usleep(1000);
    }
    if (stopped != (pid_t)new_tid || !WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected new tid=%lu initial SIGSTOP before parent cont, got pid=%ld "
               "status=%#x\n",
               new_tid, (long)stopped, status);
        return 1;
    }

    if (inspect_and_continue_stopped_thread(stopped) != 0) {
        return 1;
    }
    return 0;
}

static int handle_clone_event(pid_t stopped, int *clone_count)
{
    unsigned long new_tid = 0;
    if (ptrace(PTRACE_GETEVENTMSG, stopped, NULL, &new_tid) != 0) {
        return fail("get clone event msg");
    }
    if (inspect_and_continue_new_thread(new_tid) != 0) {
        return 1;
    }
    if (ptrace(PTRACE_CONT, stopped, NULL, NULL) != 0) {
        return fail("cont thread after clone event");
    }

    (*clone_count)++;
    printf("INFO: consumed clone event %d for tid=%lu\n", *clone_count, new_tid);
    return 0;
}

static int handle_breakpoint_stop(pid_t stopped, uintptr_t marker_addr, int mem_fd,
                                  unsigned long text_word, int *breakpoint_done)
{
    siginfo_t siginfo;
    if (ptrace(PTRACE_GETSIGINFO, stopped, NULL, &siginfo) != 0) {
        return fail("getsiginfo breakpoint tid");
    }
    if (siginfo.si_signo != SIGTRAP) {
        printf("FAIL: expected SIGTRAP siginfo for breakpoint tid=%ld, got signo=%d\n",
               (long)stopped, siginfo.si_signo);
        return 1;
    }

    struct riscv_user_regs regs = {0};
    if (get_regs(stopped, &regs) != 0) {
        return 1;
    }
    if (regs.pc != marker_addr) {
        printf("FAIL: expected marker pc=%#lx from tid=%ld, got pc=%#lx\n",
               (unsigned long)marker_addr, (long)stopped, regs.pc);
        return 1;
    }
    if (*breakpoint_done) {
        printf("FAIL: observed second breakpoint stop from tid=%ld after text restore\n",
               (long)stopped);
        return 1;
    }

    printf("INFO: breakpoint stop from tid=%ld pc=%#lx\n", (long)stopped, regs.pc);
    if (mem_write_word(mem_fd, marker_addr, text_word) != 0) {
        return fail("restore marker text");
    }

    regs.pc = marker_addr;
    if (set_regs(stopped, &regs) != 0) {
        return 1;
    }
    if (ptrace(PTRACE_SINGLESTEP, stopped, NULL, NULL) != 0) {
        return fail("singlestep stopped tid");
    }

    int status = 0;
    if (waitpid(stopped, &status, __WALL) != stopped || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected singlestep SIGTRAP from tid=%ld, status=%#x\n", (long)stopped,
               status);
        return 1;
    }
    if (ptrace(PTRACE_CONT, stopped, NULL, NULL) != 0) {
        return fail("cont stopped tid after singlestep");
    }

    *breakpoint_done = 1;
    return 0;
}

static int run_trace_loop(pid_t leader, uintptr_t marker_addr, int mem_fd, unsigned long text_word)
{
    int clone_count = 0;
    int breakpoint_done = 0;

    for (;;) {
        int status = 0;
        pid_t stopped = waitpid(-1, &status, __WALL);
        if (stopped < 0) {
            return fail("wait trace loop");
        }
        if (stopped == leader && WIFEXITED(status)) {
            if (WEXITSTATUS(status) != 42) {
                printf("FAIL: expected child exit 42, status=%#x\n", status);
                return 1;
            }
            if (clone_count != 2 || !breakpoint_done) {
                printf("FAIL: child exited before expected events, clone_count=%d breakpoint=%d\n",
                       clone_count, breakpoint_done);
                return 1;
            }
            return 0;
        }
        if (!WIFSTOPPED(status)) {
            printf("FAIL: expected stop or exit, got pid=%ld status=%#x\n", (long)stopped,
                   status);
            return 1;
        }
        if (WSTOPSIG(status) == SIGSTOP) {
            if (inspect_and_continue_stopped_thread(stopped) != 0) {
                return 1;
            }
            continue;
        }
        if (WSTOPSIG(status) != SIGTRAP) {
            printf("FAIL: expected SIGTRAP stop or exit, got pid=%ld status=%#x\n",
                   (long)stopped, status);
            return 1;
        }

        unsigned int event = (unsigned int)status >> 16;
        if (event == PTRACE_EVENT_CLONE) {
            if (handle_clone_event(stopped, &clone_count) != 0) {
                return 1;
            }
            continue;
        }
        if (event != 0) {
            printf("FAIL: unexpected event=%u from pid=%ld status=%#x\n", event, (long)stopped,
                   status);
            return 1;
        }
        if (handle_breakpoint_stop(stopped, marker_addr, mem_fd, text_word, &breakpoint_done)
            != 0) {
            return 1;
        }
    }
}

static void run_child(int write_fd)
{
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
        _exit(101);
    }

    struct trace_addrs addrs = {.text_addr = (uintptr_t)thread_marker};
    if (write(write_fd, &addrs, sizeof(addrs)) != (ssize_t)sizeof(addrs)) {
        _exit(102);
    }
    close(write_fd);

    if (kill(getpid(), SIGSTOP) != 0) {
        _exit(103);
    }

    pthread_t threads[2];
    struct worker_arg args[2] = {
        {.id = 1, .result = 0},
        {.id = 2, .result = 0},
    };

    for (int i = 0; i < 2; i++) {
        if (pthread_create(&threads[i], NULL, worker_main, &args[i]) != 0) {
            _exit(104);
        }
    }
    for (int i = 0; i < 2; i++) {
        if (pthread_join(threads[i], NULL) != 0) {
            _exit(105);
        }
    }

    _exit(args[0].result == 101 && args[1].result == 102 ? 42 : 106);
}

int main(void)
{
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        return fail("pipe");
    }

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }
    if (pid == 0) {
        close(pipefd[0]);
        run_child(pipefd[1]);
    }

    close(pipefd[1]);
    struct trace_addrs addrs = {0};
    if (read(pipefd[0], &addrs, sizeof(addrs)) != (ssize_t)sizeof(addrs)) {
        return fail("read trace addrs");
    }
    close(pipefd[0]);

    int status = 0;
    if (waitpid(pid, &status, __WALL) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected initial SIGSTOP, status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)PTRACE_O_TRACECLONE) != 0) {
        return fail("set traceclone option");
    }

    char mem_path[64];
    snprintf(mem_path, sizeof(mem_path), "/proc/%d/mem", pid);
    int mem_fd = open(mem_path, O_RDWR);
    if (mem_fd < 0) {
        return fail("open proc mem");
    }

    unsigned long text_word = 0;
    if (mem_read_word(mem_fd, addrs.text_addr, &text_word) != 0) {
        close(mem_fd);
        return fail("read marker text");
    }
    unsigned long breakpoint_word = (text_word & ~0xffffffffUL) | RISCV_EBREAK_INSN;
    if (mem_write_word(mem_fd, addrs.text_addr, breakpoint_word) != 0) {
        close(mem_fd);
        return fail("write marker breakpoint");
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        close(mem_fd);
        return fail("cont child to clone events");
    }
    usleep(100000);
    if (run_trace_loop(pid, addrs.text_addr, mem_fd, text_word) != 0) {
        close(mem_fd);
        return 1;
    }
    close(mem_fd);

    puts("DONE: 1 pass, 0 fail");
    return 0;
}
