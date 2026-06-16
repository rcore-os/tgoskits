/*
 * test_sigaltstack.c -- sigaltstack 栈大小校验测试
 *
 * 测试内容：
 *   1. 查询当前备用信号栈（传 NULL）
 *   2. 设置 SIGSTKSZ 大小的栈并验证回读一致
 *   3. 设置恰好为 MINSIGSTKSZ 的栈（必须成功）
 *   4. 明显小于内核最小要求的栈应被拒绝，返回 ENOMEM
 *   5. 连续设置两个栈，验证 old_ss 返回值正确
 *   6. 非法 ss_flags 应被拒绝，返回 EINVAL
 *   7. 禁用备用栈时，old_ss 应返回被禁用前的栈
 *   8. SA_ONSTACK handler 必须运行在备用栈上
 *   9. handler 正在备用栈上运行时，sigaltstack() 改栈应返回 EPERM
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <stdint.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>

/*
 * Use a flag bit that is not a Linux sigaltstack flag.  SS_ONSTACK is
 * intentionally not used here: Linux accepts it as a historical no-op even
 * though portable programs should not pass it in ss_flags.
 */
#define INVALID_SS_FLAGS 0x40000000

static char *g_alt_base;
static size_t g_alt_size;
static char *g_other_stack;

static volatile sig_atomic_t g_handler_called;
static volatile sig_atomic_t g_handler_on_alt_stack;
static volatile sig_atomic_t g_handler_saw_ss_onstack;
static volatile sig_atomic_t g_handler_change_stack_eperm;
static volatile sig_atomic_t g_handler_siginfo_ok;

static void on_altstack_handler(int signo, siginfo_t *si, void *ctx)
{
    (void)ctx;
    char marker;
    uintptr_t sp = (uintptr_t)&marker;
    uintptr_t alt_lo = (uintptr_t)g_alt_base;
    uintptr_t alt_hi = alt_lo + g_alt_size;

    g_handler_called = 1;
    g_handler_siginfo_ok = (signo == SIGUSR1 && si && si->si_signo == SIGUSR1);
    g_handler_on_alt_stack = (sp >= alt_lo && sp < alt_hi);

    stack_t current;
    if (sigaltstack(NULL, &current) == 0) {
        g_handler_saw_ss_onstack = (current.ss_flags & SS_ONSTACK) != 0;
    }

    stack_t replacement = {
        .ss_sp = g_other_stack,
        .ss_size = SIGSTKSZ,
        .ss_flags = 0,
    };
    errno = 0;
    int rc = sigaltstack(&replacement, NULL);
    g_handler_change_stack_eperm = (rc == -1 && errno == EPERM);
}

static void reset_altstack_handler_state(void)
{
    g_handler_called = 0;
    g_handler_on_alt_stack = 0;
    g_handler_saw_ss_onstack = 0;
    g_handler_change_stack_eperm = 0;
    g_handler_siginfo_ok = 0;
}

int main(void)
{
    TEST_START("sigaltstack");

    /* 1. 查询当前备用信号栈 */
    {
        stack_t old;
        CHECK_RET(sigaltstack(NULL, &old), 0, "查询当前栈");
        CHECK((old.ss_flags & SS_DISABLE) != 0, "初始备用栈处于禁用状态");
    }

    /* 2. 设置 SIGSTKSZ 大小的栈，验证回读一致 */
    {
        void *buf = malloc(SIGSTKSZ);
        CHECK(buf != NULL, "分配 SIGSTKSZ");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = SIGSTKSZ, .ss_flags = 0 };
            CHECK_RET(sigaltstack(&ss, NULL), 0, "设置 SIGSTKSZ 栈");

            stack_t check;
            CHECK_RET(sigaltstack(NULL, &check), 0, "查询已设置的栈");
            CHECK(check.ss_sp == buf, "回读 sp 一致");
            CHECK((size_t)check.ss_size == (size_t)SIGSTKSZ, "回读 size 一致");

            stack_t disable = { .ss_flags = SS_DISABLE };
            CHECK_RET(sigaltstack(&disable, NULL), 0, "禁用 SIGSTKSZ 栈");
            free(buf);
        }
    }

    /* 3. 恰好 MINSIGSTKSZ 的栈必须被接受
     *
     * Linux 内核检查条件: ss_size < MINSIGSTKSZ（严格小于）。
     * 大小等于 MINSIGSTKSZ 是合法的，不应被拒绝。
     */
    {
        void *buf = malloc(MINSIGSTKSZ);
        CHECK(buf != NULL, "分配 MINSIGSTKSZ");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = MINSIGSTKSZ, .ss_flags = 0 };
            CHECK_RET(sigaltstack(&ss, NULL), 0, "设置恰好 MINSIGSTKSZ 的栈");

            stack_t disable = { .ss_flags = SS_DISABLE };
            CHECK_RET(sigaltstack(&disable, NULL), 0, "禁用 MINSIGSTKSZ 栈");
            free(buf);
        }
    }

    /*
     * 4. 明显过小的栈应返回 ENOMEM。
     *
     * 不使用 MINSIGSTKSZ-1：现代 glibc 的 MINSIGSTKSZ 可能大于内核
     * 实际下限，在 Linux 上不一定触发 ENOMEM。ss_size=1 是稳定的
     * 非法小栈。
     */
    {
        void *buf = malloc(MINSIGSTKSZ);
        CHECK(buf != NULL, "分配缓冲区（过小测试）");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = 1, .ss_flags = 0 };
            errno = 0;
            int rc = sigaltstack(&ss, NULL);
            CHECK(rc == -1 && errno == ENOMEM, "拒绝 ss_size=1 的过小栈");
            if (rc == 0) {
                stack_t disable = { .ss_flags = SS_DISABLE };
                sigaltstack(&disable, NULL);
            }
            free(buf);
        }
    }

    /* 5. 连续设置两个栈，old_ss 应返回前一个栈的信息 */
    {
        void *buf = malloc(SIGSTKSZ);
        CHECK(buf != NULL, "分配第一个栈");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = SIGSTKSZ, .ss_flags = 0 };
            CHECK_RET(sigaltstack(&ss, NULL), 0, "设置第一个栈");

            void *buf2 = malloc(SIGSTKSZ + 1024);
            CHECK(buf2 != NULL, "分配第二个栈");
            if (buf2) {
                stack_t ss2 = { .ss_sp = buf2, .ss_size = SIGSTKSZ + 1024, .ss_flags = 0 };
                stack_t old2;
                CHECK_RET(sigaltstack(&ss2, &old2), 0, "设置第二个栈，取回旧栈");
                CHECK(old2.ss_sp == buf, "旧栈 sp 是第一个缓冲区");
                CHECK((size_t)old2.ss_size == (size_t)SIGSTKSZ, "旧栈 size 是 SIGSTKSZ");

                stack_t disable2 = { .ss_flags = SS_DISABLE };
                CHECK_RET(sigaltstack(&disable2, NULL), 0, "禁用第二个栈");
                free(buf2);
            }

            stack_t disable = { .ss_flags = SS_DISABLE };
            sigaltstack(&disable, NULL);
            free(buf);
        }
    }

    /* 6. 非法 ss_flags 应返回 EINVAL */
    {
        void *buf = malloc(SIGSTKSZ);
        CHECK(buf != NULL, "分配非法 flags 测试栈");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = SIGSTKSZ, .ss_flags = INVALID_SS_FLAGS };
            errno = 0;
            int rc = sigaltstack(&ss, NULL);
            CHECK(rc == -1 && errno == EINVAL, "拒绝未知 ss_flags");
            if (rc == 0) {
                stack_t disable = { .ss_flags = SS_DISABLE };
                sigaltstack(&disable, NULL);
            }
            free(buf);
        }
    }

    /* 7. 禁用时 old_ss 应回填被禁用前的栈信息，之后查询应为 SS_DISABLE */
    {
        void *buf = malloc(SIGSTKSZ);
        CHECK(buf != NULL, "分配禁用 old_ss 测试栈");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = SIGSTKSZ, .ss_flags = 0 };
            CHECK_RET(sigaltstack(&ss, NULL), 0, "设置待禁用备用栈");

            stack_t disable = { .ss_flags = SS_DISABLE };
            stack_t old_disable;
            CHECK_RET(sigaltstack(&disable, &old_disable), 0,
                      "禁用备用栈并取回旧栈");
            CHECK(old_disable.ss_sp == buf, "禁用 old_ss sp 是原备用栈");
            CHECK((size_t)old_disable.ss_size == (size_t)SIGSTKSZ,
                  "禁用 old_ss size 是 SIGSTKSZ");

            stack_t current;
            CHECK_RET(sigaltstack(NULL, &current), 0, "禁用后查询当前栈");
            CHECK((current.ss_flags & SS_DISABLE) != 0, "禁用后报告 SS_DISABLE");
            free(buf);
        }
    }

    /*
     * 8-9. man 2 sigaltstack 语义：
     *   - SA_ONSTACK signal handler 应运行在备用栈上；
     *   - old_ss.ss_flags 在备用栈执行期间应报告 SS_ONSTACK；
     *   - 当前线程正在备用栈上执行时，修改备用栈应失败并置 errno=EPERM。
     */
    {
        g_alt_size = SIGSTKSZ;
        g_alt_base = malloc(g_alt_size);
        g_other_stack = malloc(SIGSTKSZ);
        CHECK(g_alt_base != NULL, "分配 SA_ONSTACK 备用栈");
        CHECK(g_other_stack != NULL, "分配 handler 内替换栈");

        if (g_alt_base && g_other_stack) {
            stack_t ss = { .ss_sp = g_alt_base, .ss_size = g_alt_size, .ss_flags = 0 };
            CHECK_RET(sigaltstack(&ss, NULL), 0, "设置 SA_ONSTACK 备用栈");

            struct sigaction sa;
            struct sigaction old_sa;
            memset(&sa, 0, sizeof(sa));
            CHECK_RET(sigemptyset(&sa.sa_mask), 0, "清空 handler mask");
            sa.sa_sigaction = on_altstack_handler;
            sa.sa_flags = SA_ONSTACK | SA_SIGINFO;
            CHECK_RET(sigaction(SIGUSR1, &sa, &old_sa), 0, "安装 SA_ONSTACK handler");

            reset_altstack_handler_state();
            CHECK_RET(raise(SIGUSR1), 0, "触发 SA_ONSTACK handler");

            CHECK(g_handler_called, "handler 被调用");
            CHECK(g_handler_siginfo_ok, "handler 收到正确 siginfo");
            CHECK(g_handler_on_alt_stack, "handler 局部变量位于备用栈区间");
            CHECK(g_handler_saw_ss_onstack, "handler 内查询到 SS_ONSTACK");
            CHECK(g_handler_change_stack_eperm, "handler 内修改备用栈返回 EPERM");

            CHECK_RET(sigaction(SIGUSR1, &old_sa, NULL), 0, "恢复 SIGUSR1 handler");
            stack_t disable = { .ss_flags = SS_DISABLE };
            CHECK_RET(sigaltstack(&disable, NULL), 0, "禁用 SA_ONSTACK 备用栈");
        }

        free(g_other_stack);
        free(g_alt_base);
        g_other_stack = NULL;
        g_alt_base = NULL;
        g_alt_size = 0;
    }

    TEST_DONE();
}
