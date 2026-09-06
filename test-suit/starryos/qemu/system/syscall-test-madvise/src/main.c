/*
 * !test-madvise — madvise(2) FREE / REMOVE / DONTNEED / PAGEOUT 测试
 *
 * ground truth: man 2 madvise + Linux v7.1 mm/madvise.c。覆盖浏览器内存管理:
 * MADV_FREE(V8/分配器惰性回收, 仅私有匿名) / MADV_REMOVE(shmem 打洞释放共享内存) /
 * MADV_DONTNEED(立即丢弃) / MADV_PAGEOUT(文件页 best-effort reclaim) +
 * errno(EINVAL 映射类型不符 / 未对齐 / 非法 advice, ENOMEM 未映射)。
 *
 * =====================================================================
 * 语义 (man 2 madvise)
 * =====================================================================
 *   MADV_FREE(4.5+): 仅私有匿名页; 惰性回收(可延迟到内存压力); 写后取消 free。
 *     文件后备 -> EINVAL。回收后读到 0 或原值(实现定义), 不可依赖内容。
 *   MADV_REMOVE(2.6.16+): 释放该范围页及后备存储(等价 fallocate PUNCH_HOLE);
 *     要求 shared+writable 文件后备(shmem/tmpfs); 匿名 -> EINVAL。打洞后读到 0。
 *   MADV_DONTNEED: 立即丢弃; 匿名下次访问得零页。
 *   addr 须页对齐否则 EINVAL; 非法 advice -> EINVAL; 范围含未映射 -> ENOMEM。
 *
 * =====================================================================
 * Linux v7.1 源码对齐 (mm/madvise.c)
 * =====================================================================
 *   madvise_remove L1000: !VM_LOCKED, 无文件后备(!f||!f_mapping||!host) -> EINVAL;
 *     否则 vfs_fallocate PUNCH_HOLE。
 *   madvise_free_single_vma L799: !vma_is_anonymous -> EINVAL(L813)。
 *   check_input_range: addr 未对齐 -> EINVAL; 范围 gap -> ENOMEM。
 *   StarryOS: syscall/mm/mmap.rs sys_madvise。
 * =====================================================================
 */

#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#include <string.h>

#ifndef MADV_FREE
#define MADV_FREE 8
#endif
#ifndef MADV_REMOVE
#define MADV_REMOVE 9
#endif
#ifndef MADV_DONTFORK
#define MADV_DONTFORK 10
#endif
#ifndef MADV_DOFORK
#define MADV_DOFORK 11
#endif
#ifndef MADV_HUGEPAGE
#define MADV_HUGEPAGE 14
#endif
#ifndef MADV_NOHUGEPAGE
#define MADV_NOHUGEPAGE 15
#endif
#ifndef MADV_DONTDUMP
#define MADV_DONTDUMP 16
#endif
#ifndef MADV_DODUMP
#define MADV_DODUMP 17
#endif
#ifndef MADV_COLD
#define MADV_COLD 20
#endif
#ifndef MADV_PAGEOUT
#define MADV_PAGEOUT 21
#endif
#ifndef MADV_DONTNEED_LOCKED
#define MADV_DONTNEED_LOCKED 24
#endif

static long PS;

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死\n==== test-madvise 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

/* 建一个 memfd 后备的共享可写映射(shmem 语义), 写入 pattern。返回映射地址, *fd_out=fd。 */
static void *memfd_shared_map(int *fd_out, unsigned char pattern)
{
    int fd = memfd_create("madv", 0);
    if (fd < 0) return NULL;
    if (ftruncate(fd, PS) != 0) { close(fd); return NULL; }
    void *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) { close(fd); return NULL; }
    memset(p, pattern, (size_t)PS);
    *fd_out = fd;
    return p;
}

/* ===== A. MADV_FREE 私有匿名: 成功 + 写后取消 free ===== */
static int test_madv_free_anon(void)
{
    TEST_START("A. MADV_FREE 私有匿名 + 写取消 free");
    void *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED, "mmap 匿名私有");
    if (p == MAP_FAILED) { TEST_DONE(); }

    memset(p, 0xAB, (size_t)PS);
    CHECK(madvise(p, (size_t)PS, MADV_FREE) == 0, "MADV_FREE 私有匿名 -> 成功");
    /* 写后取消 free: 写入的值必须持久可读 */
    *(volatile unsigned char *)p = 0x5A;
    CHECK(*(volatile unsigned char *)p == 0x5A, "MADV_FREE 后写入的值持久(写取消 free)");
    munmap(p, (size_t)PS);
    TEST_DONE();
}

/* ===== B. MADV_FREE errno: 文件后备 -> EINVAL ===== */
static int test_madv_free_errno(void)
{
    TEST_START("B. MADV_FREE 文件后备 -> EINVAL");
    int fd = -1;
    void *p = memfd_shared_map(&fd, 0x11);
    if (!p) { CHECK(0, "memfd 前置"); TEST_DONE(); }
    errno = 0;
    /* MADV_FREE 只对私有匿名; 文件后备(memfd shared)-> EINVAL */
    CHECK(madvise(p, (size_t)PS, MADV_FREE) == -1 && errno == EINVAL,
          "MADV_FREE 文件后备映射 -> EINVAL");
    munmap(p, (size_t)PS);
    close(fd);
    TEST_DONE();
}

/* ===== C. MADV_REMOVE 匿名 -> EINVAL(要求文件后备) ===== */
static int test_madv_remove_anon(void)
{
    TEST_START("C. MADV_REMOVE 匿名 -> EINVAL");
    void *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { CHECK(0, "mmap"); TEST_DONE(); }
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_REMOVE) == -1 && errno == EINVAL,
          "MADV_REMOVE 私有匿名 -> EINVAL(无文件后备)");
    munmap(p, (size_t)PS);
    TEST_DONE();
}

/* ===== D. MADV_REMOVE shmem/memfd -> 打洞置零 ===== */
static int test_madv_remove_shmem(void)
{
    TEST_START("D. MADV_REMOVE shmem(memfd) -> 打洞置零");
    int fd = -1;
    void *p = memfd_shared_map(&fd, 0xCD);
    if (!p) { CHECK(0, "memfd 前置"); TEST_DONE(); }
    CHECK(((volatile unsigned char *)p)[0] == 0xCD, "打洞前有数据 0xCD");
    int r = madvise(p, (size_t)PS, MADV_REMOVE);
    CHECK(r == 0, "MADV_REMOVE shmem 成功");
    if (r == 0) {
        int zero = 1;
        for (long i = 0; i < PS; i++) {
            if (((volatile unsigned char *)p)[i] != 0) { zero = 0; break; }
        }
        CHECK(zero, "MADV_REMOVE 后该范围读到全 0(打洞释放后备)");
    }
    munmap(p, (size_t)PS);
    close(fd);
    TEST_DONE();
}

/* ===== E. MADV_DONTNEED 匿名 + errno 边界 ===== */
static int test_dontneed_and_errno(void)
{
    TEST_START("E. MADV_DONTNEED + errno(未对齐/非法advice/未映射)");
    void *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { CHECK(0, "mmap"); TEST_DONE(); }
    memset(p, 0xEE, (size_t)PS);
    CHECK(madvise(p, (size_t)PS, MADV_DONTNEED) == 0, "MADV_DONTNEED 匿名 -> 成功");
    CHECK(*(volatile unsigned char *)p == 0, "MADV_DONTNEED 后重访问得零页");

    /* 非法 advice -> EINVAL */
    errno = 0;
    CHECK(madvise(p, (size_t)PS, 0x7fff) == -1 && errno == EINVAL, "非法 advice -> EINVAL");
    /* addr 未页对齐 -> EINVAL (man: addr is not page-aligned) */
    errno = 0;
    CHECK(madvise((char *)p + 1, (size_t)PS, MADV_DONTNEED) == -1 && errno == EINVAL,
          "addr 未页对齐 -> EINVAL");
    /* len=0 -> 0 no-op */
    CHECK(madvise(p, 0, MADV_DONTNEED) == 0, "len=0 -> 0(no-op)");
    munmap(p, (size_t)PS);

    /* 未映射范围 -> ENOMEM */
    void *u = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (u != MAP_FAILED) {
        munmap(u, (size_t)PS);
        errno = 0;
        CHECK(madvise(u, (size_t)PS, MADV_DONTNEED) == -1 && errno == ENOMEM,
              "未映射范围 -> ENOMEM");
    }
    TEST_DONE();
}

/* ===== F. MADV_DONTNEED shared 文件后备 -> 从后备重读(非零), 区别于匿名得零页 ===== */
static int test_dontneed_file_backed(void)
{
    TEST_START("F. MADV_DONTNEED shared 文件后备 -> 重访问从后备重读");
    int fd = -1;
    void *p = memfd_shared_map(&fd, 0x3C);
    if (!p) { CHECK(0, "memfd 前置"); TEST_DONE(); }
    CHECK(((volatile unsigned char *)p)[0] == 0x3C, "DONTNEED 前有数据 0x3C");
    CHECK(madvise(p, (size_t)PS, MADV_DONTNEED) == 0,
          "MADV_DONTNEED shared 文件后备 -> 成功");
    /* man: shared 文件后备下 DONTNEED 后重访问从后备重填(非零), 匿名才得零页 */
    int same = 1;
    for (long i = 0; i < PS; i++) {
        if (((volatile unsigned char *)p)[i] != 0x3C) { same = 0; break; }
    }
    CHECK(same, "MADV_DONTNEED 后 shared 映射从后备重读原值 0x3C(非零页)");
    munmap(p, (size_t)PS);
    close(fd);
    TEST_DONE();
}

/* ===== G. Linux VMA walk: hole returns ENOMEM without rolling back mapped VMAs ===== */
static int test_dontneed_hole_keeps_prefix_and_suffix_effects(void)
{
    TEST_START("G. MADV_DONTNEED 跨 hole: ENOMEM + 已映射 VMA 保留效果");
    unsigned char *p = mmap(NULL, (size_t)PS * 3,
                            PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { CHECK(0, "mmap 三页"); TEST_DONE(); }

    memset(p, 0x31, (size_t)PS);
    memset(p + PS * 2, 0x73, (size_t)PS);
    CHECK(munmap(p + PS, (size_t)PS) == 0, "解除中间页形成 VMA hole");

    errno = 0;
    long rc = syscall(SYS_madvise, p, (size_t)PS * 3, MADV_DONTNEED);
    CHECK(rc == -1 && errno == ENOMEM,
          "跨未映射 hole 返回 ENOMEM");
    CHECK(*(volatile unsigned char *)p == 0,
          "hole 前已映射 VMA 的 DONTNEED 效果不回滚");
    CHECK(*(volatile unsigned char *)(p + PS * 2) == 0,
          "Linux VMA walk 越过 hole 后继续处理后续 VMA");

    munmap(p, (size_t)PS);
    munmap(p + PS * 2, (size_t)PS);
    TEST_DONE();
}

/* ===== H. Linux VMA policy advice ===== */
static int test_advice_contracts(void)
{
    TEST_START("H. Linux VMA advice policy");
    void *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { CHECK(0, "mmap"); TEST_DONE(); }
    memset(p, 0x77, (size_t)PS);

    CHECK(madvise(p, (size_t)PS, MADV_NORMAL) == 0,
          "MADV_NORMAL resets access policy");
    CHECK(madvise(p, (size_t)PS, MADV_RANDOM) == 0,
          "MADV_RANDOM records random access policy");
    CHECK(madvise(p, (size_t)PS, MADV_SEQUENTIAL) == 0,
          "MADV_SEQUENTIAL records sequential access policy");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_WILLNEED) == -1 && errno == EBADF,
          "MADV_WILLNEED anonymous mapping without swap -> EBADF");

    CHECK(madvise(p, (size_t)PS, MADV_DONTFORK) == 0,
          "MADV_DONTFORK records VM_DONTCOPY");
    pid_t omitted = fork();
    CHECK(omitted >= 0, "fork MADV_DONTFORK child");
    if (omitted == 0) {
        volatile unsigned char value = *(volatile unsigned char *)p;
        (void)value;
        _exit(1);
    }
    if (omitted > 0) {
        int status = 0;
        CHECK(waitpid(omitted, &status, 0) == omitted,
              "wait MADV_DONTFORK child");
        CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV,
              "MADV_DONTFORK omits VMA from child mm");
    }

    CHECK(madvise(p, (size_t)PS, MADV_DOFORK) == 0,
          "MADV_DOFORK clears VM_DONTCOPY");
    pid_t inherited = fork();
    CHECK(inherited >= 0, "fork MADV_DOFORK child");
    if (inherited == 0)
        _exit(*(volatile unsigned char *)p == 0x77 ? 0 : 1);
    if (inherited > 0) {
        int status = 0;
        CHECK(waitpid(inherited, &status, 0) == inherited,
              "wait MADV_DOFORK child");
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "MADV_DOFORK restores child mapping");
    }

    CHECK(madvise(p, (size_t)PS, MADV_DONTDUMP) == 0,
          "MADV_DONTDUMP records VM_DONTDUMP");
    CHECK(madvise(p, (size_t)PS, MADV_DODUMP) == 0,
          "MADV_DODUMP clears VM_DONTDUMP");

    /* Linux records THP advice in the VMA flags.  It does not require an
     * already-materialized huge page, nor does MADV_NOHUGEPAGE eagerly split
     * an aligned huge mapping. */
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_HUGEPAGE) == 0,
          "MADV_HUGEPAGE records a VMA preference");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_NOHUGEPAGE) == 0,
          "MADV_NOHUGEPAGE records a VMA prohibition");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_NOHUGEPAGE) == 0,
          "repeating identical THP advice is a successful no-op");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_COLD) == -1 && errno == EOPNOTSUPP,
          "MADV_COLD unsupported -> EOPNOTSUPP");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_PAGEOUT) == -1 && errno == EOPNOTSUPP,
          "MADV_PAGEOUT without anonymous reclaim backend -> EOPNOTSUPP");

    CHECK(*(volatile unsigned char *)p == 0x77, "advice 后内容不变(0x77 持久)");
    munmap(p, (size_t)PS);
    TEST_DONE();
}

/* ===== I. locked VMA reclaim advice ===== */
static int test_locked_dontneed_contract(void)
{
    TEST_START("I. locked VMA rejects DONTNEED/FREE");
    unsigned char *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { CHECK(0, "mmap locked advice fixture"); TEST_DONE(); }
    *p = 0x5A;
    CHECK(mlock(p, (size_t)PS) == 0, "mlock advice fixture");

    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_DONTNEED) == -1 && errno == EINVAL,
          "MADV_DONTNEED rejects VM_LOCKED");
    errno = 0;
    CHECK(madvise(p, (size_t)PS, MADV_FREE) == -1 && errno == EINVAL,
          "MADV_FREE rejects VM_LOCKED");
    CHECK(*p == 0x5A, "rejected advice preserves locked contents");

    CHECK(madvise(p, (size_t)PS, MADV_DONTNEED_LOCKED) == 0,
          "MADV_DONTNEED_LOCKED explicitly discards locked page");
    CHECK(*p == 0, "MADV_DONTNEED_LOCKED refaults anonymous zero page");
    CHECK(munlock(p, (size_t)PS) == 0, "munlock advice fixture");
    munmap(p, (size_t)PS);
    TEST_DONE();
}

/* ===== J. MADV_PAGEOUT 对磁盘文件 clean page 执行同步 reclaim ===== */
static int test_pageout_clean_file(void)
{
    TEST_START("I. MADV_PAGEOUT 磁盘文件 clean page reclaim");
    const char *path = "/madv-pageout-test.bin";
    unlink(path);
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0) { CHECK(0, "创建磁盘后备文件"); TEST_DONE(); }
    CHECK(ftruncate(fd, PS) == 0, "扩展磁盘后备文件");
    unsigned char pattern = 0x4D;
    CHECK(pwrite(fd, &pattern, 1, 0) == 1, "写入文件 pattern");
    CHECK(fsync(fd) == 0, "PAGEOUT 前文件已同步为 clean");

    unsigned char *p = mmap(NULL, (size_t)PS, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap clean file page");
    if (p != MAP_FAILED) {
        CHECK(*(volatile unsigned char *)p == pattern, "fault clean file page");
        unsigned char resident = 0;
        CHECK(mincore(p, (size_t)PS, &resident) == 0 && (resident & 1) != 0,
              "PAGEOUT 前 PTE/page cache resident");

        errno = 0;
        long rc = syscall(SYS_madvise, p, (size_t)PS, MADV_PAGEOUT);
        CHECK(rc == 0, "MADV_PAGEOUT clean file mapping -> 成功");
        resident = 1;
        CHECK(mincore(p, (size_t)PS, &resident) == 0 && (resident & 1) == 0,
              "MADV_PAGEOUT 撤销 PTE 并回收 clean cache page");
        CHECK(*(volatile unsigned char *)p == pattern,
              "PAGEOUT 后 fault 从文件恢复原值");
        munmap(p, (size_t)PS);
    }
    close(fd);
    unlink(path);
    TEST_DONE();
}

/* ===== K. MADV_PAGEOUT 脏文件页是 best-effort，不向 ABI 泄漏内部 Busy ===== */
static int test_pageout_dirty_file(void)
{
    TEST_START("J. MADV_PAGEOUT 脏文件页 best-effort reclaim");
    const char *path = "/madv-pageout-dirty-test.bin";
    unlink(path);
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0) { CHECK(0, "创建磁盘后备文件"); TEST_DONE(); }
    CHECK(ftruncate(fd, PS) == 0, "扩展磁盘后备文件");

    unsigned char *p = mmap(NULL, (size_t)PS, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap shared writable file page");
    if (p != MAP_FAILED) {
        *(volatile unsigned char *)p = 0x6B;
        errno = 0;
        long rc = syscall(SYS_madvise, p, (size_t)PS, MADV_PAGEOUT);
        CHECK(rc == 0,
              "MADV_PAGEOUT dirty file mapping is a successful best-effort request");
        CHECK(*(volatile unsigned char *)p == 0x6B,
              "dirty data survives PAGEOUT and any following refault");
        CHECK(fsync(fd) == 0, "PAGEOUT 后仍可同步文件");
        unsigned char stored = 0;
        CHECK(pread(fd, &stored, 1, 0) == 1 && stored == 0x6B,
              "dirty data remains owned until successful writeback");
        munmap(p, (size_t)PS);
    }
    close(fd);
    unlink(path);
    TEST_DONE();
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    PS = sysconf(_SC_PAGESIZE);
    int fail = 0;
    fail |= test_madv_free_anon();
    fail |= test_madv_free_errno();
    fail |= test_madv_remove_anon();
    fail |= test_madv_remove_shmem();
    fail |= test_dontneed_and_errno();
    fail |= test_dontneed_file_backed();
    fail |= test_dontneed_hole_keeps_prefix_and_suffix_effects();
    fail |= test_advice_contracts();
    fail |= test_locked_dontneed_contract();
    fail |= test_pageout_clean_file();
    fail |= test_pageout_dirty_file();
    printf("\n==== test-madvise 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
