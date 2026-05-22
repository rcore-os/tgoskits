#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setuid(2) — set user identity.
 *
 * man 2 setuid §"DESCRIPTION":
 *   "setuid() sets the effective user ID of the calling process. If the
 *    calling process is privileged (more precisely: if the process has the
 *    CAP_SETUID capability in its user namespace), the real UID and saved
 *    set-user-ID are also set."
 *
 *   "Under Linux, setuid() is implemented like the POSIX version with the
 *    _POSIX_SAVED_IDS feature."
 *
 * man §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is
 *    set to indicate the error."
 *
 * man §"ERRORS":
 *   "EAGAIN — uid does not match the real UID and uid brings the number of
 *    processes belonging to the real user ID to one greater than RLIMIT_NPROC."
 *   "EINVAL — uid is not valid in this user namespace."
 *   "EPERM — The user is not privileged (does not have the CAP_SETUID
 *    capability in its user namespace) and uid does not match the real UID
 *    or saved set-user-ID of the calling process."
 *
 * 测试方式（self-contained，root 与 unprivileged 子进程 fork 隔离）：
 *   (a) setuid(getuid()) — 设当前 uid 应总成功，行为 idempotent
 *   (b) root: setuid(uid) → uid+euid+suid 全设
 *   (c) root + setuid(0) → 仍为 root
 *   (d) raw syscall vs libc（成功路径）
 *   (e) unprivileged child setuid(0) → -1 EPERM
 *   (f) **raw vs libc 失败路径**（codex P1）— unpriv setuid(0) raw vs libc errno 一致性
 *   (g) **setuid 不可逆性**（root → setuid(1000) → 再 setuid(0) 应 EPERM）
 *   (h) **fork 后 child 独立 setuid 不影响 parent**
 *   (i) **setuid(geteuid()) 不应触发 capability flush**（man Notes）
 *
 * Skip（man 列但不实用）：
 *   - EAGAIN (RLIMIT_NPROC): 需 fork bomb + ulimit 设置，CI 难触发
 *   - EINVAL (Linux 接受任何 u32 — uid 永不"invalid"，无 user namespace 概念时)
 *   - ENOMEM: 不可控触发
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setuid_idempotent_to_self(void)
{
    /* 怎么测：setuid(getuid()) — 设回当前 uid
     * 期望：返回 0（永远成功 — uid 已是合法值之一）
     * 为什么：man EPERM "uid does not match the real UID or saved set-user-ID"；
     *           设回自己 uid 不触发 EPERM */
    uid_t u = getuid();
    int rc = setuid(u);
    CHECK(rc == 0,                                                "setuid (a) setuid(getuid()) -> 0 (idempotent)");
}

static void setuid_root_sets_all_three(void)
{
    /* 怎么测：root 调 setuid(0) → 检查 ruid/euid/suid 三者
     * 期望：man "If the calling process is privileged ... the real UID and
     *           saved set-user-ID are also set."
     * 为什么：CAP_SETUID 时 setuid 影响所有 3 个 ID */
    uid_t u = getuid();
    if (u != 0) {
        printf("  setuid (b) skip: not running as root (uid=%u)\n", (unsigned)u);
        return;
    }
    int rc = setuid(0);
    CHECK(rc == 0,                                                "setuid (b) root setuid(0) -> 0");
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) == 0) {
        CHECK(r == 0 && e == 0 && s == 0,                         "setuid (b) root setuid(0): r=e=s=0");
    }
}

static void setuid_returns_zero_on_success(void)
{
    /* 测什么: man §RETURN VALUE — "On success, zero is returned."
     * 怎么测: setuid(getuid()) 总成功 (idempotent), 验证 rc.
     * 期望:   rc == 0.
     * 为什么: 显式断言 success path 返 0 (而非 == getuid() 或其他奇怪值). */
    int rc = setuid(getuid());
    CHECK(rc == 0,                                                "setuid (c) returns 0 on success");
}

static void setuid_raw_syscall_matches_libc(void)
{
    /* 测什么: libc 应直接转发 syscall, 无值转换 / 内部 cache.
     *         man §HISTORY: "The glibc setuid() wrapper function transparently
     *         deals with the variation across kernel versions."
     * 怎么测: raw syscall + libc 各跑一次 setuid(getuid()), 都应返 0.
     * 期望:   两次 rc == 0.
     * 为什么: 验证 starry sys_setuid ABI 与 libc 包装契约一致 (无 errno
     *         偏移 / 无 16-bit truncation). */
    uid_t u = getuid();
    long rc1 = syscall(SYS_setuid, u);
    int rc2 = setuid(u);
    CHECK(rc1 == 0 && rc2 == 0,                                   "setuid (d) raw syscall == libc (both = 0)");
}

static void setuid_unprivileged_eperm_in_child(void)
{
    /* 怎么测：fork → child 通过 setresuid 切到非 root → 然后 setuid(任意非自己 uid)
     * 期望：-1 EPERM（不能切到不属于当前权限范围的 uid）
     * 为什么：man EPERM — "uid does not match the real UID or saved set-user-ID"
     * 注：必须在 root 启动；child 内先切到 1000，再试 setuid(0) → EPERM */
    if (getuid() != 0) {
        printf("  setuid (e) skip: requires root to setup unprivileged child\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* child: 切到非 root */
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(99); /* setresuid 失败 — 不该发生在 root */
        }
        /* 现在尝试 setuid(0) — 应失败 EPERM */
        errno = 0;
        int rc = setuid(0);
        if (rc == -1 && errno == EPERM) _exit(0);  /* PASS */
        _exit(1);  /* FAIL */
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            int es = WEXITSTATUS(status);
            CHECK(WIFEXITED(status) && es == 0,                   "setuid (e) unprivileged child setuid(0) -> -1 EPERM");
            if (es == 99) printf("  (setresuid setup failed in child)\n");
        }
    } else {
        CHECK(0, "setuid (e) fork failed");
    }
}

/* (f) raw vs libc 失败路径 errno 一致性
 *
 * codex P1 on PR #5 boundary.c:91 — 当前 raw-vs-libc 测只覆盖成功路径
 * (setuid(getuid()) -> 0)。应加失败路径：unpriv setuid(0) → raw 返
 * -EPERM (内核 ABI)，libc 返 -1+errno=EPERM。验证两者一致。
 */
static void setuid_raw_vs_libc_failed_path(void)
{
    if (getuid() != 0) {
        printf("  setuid (f) skip: requires root to setup unprivileged child\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);

        /* raw syscall: kernel ABI 返 -errno (负值) */
        errno = 0;
        long raw_rc = syscall(SYS_setuid, 0);
        int raw_err = errno;

        /* libc 包装: 返 -1 + errno */
        errno = 0;
        int libc_rc = setuid(0);
        int libc_err = errno;

        /* 期望：
         *   raw: rc 返 -1 (libc-wrapped syscall) 或 -EPERM (raw kernel return);
         *        errno 设为 EPERM
         *   libc: rc == -1, errno == EPERM
         * 验证两者 errno 一致 */
        if (raw_rc == -1 && raw_err == EPERM
            && libc_rc == -1 && libc_err == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setuid (f) raw syscall == libc on FAIL path (both -1 EPERM)");
        }
    } else {
        CHECK(0, "setuid (f) fork failed");
    }
}

/* (g) setuid 不可逆性 — root → setuid(1000) 后 setuid(0) 应 EPERM */
static void setuid_irreversibility(void)
{
    if (getuid() != 0) {
        printf("  setuid (g) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* root: setuid(1000) → all 3 IDs become 1000，saved=1000 */
        if (setuid(1000) != 0) _exit(99);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(98);
        if (r != 1000 || e != 1000 || s != 1000) _exit(97);

        /* 现在不再是 root；setuid(0) 应失败（0 不在 {1000,1000,1000} 集合内）*/
        errno = 0;
        int rc = setuid(0);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setuid (g) irreversibility: root→setuid(1000)→setuid(0)→-1 EPERM");
        }
    }
}

/* (h) fork 后 child 独立 setuid 不影响 parent */
static void setuid_fork_child_independent(void)
{
    if (getuid() != 0) {
        printf("  setuid (h) skip: requires root\n");
        return;
    }
    uid_t parent_uid_before = getuid();
    pid_t pid = fork();
    if (pid == 0) {
        /* child setuid; 不应影响 parent */
        if (setuid(2000) != 0) _exit(1);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        waitpid_safely(pid, &status);
        /* parent uid 仍是原 root */
        CHECK(getuid() == parent_uid_before,
              "setuid (h) fork child setuid 不影响 parent");
    }
}

/* (i) setuid(geteuid()) 应成功（idempotent in another form），不引发副作用
 *     注：完整 capability flush 验证需要 cap_get/set 复杂操作，本测仅验证
 *     函数返 0 + cred 状态保持 */
static void setuid_self_euid_no_flush(void)
{
    uid_t e0 = geteuid();
    uid_t r0, eu0, s0;
    if (getresuid(&r0, &eu0, &s0) != 0) { CHECK(0, "skip"); return; }
    int rc = setuid(e0);  /* setuid(self.euid) */
    CHECK(rc == 0,                                                "setuid (i) setuid(geteuid()) -> 0");
    uid_t r1, eu1, s1;
    if (getresuid(&r1, &eu1, &s1) == 0) {
        /* root: r/e/s 全设为 e0（同原值）；non-root: 仅 euid 设为 e0（同原值）;
         * 实际 cred 应无变化 */
        CHECK(r1 == r0 && eu1 == eu0 && s1 == s0,                  "setuid (i) setuid(geteuid()) 不改变任何 ID");
    }
}

/* codex P1 (adopted) — setuid.c:259 errno coverage:
 *
 * man 2 setuid §"ERRORS": EAGAIN / EINVAL / EPERM
 * 主 suite 之前仅 EPERM 路径有 case (e/f), EAGAIN/EINVAL 缺.
 * - EAGAIN: NPROC 限. Linux 3.1+ 不再触发. 仅 best-effort 注释.
 * - EINVAL: uid invalid (Linux uid_valid 拒 (uid_t)-1). starry 不实现.
 *   → 加 KNOWN-STARRY-BUG soft case + ref bug-*. */
static void setuid_einval_uid_minus_one(void)
{
    if (getuid() != 0) {
        printf("  setuid (j) skip: requires root to fork unpriv child\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* root 自身: setuid((uid_t)-1) → Linux EINVAL; starry 接受. */
        errno = 0;
        int rc = setuid((uid_t)-1);
        int err = errno;
        if (rc == -1 && err == EINVAL) _exit(0);   /* Linux 行为 */
        if (rc == 0)                   _exit(2);   /* starry bug A: 接受 */
        if (rc == -1 && err == EPERM)  _exit(3);   /* starry bug B: fall-through EPERM */
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "setuid (j) Linux 行为: root setuid((uid_t)-1) -> -1 EINVAL");
    } else if (ec == 2) {
        printf("  KNOWN-STARRY-BUG | setuid (j) starry 接受 setuid((uid_t)-1) [bug A] | see bugfix/bug-starry-setuid-setgid-no-uidvalid\n");
    } else if (ec == 3) {
        printf("  KNOWN-STARRY-BUG | setuid (j) starry 返 EPERM 而非 EINVAL [bug B fall-through] | see bugfix/bug-starry-setuid-setgid-no-uidvalid\n");
    } else {
        CHECK(0, "setuid (j) unexpected outcome");
    }
}

int setuid_run(void)
{
    printf("\n----- setuid -----\n");
    setuid_idempotent_to_self();
    setuid_root_sets_all_three();
    setuid_returns_zero_on_success();
    setuid_raw_syscall_matches_libc();
    setuid_unprivileged_eperm_in_child();
    setuid_raw_vs_libc_failed_path();   /* (f) new */
    setuid_irreversibility();           /* (g) new */
    setuid_fork_child_independent();    /* (h) new */
    setuid_self_euid_no_flush();        /* (i) new */
    setuid_einval_uid_minus_one();      /* (j) codex P1 — EINVAL coverage */
    printf("  ----- setuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
