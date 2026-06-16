#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <unistd.h>

/*
 * test-uid-gid-getters —— 6 个 getter syscall 地毯式全覆盖测例汇总
 *
 * 设计：
 *   - 11 模块（6 syscall + cross_consistency + boundary + matrix + procfs_visibility + uid32_no_trunc）
 *   - 每模块自计 __pass/__fail；run() return __fail
 *   - main 按顺序跑 + 汇总
 *
 * 覆盖 syscall（man 2）:
 *   getuid    — return real user ID
 *   geteuid   — return effective user ID
 *   getgid    — return real group ID
 *   getegid   — return effective group ID
 *   getresuid — return real, effective, saved user IDs (Linux-specific)
 *   getresgid — return real, effective, saved group IDs (Linux-specific)
 *
 * 测试自包含（feedback_test_self_contained.md 原则）：
 *   不依赖 test-credentials 等前辈测例覆盖任何东西；自己每个 syscall 独立验证
 *   man 描述的全部行为 + 边界 + 跨 syscall 一致性。
 *
 * 退出码：0 = 全 PASS；非 0 = 有 case 不达预期。
 */

int getuid_run(void);
int geteuid_run(void);
int getgid_run(void);
int getegid_run(void);
int getresuid_run(void);
int getresgid_run(void);
int cross_consistency_run(void);
int boundary_run(void);
int matrix_run(void);
int procfs_visibility_run(void);
int uid32_no_trunc_run(void);

int main(void)
{
    TEST_START("uid/gid getters: 地毯式全覆盖（11 模块 / 6 syscall）");

    /* main.c 自身不调用 CHECK，避免 -Werror=unused-variable 误报 */
    (void)__pass;
    (void)__fail;

    int total_fail = 0;
    int rc;

    #define COLLECT(label, call) do {                                          \
        rc = (call);                                                            \
        if (rc < 0 || rc > 1000000) {                                           \
            printf("  %-18s : <suspect rc=%d, treat as 1 hard failure>\n", label, rc); \
            rc = 1;                                                             \
        } else {                                                                \
            printf("  %-18s : %d fail\n", label, rc);                           \
        }                                                                       \
        total_fail += rc;                                                       \
    } while (0)

    printf("================================================\n");

    COLLECT("getuid",          getuid_run());
    COLLECT("geteuid",         geteuid_run());
    COLLECT("getgid",          getgid_run());
    COLLECT("getegid",         getegid_run());
    COLLECT("getresuid",       getresuid_run());
    COLLECT("getresgid",       getresgid_run());
    COLLECT("cross_consist",   cross_consistency_run());
    COLLECT("boundary",        boundary_run());
    COLLECT("matrix(man-first)", matrix_run());
    COLLECT("procfs_visibility", procfs_visibility_run());
    COLLECT("uid32_no_trunc",  uid32_no_trunc_run());

    #undef COLLECT

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
