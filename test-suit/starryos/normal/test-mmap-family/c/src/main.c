/*
 * test_mmap_family.c -- mmap / munmap / mprotect 边界语义测试
 *
 * 对照 man 2 mmap / munmap / mprotect 与 Linux 内核实现。
 * 注：MAP_ANONYMOUS + 有效 fd 在 Linux 上 fd 被忽略并成功（POSIX 语义），
 *     故不作为断言。StarryOS 当前在此比 Linux 更严格，属独立议题。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <fcntl.h>

#ifndef PROT_GROWSDOWN
#define PROT_GROWSDOWN 0x01000000
#endif
#ifndef PROT_GROWSUP
#define PROT_GROWSUP 0x02000000
#endif
#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

/* mmap 失败 → MAP_FAILED + errno 期望值 */
#define CHECK_MMAP_ERR(call, exp_errno, msg) do {                       \
    errno = 0;                                                          \
    void *_r = (call);                                                  \
    if (_r == MAP_FAILED && errno == (exp_errno)) {                     \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",         \
               __FILE__, __LINE__, msg, errno);                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected MAP_FAILED+errno=%d got ptr=%p errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno));\
        __fail++;                                                       \
        if (_r != MAP_FAILED) { munmap(_r, 4096); }                    \
    }                                                                   \
} while(0)

int main(void)
{
    TEST_START("mmap/munmap/mprotect");

    long pagesize = sysconf(_SC_PAGESIZE);
    CHECK(pagesize > 0, "sysconf _SC_PAGESIZE");
    size_t ps = (size_t)pagesize;

    /* ===================== mmap ===================== */

    /* happy path: PRIVATE|ANONYMOUS → 有效地址 */
    void *p = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED, "mmap PRIVATE|ANON happy path");
    if (p != MAP_FAILED) {
        *(volatile unsigned char *)p = 0xAB;
        CHECK(*(volatile unsigned char *)p == 0xAB, "mmap 分配页面可读写");
    }

    /* length = 0 → EINVAL */
    CHECK_MMAP_ERR(mmap(NULL, 0, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0),
                   EINVAL, "mmap length=0 → EINVAL");

    /* 既不 PRIVATE 也不 SHARED → EINVAL */
    CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ | PROT_WRITE,
                        MAP_ANONYMOUS, -1, 0),
                   EINVAL, "mmap 无 PRIVATE/SHARED → EINVAL");

    /* PRIVATE | SHARED 同时设置 → EINVAL (Linux 拒绝混合) */
    CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_SHARED | MAP_ANONYMOUS, -1, 0),
                   EINVAL, "mmap PRIVATE|SHARED 同时设置 → EINVAL");

    /* offset 非页对齐 → EINVAL */
    CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 1),
                   EINVAL, "mmap offset 非页对齐 → EINVAL");

    /* 非 ANON + 无效 fd(999) → EBADF */
    CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ, MAP_PRIVATE, 999, 0),
                   EBADF, "mmap 文件背景 + 无效 fd → EBADF");

    /* MAP_FIXED_NOREPLACE 覆盖已映射区间 → EEXIST
     * p 已经成功映射在 ps 的地址上，用 FIXED_NOREPLACE 指向 p 应该 EEXIST。*/
    CHECK_MMAP_ERR(mmap(p, ps, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0),
                   EEXIST, "mmap FIXED_NOREPLACE 覆盖已映射 → EEXIST");

    /* ===================== mprotect ===================== */

    /* mprotect happy path: READ|WRITE → 0 */
    CHECK_RET(mprotect(p, ps, PROT_READ | PROT_WRITE), 0,
              "mprotect READ|WRITE happy path");

    /* mprotect 非法 prot 位 → EINVAL */
    CHECK_ERR(mprotect(p, ps, (int)0x80000000u),
              EINVAL, "mprotect 非法 prot 位 → EINVAL");

    /* PROT_GROWSDOWN + PROT_GROWSUP 互斥 → EINVAL */
    CHECK_ERR(mprotect(p, ps, PROT_READ | PROT_GROWSDOWN | PROT_GROWSUP),
              EINVAL, "mprotect GROWSDOWN+GROWSUP → EINVAL");

    /* mprotect length=0 → 0（man: 长度 0 时成功无副作用）*/
    CHECK_RET(mprotect(p, 0, PROT_READ | PROT_WRITE), 0,
              "mprotect length=0 → 0 (no-op)");

    /* mprotect 未对齐 addr → EINVAL
     * musl 的 libc mprotect 会把 addr 向下对齐再 syscall，需直接走 syscall 层。*/
    {
        errno = 0;
        long _rc = syscall(SYS_mprotect, (char *)p + 1, ps,
                           PROT_READ | PROT_WRITE);
        CHECK(_rc == -1 && errno == EINVAL,
              "mprotect addr 未对齐 → EINVAL (直接 syscall)");
    }

    /* mprotect 对未映射区间 → ENOMEM
     * 申请一页然后立刻 munmap，再 mprotect 那个区间。*/
    void *gone = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(gone != MAP_FAILED, "mmap 准备 gone 页用于 mprotect ENOMEM 测试");
    CHECK_RET(munmap(gone, ps), 0, "munmap gone 页");
    CHECK_ERR(mprotect(gone, ps, PROT_READ),
              ENOMEM, "mprotect 未映射区间 → ENOMEM");

    /* ===================== munmap ===================== */

    /* happy path */
    CHECK_RET(munmap(p, ps), 0, "munmap happy path");

    /* munmap 已解映射区间 → 0（Linux: 不是 error）*/
    CHECK_RET(munmap(p, ps), 0, "munmap 已解映射区间 → 0 (幂等)");

    /* length = 0 → EINVAL */
    void *q = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(q != MAP_FAILED, "mmap 另一页用于 munmap 边界测试");
    CHECK_ERR(munmap(q, 0), EINVAL, "munmap length=0 → EINVAL");

    /* 未对齐 addr → EINVAL */
    CHECK_ERR(munmap((char *)q + 1, ps),
              EINVAL, "munmap addr 未对齐 → EINVAL");

    if (q != MAP_FAILED) {
        munmap(q, ps);
    }

    TEST_DONE();
}
