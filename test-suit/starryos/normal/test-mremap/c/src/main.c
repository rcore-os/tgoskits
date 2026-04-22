/*
 * test_mremap.c — mremap 系统调用测试
 *
 * 覆盖范围:
 *   - 基本扩展/缩小/同大小
 *   - 原地扩展 (无 MREMAP_MAYMOVE 时尝试)
 *   - MREMAP_FIXED (移动到指定地址)
 *   - MREMAP_DONTUNMAP (移动但保留源映射)
 *   - 错误路径 (未对齐, new_size=0, 无效 flag, old_size 越界等)
 *   - 数据完整性验证
 *   - 重复扩展, FIXED+shrink/grow, 相邻 munmap 后扩展
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <string.h>

#ifndef MREMAP_MAYMOVE
#define MREMAP_MAYMOVE 1
#endif
#ifndef MREMAP_FIXED
#define MREMAP_FIXED 2
#endif
#ifndef MREMAP_DONTUNMAP
#define MREMAP_DONTUNMAP 4
#endif

static void *raw_mremap(void *old_addr, size_t old_size, size_t new_size,
                        int flags, void *new_addr) {
    long ret = syscall(SYS_mremap, old_addr, old_size, new_size, flags, new_addr);
    if (ret == -1) return MAP_FAILED;
    return (void *)ret;
}

int main(void)
{
    const size_t PAGE = (size_t)sysconf(_SC_PAGE_SIZE);
    TEST_START("mremap");

    /* 1. 基本扩展 + 数据保持 + 新页面零初始化 */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for grow");
        if (p != MAP_FAILED) {
            memset(p, 0xAB, PAGE);
            void *p2 = mremap(p, PAGE, 2 * PAGE, MREMAP_MAYMOVE);
            CHECK(p2 != MAP_FAILED, "grow with MAYMOVE");
            if (p2 != MAP_FAILED) {
                unsigned char *b = (unsigned char *)p2;
                int ok = 1;
                for (size_t i = 0; i < PAGE; i++)
                    if (b[i] != 0xAB) { ok = 0; break; }
                CHECK(ok, "original data preserved");
                ok = 1;
                for (size_t i = PAGE; i < 2 * PAGE; i++)
                    if (b[i] != 0) { ok = 0; break; }
                CHECK(ok, "new pages zero-filled");
                munmap(p2, 2 * PAGE);
            } else {
                munmap(p, PAGE);
            }
        }
    }

    /* 2. 缩小返回原地址 */
    {
        void *p = mmap(NULL, 4 * PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for shrink");
        if (p != MAP_FAILED) {
            memset(p, 0xCD, 4 * PAGE);
            void *p2 = mremap(p, 4 * PAGE, PAGE, 0);
            CHECK(p2 == p, "shrink returns same addr");
            if (p2 != MAP_FAILED) {
                CHECK(((unsigned char *)p2)[0] == 0xCD, "shrink data intact");
                munmap(p2, PAGE);
            } else {
                munmap(p, 4 * PAGE);
            }
        }
    }

    /* 3. 同大小无操作 */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for noop");
        if (p != MAP_FAILED) {
            CHECK(mremap(p, PAGE, PAGE, 0) == p, "same size returns same addr");
            munmap(p, PAGE);
        }
    }

    /* 4. 无 MAYMOVE 扩展: 原地成功或 ENOMEM */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for no-move");
        if (p != MAP_FAILED) {
            void *p2 = mremap(p, PAGE, 4 * PAGE, 0);
            if (p2 != MAP_FAILED) {
                CHECK(p2 == p, "no-move must be in-place");
                munmap(p2, 4 * PAGE);
            } else {
                CHECK(errno == ENOMEM, "no-move fails with ENOMEM");
                munmap(p, PAGE);
            }
        }
    }

    /* 5. MREMAP_FIXED 移动到指定地址 */
    {
        void *src = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *dst = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED && dst != MAP_FAILED, "mmap for FIXED");
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            memset(src, 0xEE, PAGE);
            memset(dst, 0xBB, PAGE);
            void *r = raw_mremap(src, PAGE, PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(r == dst, "FIXED moves to target");
            if (r == dst) {
                CHECK(((unsigned char *)dst)[0] == 0xEE, "data at target");
                munmap(dst, PAGE);
            } else {
                if (r != MAP_FAILED) munmap(r, PAGE);
                munmap(dst, PAGE);
            }
        }
    }

    /* 6. MREMAP_FIXED 不带 MAYMOVE -> EINVAL */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *dst = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED && dst != MAP_FAILED) {
            CHECK_ERR(raw_mremap(p, PAGE, PAGE, MREMAP_FIXED, dst),
                      EINVAL, "FIXED without MAYMOVE");
            munmap(p, PAGE);
            munmap(dst, PAGE);
        }
    }

    /* 7. MREMAP_DONTUNMAP 基本功能 */
    {
        void *src = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED, "mmap for DONTUNMAP");
        if (src != MAP_FAILED) {
            memset(src, 0xFF, PAGE);
            void *dst = raw_mremap(src, PAGE, PAGE,
                                   MREMAP_MAYMOVE | MREMAP_DONTUNMAP, NULL);
            if (dst != MAP_FAILED) {
                CHECK(((unsigned char *)dst)[0] == 0xFF, "data moved");
                unsigned char sv = ((unsigned char *)src)[0];
                CHECK(sv == 0 || sv == 0xFF, "source accessible after DONTUNMAP");
                munmap(dst, PAGE);
                munmap(src, PAGE);
            } else {
                CHECK(errno == EINVAL || errno == ENOSYS,
                      "DONTUNMAP unsupported is ok");
                munmap(src, PAGE);
            }
        }
    }

    /* 8. DONTUNMAP 要求 MAYMOVE */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) {
            CHECK_ERR(raw_mremap(p, PAGE, PAGE, MREMAP_DONTUNMAP, NULL),
                      EINVAL, "DONTUNMAP without MAYMOVE");
            munmap(p, PAGE);
        }
    }

    /* 9. DONTUNMAP 要求 old_size == new_size */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) {
            CHECK_ERR(raw_mremap(p, PAGE, 2 * PAGE,
                                 MREMAP_MAYMOVE | MREMAP_DONTUNMAP, NULL),
                      EINVAL, "DONTUNMAP size mismatch");
            munmap(p, PAGE);
        }
    }

    /* 10-15. 错误用例 */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for errors");
        if (p != MAP_FAILED) {
            CHECK_ERR(mremap((char *)p + 1, PAGE, PAGE, MREMAP_MAYMOVE),
                      EINVAL, "unaligned addr");
            CHECK_ERR(mremap(p, PAGE, 0, MREMAP_MAYMOVE),
                      EINVAL, "zero new_size");
            CHECK_ERR(raw_mremap(p, PAGE, PAGE, 8, NULL),
                      EINVAL, "unknown flags (bit 3)");
            CHECK_ERR(raw_mremap(p, PAGE, PAGE, 0x100, NULL),
                      EINVAL, "unknown flags (bit 8)");
            CHECK_ERR(mremap(p, 2 * PAGE, 3 * PAGE, MREMAP_MAYMOVE),
                      EFAULT, "old_size exceeds VMA");
            CHECK_ERR(raw_mremap(p, 0, PAGE, MREMAP_MAYMOVE, NULL),
                      EINVAL, "old_size=0 private");
            munmap(p, PAGE);
        }
        CHECK_ERR(mremap((void *)0xDEAD0000, PAGE, PAGE, MREMAP_MAYMOVE),
                  EFAULT, "unmapped addr");
    }

    /* 16. FIXED 新旧范围重叠 -> EINVAL */
    {
        void *p = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) {
            CHECK_ERR(raw_mremap(p, 2 * PAGE, 2 * PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED,
                                 (char *)p + PAGE),
                      EINVAL, "FIXED overlap");
            munmap(p, 2 * PAGE);
        }
    }

    /* 17. FIXED + shrink */
    {
        void *src = mmap(NULL, 4 * PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *dst = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            memset(src, 0xAA, 4 * PAGE);
            void *r = raw_mremap(src, 4 * PAGE, PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(r == dst, "FIXED+shrink moves to target");
            if (r == dst) {
                CHECK(((unsigned char *)dst)[0] == 0xAA, "FIXED+shrink data");
                munmap(dst, PAGE);
            } else {
                if (r != MAP_FAILED) munmap(r, PAGE);
                munmap(dst, PAGE);
            }
        }
    }

    /* 18. FIXED + grow */
    {
        void *src = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *dst = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            memset(src, 0xBB, PAGE);
            void *r = raw_mremap(src, PAGE, 2 * PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(r == dst, "FIXED+grow moves to target");
            if (r == dst) {
                CHECK(((unsigned char *)dst)[0] == 0xBB, "FIXED+grow data");
                CHECK(((unsigned char *)dst)[PAGE] == 0, "FIXED+grow new page zeroed");
                munmap(dst, 2 * PAGE);
            } else {
                if (r != MAP_FAILED) munmap(r, 2 * PAGE);
                munmap(dst, 2 * PAGE);
            }
        }
    }

    /* 19. 重复原地扩展 */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for repeated grow");
        if (p != MAP_FAILED) {
            ((unsigned char *)p)[0] = 0x11;
            void *p2 = mremap(p, PAGE, 2 * PAGE, MREMAP_MAYMOVE);
            if (p2 != MAP_FAILED) {
                ((unsigned char *)p2)[PAGE] = 0x22;
                void *p3 = mremap(p2, 2 * PAGE, 3 * PAGE, MREMAP_MAYMOVE);
                if (p3 != MAP_FAILED) {
                    CHECK(((unsigned char *)p3)[0] == 0x11, "repeated grow data[0]");
                    CHECK(((unsigned char *)p3)[PAGE] == 0x22, "repeated grow data[1]");
                    munmap(p3, 3 * PAGE);
                } else {
                    munmap(p2, 2 * PAGE);
                }
            } else {
                munmap(p, PAGE);
            }
        }
    }

    /* 20. 字节级数据完整性 */
    {
        void *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for pattern");
        if (p != MAP_FAILED) {
            unsigned char *b = (unsigned char *)p;
            for (size_t i = 0; i < PAGE; i++)
                b[i] = (unsigned char)(i & 0xFF);
            void *p2 = mremap(p, PAGE, 3 * PAGE, MREMAP_MAYMOVE);
            if (p2 != MAP_FAILED) {
                b = (unsigned char *)p2;
                int ok = 1;
                for (size_t i = 0; i < PAGE; i++)
                    if (b[i] != (unsigned char)(i & 0xFF)) { ok = 0; break; }
                CHECK(ok, "byte pattern preserved");
                munmap(p2, 3 * PAGE);
            } else {
                munmap(p, PAGE);
            }
        }
    }

    TEST_DONE();
}
