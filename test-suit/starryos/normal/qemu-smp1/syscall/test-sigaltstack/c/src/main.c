/*
 * test_sigaltstack.c -- sigaltstack 栈大小校验测试
 *
 * 测试内容：
 *   1. 查询当前备用信号栈（传 NULL）
 *   2. 设置 SIGSTKSZ 大小的栈并验证回读一致
 *   3. 设置恰好为 MINSIGSTKSZ 的栈（必须成功）
 *   4. 小于 MINSIGSTKSZ 的栈应被拒绝，返回 ENOMEM
 *   5. 连续设置两个栈，验证 old_ss 返回值正确
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <signal.h>
#include <stdlib.h>
#include <errno.h>

int main(void)
{
    TEST_START("sigaltstack");

    /* 1. 查询当前备用信号栈 */
    {
        stack_t old;
        CHECK_RET(sigaltstack(NULL, &old), 0, "查询当前栈");
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

    /* 4. 小于 MINSIGSTKSZ 应返回 ENOMEM */
    {
        void *buf = malloc(MINSIGSTKSZ);
        CHECK(buf != NULL, "分配缓冲区（过小测试）");
        if (buf) {
            stack_t ss = { .ss_sp = buf, .ss_size = MINSIGSTKSZ - 1, .ss_flags = 0 };
            int rc = sigaltstack(&ss, NULL);
            CHECK(rc == -1 && errno == ENOMEM, "拒绝 MINSIGSTKSZ-1");
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

    TEST_DONE();
}
