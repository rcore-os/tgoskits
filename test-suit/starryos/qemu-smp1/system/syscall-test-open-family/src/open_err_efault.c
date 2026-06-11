#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

/* EFAULT — pathname 指向「调用进程不可访问地址空间外」的内存。
 *
 * man 2 open §"EFAULT" 原文：
 *   "EFAULT — pathname points outside your accessible address space."
 *
 * 注：glibc 的 open() 包装在某些版本会先做 NULL 检查并返 EFAULT 自己。本模块
 * 用 raw syscall（SYS_openat）绕过 libc，直接在 kernel 路径触发 vm_load_string
 * 的访问检查。 */

/* glibc 的 open 包装会先做 NULL 检查，所以用 raw syscall 触发 kernel 路径 */
static int raw_openat(int dirfd, const char *path, int flags)
{
    return (int)syscall(SYS_openat, dirfd, path, flags, 0);
}

static void efault_null_pathname(void)
{
    /* 怎么测：raw openat(AT_FDCWD, NULL, O_RDONLY)
     * 期望：-1 + EFAULT
     * 为什么：vm_load_string 对 NULL 触发 page fault → kernel 报 EFAULT */
    errno = 0;
    int fd = raw_openat(AT_FDCWD, NULL, O_RDONLY);
    CHECK(fd == -1 && errno == EFAULT,                            "efault: NULL pathname -> EFAULT");
    if (fd >= 0) close(fd);
}

static void efault_kernel_address(void)
{
    /* 怎么测：传一个明显是内核地址的指针（非用户地址空间）
     * 期望：-1 + EFAULT
     * 为什么：vm_load_string 越界检查 */
    errno = 0;
    /* 64 位上 0xdeadbeefdeadbeef 在多数 arch 是非法地址 */
    const char *bad = (const char *)(uintptr_t)0xdeadbeefdeadbeefULL;
    int fd = raw_openat(AT_FDCWD, bad, O_RDONLY);
    CHECK(fd == -1 && errno == EFAULT,                            "efault: kernel-ish address -> EFAULT");
    if (fd >= 0) close(fd);
}

int open_err_efault_run(void)
{
    printf("\n----- open_err_efault -----\n");
    efault_null_pathname();
    efault_kernel_address();
    printf("  ----- open_err_efault: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
