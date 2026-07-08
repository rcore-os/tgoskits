/* uid32_no_trunc.c — 验证 getter syscall 返 32-bit ID 不被截断到 16-bit.
 *
 * man 2 getuid §"HISTORY":
 *   "The original Linux getuid() and geteuid() system calls supported only
 *    16-bit user IDs. Subsequently, Linux 2.4 added getuid32() and
 *    geteuid32(), supporting 32-bit IDs. The glibc getuid() and geteuid()
 *    wrapper functions transparently deal with the variations across
 *    kernel versions."
 *
 * man 2 getresuid §"HISTORY":
 *   "The original Linux getresuid() and getresgid() system calls supported
 *    only 16-bit user and group IDs. Subsequently, Linux 2.4 added
 *    getresuid32() and getresgid32(), supporting 32-bit IDs."
 *
 * 设计：当代 starry/Linux 应直接是 32-bit cred 字段; 验 setresuid 高位
 *       (>65535) 后 getter 返完整 32-bit 不被截断到低 16 位.
 *
 * Linux 行为: setresuid(100000,200000,300000) → getresuid 返 (100000,200000,300000)
 * starry 行为: 应该一致 (cred 字段是 u32 / kUidT 是 u32)
 *
 * 若 starry 内部按 16-bit 处理 (老 ABI), 将返低 16 位:
 *   100000 & 0xffff = 34464, 200000 & 0xffff = 3392, etc — 验失败.
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
    pid_t r;
    do {
        r = waitpid(pid, status, 0);
    } while (r < 0 && errno == EINTR);
    return r == pid ? 0 : -1;
}

static void uid32_getresuid_no_truncate(void)
{
    /* 测什么: 验 starry getresuid 输出未被截断到 16-bit (man HISTORY 暗指
     *         32-bit getuid32 自 Linux 2.4 起标准, 当代 cred 字段是 u32).
     * 怎么测: root fork → child setresuid(100001,200002,300003) → getresuid →
     *         验三槽 == 设值 (远 > 65535, 截断会变成低 16 位).
     * 期望:   r=100001 e=200002 s=300003 (全位保留).
     * 为什么: 验 starry cred.uid/euid/suid 字段宽度 + sys_getresuid vm_write
     *         路径不丢高位. 若失败 = starry 内部存了 u16 → 安全风险
     *         (高位 UID 被映射到不同身份). */
    if (getuid() != 0) {
        printf("  uid32 (a) skip: not root — 需 CAP_SETUID 设 >16-bit UID\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(100001, 200002, 300003) != 0) _exit(99);
        uid_t r = 0, e = 0, s = 0;
        if (getresuid(&r, &e, &s) != 0) _exit(98);
        /* 验全 32 位保留 */
        if (r == 100001 && e == 200002 && s == 300003) _exit(0);
        /* 若被 16-bit 截断, r 会是 100001 & 0xffff = 34465 */
        if (r == (100001 & 0xffff)) _exit(20);  /* truncation bug */
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (a) getresuid 返完整 32-bit (100001,200002,300003)");
        else if (ec == 20)  CHECK(0, "uid32 (a) FAIL: getresuid 截断到 16-bit (cred field 实际是 u16?)");
        else                CHECK(0, "uid32 (a) failed (ec != 0)");
    }
}

static void uid32_getresgid_no_truncate(void)
{
    /* 测什么/怎么测/期望/为什么: 同 (a), 但 GID 维度. 验 starry sys_getresgid
     *         vm_write 高位保留 (gid_t 32-bit). */
    if (getuid() != 0) {
        printf("  uid32 (b) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(100100, 200200, 300300) != 0) _exit(99);
        gid_t r = 0, e = 0, s = 0;
        if (getresgid(&r, &e, &s) != 0) _exit(98);
        if (r == 100100 && e == 200200 && s == 300300) _exit(0);
        if (r == (100100 & 0xffff)) _exit(20);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (b) getresgid 返完整 32-bit (100100,200200,300300)");
        else if (ec == 20)  CHECK(0, "uid32 (b) FAIL: getresgid 截断到 16-bit");
        else                CHECK(0, "uid32 (b) failed");
    }
}

static void uid32_getuid_geteuid_no_truncate(void)
{
    /* 测什么: 同 (a)/(b), 但 getuid/geteuid 路径 (基本 0-arg getter).
     * 怎么测: root fork → child setresuid(123456, 234567, 0) → getuid/geteuid
     *         验返设值.
     * 期望:   getuid==123456, geteuid==234567.
     * 为什么: 验 sys_getuid/geteuid 路径与 sys_getresuid 同源 (都从 cred 读),
     *         32-bit 不被截断. */
    if (getuid() != 0) {
        printf("  uid32 (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 注意: 必须 saved 留 0 (root) 才能后续完成 child exit cleanup */
        if (setresuid(123456, 234567, 0) != 0) _exit(99);
        uid_t u = getuid();
        uid_t eu = geteuid();
        if (u == 123456 && eu == 234567) _exit(0);
        if (u == (123456 & 0xffff)) _exit(20);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0)        CHECK(1, "uid32 (c) getuid/geteuid 返完整 32-bit");
        else if (ec == 20)  CHECK(0, "uid32 (c) FAIL: 截断到 16-bit");
        else                CHECK(0, "uid32 (c) failed");
    }
}

int uid32_no_trunc_run(void)
{
    printf("\n----- uid32_no_trunc (man HISTORY 32-bit ID) -----\n");
    uid32_getresuid_no_truncate();
    uid32_getresgid_no_truncate();
    uid32_getuid_geteuid_no_truncate();
    printf("  ----- uid32_no_trunc: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
