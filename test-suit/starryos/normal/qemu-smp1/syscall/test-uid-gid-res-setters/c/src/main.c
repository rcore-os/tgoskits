#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <unistd.h>

/*
 * test-uid-gid-res-setters —— setresuid / setresgid 地毯式全覆盖
 *
 * Group D of uid/gid 5 分组测例方案。
 *
 * man 2 setresuid §"DESCRIPTION":
 *   "setresuid() sets the real user ID, the effective user ID, and the
 *    saved set-user-ID of the calling process."
 *   "An unprivileged process may change its real UID, effective UID, and
 *    saved set-user-ID, each to one of: the current real UID, the current
 *    effective UID, or the current saved set-user-ID."
 *   "Privileged processes (on Linux, those having the CAP_SETUID capability)
 *    may set their real UID, effective UID, and saved set-user-ID to
 *    arbitrary values."
 *   "If one of the arguments equals -1, the corresponding value is not changed."
 *
 * 测试自包含 — 三参数独立性是 setresuid/setresgid 与 setre/setuid 簇的
 * 关键区别（不像 setreuid 有 saved-set-id 自动更新；setres-簇是显式三参数）。
 */

int setresuid_run(void);
int setresgid_run(void);
int three_arg_independence_run(void);
int boundary_run(void);
int matrix_run(void);
int fsuid_follow_run(void);
int procfs_visibility_run(void);
int nptl_sync_run(void);
int core_dump_inhibit_run(void);
int errno_preservation_run(void);

int main(void)
{
    TEST_START("uid/gid res-setters: setresuid + setresgid 地毯式（10 模块: setresuid + setresgid + three_arg_independence + boundary + matrix + procfs_visibility + fsuid_follow + nptl_sync + core_dump_inhibit + errno_preservation）");

    (void)__pass;
    (void)__fail;

    int total_fail = 0;
    int rc;

    #define COLLECT(label, call) do {                                          \
        rc = (call);                                                            \
        if (rc < 0 || rc > 1000000) {                                           \
            printf("  %-22s : <suspect rc=%d, treat as 1 hard failure>\n", label, rc); \
            rc = 1;                                                             \
        } else {                                                                \
            printf("  %-22s : %d fail\n", label, rc);                           \
        }                                                                       \
        total_fail += rc;                                                       \
    } while (0)

    printf("================================================\n");

    COLLECT("setresuid",              setresuid_run());
    COLLECT("setresgid",              setresgid_run());
    COLLECT("three_arg_indep",        three_arg_independence_run());
    COLLECT("boundary",               boundary_run());
    COLLECT("fsuid_follow",           fsuid_follow_run());
    COLLECT("procfs_visibility",      procfs_visibility_run());
    COLLECT("nptl_sync(V1)",          nptl_sync_run());
    COLLECT("core_dump_inhibit(N2)",  core_dump_inhibit_run());
    COLLECT("matrix(man-first)",      matrix_run());
    COLLECT("errno_preservation",     errno_preservation_run());

    #undef COLLECT

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
