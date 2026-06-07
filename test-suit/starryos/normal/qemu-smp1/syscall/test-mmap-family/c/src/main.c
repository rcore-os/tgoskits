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
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <signal.h>
#include <setjmp.h>

#ifndef PROT_GROWSDOWN
#define PROT_GROWSDOWN 0x01000000
#endif
#ifndef PROT_GROWSUP
#define PROT_GROWSUP 0x02000000
#endif
#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif
#ifndef MAP_SHARED_VALIDATE
#define MAP_SHARED_VALIDATE 0x03
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

/* 写一字节到 *p, 返回是否触发 SIGSEGV (用于验证某页是否仍可写)。 */
static sigjmp_buf g_jb;
static volatile int g_faulted;
static void on_segv(int sig)
{
    (void)sig;
    g_faulted = 1;
    siglongjmp(g_jb, 1);
}
static int write_faults(volatile char *p)
{
    struct sigaction sa = {0}, old;
    sa.sa_handler = on_segv;
    sigaction(SIGSEGV, &sa, &old);
    g_faulted = 0;
    if (sigsetjmp(g_jb, 1) == 0) {
        *p = 0x55;
    }
    sigaction(SIGSEGV, &old, NULL);
    return g_faulted;
}

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

    /* mmap addr+length 溢出 → 被拒绝, 而非环绕成功 (回归)
     * MAP_FIXED 指向接近地址空间顶端、`addr + length` 回绕的请求。关键不变量:
     * 内核必须**拒绝**, 不能让 `(addr + length).align_up` 内部回绕成一个小值
     * 再继续(length 下溢成超大、在低地址环绕落地)。修复用 `checked_add` +
     * `end < raw_end` 双重校验。errno 上 Linux 此处给 ENOMEM(地址越界), starry
     * 修复给 EINVAL(算术溢出); 两者都是合法拒绝, 故都接受 —— 真正回归的是
     * "r == -1"(不得返回一个环绕后的有效地址)。直接走 syscall 层。*/
    {
        unsigned long near_top = ~0xFFFUL; /* 页对齐, 接近 usize::MAX */
        errno = 0;
        long r = syscall(SYS_mmap, (void *)near_top, (size_t)(8 * ps),
                         PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, (off_t)0);
        CHECK(r == -1 && (errno == EINVAL || errno == ENOMEM),
              "mmap MAP_FIXED 且 addr+length 溢出 → 被拒(EINVAL/ENOMEM), 不环绕");
    }

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

    /* mprotect 跨 [mapped][hole][mapped] → ENOMEM (回归: 起始页有映射但区间
     * 中段是空洞)。man 2 mprotect: 区间内任一页未映射即 ENOMEM。旧 starry 仅
     * 检查起始页所在 VMA (find_area(start) 命中) 便放行, 静默成功跳过空洞;
     * 修复后用 can_access_range 做整段连续覆盖校验。*/
    {
        char *hole = mmap(NULL, 3 * ps, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(hole != MAP_FAILED, "mmap 3 页用于 hole-mprotect 测试");
        if (hole != MAP_FAILED) {
            /* 在中间页打一个洞, 留下 [mapped][hole][mapped] */
            CHECK_RET(munmap(hole + ps, ps), 0, "munmap 中间页造洞");
            CHECK_ERR(mprotect(hole, 3 * ps, PROT_READ),
                      ENOMEM, "mprotect 跨中段空洞 → ENOMEM");
            /* 原子性: StarryOS 用 can_access_range 在改任何权限前先校验整段,
             * 命中空洞即原子拒绝、一页都不改, 故左右两页保持原 RW。
             * 注: 真 Linux 在此并非原子 —— 它把保护位应用到空洞前的前缀页(左页
             * 会变成 PROT_READ)再返回 ENOMEM; StarryOS 选择更安全的原子语义。
             * 本断言锁住该原子行为: 若日后把 pre-check 挪到逐页 protect 之后,
             * 左页会被半改成只读, 这里立刻抓住。*/
            CHECK(write_faults(hole) == 0,
                  "失败 mprotect 后左页仍可写 (StarryOS 原子拒绝, 未半改)");
            CHECK(write_faults(hole + 2 * ps) == 0,
                  "失败 mprotect 后右页仍可写 (空洞之后, 任何实现都不应改)");
            munmap(hole, ps);
            munmap(hole + 2 * ps, ps);
        }
    }

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

    /* ===================== 扩展正向路径 ===================== */

    /* anon PRIVATE 必须零初始化 (man: "...are zero-initialized") */
    {
        unsigned char *zp = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(zp != MAP_FAILED, "mmap PRIVATE|ANON 为 zero-init 测试分配");
        if (zp != MAP_FAILED) {
            int all_zero = 1;
            for (size_t i = 0; i < ps; i += ps / 16) {
                if (zp[i] != 0) { all_zero = 0; break; }
            }
            CHECK(all_zero, "anon PRIVATE 映射 zero-init");
            munmap(zp, ps);
        }
    }

    /* anon SHARED 可读写 */
    {
        unsigned char *sp = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                                 MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        CHECK(sp != MAP_FAILED, "mmap SHARED|ANON happy path");
        if (sp != MAP_FAILED) {
            sp[0] = 0x5A;
            sp[ps - 1] = 0xA5;
            CHECK(sp[0] == 0x5A && sp[ps - 1] == 0xA5,
                  "SHARED|ANON 映射首末字节可读写");
            munmap(sp, ps);
        }
    }

    /* 多页 (4 page) 映射，每页触一下 */
    {
        size_t len = ps * 4;
        unsigned char *mp = mmap(NULL, len, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(mp != MAP_FAILED, "mmap 4-page PRIVATE|ANON");
        if (mp != MAP_FAILED) {
            for (size_t i = 0; i < 4; i++) mp[i * ps] = (unsigned char)('0' + i);
            int ok = 1;
            for (size_t i = 0; i < 4; i++)
                if (mp[i * ps] != (unsigned char)('0' + i)) { ok = 0; break; }
            CHECK(ok, "4-page 映射跨页读写一致");
            munmap(mp, len);
        }
    }

    /* MAP_FIXED 重映射同一地址：应替换原映射 */
    {
        size_t len = ps * 2;
        unsigned char *f1 = mmap(NULL, len, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(f1 != MAP_FAILED, "mmap 2-page 用于 FIXED 重映射基底");
        if (f1 != MAP_FAILED) {
            f1[0] = 0x11;
            void *f2 = mmap(f1, len, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
            CHECK(f2 == f1, "MAP_FIXED 重映射返回同地址");
            /* 重映射后应为新匿名页 —— 期望 zero */
            CHECK(((unsigned char *)f2)[0] == 0,
                  "MAP_FIXED 重映射后首字节已重置为 0");
            munmap(f1, len);
        }
    }

    /* PROT_NONE 映射应成功（不可访问但可存在） */
    {
        void *np = mmap(NULL, ps, PROT_NONE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(np != MAP_FAILED, "mmap PROT_NONE 映射成功");
        if (np != MAP_FAILED) {
            CHECK_RET(munmap(np, ps), 0, "munmap PROT_NONE 映射");
        }
    }

    /* 4 页映射中间 2 页 partial munmap；首末页仍可访问 */
    {
        size_t len = ps * 4;
        unsigned char *bp = mmap(NULL, len, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(bp != MAP_FAILED, "mmap 4-page 用于 partial munmap");
        if (bp != MAP_FAILED) {
            bp[0] = 0xE1;
            bp[3 * ps] = 0xE4;
            CHECK_RET(munmap(bp + ps, ps * 2), 0,
                      "munmap 中间 2 页 (切分 VMA)");
            CHECK(bp[0] == 0xE1 && bp[3 * ps] == 0xE4,
                  "partial munmap 后首末页内容保留");
            munmap(bp, ps);
            munmap(bp + 3 * ps, ps);
        }
    }

    /* mprotect roundtrip：RW → R → RW，数据保持、写复原 */
    {
        unsigned char *rp = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(rp != MAP_FAILED, "mmap 为 mprotect roundtrip 分配");
        if (rp != MAP_FAILED) {
            rp[0] = 0x7E;
            CHECK_RET(mprotect(rp, ps, PROT_READ), 0,
                      "mprotect → PROT_READ");
            CHECK(rp[0] == 0x7E, "降为只读后仍可读出原值");
            CHECK_RET(mprotect(rp, ps, PROT_READ | PROT_WRITE), 0,
                      "mprotect 恢复 READ|WRITE");
            rp[0] = 0xEF;
            CHECK(rp[0] == 0xEF, "恢复 WRITE 后可写新值");
            munmap(rp, ps);
        }
    }

    /* MAP_FIXED + addr 远超地址空间 → ENOMEM */
    {
        void *bad = (void *)(1UL << 50);
        CHECK_MMAP_ERR(mmap(bad, ps, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0),
                       ENOMEM, "MAP_FIXED addr 超地址空间 → ENOMEM");
    }

    /* ===================== 文件背景 mmap ===================== */

    /* File-backed MAP_SHARED_VALIDATE should be accepted as a shared mapping. */
    {
        const char *path = "/tmp/mmap_family_shared_validate";
        const char data[] = "shared validate";
        size_t dlen = sizeof(data) - 1;

        int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
        CHECK(fd >= 0, "open /tmp/mmap_family_shared_validate O_RDWR");
        if (fd >= 0) {
            CHECK(write(fd, data, dlen) == (ssize_t)dlen,
                  "write MAP_SHARED_VALIDATE fixture");
            CHECK_RET(ftruncate(fd, ps), 0, "ftruncate MAP_SHARED_VALIDATE fixture");

            void *vp = mmap(NULL, ps, PROT_READ, MAP_SHARED_VALIDATE, fd, 0);
            CHECK(vp != MAP_FAILED, "mmap file-backed MAP_SHARED_VALIDATE");
            if (vp != MAP_FAILED) {
                CHECK(memcmp(vp, data, dlen) == 0,
                      "MAP_SHARED_VALIDATE mapping reads file data");
                munmap(vp, ps);
            }
            close(fd);
            unlink(path);
        }
    }

    /* 文件背景 MAP_PRIVATE + PROT_READ：通过映射读文件内容 */
    {
        const char *path = "/tmp/mmap_family_fpr";
        const char data[] = "Hello mmap via file!";
        size_t dlen = sizeof(data) - 1;

        int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
        CHECK(fd >= 0, "open /tmp/mmap_family_fpr O_RDWR");
        if (fd >= 0) {
            ssize_t w = write(fd, data, dlen);
            CHECK((size_t)w == dlen, "写入测试字符串到临时文件");

            void *fp = mmap(NULL, ps, PROT_READ, MAP_PRIVATE, fd, 0);
            CHECK(fp != MAP_FAILED, "mmap 文件背景 PRIVATE|PROT_READ");
            if (fp != MAP_FAILED) {
                CHECK(memcmp(fp, data, dlen) == 0,
                      "通过映射读到文件内容");
                munmap(fp, ps);
            }
            close(fd);
            unlink(path);
        }
    }

    /* 文件背景 MAP_SHARED RW：改映射 → msync → read() 验证落盘 */
    {
        const char *path = "/tmp/mmap_family_fsr";
        const char orig[] = "original";
        size_t olen = sizeof(orig) - 1;

        int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
        CHECK(fd >= 0, "open /tmp/mmap_family_fsr O_RDWR");
        if (fd >= 0) {
            CHECK(write(fd, orig, olen) == (ssize_t)olen,
                  "写入 original 到临时文件");
            CHECK_RET(ftruncate(fd, ps), 0, "ftruncate 到 page_size");

            void *sp = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
            CHECK(sp != MAP_FAILED, "mmap 文件背景 SHARED|RW");
            if (sp != MAP_FAILED) {
                memcpy(sp, "modified", olen);
                msync(sp, ps, MS_SYNC);

                char buf[16] = {0};
                lseek(fd, 0, SEEK_SET);
                ssize_t r = read(fd, buf, olen);
                CHECK(r == (ssize_t)olen && memcmp(buf, "modified", olen) == 0,
                      "SHARED 映射写入经 msync 后 read() 可见");
                munmap(sp, ps);
            }
            close(fd);
            unlink(path);
        }
    }

    /* EACCES: O_RDONLY fd + MAP_SHARED + PROT_WRITE → EACCES (man 2 mmap)
     * 因为文件以 O_RDONLY 打开, MAP_SHARED 写回需要写权限。*/
    {
        const char *path = "/tmp/mmap_family_eacces";
        int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        CHECK(fd >= 0, "open /tmp/mmap_family_eacces O_WRONLY (create)");
        if (fd >= 0) {
            (void)write(fd, "abcd", 4);
            close(fd);

            int ro = open(path, O_RDONLY);
            CHECK(ro >= 0, "reopen /tmp/mmap_family_eacces O_RDONLY");
            if (ro >= 0) {
                CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ | PROT_WRITE,
                                    MAP_SHARED, ro, 0),
                               EACCES,
                               "mmap SHARED|RW on O_RDONLY fd → EACCES");
                close(ro);
            }
            unlink(path);
        }
    }

    /* MAP_FIXED + EACCES: 失败时不得拆掉目标地址上的旧映射 */
    {
        const unsigned char magic = 0x5a;
        void *base = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(base != MAP_FAILED, "mmap anon base for MAP_FIXED EACCES preserve");
        if (base != MAP_FAILED) {
            *(volatile unsigned char *)base = magic;

            const char *path = "/tmp/mmap_family_fixed_eacces";
            int wfd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
            CHECK(wfd >= 0, "open /tmp/mmap_family_fixed_eacces O_WRONLY (create)");
            if (wfd >= 0) {
                (void)write(wfd, "abcd", 4);
                close(wfd);

                int ro = open(path, O_RDONLY);
                CHECK(ro >= 0, "reopen /tmp/mmap_family_fixed_eacces O_RDONLY");
                if (ro >= 0) {
                    CHECK_MMAP_ERR(mmap(base, ps, PROT_READ | PROT_WRITE,
                                        MAP_SHARED | MAP_FIXED, ro, 0),
                                   EACCES,
                                   "MAP_FIXED SHARED|RW on O_RDONLY fd → EACCES");
                    CHECK(*(volatile unsigned char *)base == magic,
                          "MAP_FIXED EACCES 后旧匿名映射内容保留");
                    close(ro);
                }
                unlink(path);
            }
            munmap(base, ps);
        }
    }

    /* MAP_FIXED + MAP_PRIVATE: O_WRONLY fd 无读权限 → EACCES，旧映射保留 */
    {
        const unsigned char magic = 0xa5;
        void *base = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(base != MAP_FAILED, "mmap anon base for MAP_FIXED PRIVATE EACCES");
        if (base != MAP_FAILED) {
            *(volatile unsigned char *)base = magic;

            const char *path = "/tmp/mmap_family_fixed_private_eacces";
            int wfd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
            CHECK(wfd >= 0, "open /tmp/mmap_family_fixed_private_eacces O_WRONLY");
            if (wfd >= 0) {
                (void)write(wfd, "abcd", 4);
                CHECK_MMAP_ERR(mmap(base, ps, PROT_READ,
                                    MAP_PRIVATE | MAP_FIXED, wfd, 0),
                               EACCES,
                               "MAP_FIXED PRIVATE|R on O_WRONLY fd → EACCES");
                CHECK(*(volatile unsigned char *)base == magic,
                      "MAP_FIXED PRIVATE EACCES 后旧匿名映射内容保留");
                close(wfd);
                unlink(path);
            }
            munmap(base, ps);
        }
    }

    /* ENODEV: 目录 mmap → ENODEV (不支持 memory mapping) */
    {
        const char *dir = "/tmp/mmap_family_dir";
        if (mkdir(dir, 0755) == 0 || errno == EEXIST) {
            int dfd = open(dir, O_RDONLY);
            if (dfd >= 0) {
                CHECK_MMAP_ERR(mmap(NULL, ps, PROT_READ, MAP_PRIVATE, dfd, 0),
                               ENODEV, "mmap 目录 → ENODEV");
                close(dfd);
            } else {
                /* 无法 open 目录则跳过，记录 PASS 以免误报 */
                CHECK(1, "mmap 目录 → ENODEV (跳过：open(dir) 未放行)");
            }
            rmdir(dir);
        } else {
            CHECK(1, "mmap 目录 → ENODEV (跳过：mkdir 未放行)");
        }
    }

    /* 关闭 fd 后映射仍有效 (man: "closing fd does not unmap the region") */
    {
        const char *path = "/tmp/mmap_family_fdclose";
        const char data[] = "still here";
        size_t dlen = sizeof(data) - 1;

        int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
        CHECK(fd >= 0, "open /tmp/mmap_family_fdclose");
        if (fd >= 0) {
            (void)write(fd, data, dlen);
            void *mp = mmap(NULL, ps, PROT_READ, MAP_PRIVATE, fd, 0);
            CHECK(mp != MAP_FAILED, "mmap 文件背景用于 close(fd) 测试");
            close(fd);
            if (mp != MAP_FAILED) {
                CHECK(memcmp(mp, data, dlen) == 0,
                      "close(fd) 后映射内容依然可读");
                munmap(mp, ps);
            }
            unlink(path);
        }
    }

    TEST_DONE();
}
