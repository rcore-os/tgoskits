#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <unistd.h>

/*
 * test-uid-gid-direct-setters —— setuid / setgid 地毯式全覆盖
 *
 * Group B of uid/gid 5 分组测例方案。
 *
 * man 2 setuid §"DESCRIPTION":
 *   "setuid() sets the effective user ID of the calling process. If the
 *    calling process is privileged (more precisely: if the process has the
 *    CAP_SETUID capability in its user namespace), the real UID and saved
 *    set-user-ID are also set."
 *
 *   "setuid(geteuid()) does not flush capabilities."
 *
 * man 2 setgid §"DESCRIPTION" (analogous):
 *   "setgid() sets the effective group ID of the calling process. If the
 *    calling process is privileged (the process has the CAP_SETGID
 *    capability), the real GID and saved set-group-ID are also set."
 *
 * 测试自包含 — 不依赖前辈测例 / 不假设权限模型；自行 root 与非 root 双路径测。
 *
 * 退出码：0 = 全 PASS；非 0 = 有 case 不达预期。
 */

int setuid_run(void);
int setgid_run(void);
int cross_root_unprivileged_run(void);
int boundary_run(void);
int matrix_run(void);
int procfs_visibility_run(void);
int nptl_sync_run(void);
int core_dump_inhibit_run(void);
int uid32_no_trunc_run(void);
int errno_preservation_run(void);

int main(void)
{
    TEST_START("uid/gid direct setters: setuid + setgid 地毯式（10 模块: setuid + setgid + boundary + core_dump_inhibit + cross_root_unprivileged + errno_preservation + matrix + nptl_sync + procfs_visibility + uid32_no_trunc）");

    /* main.c 自身不调用 CHECK，避免 -Werror=unused-variable */
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

    COLLECT("setuid",                 setuid_run());
    COLLECT("setgid",                 setgid_run());
    COLLECT("cross_root_unpriv",      cross_root_unprivileged_run());
    COLLECT("boundary",               boundary_run());
    COLLECT("matrix(man-first)",      matrix_run());
    COLLECT("procfs_visibility",      procfs_visibility_run());
    COLLECT("nptl_sync(V1)",          nptl_sync_run());
    COLLECT("core_dump_inhibit(N2)",  core_dump_inhibit_run());
    COLLECT("uid32_no_trunc",         uid32_no_trunc_run());
    COLLECT("errno_preservation",     errno_preservation_run());

    #undef COLLECT

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
