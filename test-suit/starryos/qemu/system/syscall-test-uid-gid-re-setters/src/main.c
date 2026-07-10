#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <unistd.h>

/*
 * test-uid-gid-re-setters —— setreuid / setregid 地毯式全覆盖
 *
 * Group C of uid/gid 5 分组测例方案。
 *
 * man 2 setreuid §"DESCRIPTION":
 *   "setreuid() sets real and effective user IDs of the calling process.
 *    Supplying a value of -1 for either the real or effective user ID
 *    forces the system to leave that ID unchanged."
 *   "Unprivileged processes may only set the effective user ID to the real
 *    user ID, the effective user ID, or the saved set-user-ID."
 *   "Unprivileged users may only set the real user ID to the real user ID
 *    or the effective user ID."
 *
 * man §"saved set-user-ID auto-update":
 *   "If the real user ID is set (i.e., ruid is not -1) or the effective user
 *    ID is set to a value not equal to the previous real user ID, the saved
 *    set-user-ID will be set to the new effective user ID."
 *
 * 测试自包含 — root 与 unprivileged 双路径；设计核心是 saved-set-uid 的
 * 自动更新规则（最容易被实现错过的语义）。
 */

int setreuid_run(void);
int setregid_run(void);
int saved_id_semantics_run(void);
int boundary_run(void);
int matrix_run(void);
int procfs_visibility_run(void);
int nptl_sync_run(void);
int core_dump_inhibit_run(void);
int errno_preservation_run(void);

int main(void)
{
    TEST_START("uid/gid re-setters: setreuid + setregid 地毯式（9 模块: setreuid + setregid + saved_id_semantics + boundary + matrix + procfs_visibility + nptl_sync + core_dump_inhibit + errno_preservation）");

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

    COLLECT("setreuid",               setreuid_run());
    COLLECT("setregid",               setregid_run());
    COLLECT("saved_id_semantics",     saved_id_semantics_run());
    COLLECT("boundary",               boundary_run());
    COLLECT("matrix(man-first)",      matrix_run());
    COLLECT("procfs_visibility",      procfs_visibility_run());
    COLLECT("nptl_sync(V1)",          nptl_sync_run());
    COLLECT("core_dump_inhibit(N2)",  core_dump_inhibit_run());
    COLLECT("errno_preservation",     errno_preservation_run());

    #undef COLLECT

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
