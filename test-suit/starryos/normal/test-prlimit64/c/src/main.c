/*
 * test_prlimit64.c — prlimit64 资源限制语义验证
 *
 * 已知 bug: 提高 hard limit 时返回 Ok(0) 但不实际生效,
 * 调用方认为限制已修改, 实际未变。
 *
 * 覆盖范围:
 *   1. 读取当前限制 (NOFILE, STACK)
 *   2. 降低 soft limit / hard limit
 *   3. 提高 hard limit 不能静默丢弃
 *   4. old_limit 先读后写顺序
 *   5. 错误路径 (soft > hard, 无效 resource)
 *   6. getrusage 基本功能
 */

#include "test_framework.h"
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/syscall.h>
#include <unistd.h>

/* prlimit() wrapper — musl 某些版本不导出 prlimit() */
static int my_prlimit(pid_t pid, int resource,
                      const struct rlimit *new_limit,
                      struct rlimit *old_limit)
{
    /* 64 位系统上 rlimit == rlimit64 */
    return syscall(SYS_prlimit64, pid, resource, new_limit, old_limit);
}

int main(void) {
    TEST_START("prlimit64");

    /* 1. 读取 NOFILE 限制 */
    {
        struct rlimit lim;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &lim), 0,
                  "get NOFILE limits");
        CHECK(lim.rlim_cur <= lim.rlim_max, "soft <= hard");
        CHECK(lim.rlim_max > 0, "hard > 0");
    }

    /* 2. 读取 STACK 限制 */
    {
        struct rlimit lim;
        CHECK_RET(my_prlimit(0, RLIMIT_STACK, NULL, &lim), 0,
                  "get STACK limits");
        CHECK(lim.rlim_cur <= lim.rlim_max, "stack soft <= hard");
    }

    /* 3. 降低 soft limit */
    {
        struct rlimit old, new_lim, check;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "read original NOFILE before lowering soft");
        if (old.rlim_cur > 1) {
            new_lim.rlim_cur = old.rlim_cur - 1;
            new_lim.rlim_max = old.rlim_max;
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &new_lim, NULL), 0,
                      "lower soft limit");
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &check), 0,
                      "re-read NOFILE after lowering soft");
            CHECK(check.rlim_cur == old.rlim_cur - 1, "soft actually lowered");
            CHECK(check.rlim_max == old.rlim_max, "hard unchanged");
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &old, NULL), 0,
                      "restore NOFILE after lowering soft");
        }
    }

    /* 4. 降低 hard limit */
    {
        struct rlimit old, new_lim, check;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "read original NOFILE before lowering hard");
        if (old.rlim_max > 2) {
            new_lim.rlim_cur = old.rlim_max - 1;
            new_lim.rlim_max = old.rlim_max - 1;
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &new_lim, NULL), 0,
                      "lower hard limit");
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &check), 0,
                      "re-read NOFILE after lowering hard");
            CHECK(check.rlim_max == old.rlim_max - 1, "hard actually lowered");
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &old, NULL), 0,
                      "restore NOFILE after lowering hard");
        }
    }

    /* 5. 提高 hard limit: 不能静默不生效 (核心测试) */
    {
        struct rlimit old, new_lim;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "read original NOFILE before raising hard");
        if (old.rlim_max <= RLIM_INFINITY - 100) {
            new_lim.rlim_cur = old.rlim_cur;
            new_lim.rlim_max = old.rlim_max + 100;
            errno = 0;
            int ret = my_prlimit(0, RLIMIT_NOFILE, &new_lim, NULL);
            if (ret == 0) {
                struct rlimit check;
                CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &check), 0,
                          "re-read NOFILE after raising hard");
                CHECK(check.rlim_max == old.rlim_max + 100,
                      "raise hard: success must take effect");
                CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &old, NULL), 0,
                          "restore NOFILE after raising hard");
            } else {
                CHECK(errno == EPERM, "raise hard: fail must be EPERM");
            }
        }
    }

    /* 6. 同时提高 soft + hard */
    {
        struct rlimit old, new_lim;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "read original NOFILE before raising both");
        if (old.rlim_max <= RLIM_INFINITY - 2) {
            new_lim.rlim_cur = old.rlim_max + 1;
            new_lim.rlim_max = old.rlim_max + 2;
            errno = 0;
            int ret = my_prlimit(0, RLIMIT_NOFILE, &new_lim, NULL);
            if (ret == 0) {
                struct rlimit check;
                CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &check), 0,
                          "re-read NOFILE after raising both");
                CHECK(check.rlim_cur == old.rlim_max + 1,
                      "raise both: soft takes effect");
                CHECK(check.rlim_max == old.rlim_max + 2,
                      "raise both: hard takes effect");
                CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &old, NULL), 0,
                          "restore NOFILE after raising both");
            } else {
                CHECK(errno == EPERM, "raise both: fail must be EPERM");
            }
        }
    }

    /* 7. old_limit 应先于 new_limit 生效 */
    {
        struct rlimit saved, old, new_lim;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &saved), 0,
                  "read original NOFILE before set+get");
        if (saved.rlim_cur > 1) {
            new_lim.rlim_cur = saved.rlim_cur - 1;
            new_lim.rlim_max = saved.rlim_max;
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &new_lim, &old), 0,
                      "set+get atomically");
            CHECK(old.rlim_cur == saved.rlim_cur, "old has original soft");
            CHECK(old.rlim_max == saved.rlim_max, "old has original hard");
            CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, &saved, NULL), 0,
                      "restore NOFILE after set+get");
        }
    }

    /* 8. soft > hard 应返回 EINVAL */
    {
        struct rlimit old, new_lim;
        CHECK_RET(my_prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "read original NOFILE before invalid soft>hard");
        if (old.rlim_max < RLIM_INFINITY) {
            new_lim.rlim_cur = old.rlim_max + 1;
            new_lim.rlim_max = old.rlim_max;
            CHECK_ERR(my_prlimit(0, RLIMIT_NOFILE, &new_lim, NULL), EINVAL,
                      "soft > hard rejected");
        }
    }

    /* 9. 无效 resource 应返回 EINVAL */
    {
        struct rlimit lim;
        CHECK_ERR(my_prlimit(0, 999, NULL, &lim), EINVAL,
                  "invalid resource rejected");
    }

    /* 10. getrusage RUSAGE_SELF 基本功能 */
    {
        struct rusage usage;
        CHECK_RET(getrusage(RUSAGE_SELF, &usage), 0,
                  "getrusage RUSAGE_SELF");
        CHECK(usage.ru_utime.tv_sec >= 0, "utime >= 0");
        CHECK(usage.ru_stime.tv_sec >= 0, "stime >= 0");
    }

    /* 11. getrusage 无效 who 应返回 EINVAL */
    {
        struct rusage usage;
        CHECK_ERR(getrusage(42, &usage), EINVAL,
                  "getrusage invalid who rejected");
    }

    TEST_DONE();
}
