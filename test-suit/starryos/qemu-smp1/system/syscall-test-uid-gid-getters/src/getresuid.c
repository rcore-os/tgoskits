#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* getresuid(2) — get real, effective and saved user IDs (Linux-specific).
 *
 * man 2 getresuid §"DESCRIPTION":
 *   "getresuid() returns the real UID, the effective UID, and the saved
 *    set-user-ID of the calling process, in the arguments ruid, euid, and
 *    suid, respectively. getresgid() performs the analogous task for the
 *    process's group IDs."
 *
 * man §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is
 *    set to indicate the error."
 *
 * man §"ERRORS":
 *   "EFAULT — One of the arguments specified an address outside the calling
 *    process's address space."
 *
 * 测试覆盖：
 *   (a) 基础三参数填写 + 返回 0
 *   (b) ruid 等于 getuid() 返回值
 *   (c) euid 等于 geteuid() 返回值
 *   (d) suid 默认等于 euid（启动时 saved=effective，man "fork() and exec()" 节）
 *   (e) NULL pointer args → -1 EFAULT（任一指针 NULL 都应报错）
 *   (f) raw syscall 同
 */

static void getresuid_basic_writes_three(void)
{
    /* codex P1 (adopted): 验证三个 out-arg **都** 被写入。双 sentinel 跑两次 */
    uid_t r1 = 0xDEADBEEF, e1 = 0xDEADBEEF, s1 = 0xDEADBEEF;
    int rc1 = getresuid(&r1, &e1, &s1);
    CHECK(rc1 == 0,                                                "getresuid (a) basic: returns 0");
    CHECK(r1 != 0xDEADBEEF,                                        "getresuid (a) basic: ruid written (sentinel #1)");
    CHECK(e1 != 0xDEADBEEF,                                        "getresuid (a) basic: euid written (sentinel #1)");
    CHECK(s1 != 0xDEADBEEF,                                        "getresuid (a) basic: suid written (sentinel #1)");
    uid_t r2 = 0xCAFEBABE, e2 = 0xCAFEBABE, s2 = 0xCAFEBABE;
    int rc2 = getresuid(&r2, &e2, &s2);
    CHECK(rc2 == 0,                                                "getresuid (a) basic: 2nd returns 0");
    CHECK(r2 != 0xCAFEBABE,                                        "getresuid (a) basic: ruid written (sentinel #2)");
    CHECK(e2 != 0xCAFEBABE,                                        "getresuid (a) basic: euid written (sentinel #2)");
    CHECK(s2 != 0xCAFEBABE,                                        "getresuid (a) basic: suid written (sentinel #2)");
    CHECK(r1 == r2 && e1 == e2 && s1 == s2,                        "getresuid (a) basic: 两次调用结果一致");
    printf("  ruid=%u euid=%u suid=%u\n", (unsigned)r1, (unsigned)e1, (unsigned)s1);
}

static void getresuid_ruid_equals_getuid(void)
{
    /* 测什么: 隐含规范 — getresuid().ruid == getuid() (同一 cred.uid 字段).
     * 怎么测: 同一进程内调 getresuid + getuid 比对 r 槽.
     * 期望:   r == g.
     * 为什么: 验证 starry sys_getresuid 写出的 ruid 与 sys_getuid 读出的同源
     *         (都基于 cred.uid). 不一致 = cred 子系统内部 bug. */
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) { CHECK(0, "skip: getresuid failed"); return; }
    uid_t g = getuid();
    CHECK(r == g,                                                  "getresuid (b) ruid == getuid()");
}

static void getresuid_euid_equals_geteuid(void)
{
    /* 测什么: 隐含规范 — getresuid().euid == geteuid().
     * 怎么测: 同一进程调 getresuid + geteuid 比对 e 槽.
     * 期望:   e == ge.
     * 为什么: 同 (b), 但 euid 维度. 验证 starry cred.euid 在两 syscall 路径一致. */
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) { CHECK(0, "skip: getresuid failed"); return; }
    uid_t ge = geteuid();
    CHECK(e == ge,                                                 "getresuid (c) euid == geteuid()");
}

static void getresuid_suid_equals_euid_at_startup(void)
{
    /* 测什么: man execve(2) — exec 后 saved-set-user-ID = effective UID.
     *         所以测试启动时 (尚未 setresuid 修改) saved == effective.
     * 怎么测: 调 getresuid, 比对 s == e.
     * 期望:   s == e (启动后未做 setresuid 修改 saved 的操作).
     * 为什么: 验证 starry exec 流程正确设 cred.suid = cred.euid;
     *         也为后续 setresuid 矩阵 (Group D) 提供 known baseline. */
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) { CHECK(0, "skip: getresuid failed"); return; }
    CHECK(s == e,                                                  "getresuid (d) suid == euid at startup");
}

static void getresuid_null_pointer_efault(void)
{
    /* 怎么测：3 个指针中任一 NULL → 用 raw syscall 直达内核
     * 期望：-1 EFAULT
     * 为什么：man EFAULT — "One of the arguments specified an address outside ..."
     *
     * 注：libc getresuid() 包装可能会做 NULL 校验且不调 syscall；用 raw syscall 直测内核。 */
    uid_t r;
    long rc;

    errno = 0;
    rc = syscall(SYS_getresuid, NULL, &r, &r);
    CHECK(rc == -1 && errno == EFAULT,                             "getresuid (e1) NULL ruid -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresuid, &r, NULL, &r);
    CHECK(rc == -1 && errno == EFAULT,                             "getresuid (e2) NULL euid -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresuid, &r, &r, NULL);
    CHECK(rc == -1 && errno == EFAULT,                             "getresuid (e3) NULL suid -> -1 EFAULT");
}

static void getresuid_raw_matches_libc(void)
{
    /* 测什么: libc 应直接转发到 syscall, 不做值转换 / 内部 cache.
     * 怎么测: 同一进程内 libc 调一次 + raw syscall 调一次, 比对 r/e/s.
     * 期望:   两次都返 0 + 三槽都相等.
     * 为什么: 验证 libc wrapper 无额外语义改造 — 直接对接 starry syscall ABI. */
    uid_t r1 = 0, e1 = 0, s1 = 0;
    uid_t r2 = 0, e2 = 0, s2 = 0;
    int libc_rc = getresuid(&r1, &e1, &s1);
    long raw_rc = syscall(SYS_getresuid, &r2, &e2, &s2);
    CHECK(libc_rc == 0 && raw_rc == 0 && r1 == r2 && e1 == e2 && s1 == s2,
          "getresuid (f) raw syscall matches libc wrapper");
}

/* (g) 三个 out-arg 指向同一地址 — last-write 胜出（应等于 suid）
 *
 * man 2 getresuid 未明禁此用法；按内核拷贝顺序 ruid → euid → suid，
 * 最终值应为 suid。这验证了内核不假设三参数 disjoint。 */
static void getresuid_same_pointer_last_write(void)
{
    uid_t v = 0xdeadbeef;
    int rc = getresuid(&v, &v, &v);
    CHECK(rc == 0,                                                 "getresuid (g) same-pointer 3-args -> 0 (kernel doesn't reject aliased args)");
    uid_t s_alone;
    uid_t r_alone, e_alone;
    if (getresuid(&r_alone, &e_alone, &s_alone) == 0) {
        CHECK(v == s_alone,                                        "getresuid (g) same-pointer: v == suid (last-write wins)");
    }
}

/* (h) 成功路径 errno 保留 — man 未规定 getresuid 不改 errno，
 *     但 Linux 实测成功路径不动 errno（仅失败路径设）。
 *     这覆盖 starry vm_write 成功后是否误改 errno。 */
static void getresuid_success_preserves_errno(void)
{
    uid_t r, e, s;
    errno = 4242;
    int rc = getresuid(&r, &e, &s);
    CHECK(rc == 0,                                                 "getresuid (h) success path: rc == 0");
    CHECK(errno == 4242,                                           "getresuid (h) success path: errno preserved (4242)");
    errno = 0;
}

int getresuid_run(void)
{
    printf("\n----- getresuid -----\n");
    getresuid_basic_writes_three();
    getresuid_ruid_equals_getuid();
    getresuid_euid_equals_geteuid();
    getresuid_suid_equals_euid_at_startup();
    getresuid_null_pointer_efault();
    getresuid_raw_matches_libc();
    getresuid_same_pointer_last_write();   /* (g) new */
    getresuid_success_preserves_errno();   /* (h) new */
    printf("  ----- getresuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
