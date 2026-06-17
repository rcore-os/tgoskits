/*
 * test_madvise.c -- madvise 系统调用边界语义测试
 *
 * 测试内容（每条对应一个已知 Linux 返回分支）：
 *   1. 合法映射 + MADV_NORMAL 应返回 0
 *   2. advice 值非法 → errno=EINVAL
 *   3. addr 非页对齐 → errno=EINVAL
 *   4. 区间已 munmap 且 advice=MADV_DONTNEED → errno=ENOMEM
 *
 * 针对 StarryOS：kernel/src/syscall/mm/mmap.rs 的 sys_madvise 是 Ok(0) 桩，
 * 上述 2/3/4 三条断言会 FAIL。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <unistd.h>

int main(void)
{
    TEST_START("madvise");

    long pagesize = sysconf(_SC_PAGESIZE);
    CHECK(pagesize > 0, "sysconf _SC_PAGESIZE");

    /* 分配一页用于后续测试 */
    void *page = mmap(NULL, (size_t)pagesize, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(page != MAP_FAILED, "mmap 分配一页");

    /* 1. happy path: MADV_NORMAL 合法映射 → 0 */
    CHECK_RET(madvise(page, (size_t)pagesize, MADV_NORMAL), 0,
              "madvise MADV_NORMAL 合法映射");

    /* 2. 非法 advice 值 → EINVAL
     *    0x12345 不在任何已定义的 MADV_* 常量里。 */
    CHECK_ERR(madvise(page, (size_t)pagesize, 0x12345),
              EINVAL,
              "madvise 非法 advice → EINVAL");

    /* 3. addr 非页对齐 → EINVAL */
    CHECK_ERR(madvise((char *)page + 1, (size_t)pagesize, MADV_NORMAL),
              EINVAL,
              "madvise addr 未页对齐 → EINVAL");

    /* 4. 已 munmap 的区间 + MADV_DONTNEED → ENOMEM
     *    man 2 madvise: "Addresses in the specified range are not currently
     *    mapped, or are outside the address space of the process." */
    void *gone = mmap(NULL, (size_t)pagesize, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(gone != MAP_FAILED, "mmap 另一页用于测 ENOMEM");
    CHECK_RET(munmap(gone, (size_t)pagesize), 0, "munmap 释放该页");
    CHECK_ERR(madvise(gone, (size_t)pagesize, MADV_DONTNEED),
              ENOMEM,
              "madvise 未映射区间 MADV_DONTNEED → ENOMEM");

    munmap(page, (size_t)pagesize);

    TEST_DONE();
}
