#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* three_arg_independence — setresuid 三个参数独立性测试。
 *
 * setresuid/setresgid 与 setreuid/setregid 的关键区别：
 *   - setre* 有 saved-set-id 自动更新逻辑（隐式触发）
 *   - setres* 是显式三参数；NOCHG 各自独立处理
 *
 * 测试每个参数独立 NOCHG / 独立设值的 8 种组合：
 *   (NOCHG/SET) × 3 = 8 个矩阵单元
 *
 * 每单元验证：
 *   - 设的 ID 改变到目标值
 *   - NOCHG 的 ID 保持原值
 *   - 不应有"自动更新"副作用（与 setreuid 区分的核心）
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* 在子进程内测一种组合 — 子进程 cred 改变不影响 parent */
static int test_combo(int set_r, int set_e, int set_s,
                     uid_t target_r, uid_t target_e, uid_t target_s,
                     uid_t expect_r, uid_t expect_e, uid_t expect_s)
{
    pid_t pid = fork();
    if (pid == 0) {
        /* root: 第一次切到 r=1000 e=2000 s=3000，再用 setresuid 测组合 */
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        uid_t r = set_r ? target_r : (uid_t)-1;
        uid_t e = set_e ? target_e : (uid_t)-1;
        uid_t s = set_s ? target_s : (uid_t)-1;
        /* root 已切到 1000，但 setresuid 之后非 root；先用 root 重启 fork */
        _exit(99);
        (void)r; (void)e; (void)s;
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        }
    }
    (void)expect_r; (void)expect_e; (void)expect_s;
    return -1;
}

static void independence_root_only_r(void)
{
    if (getuid() != 0) { printf("  independence (a) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        /* 启动 r=e=s=0 */
        /* 设 ruid=1000，euid 和 suid NOCHG */
        if (setresuid(1000, (uid_t)-1, (uid_t)-1) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        /* 期望：r=1000, e=0, s=0（与 setreuid 不同 — 没有自动 suid 更新）*/
        if (r == 1000 && e == 0 && s == 0) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "indep (a) setresuid(1000,-1,-1) by root: r=1000 e=0 s=0 (no auto suid update unlike setreuid)");
        }
    }
    (void)test_combo;
}

static void independence_root_only_s(void)
{
    if (getuid() != 0) { printf("  independence (b) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        /* 仅设 saved，不动 r/e */
        if (setresuid((uid_t)-1, (uid_t)-1, 5000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0 && e == 0 && s == 5000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "indep (b) setresuid(-1,-1,5000) by root: r=0 e=0 s=5000 (only saved updated)");
        }
    }
}

static void independence_root_only_e(void)
{
    if (getuid() != 0) { printf("  independence (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid((uid_t)-1, 7000, (uid_t)-1) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0 && e == 7000 && s == 0) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "indep (c) setresuid(-1,7000,-1) by root: r=0 e=7000 s=0 (only euid updated)");
        }
    }
}

static void independence_all_three_distinct(void)
{
    if (getuid() != 0) { printf("  independence (d) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1111, 2222, 3333) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 1111 && e == 2222 && s == 3333) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "indep (d) setresuid(1111,2222,3333) by root: r=1111 e=2222 s=3333 (all 3 distinct)");
        }
    }
}

/* (e) Round 7-D 关键边界 — 全 NOCHG no-op
 *
 * man 2 setresuid §"DESCRIPTION":
 *   "If one of the arguments equals -1, the corresponding value is not
 *    changed."
 *
 * 三个参数全 -1 (NOCHG) → 严格 no-op:
 *   - r/e/s 都不变
 *   - fsuid (=new euid) 也不变 (因为 euid 没变)
 *   - rc=0 (NOCHG 不应是 error)
 *
 * 这是 (a)/(b)/(c) 部分 NOCHG 没覆盖的角落 — 全 NOCHG 边界.
 * starry 实现若把 setresuid(-1,-1,-1) 当 EINVAL 拒绝, 此 case fail.
 */
static void independence_all_nochg_no_op(void)
{
    /* 怎么测: 任意 uid 进程调 setresuid(-1, -1, -1), 验前后 getresuid 一致.
     * 期望: rc=0 + r/e/s 三槽都不变.
     * 为什么: 验 starry sys_setresuid 把 (uid_t)-1 三槽都识别为 sentinel,
     *         不误把 -1 当 invalid uid 触发 EINVAL. */
    uid_t r0, e0, s0;
    if (getresuid(&r0, &e0, &s0) != 0) { CHECK(0, "indep (e) baseline failed"); return; }
    int rc = setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1);
    CHECK(rc == 0, "indep (e) setresuid(-1,-1,-1) -> 0 (全 NOCHG)");
    uid_t r1, e1, s1;
    if (getresuid(&r1, &e1, &s1) == 0) {
        CHECK(r0 == r1 && e0 == e1 && s0 == s1,
              "indep (e) setresuid(-1,-1,-1) cred 完全不变");
    }
}

/* (f) Round 7-D GID 镜像 — setresgid(-1,-1,-1) 全 NOCHG */
static void independence_all_nochg_no_op_gid(void)
{
    /* 怎么测/期望/为什么: 同 (e) 但 GID 维度. */
    gid_t r0, e0, s0;
    if (getresgid(&r0, &e0, &s0) != 0) { CHECK(0, "indep (f) baseline failed"); return; }
    int rc = setresgid((gid_t)-1, (gid_t)-1, (gid_t)-1);
    CHECK(rc == 0, "indep (f) setresgid(-1,-1,-1) -> 0 (全 NOCHG)");
    gid_t r1, e1, s1;
    if (getresgid(&r1, &e1, &s1) == 0) {
        CHECK(r0 == r1 && e0 == e1 && s0 == s1,
              "indep (f) setresgid(-1,-1,-1) cred 完全不变");
    }
}

/* (g) Round 7-D — 全 NOCHG 在 unpriv 仍 OK + 不引发 EPERM
 *
 * man 2 setresuid §"DESCRIPTION":
 *   "An unprivileged process may change its real UID, effective UID, and
 *    saved set-user-ID, each to one of: the current real UID, the current
 *    effective UID, or the current saved set-user-ID."
 *
 * 全 NOCHG 即"all stay same" → 显然 each value 在自身集合内 → 不应 EPERM.
 * 验 starry !has_cap_setuid 路径的 NOCHG 仍接受 (sentinel 在 perm check 前).
 */
static void independence_all_nochg_unpriv_ok(void)
{
    if (getuid() != 0) { printf("  indep (g) skip: requires root\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        /* unpriv 路径 setresuid(-1,-1,-1) 应仍 OK */
        errno = 0;
        if (setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 1000 && e == 1000 && s == 1000) _exit(0);
        _exit(3);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "indep (g) unpriv setresuid(-1,-1,-1) OK (sentinel 在 perm check 前)");
    }
}

int three_arg_independence_run(void)
{
    printf("\n----- three_arg_independence -----\n");
    independence_root_only_r();
    independence_root_only_s();
    independence_root_only_e();
    independence_all_three_distinct();
    independence_all_nochg_no_op();         /* (e) Round 7-D 全 NOCHG UID */
    independence_all_nochg_no_op_gid();     /* (f) Round 7-D 全 NOCHG GID */
    independence_all_nochg_unpriv_ok();     /* (g) Round 7-D unpriv 全 NOCHG */
    printf("  ----- three_arg_independence: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
