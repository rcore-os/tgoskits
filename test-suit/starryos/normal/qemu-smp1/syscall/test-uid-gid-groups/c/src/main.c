#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <unistd.h>

/*
 * test-uid-gid-groups —— getgroups / setgroups 地毯式全覆盖
 *
 * Group E (最终) of uid/gid 5 分组测例方案。
 *
 * man 2 getgroups §"DESCRIPTION":
 *   "getgroups() returns the supplementary group IDs of the calling process
 *    in list. The argument size should be set to the maximum number of items
 *    that can be stored in the buffer pointed to by list."
 *   "If size is zero, list is not modified, but the total number of
 *    supplementary group IDs for the process is returned."
 *
 * man 2 setgroups §"DESCRIPTION":
 *   "setgroups() sets the supplementary group IDs for the calling process."
 *   "The maximum number of supplementary group IDs is NGROUPS_MAX (32 on
 *    Linux 2.0.x, 65536 since Linux 2.4.x and the value is exported via
 *    /proc/sys/kernel/ngroups_max)."
 *   "Appropriate privileges (Linux: the CAP_SETGID capability in the user
 *    namespace containing the process's effective user ID) are required."
 *
 * man §"ERRORS":
 *   "EFAULT — list has an invalid address."
 *   "EINVAL — size is greater than NGROUPS_MAX (32 or 65536)."
 *   "EPERM — The calling process has insufficient privilege (the caller does
 *    not have the CAP_SETGID capability)."
 *
 * 测试自包含 — 完整 round-trip：setgroups → getgroups 验内容；边界 NGROUPS_MAX。
 */

int getgroups_run(void);
int setgroups_run(void);
int round_trip_run(void);
int boundary_run(void);
int matrix_run(void);
/* procfs_visibility 已迁移到 bug-starry-procfs-groups-not-synced-after-
 * setgroups 分支独立复现 (starry procfs Groups 行间歇不同步 setgroups). */
int nptl_sync_run(void);
int ngroups_max_sysconf_run(void);
int errno_preservation_run(void);

int main(void)
{
    TEST_START("uid/gid groups: getgroups + setgroups 地毯式（8 模块: getgroups + setgroups + round_trip + boundary + matrix + nptl_sync + ngroups_max_sysconf + errno_preservation；procfs_visibility 已搬到 bug-starry-procfs-groups-not-synced-after-setgroups 分支）");

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

    COLLECT("getgroups",          getgroups_run());
    COLLECT("setgroups",          setgroups_run());
    COLLECT("round_trip",         round_trip_run());
    COLLECT("boundary",           boundary_run());
    COLLECT("matrix(man-first)",  matrix_run());
    /* procfs_visibility removed: 迁 bug-starry-procfs-groups-not-synced-after-setgroups */
    COLLECT("nptl_sync(V1)",      nptl_sync_run());
    COLLECT("ngroups_max_sysconf",ngroups_max_sysconf_run());
    COLLECT("errno_preservation",errno_preservation_run());

    #undef COLLECT

    printf("  -------------------------------------------\n");
    printf("  TOTAL: %d fail | RESULT: %s\n",
           total_fail, total_fail == 0 ? "ALL PASS" : "HAS FAILURES");
    printf("================================================\n\n");

    return total_fail > 0 ? 1 : 0;
}
