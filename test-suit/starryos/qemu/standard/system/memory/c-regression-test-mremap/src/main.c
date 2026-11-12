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
#include <signal.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
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
#ifndef MAP_HUGETLB
#define MAP_HUGETLB 0x40000
#endif
#ifndef MAP_NORESERVE
#define MAP_NORESERVE 0x4000
#endif

static void *raw_mremap(void *old_addr, size_t old_size, size_t new_size,
                        int flags, void *new_addr) {
    long ret = syscall(SYS_mremap, old_addr, old_size, new_size, flags, new_addr);
    if (ret == -1) return MAP_FAILED;
    return (void *)ret;
}

static void *raw_mremap_word_flags(void *old_addr, size_t old_size, size_t new_size,
                                   unsigned long flags, void *new_addr) {
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
            CHECK_ERR(raw_mremap_word_flags(p, PAGE, PAGE, 1UL << 32, NULL),
                      EINVAL, "unknown flags in the upper word");
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

    /* 21. 页对齐 VMA 中段 mremap */
    {
        void *p = mmap(NULL, 3 * PAGE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(p != MAP_FAILED, "mmap for mid-VMA move");
        if (p != MAP_FAILED) {
            unsigned char *b = (unsigned char *)p;
            memset(b, 0x11, PAGE);
            memset(b + PAGE, 0x22, PAGE);
            memset(b + 2 * PAGE, 0x33, PAGE);

            void *r = mremap(b + PAGE, PAGE, 2 * PAGE, MREMAP_MAYMOVE);
            CHECK(r != MAP_FAILED, "mid-VMA old_address can move");
            CHECK(b[0] == 0x11, "left fragment remains mapped after move");
            CHECK(b[2 * PAGE] == 0x33, "right fragment remains mapped after move");
            if (r != MAP_FAILED) {
                unsigned char *moved = (unsigned char *)r;
                CHECK(moved[0] == 0x22, "middle page data moved");
                CHECK(moved[PAGE] == 0, "expanded page is zero-filled");
                munmap(r, 2 * PAGE);
                munmap(p, PAGE);
                munmap(b + 2 * PAGE, PAGE);
            } else {
                munmap(p, 3 * PAGE);
            }
        }
    }

    /* 22. FIXED 失败后源映射应保持完整 */
    {
        void *src = mmap(NULL, 4 * PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED, "mmap for failed move rollback");
        if (src != MAP_FAILED) {
            unsigned char *b = (unsigned char *)src;
            memset(b, 0x41, PAGE);
            memset(b + PAGE, 0x42, PAGE);
            memset(b + 2 * PAGE, 0x43, PAGE);
            memset(b + 3 * PAGE, 0x44, PAGE);

            errno = 0;
            void *r = raw_mremap(src, 4 * PAGE, PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED,
                                 NULL);
            CHECK(r == MAP_FAILED && errno != 0, "FIXED shrink to invalid target fails");
            CHECK(b[0] == 0x41, "failed move keeps first page mapped");
            CHECK(b[PAGE] == 0x42, "failed move keeps second page mapped");
            CHECK(b[2 * PAGE] == 0x43, "failed move keeps third page mapped");
            CHECK(b[3 * PAGE] == 0x44, "failed move keeps fourth page mapped");
            b[3 * PAGE] = 0x55;
            CHECK(b[3 * PAGE] == 0x55, "failed move source remains writable");
            munmap(src, 4 * PAGE);
        }
    }

    /* 23. 移动后重新分配源地址不应破坏目标页 */
    {
        void *src = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *dst = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED && dst != MAP_FAILED, "mmap for move lifetime");
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            memset(src, 0x5A, PAGE);
            void *r = raw_mremap(src, PAGE, PAGE,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(r == dst, "FIXED move for lifetime check");
            if (r == dst) {
                void *reused = mmap(src, PAGE, PROT_READ | PROT_WRITE,
                                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
                CHECK(reused == src, "old address can be reused");
                if (reused == src) {
                    memset(reused, 0xC3, PAGE);
                }
                ((unsigned char *)dst)[0] = 0x7E;
                CHECK(((unsigned char *)dst)[0] == 0x7E, "target remains writable");
                CHECK(((unsigned char *)dst)[PAGE - 1] == 0x5A,
                      "target keeps moved frame after source reuse");
                if (reused == src) munmap(reused, PAGE);
                munmap(dst, PAGE);
            } else {
                if (r != MAP_FAILED) munmap(r, PAGE);
                munmap(dst, PAGE);
                munmap(src, PAGE);
            }
        }
    }

    /* 24. 2 MiB leaf 的 4 KiB 中段移动必须事务式 split，且不复制邻页。 */
    {
        const size_t HUGE = 2UL * 1024 * 1024;
        void *base = mmap(NULL, HUGE, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB, -1, 0);
        void *target = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(PAGE == 4096, "THP transaction requires 4 KiB base pages");
        CHECK(base != MAP_FAILED && target != MAP_FAILED,
              "mmap 2 MiB leaf and fixed target");
        if (base != MAP_FAILED && target != MAP_FAILED) {
            unsigned char *bytes = (unsigned char *)base;
            bytes[0] = 0x19;
            bytes[PAGE] = 0x37;
            bytes[2 * PAGE] = 0x73;
            memset(target, 0xc4, PAGE);

            void *moved = raw_mremap(bytes + PAGE, PAGE, PAGE,
                                     MREMAP_MAYMOVE | MREMAP_FIXED, target);
            CHECK(moved == target, "partial huge leaf moves to fixed target");
            if (moved == target) {
                CHECK(((unsigned char *)moved)[0] == 0x37,
                      "moved subpage preserves data");
                CHECK(bytes[0] == 0x19,
                      "left huge-leaf neighbor remains mapped");
                CHECK(bytes[2 * PAGE] == 0x73,
                      "right huge-leaf neighbor remains mapped");
                ((unsigned char *)moved)[PAGE - 1] = 0x4d;
                CHECK(((unsigned char *)moved)[PAGE - 1] == 0x4d,
                      "moved subpage remains writable");
                munmap(moved, PAGE);
                munmap(base, HUGE);
            } else {
                if (moved != MAP_FAILED) munmap(moved, PAGE);
                munmap(target, PAGE);
                munmap(base, HUGE);
            }
        } else {
            if (base != MAP_FAILED) munmap(base, HUGE);
            if (target != MAP_FAILED) munmap(target, PAGE);
        }
    }

    /* 25. Linux DONTUNMAP 保留目标锁定属性，但清除源 VMA 的锁定属性。 */
    {
        void *src = mmap(NULL, PAGE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED, "mmap for DONTUNMAP lock transfer");
        if (src != MAP_FAILED) {
            memset(src, 0x6b, PAGE);
            CHECK(mlock(src, PAGE) == 0, "lock DONTUNMAP source");
            void *dst = raw_mremap(src, PAGE, PAGE,
                                   MREMAP_MAYMOVE | MREMAP_DONTUNMAP, NULL);
            CHECK(dst != MAP_FAILED, "DONTUNMAP moves a locked VMA");
            if (dst != MAP_FAILED) {
                errno = 0;
                CHECK(msync(dst, PAGE, MS_INVALIDATE) == -1 && errno == EBUSY,
                      "DONTUNMAP target keeps VM_LOCKED");
                errno = 0;
                CHECK(msync(src, PAGE, MS_INVALIDATE) == 0,
                      "DONTUNMAP source clears VM_LOCKED");
                CHECK(((unsigned char *)dst)[0] == 0x6b,
                      "locked DONTUNMAP target keeps data");
                munlock(dst, PAGE);
                munmap(dst, PAGE);
                munmap(src, PAGE);
            } else {
                munlock(src, PAGE);
                munmap(src, PAGE);
            }
        }
    }

    /* 26. Linux remap_move() 允许一次移动权限不同的相邻 VMA。 */
    {
        unsigned char *src = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        unsigned char *dst = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(src != MAP_FAILED && dst != MAP_FAILED,
              "mmap for multi-VMA move");
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            memset(src, 0x31, PAGE);
            memset(src + PAGE, 0x62, PAGE);
            CHECK(mprotect(src, PAGE, PROT_READ) == 0,
                  "split source into read-only and writable VMAs");

            void *moved = raw_mremap(src, 2 * PAGE, 2 * PAGE,
                                     MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(moved == dst, "move adjacent VMAs with different permissions");
            if (moved == dst) {
                CHECK(dst[0] == 0x31 && dst[PAGE] == 0x62,
                      "multi-VMA move preserves both pages");
                dst[PAGE] = 0x73;
                CHECK(dst[PAGE] == 0x73,
                      "multi-VMA move preserves writable suffix");

                pid_t child = fork();
                CHECK(child >= 0, "fork permission probe");
                if (child == 0) {
                    dst[0] = 0x7f;
                    _exit(0);
                } else if (child > 0) {
                    int status = 0;
                    CHECK(waitpid(child, &status, 0) == child,
                          "wait for permission probe");
                    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV,
                          "multi-VMA move preserves read-only prefix");
                }
                munmap(dst, 2 * PAGE);
            } else {
                munmap(src, 2 * PAGE);
                munmap(dst, 2 * PAGE);
            }
        } else {
            if (src != MAP_FAILED) munmap(src, 2 * PAGE);
            if (dst != MAP_FAILED) munmap(dst, 2 * PAGE);
        }
    }

    /* 27. Sparse VMA publication and relocation scale with materialized
     * leaves, not with the number of virtual 4 KiB pages. Only the endpoints
     * are faulted into a 4 GiB source; the middle remains a lazy hole. */
    {
        const size_t SPARSE = 4ULL * 1024 * 1024 * 1024;
        unsigned char *src = mmap(NULL, SPARSE, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                                  -1, 0);
        unsigned char *dst = mmap(NULL, SPARSE, PROT_NONE,
                                  MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                                  -1, 0);
        CHECK(src != MAP_FAILED && dst != MAP_FAILED,
              "mmap sparse 4 GiB source and target");
        if (src != MAP_FAILED && dst != MAP_FAILED) {
            src[0] = 0x2a;
            src[SPARSE - PAGE] = 0x7c;
            void *moved = raw_mremap(src, SPARSE, SPARSE,
                                     MREMAP_MAYMOVE | MREMAP_FIXED, dst);
            CHECK(moved == dst, "mremap sparse 4 GiB VMA by resident leaves");
            if (moved == dst) {
                CHECK(dst[0] == 0x2a, "sparse move preserves first resident page");
                CHECK(dst[SPARSE - PAGE] == 0x7c,
                      "sparse move preserves last resident page");
                CHECK(dst[SPARSE / 2] == 0,
                      "sparse move keeps untouched middle page lazy and zero-filled");
                munmap(dst, SPARSE);
            } else {
                munmap(src, SPARSE);
                munmap(dst, SPARSE);
            }
        } else {
            if (src != MAP_FAILED) munmap(src, SPARSE);
            if (dst != MAP_FAILED) munmap(dst, SPARSE);
        }
    }

    TEST_DONE();
}
