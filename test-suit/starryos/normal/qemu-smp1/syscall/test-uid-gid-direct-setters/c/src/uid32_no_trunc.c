/* uid32_no_trunc.c — setuid/setgid 32-bit 不截断验证 (Group B Round 7-B 补).
 *
 * man 2 setuid §"HISTORY":
 *   "The original Linux setuid() system call supported only 16-bit user IDs.
 *    Subsequently, Linux 2.4 added setuid32() supporting 32-bit IDs. The
 *    glibc setuid() wrapper function transparently deals with the variation
 *    across kernel versions."
 *
 * man 2 setgid §"HISTORY":
 *   "Linux 2.4 added setgid32() supporting 32-bit IDs."
 *
 * 设计: 验证 starry sys_setuid/setgid 接受 32-bit (>65535) ID 并完整存入
 *       cred (而非按 u16 截断, 因为截断会导致高位 UID 被映射到不同身份 —
 *       严重安全 bug).
 *
 * Linux 行为: root setuid(100001) → all 3 IDs == 100001
 * starry 行为: 应一致 (cred 字段是 u32)
 *
 * 若 starry 内部 u16 处理: r/e/s 会变成 100001 & 0xffff = 34465 → 验失败.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setuid_32bit_no_truncate(void)
{
    /* 测什么: man HISTORY — setuid 自 Linux 2.4 起 32-bit. 验 starry sys_setuid
     *         接受 >16-bit uid 并完整保留 (不截断到低 16).
     * 怎么测: root fork → child setuid(100001) → getresuid → 验三槽 == 100001.
     * 期望:   r=e=s=100001 (32-bit 完整).
     * 为什么: starry cred.uid u32, 若实现按 u16 处理 → 100001 → 34465 →
     *         高位 UID 映射到不同身份 → 安全风险. */
    if (getuid() != 0) {
        printf("  uid32 (a) skip: not root — 需 CAP_SETUID 设任意 uid\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setuid(100001) != 0) _exit(99);
        uid_t r = 0, e = 0, s = 0;
        if (getresuid(&r, &e, &s) != 0) _exit(98);
        if (r == 100001 && e == 100001 && s == 100001) _exit(0);
        if (r == (100001 & 0xffff)) _exit(20);  /* 16-bit truncation bug */
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (a) root setuid(100001) → r=e=s=100001 (32-bit 完整)");
        else if (ec == 20)  CHECK(0, "uid32 (a) FAIL: setuid 后 cred 被截断到 16-bit");
        else                CHECK(0, "uid32 (a) failed (ec != 0)");
    }
}

static void setgid_32bit_no_truncate(void)
{
    /* 测什么/怎么测/期望/为什么: 同 (a), 但 GID 维度. 验 starry sys_setgid
     *         在 32-bit gid 不截断. */
    if (getuid() != 0) {
        printf("  uid32 (b) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setgid(200200) != 0) _exit(99);
        gid_t r = 0, e = 0, s = 0;
        if (getresgid(&r, &e, &s) != 0) _exit(98);
        if (r == 200200 && e == 200200 && s == 200200) _exit(0);
        if (r == (200200 & 0xffff)) _exit(20);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (b) root setgid(200200) → r=e=s=200200 (32-bit 完整)");
        else if (ec == 20)  CHECK(0, "uid32 (b) FAIL: setgid 后 cred 被截断到 16-bit");
        else                CHECK(0, "uid32 (b) failed");
    }
}

static void setuid_32bit_via_raw_syscall(void)
{
    /* 测什么: 同 (a), 但走 raw syscall 直达内核 — 验 libc wrapper 不做
     *         16-bit downgrade (man HISTORY "glibc wrapper transparently deals").
     * 怎么测: root fork → child syscall(SYS_setuid, 300003) → getresuid 验.
     * 期望:   r=e=s=300003.
     * 为什么: 若 libc 包装混淆 setuid/setuid32 选错, 32-bit uid 会丢高位 —
     *         此 case 排除 libc 因素, 纯验 starry kernel ABI. */
    if (getuid() != 0) {
        printf("  uid32 (c) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        long rc = syscall(SYS_setuid, 300003);
        if (rc != 0) _exit(99);
        uid_t r = 0, e = 0, s = 0;
        if (getresuid(&r, &e, &s) != 0) _exit(98);
        if (r == 300003 && e == 300003 && s == 300003) _exit(0);
        if (r == (300003 & 0xffff)) _exit(20);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (c) raw syscall(SYS_setuid, 300003) → r=e=s=300003");
        else if (ec == 20)  CHECK(0, "uid32 (c) FAIL: raw setuid 被截断");
        else                CHECK(0, "uid32 (c) failed");
    }
}

int uid32_no_trunc_run(void)
{
    printf("\n----- uid32_no_trunc (man HISTORY 32-bit ID) -----\n");
    setuid_32bit_no_truncate();
    setgid_32bit_no_truncate();
    setuid_32bit_via_raw_syscall();
    printf("  ----- uid32_no_trunc: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
