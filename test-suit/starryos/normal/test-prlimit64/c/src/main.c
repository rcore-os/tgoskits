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
 *   7. getrlimit/setrlimit libc 接口路径
 *
 * getrlimit/setrlimit 测试逻辑:
 *   - 通过 getrlimit 读取 RLIMIT_NOFILE/RLIMIT_STACK, 确认 soft <= hard。
 *   - raw prlimit64 的坏用户指针必须返回 EFAULT。
 *   - 无效 resource 和 cur > max 必须返回 EINVAL。
 *   - getrlimit/setrlimit 的 NULL libc wrapper 路径按当前 Linux 行为作为 no-op。
 *   - 降低 soft limit 后, 再用 getrlimit 确认 soft 生效、hard 不变。
 *   - 降低 hard limit、设置 cur == max、再恢复原 hard limit, 确认成功返回不会静默丢弃修改。
 *
 * 在 riscv64/musl 上, getrlimit/setrlimit 会走 prlimit64 syscall,
 * 因此这里覆盖的是应用常用 libc API 到 StarryOS prlimit64 实现的路径。
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

    /* 10. raw prlimit64 用户地址错误路径 */
    {
        CHECK_ERR(my_prlimit(0, RLIMIT_NOFILE, NULL, (struct rlimit *)1), EFAULT,
                  "bad old_limit pointer rejected");
        CHECK_ERR(my_prlimit(0, RLIMIT_NOFILE, (const struct rlimit *)1, NULL), EFAULT,
                  "bad new_limit pointer rejected");
    }

    /* 11. getrlimit/setrlimit libc 接口路径 */
    {
        struct rlimit saved, stack, check;
        struct rlimit *null_limit = NULL;
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &saved), 0,
                  "getrlimit reads original NOFILE");
        CHECK(saved.rlim_cur <= saved.rlim_max,
              "getrlimit reports soft <= hard");
        CHECK_RET(getrlimit(RLIMIT_STACK, &stack), 0,
                  "getrlimit reads STACK");
        CHECK(stack.rlim_cur <= stack.rlim_max,
              "getrlimit STACK reports soft <= hard");

        CHECK_ERR(getrlimit(-1, &check), EINVAL,
                  "getrlimit invalid resource rejected");
        CHECK_RET(getrlimit(RLIMIT_NOFILE, null_limit), 0,
                  "getrlimit NULL wrapper path is a no-op");

        struct rlimit invalid = {
            .rlim_cur = 2,
            .rlim_max = 1,
        };
        CHECK_ERR(setrlimit(RLIMIT_NOFILE, &invalid), EINVAL,
                  "setrlimit rejects soft > hard");
        CHECK_ERR(setrlimit(-1, &saved), EINVAL,
                  "setrlimit invalid resource rejected");
        CHECK_RET(setrlimit(RLIMIT_NOFILE, null_limit), 0,
                  "setrlimit NULL wrapper path is a no-op");
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &check), 0,
                  "getrlimit after setrlimit NULL no-op");
        CHECK(check.rlim_cur == saved.rlim_cur && check.rlim_max == saved.rlim_max,
              "setrlimit NULL leaves limits unchanged");

        if (saved.rlim_cur > 1) {
            struct rlimit lowered_soft = {
                .rlim_cur = saved.rlim_cur - 1,
                .rlim_max = saved.rlim_max,
            };
            CHECK_RET(setrlimit(RLIMIT_NOFILE, &lowered_soft), 0,
                      "setrlimit lowers soft limit");
            CHECK_RET(getrlimit(RLIMIT_NOFILE, &check), 0,
                      "getrlimit after lowering soft");
            CHECK(check.rlim_cur == lowered_soft.rlim_cur,
                  "getrlimit observes lowered soft");
            CHECK(check.rlim_max == saved.rlim_max,
                  "lowering soft keeps hard unchanged");
            CHECK_RET(setrlimit(RLIMIT_NOFILE, &saved), 0,
                      "restore after lowering soft");
        }

        if (saved.rlim_max > 2 && saved.rlim_max < RLIM_INFINITY) {
            struct rlimit equal_limits = {
                .rlim_cur = saved.rlim_max - 1,
                .rlim_max = saved.rlim_max - 1,
            };
            struct rlimit lowered_hard = {
                .rlim_cur = saved.rlim_max - 2,
                .rlim_max = saved.rlim_max - 1,
            };
            CHECK_RET(setrlimit(RLIMIT_NOFILE, &equal_limits), 0,
                      "setrlimit accepts soft == hard");
            CHECK_RET(getrlimit(RLIMIT_NOFILE, &check), 0,
                      "getrlimit after setting soft == hard");
            CHECK(check.rlim_cur == equal_limits.rlim_cur,
                  "getrlimit observes soft == hard soft value");
            CHECK(check.rlim_max == equal_limits.rlim_max,
                  "getrlimit observes soft == hard max value");
            CHECK_RET(setrlimit(RLIMIT_NOFILE, &lowered_hard), 0,
                      "setrlimit lowers hard limit");
            CHECK_RET(getrlimit(RLIMIT_NOFILE, &check), 0,
                      "getrlimit after lowering hard");
            CHECK(check.rlim_cur == lowered_hard.rlim_cur,
                  "getrlimit observes lowered soft with hard");
            CHECK(check.rlim_max == lowered_hard.rlim_max,
                  "getrlimit observes lowered hard");
            CHECK_RET(setrlimit(RLIMIT_NOFILE, &saved), 0,
                      "restore hard limit after lowering");
            CHECK_RET(getrlimit(RLIMIT_NOFILE, &check), 0,
                      "getrlimit after restoring hard");
            CHECK(check.rlim_cur == saved.rlim_cur,
                  "restored soft limit is visible");
            CHECK(check.rlim_max == saved.rlim_max,
                  "restored hard limit is visible");
        }
    }

    /* 12. getrusage RUSAGE_SELF 基本功能 */
    {
        struct rusage usage;
        CHECK_RET(getrusage(RUSAGE_SELF, &usage), 0,
                  "getrusage RUSAGE_SELF");
        CHECK(usage.ru_utime.tv_sec >= 0, "utime >= 0");
        CHECK(usage.ru_stime.tv_sec >= 0, "stime >= 0");
    }

    /* 13. getrusage 无效 who 应返回 EINVAL */
    {
        struct rusage usage;
        CHECK_ERR(getrusage(42, &usage), EINVAL,
                  "getrusage invalid who rejected");
    }

    TEST_DONE();
}
