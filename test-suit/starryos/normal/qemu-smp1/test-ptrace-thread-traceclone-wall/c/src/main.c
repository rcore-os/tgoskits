#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <pthread.h>
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
#ifndef PTRACE_O_TRACECLONE
#define PTRACE_O_TRACECLONE 0x00000008
#endif
#ifndef PTRACE_EVENT_CLONE
#define PTRACE_EVENT_CLONE 3
#endif
#ifndef __WALL
#define __WALL 0x40000000
#endif

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
    volatile int result;
};

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static void *worker_main(void *arg)
{
    struct worker_arg *worker = arg;
    worker->result = worker->id + 100;
    return NULL;
}

static void run_child(void)
{
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
        _exit(101);
    }
    if (kill(getpid(), SIGSTOP) != 0) {
        _exit(102);
    }

    pthread_t threads[2];
    struct worker_arg args[2] = {
        {.id = 1, .result = 0},
        {.id = 2, .result = 0},
    };

    for (int i = 0; i < 2; i++) {
        if (pthread_create(&threads[i], NULL, worker_main, &args[i]) != 0) {
            _exit(103);
        }
    }
    for (int i = 0; i < 2; i++) {
        if (pthread_join(threads[i], NULL) != 0) {
            _exit(104);
        }
    }

    _exit(args[0].result == 101 && args[1].result == 102 ? 42 : 105);
}

static int inspect_and_continue_new_thread(unsigned long new_tid)
{
    siginfo_t wait_info = {0};
    if (waitid(P_PID, (id_t)new_tid, &wait_info, WSTOPPED | __WALL) != 0) {
        return fail("waitid exact new tid SIGSTOP");
    }
    if (wait_info.si_pid != (pid_t)new_tid || wait_info.si_code != CLD_TRAPPED
        || wait_info.si_status != SIGSTOP) {
        printf("FAIL: expected exact waitid for new tid=%lu SIGSTOP, got pid=%ld code=%d "
               "status=%d\n",
               new_tid, (long)wait_info.si_pid, wait_info.si_code, wait_info.si_status);
        return 1;
    }

    wait_info.si_pid = -1;
    if (waitid(P_PID, (id_t)new_tid, &wait_info, WSTOPPED | WNOHANG | __WALL) != 0) {
        return fail("waitid exact new tid WNOHANG");
    }
    if (wait_info.si_pid != 0) {
        printf("FAIL: repeated waitid for new tid=%lu should not report same stop again, "
               "got pid=%ld code=%d status=%d\n",
               new_tid, (long)wait_info.si_pid, wait_info.si_code, wait_info.si_status);
        return 1;
    }

    siginfo_t siginfo;
    if (ptrace(PTRACE_GETSIGINFO, (pid_t)new_tid, NULL, &siginfo) != 0) {
        return fail("getsiginfo new tid");
    }
    if (siginfo.si_signo != SIGSTOP) {
        printf("FAIL: expected new tid siginfo SIGSTOP, got signo=%d\n", siginfo.si_signo);
        return 1;
    }

    struct riscv_user_regs regs = {0};
    struct iovec iov = {.iov_base = &regs, .iov_len = sizeof(regs)};
    if (ptrace(PTRACE_GETREGSET, (pid_t)new_tid, (void *)NT_PRSTATUS, &iov) != 0) {
        return fail("getregset new tid");
    }
    if (iov.iov_len == 0 || regs.pc == 0) {
        printf("FAIL: expected non-empty regs for new tid, iov_len=%ld pc=%#lx\n",
               (long)iov.iov_len, regs.pc);
        return 1;
    }

    if (ptrace(PTRACE_SETOPTIONS, (pid_t)new_tid, NULL, (void *)PTRACE_O_TRACECLONE) != 0) {
        return fail("setoptions new tid");
    }
    if (ptrace(PTRACE_CONT, (pid_t)new_tid, NULL, NULL) != 0) {
        return fail("cont new tid");
    }

    printf("INFO: inspected and continued new traced tid=%lu pc=%#lx\n", new_tid, regs.pc);
    return 0;
}

static int consume_clone_event(pid_t pid, int *clone_count)
{
    int status = 0;
    pid_t stopped = waitpid(pid, &status, __WALL);
    if (stopped != pid || !WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected clone SIGTRAP from pid=%d, got pid=%ld status=%#x\n", pid,
               (long)stopped, status);
        return 1;
    }

    unsigned int event = (unsigned int)status >> 16;
    if (event != PTRACE_EVENT_CLONE) {
        printf("FAIL: expected PTRACE_EVENT_CLONE, got event=%u status=%#x\n", event, status);
        return 1;
    }

    unsigned long new_tid = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &new_tid) != 0) {
        return fail("get clone event msg");
    }
    if (inspect_and_continue_new_thread(new_tid) != 0) {
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after clone event");
    }

    (*clone_count)++;
    printf("INFO: consumed clone event %d for tid=%lu\n", *clone_count, new_tid);
    return 0;
}

int main(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }
    if (pid == 0) {
        run_child();
    }

    int status = 0;
    if (waitpid(pid, &status, __WALL) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected initial SIGSTOP, status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)PTRACE_O_TRACECLONE) != 0) {
        return fail("set traceclone option");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont child");
    }

    int clone_count = 0;
    while (clone_count < 2) {
        if (consume_clone_event(pid, &clone_count) != 0) {
            return 1;
        }
    }

    if (waitpid(pid, &status, __WALL) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 42) {
        printf("FAIL: expected traced child exit 42, status=%#x\n", status);
        return 1;
    }

    puts("DONE: 1 pass, 0 fail");
    return 0;
}
