#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef NT_PRSTATUS
#define NT_PRSTATUS 1
#endif
#ifndef NT_FPREGSET
#define NT_FPREGSET 2
#endif
#ifndef PTRACE_GETREGSET
#define PTRACE_GETREGSET 0x4204
#endif
#ifndef PTRACE_SETREGSET
#define PTRACE_SETREGSET 0x4205
#endif
#ifndef PTRACE_GETREGS
#define PTRACE_GETREGS 12
#endif
#ifndef PTRACE_SETREGS
#define PTRACE_SETREGS 13
#endif
#ifndef PTRACE_GETFPREGS
#define PTRACE_GETFPREGS 14
#endif
#ifndef PTRACE_SETFPREGS
#define PTRACE_SETFPREGS 15
#endif
#ifndef PTRACE_SINGLESTEP
#define PTRACE_SINGLESTEP 9
#endif
#ifndef PTRACE_ATTACH
#define PTRACE_ATTACH 16
#endif
#ifndef PTRACE_KILL
#define PTRACE_KILL 8
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
#ifndef PTRACE_SETSIGINFO
#define PTRACE_SETSIGINFO 0x4203
#endif
#ifndef PTRACE_SYSCALL
#define PTRACE_SYSCALL 24
#endif
#ifndef PTRACE_O_TRACEFORK
#define PTRACE_O_TRACEFORK 0x00000002
#endif
#ifndef PTRACE_O_TRACESYSGOOD
#define PTRACE_O_TRACESYSGOOD 0x00000001
#endif
#ifndef PTRACE_O_TRACEEXEC
#define PTRACE_O_TRACEEXEC 0x00000010
#endif
#ifndef PTRACE_O_TRACEEXIT
#define PTRACE_O_TRACEEXIT 0x00000040
#endif
#ifndef PTRACE_O_TRACEVFORKDONE
#define PTRACE_O_TRACEVFORKDONE 0x00000020
#endif
#ifndef __WALL
#define __WALL 0x40000000
#endif

#define PTRACE_EVENT_FORK 1
#define PTRACE_EVENT_CLONE 3
#define PTRACE_EVENT_EXEC 4
#define PTRACE_EVENT_VFORK_DONE 5
#define PTRACE_EVENT_EXIT 6

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

struct riscv_user_fpregs {
    unsigned long f[32];
    unsigned long fcsr;
};

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int get_regs(pid_t pid, struct riscv_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

static int set_regs(pid_t pid, struct riscv_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_SETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    return 0;
}

static int wait_stop(pid_t pid, int expected_sig)
{
    siginfo_t si;
    si.si_pid = 0;

    if (waitid(P_PID, pid, &si, WSTOPPED | WEXITED) != 0) {
        printf("FAIL: waitid errno=%d (%s)\n", errno, strerror(errno));
        return -1;
    }
    if (si.si_pid != pid) {
        printf("FAIL: waitid returned pid=%d, expected %d\n", si.si_pid, pid);
        return -1;
    }
    if (si.si_uid != 0) {
        printf("FAIL: waitid returned uid=%d, expected 0\n", si.si_uid);
        return -1;
    }
    if (si.si_code == CLD_EXITED) {
        printf("FAIL: child exited prematurely, status=%d\n", si.si_status);
        return -1;
    }
    if (si.si_code != CLD_TRAPPED) {
        printf("FAIL: expected CLD_TRAPPED, got code=%d status=%d\n",
               si.si_code, si.si_status);
        return -1;
    }
    if (expected_sig > 0 && si.si_status != expected_sig) {
        printf("FAIL: expected signal %d, got %d\n", expected_sig, si.si_status);
        return -1;
    }
    return 0;
}

__attribute__((naked, noinline, aligned(4))) static void ss_step_target(void)
{
    __asm__ volatile(
        "addi a0, zero, 123\n"
        "ebreak\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_branch_target(void)
{
    __asm__ volatile(
        "beq a0, a1, 1f\n"
        "addi a2, zero, 111\n"
        "ebreak\n"
        "1:\n"
        "addi a2, zero, 222\n"
        "ebreak\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_jal_target(void)
{
    __asm__ volatile(
        "jal ra, 1f\n"
        "addi a2, zero, 111\n"
        "ebreak\n"
        "1:\n"
        "addi a2, zero, 333\n"
        "ebreak\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_jalr_target(void)
{
    __asm__ volatile(
        "jalr zero, 0(a3)\n"
        "addi a2, zero, 111\n"
        "ebreak\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_jalr_landing(void)
{
    __asm__ volatile(
        "addi a2, zero, 444\n"
        "ebreak\n");
}

__attribute__((naked, noinline, aligned(2))) static void ss_c_j_target(void)
{
    __asm__ volatile(
        ".option push\n"
        ".option rvc\n"
        "c.j 1f\n"
        "addi a2, zero, 111\n"
        "ebreak\n"
        "1:\n"
        "addi a2, zero, 555\n"
        "ebreak\n"
        ".option pop\n");
}

__attribute__((naked, noinline, aligned(2))) static void ss_c_beqz_target(void)
{
    __asm__ volatile(
        ".option push\n"
        ".option rvc\n"
        "c.beqz s0, 1f\n"
        "addi a2, zero, 111\n"
        "ebreak\n"
        "1:\n"
        "addi a2, zero, 666\n"
        "ebreak\n"
        ".option pop\n");
}

__attribute__((naked, noinline, aligned(2))) static void ss_c_jr_target(void)
{
    __asm__ volatile(
        ".option push\n"
        ".option rvc\n"
        "c.jr a3\n"
        "addi a2, zero, 111\n"
        "ebreak\n"
        ".option pop\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_c_jr_landing(void)
{
    __asm__ volatile(
        "addi a2, zero, 777\n"
        "ebreak\n");
}

static int wait_sigtrap(pid_t pid)
{
    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP, status=%#x\n", status);
        return -1;
    }
    return 0;
}

static int check_singlestep_stops_before_target_side_effect(pid_t pid,
                                                           struct riscv_user_regs *regs,
                                                           unsigned long pc,
                                                           unsigned long a0,
                                                           unsigned long a1,
                                                           unsigned long s0,
                                                           unsigned long a3,
                                                           unsigned long expected_a2)
{
    memset(regs, 0, sizeof(*regs));
    if (get_regs(pid, regs) != 0) {
        return fail("getregset before control-flow singlestep");
    }
    regs->pc = pc;
    regs->a0 = a0;
    regs->a1 = a1;
    regs->a2 = 0;
    regs->a3 = a3;
    regs->s0 = s0;
    if (set_regs(pid, regs) != 0) {
        return fail("setregset control-flow singlestep target");
    }
    memset(regs, 0, sizeof(*regs));
    if (get_regs(pid, regs) != 0) {
        return fail("getregset after setting control-flow singlestep target");
    }
    if (regs->pc != pc || regs->a0 != a0 || regs->a1 != a1 || regs->a2 != 0
        || regs->a3 != a3 || regs->s0 != s0) {
        printf("FAIL: setregset control-flow readback pc=%#lx a0=%#lx a1=%#lx a2=%#lx "
               "a3=%#lx s0=%#lx\n",
               regs->pc, regs->a0, regs->a1, regs->a2, regs->a3, regs->s0);
        return 1;
    }

    if (ptrace(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("singlestep control-flow instruction");
    }
    if (wait_sigtrap(pid) != 0) {
        return 1;
    }
    memset(regs, 0, sizeof(*regs));
    if (get_regs(pid, regs) != 0) {
        return fail("getregset after control-flow singlestep");
    }
    if (regs->a2 != 0) {
        printf("FAIL: singlestep ran target side effect early, pc=%#lx s0=%#lx a2=%#lx\n",
               regs->pc, regs->s0, regs->a2);
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after control-flow singlestep");
    }
    if (wait_sigtrap(pid) != 0) {
        return 1;
    }
    memset(regs, 0, sizeof(*regs));
    if (get_regs(pid, regs) != 0) {
        return fail("getregset after control-flow landing");
    }
    if (regs->a2 != expected_a2) {
        printf("FAIL: control-flow landing a2=%#lx expected %#lx\n",
               regs->a2, expected_a2);
        return 1;
    }
    return 0;
}

static int test_singlestep(void)
{
    printf("test 1: PTRACE_SINGLESTEP\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }

    struct riscv_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset initial");
    }

    regs.pc = (unsigned long)ss_step_target;
    regs.a0 = 0;
    if (set_regs(pid, &regs) != 0) {
        return fail("setregset singlestep target");
    }

    if (ptrace(PTRACE_SINGLESTEP, pid, NULL, NULL) != 0) {
        return fail("singlestep known instruction");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: expected SIGTRAP after known single-step, status=%#x\n", status);
        return 1;
    }
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset after known single-step");
    }
    if (regs.a0 != 123) {
        printf("FAIL: singlestep skipped instruction, a0=%#lx expected 123\n", regs.a0);
        return 1;
    }

    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_branch_target, 7, 7, 0, 0, 222)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_jal_target, 0, 0, 0, 0, 333)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_jalr_target, 0, 0, 0,
            (unsigned long)ss_jalr_landing, 444)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_c_j_target, 0, 0, 0, 0, 555)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_c_beqz_target, 0, 0, 0, 0, 666)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_c_jr_target, 0, 0, 0,
            (unsigned long)ss_c_jr_landing, 777)
        != 0) {
        return 1;
    }

    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("kill after known single-step");
    }
    waitpid(pid, &status, 0);
    printf("  ok: singlestep handled 32-bit and compressed control-flow instructions\n");
    return 0;
}

static int test_waitid_wstopped(void)
{
    printf("test 2: waitid with WSTOPPED\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(7);
    }

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    if (waitid(P_PID, pid, &si, WSTOPPED | WEXITED) != 0) {
        return fail("waitid WSTOPPED");
    }
    if (si.si_code != CLD_TRAPPED || si.si_status != SIGSTOP) {
        printf("FAIL: waitid si_code=%d si_status=%d\n", si.si_code, si.si_status);
        return 1;
    }
    if (si.si_uid != 0) {
        printf("FAIL: waitid si_uid=%d, expected 0\n", si.si_uid);
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont");
    }
    int wstatus = 0;
    if (waitpid(pid, &wstatus, 0) != pid || !WIFEXITED(wstatus)
        || WEXITSTATUS(wstatus) != 7) {
        printf("FAIL: waitid child exit\n");
        return 1;
    }

    printf("  ok: waitid(WSTOPPED) correctly reported CLD_TRAPPED/SIGSTOP\n");
    return 0;
}

static int test_fpregs(void)
{
    printf("test 3: NT_FPREGSET get/set\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);

        unsigned long f0_bits = 0;
        __asm__ volatile("fmv.x.d %0, f0" : "=r"(f0_bits));
        _exit(f0_bits == 0x4008000000000000UL ? 42 : 1);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }

    struct riscv_user_fpregs fpregs;
    struct iovec iov = {.iov_base = &fpregs, .iov_len = sizeof(fpregs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        printf("  SKIP: NT_FPREGSET not supported (errno=%d)\n", errno);
        ptrace(PTRACE_CONT, pid, NULL, NULL);
        waitpid(pid, &status, 0);
        return 0;
    }

    int all_zero = 1;
    for (int i = 0; i < 32; i++) {
        if (fpregs.f[i] != 0) {
            all_zero = 0;
            break;
        }
    }
    printf("  ok: NT_FPREGSET read, %s, fcsr=%#lx, iov_len=%zu\n",
           all_zero ? "all-zero (FP unused)" : "has non-zero values",
           fpregs.fcsr, (size_t)iov.iov_len);

    fpregs.f[0] = 0x4008000000000000UL;
    fpregs.f[1] = 0xCAFEBABEu;
    iov.iov_len = sizeof(fpregs);
    if (ptrace(PTRACE_SETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        return fail("setregset fpregs");
    }

    struct riscv_user_fpregs fpregs2;
    iov.iov_base = &fpregs2;
    iov.iov_len = sizeof(fpregs2);
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        return fail("getregset fpregs after set");
    }
    if (fpregs2.f[0] != 0x4008000000000000UL || fpregs2.f[1] != 0xCAFEBABEu) {
        printf("FAIL: fpregs write-back mismatch: f[0]=%#lx f[1]=%#lx\n",
               fpregs2.f[0], fpregs2.f[1]);
        return 1;
    }
    printf("  ok: NT_FPREGSET write and read-back match\n");

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: FP register state was not restored to tracee, status=%#x\n", status);
        return 1;
    }
    printf("  ok: NT_FPREGSET restored f0 into tracee execution state\n");
    return 0;
}

static void wait_for_release_then_exit(int fd, int exit_code)
{
    char c;
    while (read(fd, &c, 1) < 0 && errno == EINTR) {
    }
    close(fd);
    _exit(exit_code);
}

static int release_child(int fd)
{
    char c = 'R';
    if (write(fd, &c, 1) != 1) {
        return fail("write release pipe");
    }
    close(fd);
    return 0;
}

static int test_attach(void)
{
    printf("test 4: PTRACE_ATTACH\n");

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) != 0) {
        return fail("ready pipe");
    }
    if (pipe(release_pipe) != 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return fail("release pipe");
    }

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        char c = 'A';
        if (write(ready_pipe[1], &c, 1) != 1) {
            _exit(100);
        }
        close(ready_pipe[1]);
        wait_for_release_then_exit(release_pipe[0], 42);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    char c;
    if (read(ready_pipe[0], &c, 1) != 1) {
        return fail("read pipe from child");
    }
    close(ready_pipe[0]);

    if (ptrace(PTRACE_ATTACH, pid, NULL, NULL) != 0) {
        return fail("ptrace attach");
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid after attach");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: attach stop status=%#x, expected SIGSTOP\n", status);
        return 1;
    }

    struct riscv_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset after attach");
    }
    printf("  ok: attached, child stopped, pc=%#lx sp=%#lx\n",
           regs.pc, regs.sp);

    if (ptrace(PTRACE_DETACH, pid, NULL, NULL) != 0) {
        return fail("detach");
    }
    if (release_child(release_pipe[1]) != 0) {
        return 1;
    }

    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: after detach, status=%#x\n", status);
        return 1;
    }
    printf("  ok: detached, child exited 42\n");
    return 0;
}

static int test_setoptions(void)
{
    printf("test 5: PTRACE_SETOPTIONS (TRACEFORK)\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        pid_t grandchild = fork();
        if (grandchild == 0) {
            _exit(0);
        }
        if (grandchild > 0) {
            waitpid(grandchild, NULL, 0);
        }
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }

    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACEFORK) != 0) {
        return fail("setoptions");
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont");
    }

    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid fork event");
    }
    if (!WIFSTOPPED(status)) {
        printf("FAIL: expected stopped after fork, status=%#x\n", status);
        return 1;
    }

    int event = (status >> 16) & 0xFF;
    if (event != PTRACE_EVENT_FORK) {
        printf("FAIL: expected PTRACE_EVENT_FORK, got event=%d status=%#x\n",
               event, status);
        return 1;
    }
    unsigned long event_msg = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &event_msg) != 0) {
        return fail("geteventmsg");
    }
    if (event_msg == 0) {
        printf("FAIL: expected non-zero fork event message\n");
        return 1;
    }
    printf("  ok: PTRACE_O_TRACEFORK delivered event child=%lu\n", event_msg);

    int grandchild_status = 0;
    if (waitpid((pid_t)event_msg, &grandchild_status, 0) != (pid_t)event_msg) {
        return fail("waitpid traced fork child");
    }
    if (!WIFSTOPPED(grandchild_status)) {
        printf("FAIL: traced fork child was not stopped, status=%#x\n",
               grandchild_status);
        return 1;
    }
    if (ptrace(PTRACE_KILL, (pid_t)event_msg, NULL, NULL) != 0) {
        return fail("kill traced fork child");
    }
    if (waitpid((pid_t)event_msg, &grandchild_status, 0) != (pid_t)event_msg) {
        return fail("waitpid killed traced fork child");
    }
    if (!WIFSIGNALED(grandchild_status) || WTERMSIG(grandchild_status) != SIGKILL) {
        printf("FAIL: killed traced fork child status=%#x\n", grandchild_status);
        return 1;
    }
    printf("  ok: traced fork child stopped and PTRACE_KILL reported SIGKILL\n");

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("final cont");
    }
    waitpid(pid, &status, 0);
    return 0;
}

__attribute__((naked, noinline, aligned(4))) static void setregs_pc_landing(void)
{
    __asm__ volatile(
        "addi a2, zero, 77\n"
        "ebreak\n");
}

static int test_setregs(void)
{
    printf("test 6: SETREGSET modify PC\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }

    struct riscv_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    regs.pc = (unsigned long)setregs_pc_landing;
    regs.a2 = 0;
    if (set_regs(pid, &regs) != 0) {
        return fail("setregset pc landing");
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after setregset");
    }
    if (wait_sigtrap(pid) != 0) {
        return 1;
    }
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset after setregset pc landing");
    }
    if (regs.a2 != 77) {
        printf("FAIL: SETREGSET PC landing a2=%#lx expected 77\n", regs.a2);
        return 1;
    }
    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("kill after setregset");
    }
    waitpid(pid, &status, 0);

    printf("  ok: SETREGSET modified PC to a known landing block\n");
    return 0;
}

static int test_waitid_attach(void)
{
    printf("test 7: waitid(WSTOPPED) after ATTACH\n");

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) != 0) {
        return fail("ready pipe");
    }
    if (pipe(release_pipe) != 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return fail("release pipe");
    }

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        char c = 'x';
        if (write(ready_pipe[1], &c, 1) != 1) {
            _exit(100);
        }
        close(ready_pipe[1]);
        wait_for_release_then_exit(release_pipe[0], 0);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    char c;
    if (read(ready_pipe[0], &c, 1) != 1) {
        return fail("read pipe");
    }
    close(ready_pipe[0]);

    if (ptrace(PTRACE_ATTACH, pid, NULL, NULL) != 0) {
        return fail("attach");
    }

    if (wait_stop(pid, SIGSTOP) != 0) {
        return 1;
    }

    printf("  ok: waitid(WSTOPPED) after ATTACH got CLD_TRAPPED/SIGSTOP\n");

    if (ptrace(PTRACE_DETACH, pid, NULL, NULL) != 0) {
        return fail("detach");
    }
    if (release_child(release_pipe[1]) != 0) {
        return 1;
    }
    int wstatus = 0;
    if (waitpid(pid, &wstatus, 0) != pid || !WIFEXITED(wstatus)
        || WEXITSTATUS(wstatus) != 0) {
        printf("FAIL: after waitid detach, status=%#x\n", wstatus);
        return 1;
    }
    return 0;
}

static int test_syscall_trace(void)
{
    printf("test 8: PTRACE_SYSCALL entry/exit stops\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        pid_t self = getpid();
        _exit(self == getpid() ? 42 : 1);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACESYSGOOD) != 0) {
        return fail("setoptions TRACESYSGOOD");
    }

    struct riscv_user_regs regs;
    int expect_entry = 1;
    int saw_getpid = 0;
    for (int stops = 0; stops < 80 && !saw_getpid; stops++) {
        if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) != 0) {
            return fail(expect_entry ? "ptrace syscall entry" : "ptrace syscall exit");
        }
        if (wait_stop(pid, SIGTRAP | 0x80) != 0) {
            printf("FAIL: expected syscall waitid SIGTRAP|0x80\n");
            return 1;
        }

        memset(&regs, 0, sizeof(regs));
        if (get_regs(pid, &regs) != 0) {
            return fail("getregset syscall stop");
        }

        if (expect_entry && regs.a7 == SYS_getpid) {
            if (ptrace(PTRACE_SYSCALL, pid, NULL, NULL) != 0) {
                return fail("ptrace getpid exit");
            }
            if (wait_stop(pid, SIGTRAP | 0x80) != 0) {
                printf("FAIL: expected getpid-exit waitid SIGTRAP|0x80\n");
                return 1;
            }
            memset(&regs, 0, sizeof(regs));
            if (get_regs(pid, &regs) != 0) {
                return fail("getregset getpid exit");
            }
            if (regs.a0 != (unsigned long)pid) {
                printf("FAIL: getpid exit a0=%lu, expected pid=%d\n", regs.a0, pid);
                return 1;
            }
            saw_getpid = 1;
            break;
        }

        expect_entry = !expect_entry;
    }

    if (!saw_getpid) {
        printf("FAIL: did not observe SYS_getpid entry/exit stops\n");
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after syscall trace");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: syscall trace child exit status=%#x\n", status);
        return 1;
    }
    printf("  ok: PTRACE_SYSCALL reported entry and exit stops\n");
    return 0;
}

static int test_signal_resume(void)
{
    printf("test 9: PTRACE_CONT signal suppression/injection\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork suppress child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        raise(SIGUSR1);
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial suppress stop\n");
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to SIGUSR1 stop");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGUSR1) {
        printf("FAIL: expected SIGUSR1 stop, status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, 0) != 0) {
        return fail("cont suppress SIGUSR1");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: CONT(0) did not suppress SIGUSR1, status=%#x\n", status);
        return 1;
    }

    pid = fork();
    if (pid < 0) {
        return fail("fork inject child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        raise(SIGUSR1);
        _exit(43);
    }

    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial inject stop\n");
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont inject child to SIGUSR1 stop");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGUSR1) {
        printf("FAIL: expected inject SIGUSR1 stop, status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, (void *)(long)SIGTERM) != 0) {
        return fail("cont inject SIGTERM");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("wait inject SIGTERM result");
    }
    if (WIFSTOPPED(status)) {
        printf("FAIL: injected SIGTERM caused an extra ptrace stop, status=%#x\n", status);
        ptrace(PTRACE_KILL, pid, NULL, NULL);
        waitpid(pid, &status, 0);
        return 1;
    }
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM) {
        printf("FAIL: injected SIGTERM was not delivered, status=%#x\n", status);
        return 1;
    }

    printf("  ok: CONT(0) suppresses and CONT(SIGTERM) injects without extra stop\n");
    return 0;
}

__attribute__((naked, noinline, aligned(4))) static void legacy_setregs_landing(void)
{
    __asm__ volatile(
        "addi a2, zero, 88\n"
        "ebreak\n");
}

static int test_legacy_regsets(void)
{
    printf("test 10: legacy PTRACE_GETREGS/GETFPREGS\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork legacy regs");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(1);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial legacy regs stop\n");
        return 1;
    }

    struct riscv_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (ptrace(PTRACE_GETREGS, pid, NULL, &regs) != 0) {
        return fail("legacy getregs");
    }
    if (regs.pc == 0 || regs.sp == 0) {
        printf("FAIL: legacy GETREGS returned pc=%#lx sp=%#lx\n", regs.pc, regs.sp);
        return 1;
    }

    regs.pc = (unsigned long)legacy_setregs_landing;
    regs.a2 = 0;
    if (ptrace(PTRACE_SETREGS, pid, NULL, &regs) != 0) {
        return fail("legacy setregs");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("legacy cont after setregs");
    }
    if (wait_sigtrap(pid) != 0) {
        return 1;
    }
    memset(&regs, 0, sizeof(regs));
    if (ptrace(PTRACE_GETREGS, pid, NULL, &regs) != 0) {
        return fail("legacy getregs after landing");
    }
    if (regs.a2 != 88) {
        printf("FAIL: legacy SETREGS landing a2=%#lx expected 88\n", regs.a2);
        return 1;
    }
    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("legacy kill regs child");
    }
    waitpid(pid, &status, 0);

    pid = fork();
    if (pid < 0) {
        return fail("fork legacy fpregs");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);

        unsigned long f0_bits = 0;
        __asm__ volatile("fmv.x.d %0, f0" : "=r"(f0_bits));
        _exit(f0_bits == 0x4010000000000000UL ? 42 : 1);
    }

    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial legacy fpregs stop\n");
        return 1;
    }

    struct riscv_user_fpregs fpregs;
    memset(&fpregs, 0, sizeof(fpregs));
    if (ptrace(PTRACE_GETFPREGS, pid, NULL, &fpregs) != 0) {
        return fail("legacy getfpregs");
    }
    fpregs.f[0] = 0x4010000000000000UL;
    fpregs.f[2] = 0x12345678UL;
    if (ptrace(PTRACE_SETFPREGS, pid, NULL, &fpregs) != 0) {
        return fail("legacy setfpregs");
    }

    struct riscv_user_fpregs fpregs2;
    memset(&fpregs2, 0, sizeof(fpregs2));
    if (ptrace(PTRACE_GETFPREGS, pid, NULL, &fpregs2) != 0) {
        return fail("legacy getfpregs after set");
    }
    if (fpregs2.f[0] != 0x4010000000000000UL || fpregs2.f[2] != 0x12345678UL) {
        printf("FAIL: legacy fpregs mismatch f0=%#lx f2=%#lx\n",
               fpregs2.f[0], fpregs2.f[2]);
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("legacy cont fpregs child");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: legacy SETFPREGS was not restored, status=%#x\n", status);
        return 1;
    }

    printf("  ok: legacy register requests share regset semantics\n");
    return 0;
}

static int test_siginfo_roundtrip(void)
{
    printf("test 11: PTRACE_GETSIGINFO/SETSIGINFO roundtrip\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork siginfo child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        raise(SIGUSR1);
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial siginfo stop\n");
        return 1;
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to SIGUSR1 for siginfo");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGUSR1) {
        printf("FAIL: expected SIGUSR1 siginfo stop, status=%#x\n", status);
        return 1;
    }

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    if (ptrace(PTRACE_GETSIGINFO, pid, NULL, &si) != 0) {
        return fail("getsiginfo");
    }
    if (si.si_signo != SIGUSR1) {
        printf("FAIL: GETSIGINFO signo=%d expected %d\n", si.si_signo, SIGUSR1);
        return 1;
    }

    si.si_code = SI_QUEUE;
    if (ptrace(PTRACE_SETSIGINFO, pid, NULL, &si) != 0) {
        return fail("setsiginfo");
    }

    siginfo_t si2;
    memset(&si2, 0, sizeof(si2));
    if (ptrace(PTRACE_GETSIGINFO, pid, NULL, &si2) != 0) {
        return fail("getsiginfo after set");
    }
    if (si2.si_signo != SIGUSR1 || si2.si_code != SI_QUEUE) {
        printf("FAIL: siginfo after set signo=%d code=%d\n", si2.si_signo, si2.si_code);
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, 0) != 0) {
        return fail("cont suppress SIGUSR1 after siginfo set");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: siginfo child exit status=%#x\n", status);
        return 1;
    }

    printf("  ok: SETSIGINFO updates the current ptrace stop siginfo\n");
    return 0;
}

static int test_traceexit_event(void)
{
    printf("test 12: PTRACE_O_TRACEEXIT event\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork traceexit child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial traceexit stop\n");
        return 1;
    }

    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACEEXIT) != 0) {
        return fail("setoptions TRACEEXIT");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to traceexit event");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid traceexit event");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: traceexit stop status=%#x\n", status);
        return 1;
    }

    int event = (status >> 16) & 0xFF;
    if (event != PTRACE_EVENT_EXIT) {
        printf("FAIL: expected PTRACE_EVENT_EXIT, got event=%d status=%#x\n",
               event, status);
        return 1;
    }

    unsigned long event_msg = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &event_msg) != 0) {
        return fail("geteventmsg traceexit");
    }
    if (event_msg != (42UL << 8)) {
        printf("FAIL: traceexit event msg=%#lx expected %#lx\n",
               event_msg, 42UL << 8);
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after traceexit event");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: traceexit final status=%#x\n", status);
        return 1;
    }

    printf("  ok: TRACEEXIT stopped before final zombie state\n");
    return 0;
}

static char clone_thread_stack[16384] __attribute__((aligned(16)));

static long raw_clone_thread(void *stack_top)
{
    register long a0 __asm__("a0") =
        CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
    register long a1 __asm__("a1") = (long)stack_top;
    register long a2 __asm__("a2") = 0;
    register long a3 __asm__("a3") = 0;
    register long a4 __asm__("a4") = 0;
    register long a7 __asm__("a7") = SYS_clone;

    __asm__ volatile(
        "ecall\n"
        "bnez a0, 1f\n"
        "li a7, 93\n"
        "li a0, 0\n"
        "ecall\n"
        "1:\n"
        : "+r"(a0)
        : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7)
        : "memory");
    return a0;
}

static int test_traceclone_thread_event(void)
{
    printf("test 13: PTRACE_O_TRACECLONE thread event\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork traceclone child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);

        void *stack_top = clone_thread_stack + sizeof(clone_thread_stack);
        long tid = raw_clone_thread(stack_top);
        if (tid < 0) {
            _exit(2);
        }
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial traceclone stop\n");
        return 1;
    }

    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACECLONE) != 0) {
        return fail("setoptions TRACECLONE");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to traceclone event");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid traceclone event");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: traceclone stop status=%#x\n", status);
        return 1;
    }

    int event = (status >> 16) & 0xFF;
    if (event != PTRACE_EVENT_CLONE) {
        printf("FAIL: expected PTRACE_EVENT_CLONE, got event=%d status=%#x\n",
               event, status);
        return 1;
    }

    unsigned long event_msg = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &event_msg) != 0) {
        return fail("geteventmsg traceclone");
    }
    if (event_msg == 0) {
        printf("FAIL: expected non-zero clone event message\n");
        return 1;
    }

    if (waitpid((pid_t)event_msg, &status, __WALL) != (pid_t)event_msg
        || !WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
        printf("FAIL: expected traced clone tid %lu initial SIGSTOP, status=%#x\n", event_msg,
               status);
        return 1;
    }
    if (ptrace(PTRACE_CONT, (pid_t)event_msg, NULL, NULL) != 0) {
        return fail("cont traceclone child tid");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after traceclone event");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)) {
        printf("FAIL: traceclone final status=%#x\n", status);
        return 1;
    }

    printf("  ok: TRACECLONE reported thread tid=%lu\n", event_msg);
    return 0;
}

static int test_tracevforkdone_event(void)
{
    printf("test 14: PTRACE_O_TRACEVFORKDONE event\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork tracevforkdone child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);

        pid_t child = vfork();
        if (child == 0) {
            _exit(0);
        }
        if (child < 0) {
            _exit(2);
        }
        _exit(42);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial tracevforkdone stop\n");
        return 1;
    }

    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACEVFORKDONE) != 0) {
        return fail("setoptions TRACEVFORKDONE");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to tracevforkdone event");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("waitpid tracevforkdone event");
    }
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGTRAP) {
        printf("FAIL: tracevforkdone stop status=%#x\n", status);
        return 1;
    }

    int event = (status >> 16) & 0xFF;
    if (event != PTRACE_EVENT_VFORK_DONE) {
        printf("FAIL: expected PTRACE_EVENT_VFORK_DONE, got event=%d status=%#x\n",
               event, status);
        return 1;
    }

    unsigned long event_msg = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &event_msg) != 0) {
        return fail("geteventmsg tracevforkdone");
    }
    if (event_msg == 0) {
        printf("FAIL: expected non-zero vfork-done event message\n");
        return 1;
    }

    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont after tracevforkdone event");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
        || WEXITSTATUS(status) != 42) {
        printf("FAIL: tracevforkdone final status=%#x\n", status);
        return 1;
    }

    printf("  ok: TRACEVFORKDONE reported child pid=%lu\n", event_msg);
    return 0;
}

static int test_ptrace_kill_reports_signaled(void)
{
    printf("test 15: PTRACE_KILL reports SIGKILL termination\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork ptrace-kill child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        for (;;) {
        }
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial ptrace-kill stop status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("ptrace kill stopped child");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("wait ptrace-kill result");
    }
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL) {
        printf("FAIL: PTRACE_KILL status=%#x is not SIGKILL-signaled\n", status);
        return 1;
    }

    printf("  ok: PTRACE_KILL wakes and reports SIGKILL\n");
    return 0;
}

static int test_sigkill_event_stopped_tracee(void)
{
    printf("test 16: SIGKILL reports termination from event stop\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork event-kill child");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);
        pid_t grandchild = fork();
        if (grandchild == 0) {
            for (;;) {
            }
        }
        for (;;) {
        }
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial event-kill stop status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, pid, NULL, (void *)(long)PTRACE_O_TRACEFORK) != 0) {
        return fail("setoptions event-kill TRACEFORK");
    }
    if (ptrace(PTRACE_CONT, pid, NULL, NULL) != 0) {
        return fail("cont to event-kill fork event");
    }
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)
        || ((status >> 16) & 0xff) != PTRACE_EVENT_FORK) {
        printf("FAIL: expected fork event before event-kill, status=%#x\n", status);
        return 1;
    }

    unsigned long event_msg = 0;
    if (ptrace(PTRACE_GETEVENTMSG, pid, NULL, &event_msg) != 0) {
        return fail("geteventmsg event-kill");
    }
    if (event_msg != 0) {
        ptrace(PTRACE_KILL, (pid_t)event_msg, NULL, NULL);
    }
    kill(pid, SIGKILL);
    if (waitpid(pid, &status, 0) != pid) {
        return fail("wait event-stopped SIGKILL result");
    }
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL) {
        printf("FAIL: event-stopped SIGKILL status=%#x is not SIGKILL-signaled\n",
               status);
        return 1;
    }

    printf("  ok: event-stopped SIGKILL reports SIGKILL\n");
    return 0;
}

static int test_traceme_rejects_second_call(void)
{
    printf("test 17: repeated PTRACE_TRACEME is rejected\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork traceme repeat");
    }
    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(10);
        }
        errno = 0;
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) == 0 || errno != EPERM) {
            _exit(11);
        }
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        return fail("wait traceme repeat");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("FAIL: repeated TRACEME child status=%#x\n", status);
        return 1;
    }

    printf("  ok: second TRACEME failed with EPERM\n");
    return 0;
}

static int test_invalid_resume_signal(void)
{
    printf("test 18: invalid ptrace resume signal is rejected\n");

    pid_t pid = fork();
    if (pid < 0) {
        return fail("fork invalid resume signal");
    }
    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(1);
        }
        raise(SIGSTOP);
        _exit(0);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: invalid-resume initial stop status=%#x\n", status);
        return 1;
    }

    errno = 0;
    if (ptrace(PTRACE_CONT, pid, NULL, (void *)(long)9999) == 0 || errno != EIO) {
        printf("FAIL: invalid resume signo errno=%d (%s)\n", errno, strerror(errno));
        ptrace(PTRACE_KILL, pid, NULL, NULL);
        waitpid(pid, &status, 0);
        return 1;
    }

    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("kill invalid-resume child");
    }
    if (waitpid(pid, &status, 0) != pid) {
        return fail("wait invalid-resume child");
    }

    printf("  ok: invalid resume signal failed with EIO\n");
    return 0;
}

int main(void)
{
    int pass = 0;
    int fail_count = 0;

    if (test_singlestep() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_waitid_wstopped() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_fpregs() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_attach() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_setoptions() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_setregs() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_waitid_attach() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_syscall_trace() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_signal_resume() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_legacy_regsets() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_siginfo_roundtrip() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_traceexit_event() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_traceclone_thread_event() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_tracevforkdone_event() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_ptrace_kill_reports_signaled() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_sigkill_event_stopped_tracee() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_traceme_rejects_second_call() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_invalid_resume_signal() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    printf("DONE: %d pass, %d fail\n", pass, fail_count);
    return fail_count > 0 ? 1 : 0;
}
