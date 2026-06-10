#define _GNU_SOURCE
#include "test_framework.h"

#include <stdio.h>
#include <sys/types.h>
#include <unistd.h>

/* cross_consistency — 6 个 getter syscall 内部一致性矩阵.
 *
 * man 2 getuid §"DESCRIPTION":
 *   "getuid() returns the real user ID of the calling process."
 * man 2 getresuid §"DESCRIPTION":
 *   "getresuid() returns the real UID, the effective UID, and the saved
 *    set-user-ID of the calling process, in the arguments ruid, euid, and
 *    suid, respectively."
 *
 * 隐含规范 — 同一 cred 子系统的多个 reader 应返回一致数据:
 *   getuid()       == getresuid().ruid
 *   geteuid()      == getresuid().euid
 *   getgid()       == getresgid().rgid
 *   getegid()      == getresgid().egid
 *
 * 任何不一致 = starry cred 子系统内部状态不同步 (locking bug / cache bug).
 *
 * 5 维度覆盖:
 *   (a) uid pair (getuid vs getresuid).
 *   (b) gid pair (getgid vs getresgid).
 *   (c) 100x interleaved getuid/geteuid 稳定 (race detection).
 *   (d) 100x interleaved getgid/getegid 稳定 (gid 镜像).
 *   (e) 100x getresuid 全字段稳定 (multi-field cred read race).
 */

static void uid_pair_consistency(void)
{
    /* 测什么: 隐含规范 — getuid() == getresuid().ruid + geteuid() == ...euid.
     * 怎么测: 同一进程内, 调 getuid + geteuid 后再调 getresuid 比对.
     * 期望:   r2 == u, e2 == e.
     * 为什么: 验证 starry sys_getuid + sys_getresuid 都读同一 cred.uid/euid 字段;
     *         不同 syscall 路径不应有内部 cache 不同步. */
    uid_t u = getuid();
    uid_t e = geteuid();
    uid_t r2, e2, s2;
    if (getresuid(&r2, &e2, &s2) != 0) { CHECK(0, "skip"); return; }
    CHECK(u == r2,                                                 "cross (a1): getuid() == getresuid().ruid");
    CHECK(e == e2,                                                 "cross (a2): geteuid() == getresuid().euid");
}

static void gid_pair_consistency(void)
{
    /* 测什么: 同 uid_pair 但对 gid 维度.
     * 怎么测: getgid + getegid → getresgid 比对.
     * 期望:   gr2 == g, ge2 == eg.
     * 为什么: 验证 starry sys_getgid + sys_getresgid 同源 cred.gid/egid. */
    gid_t g = getgid();
    gid_t e = getegid();
    gid_t r2, e2, s2;
    if (getresgid(&r2, &e2, &s2) != 0) { CHECK(0, "skip"); return; }
    CHECK(g == r2,                                                 "cross (b1): getgid() == getresgid().rgid");
    CHECK(e == e2,                                                 "cross (b2): getegid() == getresgid().egid");
}

static void interleaved_stability(void)
{
    /* 测什么: 同一进程多次 getuid/geteuid 之间无 race / cred 不被无故变.
     * 怎么测: 100 轮内 getuid → geteuid → getuid → geteuid, 每轮验
     *         前后 getuid 一致 + 前后 geteuid 一致. 累计 n_inconsistent.
     * 期望:   n_inconsistent == 0.
     * 为什么: 防 starry cred read path 有 race (并发 setuid 影响读) /
     *         有 stale cache. 单线程纯读应永远稳定. */
    int n_inconsistent = 0;
    for (int i = 0; i < 100; i++) {
        uid_t u1 = getuid();
        uid_t e1 = geteuid();
        uid_t u2 = getuid();
        uid_t e2 = geteuid();
        if (u1 != u2 || e1 != e2) n_inconsistent++;
    }
    CHECK(n_inconsistent == 0,                                     "cross (c): 100x interleaved getuid/geteuid stable");
}

static void interleaved_stability_gid(void)
{
    /* 测什么: gid 镜像 (c) — 100x getgid/getegid 稳定.
     * 怎么测: 同 (c) 模式.
     * 期望:   n_inconsistent == 0.
     * 为什么: 覆盖 starry cred.gid/egid 读路径单线程稳定性
     *         (uid 通过 (c) 已测; 单测一边不够, 镜像必要). */
    int n_inconsistent = 0;
    for (int i = 0; i < 100; i++) {
        gid_t g1 = getgid();
        gid_t e1 = getegid();
        gid_t g2 = getgid();
        gid_t e2 = getegid();
        if (g1 != g2 || e1 != e2) n_inconsistent++;
    }
    CHECK(n_inconsistent == 0,                                     "cross (d): 100x interleaved getgid/getegid stable");
}

static void interleaved_stability_getres(void)
{
    /* 测什么: getresuid 多字段读 (r/e/s 3 个) 100x 稳定.
     * 怎么测: baseline 一次 → 100x getresuid → 比对每次 r/e/s vs baseline.
     * 期望:   全部一致.
     * 为什么: getresuid 多字段读涉及 starry vm_write 3 次写 user pointer,
     *         若有 partial-write / race 会读出 mixed cred 状态. 此测验证
     *         三字段写之间 cred 不变. */
    uid_t r0, e0, s0;
    if (getresuid(&r0, &e0, &s0) != 0) { CHECK(0, "skip"); return; }
    int n_inconsistent = 0;
    for (int i = 0; i < 100; i++) {
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) { n_inconsistent++; continue; }
        if (r != r0 || e != e0 || s != s0) n_inconsistent++;
    }
    CHECK(n_inconsistent == 0,                                     "cross (e): 100x getresuid 全字段稳定");
}

int cross_consistency_run(void)
{
    printf("\n----- cross_consistency -----\n");
    uid_pair_consistency();
    gid_pair_consistency();
    interleaved_stability();
    interleaved_stability_gid();
    interleaved_stability_getres();
    printf("  ----- cross_consistency: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
