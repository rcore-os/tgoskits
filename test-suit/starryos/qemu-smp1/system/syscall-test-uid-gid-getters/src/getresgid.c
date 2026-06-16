#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* getresgid(2) — get real, effective and saved group IDs (Linux-specific).
 *
 * man 2 getresuid §"DESCRIPTION":
 *   "getresgid() performs the analogous task for the process's group IDs."
 *   (镜像 getresuid: 三 out-arg 填 rgid/egid/sgid)
 *
 * man §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is set."
 *
 * man §"ERRORS":
 *   "EFAULT — One of the arguments specified an address outside the calling
 *    process's address space."
 *
 * 8 维度覆盖 (a-h, 镜像 getresuid):
 *   (a) 基础三参数填写 + 返回 0 (双 sentinel × 三字段都验, codex P1)
 *   (b) rgid == getgid()
 *   (c) egid == getegid()
 *   (d) sgid == egid (启动后)
 *   (e1-e3) NULL pointer 三槽 → EFAULT
 *   (f) raw syscall vs libc 一致
 *   (g) 三 out-arg aliased 同地址 → last-write (sgid) 胜出
 *   (h) 成功路径 errno 保留
 */

static void getresgid_basic_writes_three(void)
{
    /* 测什么: man §DESCRIPTION — 三个 out-arg 应都被填入 cred.gid/egid/sgid.
     *         codex P1 (adopted) — 不只验"至少一个被改", 而是验三个都被改.
     * 怎么测: 双 sentinel (0xDEADBEEF / 0xCAFEBABE) 各跑一次 getresgid,
     *         每次验三个 out-arg 都 != sentinel + 两次结果一致.
     * 期望:   每次 rc=0, 三个字段都被改 (双 sentinel 避免 cred 恰为 sentinel 假阳).
     * 为什么: 防御 starry vm_write 只写一/两个 out-arg 漏掉第三的 bug. */
    gid_t r1 = 0xDEADBEEF, e1 = 0xDEADBEEF, s1 = 0xDEADBEEF;
    int rc1 = getresgid(&r1, &e1, &s1);
    CHECK(rc1 == 0,                                                "getresgid (a) basic: returns 0");
    CHECK(r1 != 0xDEADBEEF,                                        "getresgid (a) basic: rgid written (sentinel #1)");
    CHECK(e1 != 0xDEADBEEF,                                        "getresgid (a) basic: egid written (sentinel #1)");
    CHECK(s1 != 0xDEADBEEF,                                        "getresgid (a) basic: sgid written (sentinel #1)");
    gid_t r2 = 0xCAFEBABE, e2 = 0xCAFEBABE, s2 = 0xCAFEBABE;
    int rc2 = getresgid(&r2, &e2, &s2);
    CHECK(rc2 == 0,                                                "getresgid (a) basic: 2nd returns 0");
    CHECK(r2 != 0xCAFEBABE,                                        "getresgid (a) basic: rgid written (sentinel #2)");
    CHECK(e2 != 0xCAFEBABE,                                        "getresgid (a) basic: egid written (sentinel #2)");
    CHECK(s2 != 0xCAFEBABE,                                        "getresgid (a) basic: sgid written (sentinel #2)");
    CHECK(r1 == r2 && e1 == e2 && s1 == s2,                        "getresgid (a) basic: 两次调用结果一致");
    printf("  rgid=%u egid=%u sgid=%u\n", (unsigned)r1, (unsigned)e1, (unsigned)s1);
}

static void getresgid_rgid_equals_getgid(void)
{
    /* 测什么: 隐含规范 — getresgid().rgid == getgid() (同源 cred.gid).
     * 怎么测: 同一进程调 getresgid + getgid 比对 r 槽.
     * 期望:   r == g.
     * 为什么: 验证 starry sys_getresgid 写出 rgid 与 sys_getgid 同源. */
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    gid_t g = getgid();
    CHECK(r == g,                                                  "getresgid (b) rgid == getgid()");
}

static void getresgid_egid_equals_getegid(void)
{
    /* 测什么: 隐含规范 — getresgid().egid == getegid() (同源 cred.egid).
     * 怎么测: 同一进程调 getresgid + getegid 比对 e 槽.
     * 期望:   e == ge.
     * 为什么: 验证 starry cred.egid 在两 syscall 路径一致. */
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    gid_t ge = getegid();
    CHECK(e == ge,                                                 "getresgid (c) egid == getegid()");
}

static void getresgid_sgid_equals_egid_at_startup(void)
{
    /* 测什么: man execve(2) — exec 后 saved-set-group-ID = effective GID.
     *         启动时 (尚未 setresgid) saved == effective.
     * 怎么测: 调 getresgid, 比对 s == e.
     * 期望:   s == e.
     * 为什么: 验证 starry exec 流程正确设 cred.sgid = cred.egid;
     *         为 Group D setresgid 矩阵提供 known baseline. */
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    CHECK(s == e,                                                  "getresgid (d) sgid == egid at startup");
}

static void getresgid_null_pointer_efault(void)
{
    /* 测什么: man §ERRORS — EFAULT 应在任一槽无效时触发.
     * 怎么测: 用 raw syscall 直达内核 (libc 可能拦截 NULL),
     *         3 个槽分别测 NULL.
     * 期望:   每次 rc=-1, errno=EFAULT.
     * 为什么: 验证 starry vm_write 对 NULL 的处理 fail-fast (任一槽 NULL 就报错). */
    gid_t r;
    long rc;

    errno = 0;
    rc = syscall(SYS_getresgid, NULL, &r, &r);
    CHECK(rc == -1 && errno == EFAULT,                             "getresgid (e1) NULL rgid -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresgid, &r, NULL, &r);
    CHECK(rc == -1 && errno == EFAULT,                             "getresgid (e2) NULL egid -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresgid, &r, &r, NULL);
    CHECK(rc == -1 && errno == EFAULT,                             "getresgid (e3) NULL sgid -> -1 EFAULT");
}

static void getresgid_raw_matches_libc(void)
{
    /* 测什么: libc 应直接转发到 syscall, 无值转换.
     * 怎么测: libc 调一次 + raw syscall 调一次, 比对三槽.
     * 期望:   两次 rc=0 + 三槽相等.
     * 为什么: 验证 libc wrapper 无额外语义改造, ABI 一致. */
    gid_t r1 = 0, e1 = 0, s1 = 0;
    gid_t r2 = 0, e2 = 0, s2 = 0;
    int libc_rc = getresgid(&r1, &e1, &s1);
    long raw_rc = syscall(SYS_getresgid, &r2, &e2, &s2);
    CHECK(libc_rc == 0 && raw_rc == 0 && r1 == r2 && e1 == e2 && s1 == s2,
          "getresgid (f) raw syscall matches libc wrapper");
}

static void getresgid_same_pointer_last_write(void)
{
    /* 测什么: 三 out-arg 指向同一地址 (aliased) 应不被拒. man 未明禁此用法.
     *         按内核拷贝顺序 rgid → egid → sgid, 最终值应为 sgid.
     * 怎么测: 三 out-arg 都指向同一变量 v, 调 getresgid; 然后取一次
     *         独立的 getresgid 比对 v == sgid.
     * 期望:   rc=0, v == sgid (last-write 胜出).
     * 为什么: 验证 starry 不假设 args disjoint — 实际场景下用户可能传同指针. */
    gid_t v = 0xdeadbeef;
    int rc = getresgid(&v, &v, &v);
    CHECK(rc == 0,                                                 "getresgid (g) same-pointer 3-args -> 0 (kernel doesn't reject aliased args)");
    gid_t r_alone, e_alone, s_alone;
    if (getresgid(&r_alone, &e_alone, &s_alone) == 0) {
        CHECK(v == s_alone,                                        "getresgid (g) same-pointer: v == sgid (last-write wins)");
    }
}

static void getresgid_success_preserves_errno(void)
{
    /* 测什么: man 未明文规定 getresgid 不动 errno, 但 Linux 实测成功路径
     *         不动 errno (仅失败设).
     * 怎么测: 预设 errno = 5353, 调 getresgid (成功), 验 errno 未变.
     * 期望:   rc=0, errno=5353.
     * 为什么: 防 starry vm_write 内部误设 errno; 用户依赖 errno 在成功
     *         系统调用后保持不变. */
    gid_t r, e, s;
    errno = 5353;
    int rc = getresgid(&r, &e, &s);
    CHECK(rc == 0,                                                 "getresgid (h) success path: rc == 0");
    CHECK(errno == 5353,                                           "getresgid (h) success path: errno preserved (5353)");
    errno = 0;
}

int getresgid_run(void)
{
    printf("\n----- getresgid -----\n");
    getresgid_basic_writes_three();
    getresgid_rgid_equals_getgid();
    getresgid_egid_equals_getegid();
    getresgid_sgid_equals_egid_at_startup();
    getresgid_null_pointer_efault();
    getresgid_raw_matches_libc();
    getresgid_same_pointer_last_write();
    getresgid_success_preserves_errno();
    printf("  ----- getresgid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
