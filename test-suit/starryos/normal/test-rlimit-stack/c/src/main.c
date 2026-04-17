/*
 * test_rlimit_stack.c
 *
 * 测试 RLIMIT_STACK 默认值。Linux 默认 RLIMIT_STACK soft limit
 * 为 8MB。PostgreSQL 依赖此值计算 max_stack_depth。
 */

#include "test_framework.h"
#include <sys/resource.h>

int main(void)
{
    TEST_START("rlimit_stack: RLIMIT_STACK 默认值");

    struct rlimit rl;
    int rc = getrlimit(RLIMIT_STACK, &rl);
    CHECK(rc == 0, "getrlimit(RLIMIT_STACK)");

    /* Soft limit should be at least 2MB (StarryOS sets 8MB, Linux default 8MB) */
    CHECK(rl.rlim_cur >= 2 * 1024 * 1024,
          "RLIMIT_STACK soft >= 2MB");

    /* Standard Linux default is 8MB */
    CHECK(rl.rlim_cur == 8 * 1024 * 1024,
          "RLIMIT_STACK soft == 8MB (Linux default)");

    /* Hard limit should be >= soft limit */
    CHECK(rl.rlim_max >= rl.rlim_cur,
          "RLIMIT_STACK hard >= soft");

    TEST_DONE();
}
