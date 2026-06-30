/*
 * test_sigfpe_intdiv.c — x86 #DE (divide error) must deliver SIGFPE/FPE_INTDIV.
 *
 * 回归背景 (为什么写这个测例):
 *   x86_64 整数除零触发 CPU #DE (vector 0). StarryOS 此前把 #DE 映射成 SIGTRAP,
 *   导致安装 SIGFPE 处理器的运行时(JVM、CPython 的 fpectl、各类 sandbox)永远收不到
 *   SIGFPE —— 它们要么按默认 SIGTRAP 动作崩溃, 要么(若也挂了 SIGTRAP)误分类。
 *
 * 修复 (components/axcpu uspace_common.rs ArithmeticError + x86_64/uspace.rs kind()
 *   + StarryOS kernel/src/task/user.rs + starry-signal/src/types.rs FPE_INTDIV):
 *   #DE 现在投递 SIGFPE, 且 si_code == FPE_INTDIV (整数除零)。
 *
 * man 2 sigaction / signal(7): 同步算术故障 SIGFPE 的 si_code 对整数除零为
 *   FPE_INTDIV(值 1)。处理器用 SA_SIGINFO 注册, 读 info->si_code 分类。
 *
 * 断言: 在 SIGFPE 处理器里 info->si_signo==SIGFPE 且 si_code==FPE_INTDIV。
 * 修复前: 进程收到 SIGTRAP, SIGFPE 处理器从不运行 -> 该 binary 永不打印 PASS,
 *   退出码非 0(或挂死) -> grouped runner FAIL/超时。
 * 修复后: 处理器命中, 断言通过, 打印 PASS 并 _exit(0)。
 *
 * 自包含 / musl-Alpine 友好: 只用 sigaction(SA_SIGINFO)+ 不可常量折叠的整除。
 * 不用 setjmp 回返(除零 RIP 不前进, 返回会原地再 trap), 故在处理器内直接 _exit。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <signal.h>
#include <unistd.h>

#ifndef FPE_INTDIV
#define FPE_INTDIV 1
#endif

/* x86 专属: 只有 x86 整数除零会陷入 #DE (vector 0) -> 内核应投 SIGFPE/FPE_INTDIV。
 * 其它架构(aarch64/riscv64/loongarch64)整数除零**不陷入**(ISA 定义返回值),
 * 进程根本收不到 SIGFPE, 本测例无对应行为可验 -> 在非 x86 上跳过(SKIP)。 */
#if defined(__x86_64__)

/* 仅用 async-signal-safe 调用做断言, 处理器内直接退出。 */
static void on_sigfpe(int sig, siginfo_t *info, void *uctx)
{
    (void)uctx;
    int ok_signo = (sig == SIGFPE) && info && (info->si_signo == SIGFPE);
    int ok_code  = info && (info->si_code == FPE_INTDIV);

    /* write() 是 async-signal-safe; printf 不是, 但这些是固定短串, 仍用 write。 */
    if (ok_signo && ok_code) {
        static const char m[] =
            "  PASS | sigfpe-intdiv | SIGFPE si_code==FPE_INTDIV\n"
            "SIGFPE_INTDIV_OK\n"
            "------------------------------------------------\n"
            "  DONE: 1 pass, 0 fail\n"
            "================================================\n";
        ssize_t wr = write(1, m, sizeof(m) - 1);
        (void)wr;
        _exit(0);
    }

    static const char f[] =
        "  FAIL | sigfpe-intdiv | wrong signo or si_code (not FPE_INTDIV)\n"
        "SIGFPE_INTDIV_FAIL\n";
    ssize_t wr = write(2, f, sizeof(f) - 1);
    (void)wr;
    _exit(1);
}

/*
 * 强制发射真正会陷入的 idiv:
 * 被除数也必须是不透明的 (volatile, 非常量)。若被除数是字面量 1, gcc 会把
 * `1 / z` 强度削减成无分支的 cmove 序列(整数除零是 UB, 编译器可直接给 0),
 * 根本不发 idiv —— 测例就失去意义。用 volatile 的被除数 + 除数, 编译器无法
 * 假定二者的值, 必须发射真 idiv, 运行时 den==0 触发 x86 #DE。
 */
static int divide_by_zero(void)
{
    volatile int num = 7;
    volatile int den = 0;
    volatile int r = num / den;   /* x86 #DE here */
    return r;                     /* unreachable on a correct kernel */
}

int main(void)
{
    TEST_START("x86 #DE delivers SIGFPE/FPE_INTDIV (not SIGTRAP)");

    struct sigaction sa;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sa.sa_sigaction = on_sigfpe;
    CHECK_RET(sigaction(SIGFPE, &sa, NULL), 0, "install SA_SIGINFO SIGFPE handler");

    /* 触发: 修复前内核投 SIGTRAP, 本处理器不被调用 -> 不会 _exit(0)。 */
    int v = divide_by_zero();
    (void)v;

    /* 走到这里说明既没收到 SIGFPE 也没死在除零 —— 不可能正确发生。 */
    CHECK(0, "divide-by-zero did NOT raise SIGFPE (handler never fired)");
    TEST_DONE();
}

#else /* !__x86_64__ */

int main(void)
{
    TEST_START("x86 #DE delivers SIGFPE/FPE_INTDIV (not SIGTRAP)");
    CHECK(1, "non-x86_64: integer divide-by-zero does not trap (no #DE) — skipped");
    TEST_DONE();
}

#endif
