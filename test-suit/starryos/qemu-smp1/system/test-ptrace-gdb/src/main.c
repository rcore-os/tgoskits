#define _GNU_SOURCE

#include <elf.h>
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
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
#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
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

#define ARCH_RISCV 0
#define ARCH_AARCH64 0
#define ARCH_LOONGARCH 0

#if defined(__riscv)
#undef ARCH_RISCV
#define ARCH_RISCV 1
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
typedef struct riscv_user_regs arch_user_regs;

struct riscv_user_fpregs {
    unsigned long f[32];
    unsigned long fcsr;
};
typedef struct riscv_user_fpregs arch_user_fpregs;

#define PTRACE_GDB_REG_PC(r) ((r)->pc)
#define PTRACE_GDB_REG_SP(r) ((r)->sp)
#define PTRACE_GDB_REG_A0(r) ((r)->a0)
#define PTRACE_GDB_REG_A1(r) ((r)->a1)
#define PTRACE_GDB_REG_A2(r) ((r)->a2)
#define PTRACE_GDB_REG_A3(r) ((r)->a3)
#define PTRACE_GDB_REG_A7(r) ((r)->a7)
#define PTRACE_GDB_REG_S0(r) ((r)->s0)

#elif defined(__aarch64__)
#undef ARCH_AARCH64
#define ARCH_AARCH64 1
struct aarch64_user_regs {
    unsigned long regs[31];
    unsigned long sp;
    unsigned long pc;
    unsigned long pstate;
};
typedef struct aarch64_user_regs arch_user_regs;

struct aarch64_user_fpregs {
    __uint128_t vregs[32];
    uint32_t fpsr;
    uint32_t fpcr;
    uint32_t reserved[2];
};
typedef struct aarch64_user_fpregs arch_user_fpregs;

#define PTRACE_GDB_REG_PC(r) ((r)->pc)
#define PTRACE_GDB_REG_SP(r) ((r)->sp)
#define PTRACE_GDB_REG_A0(r) ((r)->regs[0])
#define PTRACE_GDB_REG_A1(r) ((r)->regs[1])
#define PTRACE_GDB_REG_A2(r) ((r)->regs[2])
#define PTRACE_GDB_REG_A3(r) ((r)->regs[3])
#define PTRACE_GDB_REG_A7(r) ((r)->regs[8])
#define PTRACE_GDB_REG_S0(r) ((r)->regs[19])

#elif defined(__loongarch__) || defined(__loongarch64)
#undef ARCH_LOONGARCH
#define ARCH_LOONGARCH 1
struct loongarch_user_regs {
    unsigned long regs[32];
    unsigned long orig_a0;
    unsigned long csr_era;
    unsigned long csr_badv;
    unsigned long reserved[10];
};
_Static_assert(sizeof(struct loongarch_user_regs) == 45 * sizeof(unsigned long),
               "loongarch NT_PRSTATUS must match Linux user_pt_regs");
typedef struct loongarch_user_regs arch_user_regs;

struct loongarch_user_fpregs {
    unsigned long fpr[32];
    unsigned long fcc;
    uint32_t fcsr;
};
typedef struct loongarch_user_fpregs arch_user_fpregs;

#define PTRACE_GDB_REG_PC(r) ((r)->csr_era)
#define PTRACE_GDB_REG_SP(r) ((r)->regs[3])
#define PTRACE_GDB_REG_A0(r) ((r)->regs[4])
#define PTRACE_GDB_REG_A1(r) ((r)->regs[5])
#define PTRACE_GDB_REG_A2(r) ((r)->regs[6])
#define PTRACE_GDB_REG_A3(r) ((r)->regs[7])
#define PTRACE_GDB_REG_A7(r) ((r)->regs[11])
#define PTRACE_GDB_REG_S0(r) ((r)->regs[23])

#else
#error "test-ptrace-gdb needs an architecture register layout"
#endif

static void fpregs_set_f0(arch_user_fpregs *regs, unsigned long value)
{
#if ARCH_RISCV
    regs->f[0] = value;
#elif ARCH_AARCH64
    regs->vregs[0] = (__uint128_t)value;
#elif ARCH_LOONGARCH
    regs->fpr[0] = value;
#endif
}

static void fpregs_set_f1(arch_user_fpregs *regs, unsigned long value)
{
#if ARCH_RISCV
    regs->f[1] = value;
#elif ARCH_AARCH64
    regs->vregs[1] = (__uint128_t)value;
#elif ARCH_LOONGARCH
    regs->fpr[1] = value;
#endif
}

static unsigned long fpregs_get_f0(const arch_user_fpregs *regs)
{
#if ARCH_RISCV
    return regs->f[0];
#elif ARCH_AARCH64
    return (unsigned long)regs->vregs[0];
#elif ARCH_LOONGARCH
    return regs->fpr[0];
#endif
}

static unsigned long fpregs_get_f1(const arch_user_fpregs *regs)
{
#if ARCH_RISCV
    return regs->f[1];
#elif ARCH_AARCH64
    return (unsigned long)regs->vregs[1];
#elif ARCH_LOONGARCH
    return regs->fpr[1];
#endif
}

static unsigned long read_f0_bits(void)
{
    unsigned long f0_bits = 0;
#if ARCH_RISCV
    __asm__ volatile("fmv.x.d %0, f0" : "=r"(f0_bits));
#elif ARCH_AARCH64
    __asm__ volatile("fmov %0, d0" : "=r"(f0_bits));
#elif ARCH_LOONGARCH
    __asm__ volatile("movfr2gr.d %0, $f0" : "=r"(f0_bits));
#endif
    return f0_bits;
}

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

static int get_regs(pid_t pid, arch_user_regs *regs)
{
    struct iovec iov = {.iov_base = regs, .iov_len = sizeof(*regs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_PRSTATUS, &iov) != 0) {
        return -1;
    }
    if (iov.iov_len != sizeof(*regs)) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

static int set_regs(pid_t pid, arch_user_regs *regs)
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

#if ARCH_RISCV
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
#elif ARCH_AARCH64
__attribute__((naked, noinline, aligned(4))) static void ss_step_target(void)
{
    __asm__ volatile(
        "mov x0, #123\n"
        "brk #0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_b_target(void)
{
    __asm__ volatile(
        "b 1f\n"
        "mov x2, #111\n"
        "brk #0\n"
        "1:\n"
        "mov x2, #222\n"
        "brk #0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_cbz_target(void)
{
    __asm__ volatile(
        "cbz x0, 1f\n"
        "mov x2, #111\n"
        "brk #0\n"
        "1:\n"
        "mov x2, #333\n"
        "brk #0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_tbnz_target(void)
{
    __asm__ volatile(
        "tbnz x0, #0, 1f\n"
        "mov x2, #111\n"
        "brk #0\n"
        "1:\n"
        "mov x2, #444\n"
        "brk #0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_br_target(void)
{
    __asm__ volatile(
        "br x3\n"
        "mov x2, #111\n"
        "brk #0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_br_landing(void)
{
    __asm__ volatile(
        "mov x2, #555\n"
        "brk #0\n");
}
#elif ARCH_LOONGARCH
__attribute__((naked, noinline, aligned(4))) static void ss_step_target(void)
{
    __asm__ volatile(
        "addi.d $a0, $zero, 123\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_b_target(void)
{
    __asm__ volatile(
        "b 1f\n"
        "addi.d $a2, $zero, 222\n"
        "break 0\n"
        "1:\n"
        "addi.d $a2, $zero, 333\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_beq_target(void)
{
    __asm__ volatile(
        "beq $a0, $a1, 1f\n"
        "addi.d $a2, $zero, 111\n"
        "break 0\n"
        "1:\n"
        "addi.d $a2, $zero, 444\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_blt_target(void)
{
    __asm__ volatile(
        "blt $a0, $a1, 1f\n"
        "addi.d $a2, $zero, 111\n"
        "break 0\n"
        "1:\n"
        "addi.d $a2, $zero, 555\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_beqz_target(void)
{
    __asm__ volatile(
        "beqz $a0, 1f\n"
        "addi.d $a2, $zero, 111\n"
        "break 0\n"
        "1:\n"
        "addi.d $a2, $zero, 777\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_bnez_target(void)
{
    __asm__ volatile(
        "bnez $a0, 1f\n"
        "addi.d $a2, $zero, 111\n"
        "break 0\n"
        "1:\n"
        "addi.d $a2, $zero, 888\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_jirl_target(void)
{
    __asm__ volatile(
        "jirl $zero, $a3, 0\n"
        "addi.d $a2, $zero, 111\n"
        "break 0\n");
}

__attribute__((naked, noinline, aligned(4))) static void ss_jirl_landing(void)
{
    __asm__ volatile(
        "addi.d $a2, $zero, 666\n"
        "break 0\n");
}
#endif

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

#if ARCH_RISCV || ARCH_AARCH64 || ARCH_LOONGARCH
static int check_singlestep_stops_before_target_side_effect(pid_t pid,
                                                           arch_user_regs *regs,
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
    PTRACE_GDB_REG_PC(regs) = pc;
    PTRACE_GDB_REG_A0(regs) = a0;
    PTRACE_GDB_REG_A1(regs) = a1;
    PTRACE_GDB_REG_A2(regs) = 0;
    PTRACE_GDB_REG_A3(regs) = a3;
    PTRACE_GDB_REG_S0(regs) = s0;
    if (set_regs(pid, regs) != 0) {
        return fail("setregset control-flow singlestep target");
    }
    memset(regs, 0, sizeof(*regs));
    if (get_regs(pid, regs) != 0) {
        return fail("getregset after setting control-flow singlestep target");
    }
    if (PTRACE_GDB_REG_PC(regs) != pc || PTRACE_GDB_REG_A0(regs) != a0 || PTRACE_GDB_REG_A1(regs) != a1 || PTRACE_GDB_REG_A2(regs) != 0
        || PTRACE_GDB_REG_A3(regs) != a3 || PTRACE_GDB_REG_S0(regs) != s0) {
        printf("FAIL: setregset control-flow readback pc=%#lx a0=%#lx a1=%#lx a2=%#lx "
               "a3=%#lx s0=%#lx\n",
               PTRACE_GDB_REG_PC(regs), PTRACE_GDB_REG_A0(regs), PTRACE_GDB_REG_A1(regs), PTRACE_GDB_REG_A2(regs), PTRACE_GDB_REG_A3(regs), PTRACE_GDB_REG_S0(regs));
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
    if (PTRACE_GDB_REG_A2(regs) != 0) {
        printf("FAIL: singlestep ran target side effect early, pc=%#lx s0=%#lx a2=%#lx\n",
               PTRACE_GDB_REG_PC(regs), PTRACE_GDB_REG_S0(regs), PTRACE_GDB_REG_A2(regs));
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
    if (PTRACE_GDB_REG_A2(regs) != expected_a2) {
        printf("FAIL: control-flow landing a2=%#lx expected %#lx\n",
               PTRACE_GDB_REG_A2(regs), expected_a2);
        return 1;
    }
    return 0;
}
#endif

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

    arch_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset initial");
    }

    PTRACE_GDB_REG_PC(&regs) = (unsigned long)ss_step_target;
    PTRACE_GDB_REG_A0(&regs) = 0;
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
    if (PTRACE_GDB_REG_A0(&regs) != 123) {
        printf("FAIL: singlestep skipped instruction, a0=%#lx expected 123\n", PTRACE_GDB_REG_A0(&regs));
        return 1;
    }

    #if ARCH_RISCV
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
    #elif ARCH_AARCH64
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_b_target, 0, 0, 0, 0, 222)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_cbz_target, 0, 0, 0, 0, 333)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_tbnz_target, 1, 0, 0, 0, 444)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_br_target, 0, 0, 0,
            (unsigned long)ss_br_landing, 555)
        != 0) {
        return 1;
    }
    #elif ARCH_LOONGARCH
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_b_target, 0, 0, 0, 0, 333)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_beq_target, 7, 7, 0, 0, 444)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_blt_target, 3, 9, 0, 0, 555)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_beqz_target, 0, 0, 0, 0, 777)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_bnez_target, 5, 0, 0, 0, 888)
        != 0) {
        return 1;
    }
    if (check_singlestep_stops_before_target_side_effect(
            pid, &regs, (unsigned long)ss_jirl_target, 0, 0, 0,
            (unsigned long)ss_jirl_landing, 666)
        != 0) {
        return 1;
    }
    #endif

    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("kill after known single-step");
    }
    waitpid(pid, &status, 0);
    #if ARCH_RISCV
    printf("  ok: singlestep handled 32-bit and compressed control-flow instructions\n");
    #elif ARCH_AARCH64
    printf("  ok: singlestep handled aarch64 branch and register-branch instructions\n");
    #elif ARCH_LOONGARCH
    printf("  ok: singlestep handled loongarch64 branch and jirl instructions\n");
    #else
    printf("  ok: singlestep handled a known sequential instruction\n");
    #endif
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

        unsigned long f0_bits = read_f0_bits();
        _exit(f0_bits == 0x4008000000000000UL ? 42 : 1);
    }

    int status = 0;
    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial stop\n");
        return 1;
    }

    arch_user_fpregs fpregs;
    struct iovec iov = {.iov_base = &fpregs, .iov_len = sizeof(fpregs)};
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        printf("  SKIP: NT_FPREGSET not supported (errno=%d)\n", errno);
        ptrace(PTRACE_CONT, pid, NULL, NULL);
        waitpid(pid, &status, 0);
        return 0;
    }

    int all_zero = 1;
    if (fpregs_get_f0(&fpregs) != 0 || fpregs_get_f1(&fpregs) != 0) {
        all_zero = 0;
    }
    printf("  ok: NT_FPREGSET read, %s, iov_len=%zu\n",
           all_zero ? "all-zero (FP unused)" : "has non-zero values",
           (size_t)iov.iov_len);

    fpregs_set_f0(&fpregs, 0x4008000000000000UL);
    fpregs_set_f1(&fpregs, 0xCAFEBABEu);
    iov.iov_len = sizeof(fpregs);
    if (ptrace(PTRACE_SETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        return fail("setregset fpregs");
    }

    arch_user_fpregs fpregs2;
    iov.iov_base = &fpregs2;
    iov.iov_len = sizeof(fpregs2);
    if (ptrace(PTRACE_GETREGSET, pid, (void *)NT_FPREGSET, &iov) != 0) {
        return fail("getregset fpregs after set");
    }
    if (fpregs_get_f0(&fpregs2) != 0x4008000000000000UL || fpregs_get_f1(&fpregs2) != 0xCAFEBABEu) {
        printf("FAIL: fpregs write-back mismatch: f[0]=%#lx f[1]=%#lx\n",
               fpregs_get_f0(&fpregs2), fpregs_get_f1(&fpregs2));
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

    arch_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset after attach");
    }
    printf("  ok: attached, child stopped, pc=%#lx sp=%#lx\n",
           PTRACE_GDB_REG_PC(&regs), PTRACE_GDB_REG_SP(&regs));

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

static int test_same_uid_sibling_attach(void)
{
    printf("test 4b: same-uid sibling PTRACE_ATTACH\n");

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) != 0) {
        return fail("same-uid ready pipe");
    }
    if (pipe(release_pipe) != 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return fail("same-uid release pipe");
    }

    pid_t tracee = fork();
    if (tracee < 0) {
        return fail("fork same-uid tracee");
    }
    if (tracee == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(100);
        }
        if (prctl(PR_SET_DUMPABLE, 1) != 0 || prctl(PR_GET_DUMPABLE) != 1) {
            _exit(102);
        }
        char c = 'T';
        if (write(ready_pipe[1], &c, 1) != 1) {
            _exit(101);
        }
        close(ready_pipe[1]);
        wait_for_release_then_exit(release_pipe[0], 43);
    }

    close(ready_pipe[1]);
    char c;
    if (read(ready_pipe[0], &c, 1) != 1) {
        kill(tracee, SIGKILL);
        close(ready_pipe[0]);
        close(release_pipe[0]);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("read same-uid tracee ready pipe");
    }
    close(ready_pipe[0]);

    pid_t tracer = fork();
    if (tracer < 0) {
        kill(tracee, SIGKILL);
        close(release_pipe[0]);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("fork same-uid tracer");
    }
    if (tracer == 0) {
        close(release_pipe[0]);
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(110);
        }
        if (ptrace(PTRACE_ATTACH, tracee, NULL, NULL) != 0) {
            _exit(111);
        }

        int status = 0;
        if (waitpid(tracee, &status, 0) != tracee) {
            ptrace(PTRACE_KILL, tracee, NULL, NULL);
            _exit(112);
        }
        if (!WIFSTOPPED(status) || WSTOPSIG(status) != SIGSTOP) {
            ptrace(PTRACE_KILL, tracee, NULL, NULL);
            _exit(113);
        }
        if (ptrace(PTRACE_DETACH, tracee, NULL, NULL) != 0) {
            ptrace(PTRACE_KILL, tracee, NULL, NULL);
            _exit(114);
        }
        char release = 'R';
        if (write(release_pipe[1], &release, 1) != 1) {
            _exit(115);
        }
        close(release_pipe[1]);
        _exit(0);
    }

    close(release_pipe[0]);
    int tracer_status = 0;
    if (waitpid(tracer, &tracer_status, 0) != tracer) {
        kill(tracee, SIGKILL);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("wait same-uid tracer");
    }
    if (!WIFEXITED(tracer_status) || WEXITSTATUS(tracer_status) != 0) {
        printf("FAIL: same-uid sibling tracer status=%#x\n", tracer_status);
        kill(tracee, SIGKILL);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return 1;
    }
    close(release_pipe[1]);

    int tracee_status = 0;
    if (waitpid(tracee, &tracee_status, 0) != tracee || !WIFEXITED(tracee_status)
        || WEXITSTATUS(tracee_status) != 43) {
        printf("FAIL: same-uid sibling tracee status=%#x\n", tracee_status);
        return 1;
    }

    printf("  ok: non-parent same-uid tracer attached and detached\n");
    return 0;
}

static int test_same_uid_sibling_attach_rejects_nondumpable(void)
{
    printf("test 4c: same-uid sibling attach rejects nondumpable tracee\n");

    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) != 0) {
        return fail("nondumpable ready pipe");
    }
    if (pipe(release_pipe) != 0) {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return fail("nondumpable release pipe");
    }

    pid_t tracee = fork();
    if (tracee < 0) {
        return fail("fork nondumpable tracee");
    }
    if (tracee == 0) {
        close(ready_pipe[0]);
        close(release_pipe[1]);
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(100);
        }
        if (prctl(PR_GET_DUMPABLE) != 0) {
            _exit(101);
        }
        char c = 'N';
        if (write(ready_pipe[1], &c, 1) != 1) {
            _exit(102);
        }
        close(ready_pipe[1]);
        wait_for_release_then_exit(release_pipe[0], 44);
    }

    close(ready_pipe[1]);
    char c;
    if (read(ready_pipe[0], &c, 1) != 1) {
        kill(tracee, SIGKILL);
        close(ready_pipe[0]);
        close(release_pipe[0]);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("read nondumpable tracee ready pipe");
    }
    close(ready_pipe[0]);

    pid_t tracer = fork();
    if (tracer < 0) {
        kill(tracee, SIGKILL);
        close(release_pipe[0]);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("fork nondumpable tracer");
    }
    if (tracer == 0) {
        close(release_pipe[0]);
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(110);
        }
        errno = 0;
        if (ptrace(PTRACE_ATTACH, tracee, NULL, NULL) == 0 || errno != EPERM) {
            _exit(111);
        }
        errno = 0;
        if (ptrace(PTRACE_SEIZE, tracee, NULL, NULL) == 0 || errno != EPERM) {
            _exit(112);
        }
        char release = 'R';
        if (write(release_pipe[1], &release, 1) != 1) {
            _exit(113);
        }
        close(release_pipe[1]);
        _exit(0);
    }

    close(release_pipe[0]);
    int tracer_status = 0;
    if (waitpid(tracer, &tracer_status, 0) != tracer) {
        kill(tracee, SIGKILL);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return fail("wait nondumpable tracer");
    }
    if (!WIFEXITED(tracer_status) || WEXITSTATUS(tracer_status) != 0) {
        printf("FAIL: nondumpable sibling tracer status=%#x\n", tracer_status);
        kill(tracee, SIGKILL);
        close(release_pipe[1]);
        waitpid(tracee, NULL, 0);
        return 1;
    }
    close(release_pipe[1]);

    int tracee_status = 0;
    if (waitpid(tracee, &tracee_status, 0) != tracee || !WIFEXITED(tracee_status)
        || WEXITSTATUS(tracee_status) != 44) {
        printf("FAIL: nondumpable sibling tracee status=%#x\n", tracee_status);
        return 1;
    }

    printf("  ok: non-parent same-uid tracer cannot attach nondumpable target\n");
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
#if ARCH_RISCV
        "addi a2, zero, 77\n"
        "ebreak\n"
#elif ARCH_LOONGARCH
        "addi.d $a2, $zero, 77\n"
        "break 0\n"
#else
        "mov x2, #77\n"
        "brk #0\n"
#endif
    );
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

    arch_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (get_regs(pid, &regs) != 0) {
        return fail("getregset before setregset pc landing");
    }
    PTRACE_GDB_REG_PC(&regs) = (unsigned long)setregs_pc_landing;
    PTRACE_GDB_REG_A2(&regs) = 0;
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
    if (PTRACE_GDB_REG_A2(&regs) != 77) {
        printf("FAIL: SETREGSET PC landing a2=%#lx expected 77\n", PTRACE_GDB_REG_A2(&regs));
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

    arch_user_regs regs;
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

        if (expect_entry && PTRACE_GDB_REG_A7(&regs) == SYS_getpid) {
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
            if (PTRACE_GDB_REG_A0(&regs) != (unsigned long)pid) {
                printf("FAIL: getpid exit a0=%lu, expected pid=%d\n", PTRACE_GDB_REG_A0(&regs), pid);
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
#if ARCH_RISCV
        "addi a2, zero, 88\n"
        "ebreak\n"
#elif ARCH_LOONGARCH
        "addi.d $a2, $zero, 88\n"
        "break 0\n"
#else
        "mov x2, #88\n"
        "brk #0\n"
#endif
    );
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

    arch_user_regs regs;
    memset(&regs, 0, sizeof(regs));
    if (ptrace(PTRACE_GETREGS, pid, NULL, &regs) != 0) {
        return fail("legacy getregs");
    }
    if (PTRACE_GDB_REG_PC(&regs) == 0 || PTRACE_GDB_REG_SP(&regs) == 0) {
        printf("FAIL: legacy GETREGS returned pc=%#lx sp=%#lx\n", PTRACE_GDB_REG_PC(&regs), PTRACE_GDB_REG_SP(&regs));
        return 1;
    }

    PTRACE_GDB_REG_PC(&regs) = (unsigned long)legacy_setregs_landing;
    PTRACE_GDB_REG_A2(&regs) = 0;
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
    if (PTRACE_GDB_REG_A2(&regs) != 88) {
        printf("FAIL: legacy SETREGS landing a2=%#lx expected 88\n", PTRACE_GDB_REG_A2(&regs));
        return 1;
    }
    if (ptrace(PTRACE_KILL, pid, NULL, NULL) != 0) {
        return fail("legacy kill regs child");
    }
    waitpid(pid, &status, 0);

#if !(ARCH_RISCV || ARCH_LOONGARCH)
    printf("  ok: legacy GETREGS/SETREGS work; legacy FPREGS skipped on this arch\n");
    return 0;
#else
    pid = fork();
    if (pid < 0) {
        return fail("fork legacy fpregs");
    }

    if (pid == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            _exit(100);
        }
        raise(SIGSTOP);

        unsigned long f0_bits = read_f0_bits();
        _exit(f0_bits == 0x4010000000000000UL ? 42 : 1);
    }

    if (waitpid(pid, &status, WUNTRACED) != pid || !WIFSTOPPED(status)) {
        printf("FAIL: initial legacy fpregs stop\n");
        return 1;
    }

    arch_user_fpregs fpregs;
    memset(&fpregs, 0, sizeof(fpregs));
    if (ptrace(PTRACE_GETFPREGS, pid, NULL, &fpregs) != 0) {
        return fail("legacy getfpregs");
    }
    fpregs_set_f0(&fpregs, 0x4010000000000000UL);
    fpregs_set_f1(&fpregs, 0x12345678UL);
    if (ptrace(PTRACE_SETFPREGS, pid, NULL, &fpregs) != 0) {
        return fail("legacy setfpregs");
    }

    arch_user_fpregs fpregs2;
    memset(&fpregs2, 0, sizeof(fpregs2));
    if (ptrace(PTRACE_GETFPREGS, pid, NULL, &fpregs2) != 0) {
        return fail("legacy getfpregs after set");
    }
    if (fpregs_get_f0(&fpregs2) != 0x4010000000000000UL
        || fpregs_get_f1(&fpregs2) != 0x12345678UL) {
        printf("FAIL: legacy fpregs mismatch f0=%#lx f1=%#lx\n",
               fpregs_get_f0(&fpregs2), fpregs_get_f1(&fpregs2));
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
#endif
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
#if ARCH_RISCV
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
#elif ARCH_AARCH64
    register long x0 __asm__("x0") =
        CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
    register long x1 __asm__("x1") = (long)stack_top;
    register long x2 __asm__("x2") = 0;
    register long x3 __asm__("x3") = 0;
    register long x4 __asm__("x4") = 0;
    register long x8 __asm__("x8") = SYS_clone;

    __asm__ volatile(
        "svc #0\n"
        "cbnz x0, 1f\n"
        "mov x8, #93\n"
        "mov x0, #0\n"
        "svc #0\n"
        "1:\n"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8)
        : "memory");
    return x0;
#elif ARCH_LOONGARCH
    register long a0 __asm__("$a0") =
        CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
    register long a1 __asm__("$a1") = (long)stack_top;
    register long a2 __asm__("$a2") = 0;
    register long a3 __asm__("$a3") = 0;
    register long a4 __asm__("$a4") = 0;
    register long a7 __asm__("$a7") = SYS_clone;
    __asm__ volatile(
        "syscall 0\n"
        "bnez $a0, 1f\n"
        "li.w $a7, 93\n"
        "li.w $a0, 0\n"
        "syscall 0\n"
        "1:\n"
        : "+r"(a0)
        : "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a7)
        : "memory");
    return a0;
#else
#error "raw_clone_thread needs an architecture syscall sequence"
#endif
}

static pid_t raw_clone_vfork_child_exit(void)
{
    long ret = syscall(SYS_clone, CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0, 0, 0);
    if (ret == 0) {
        _exit(0);
    }
    return (pid_t)ret;
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

        pid_t child = raw_clone_vfork_child_exit();
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

    if (test_same_uid_sibling_attach() == 0) {
        pass++;
    } else {
        fail_count++;
    }

    if (test_same_uid_sibling_attach_rejects_nondumpable() == 0) {
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
