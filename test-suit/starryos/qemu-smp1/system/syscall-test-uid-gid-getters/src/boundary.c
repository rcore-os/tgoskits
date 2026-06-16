#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* boundary — getter syscall 边界 / 错误注入测试.
 *
 * man 2 getresuid §"ERRORS":
 *   "EFAULT — One of the arguments specified an address outside the calling
 *    process's address space."
 *
 * 仅 getresuid/getresgid 接受指针参数 → 才有 EFAULT 路径.
 * 0-arg getter (getuid/geteuid/getgid/getegid) 不接受任何参数, 无 EFAULT.
 *
 * 维度: 指针 invalid 位置 × 指针类型 (kernel-addr / NULL / 部分 OK)
 *   (a) kernel-space address — ruid 槽 / egid 槽
 *   (b) 部分 NULL (3 槽中 1 个) + 全 NULL
 */

static void getresuid_kernel_address_efault(void)
{
    /* 测什么: man §ERRORS — 任一 out-arg "address outside calling process's
     *         address space" 应 -1 EFAULT. kernel-space 地址典型不可写.
     * 怎么测: 用 raw syscall 传 kernel-canonical 地址 (0xdeadbeef.. / 0xffff..)
     *         至 ruid 槽 (slot 1) / egid 槽 (slot 2). 测两个不同 syscall (uid/gid 镜像).
     * 期望:   rc=-1, errno=EFAULT.
     * 为什么: starry vm_write 实现必须 check user-vs-kernel 段, 不能 OOB 写 (会
     *         crash kernel) 或 silently ignore (会让 user 误以为成功).
     *         注: 用 raw syscall 直达 — libc 可能在 wrapper 内做地址校验拦截. */
    uid_t valid;
    long rc;

    errno = 0;
    rc = syscall(SYS_getresuid, (void *)0xdeadbeefdeadbeefULL, &valid, &valid);
    CHECK(rc == -1 && errno == EFAULT,                             "boundary (a1): kernel addr as ruid slot -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresgid, &valid, (void *)0xffffffffff000000ULL, &valid);
    CHECK(rc == -1 && errno == EFAULT,                             "boundary (a2): kernel addr as egid slot -> -1 EFAULT");
}

static void getresuid_partial_efault(void)
{
    /* 测什么: man §ERRORS 隐含 — "ONE OF the arguments" 意味着任一槽无效
     *         即应失败. 测部分 NULL (1 槽) 与全 NULL.
     * 怎么测: 用 raw syscall, slot 1 设 NULL 其他正常 → 验 EFAULT.
     *         再测三槽全 NULL.
     * 期望:   两种情况都 -1 EFAULT.
     * 为什么: 验证 starry 的 NULL 检查是 fail-fast (任一槽错就停),
     *         不能因 "valid 槽都给了" 而 silently 成功. */
    uid_t valid;
    long rc;

    errno = 0;
    rc = syscall(SYS_getresuid, NULL, &valid, &valid);
    CHECK(rc == -1 && errno == EFAULT,                             "boundary (b1): partial NULL (1 of 3) -> -1 EFAULT");

    errno = 0;
    rc = syscall(SYS_getresgid, NULL, NULL, NULL);
    CHECK(rc == -1 && errno == EFAULT,                             "boundary (b2): all-NULL getresgid -> -1 EFAULT");
}

int boundary_run(void)
{
    printf("\n----- boundary -----\n");
    getresuid_kernel_address_efault();
    getresuid_partial_efault();
    printf("  ----- boundary: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
