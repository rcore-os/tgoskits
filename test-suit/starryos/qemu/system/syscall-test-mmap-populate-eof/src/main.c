/*
 * test_mmap_populate_eof.c — file-backed mmap populate is bounded at EOF (PR #1164).
 *
 * 回归背景 (为什么写这个测例):
 *   文件映射 populate 此前对映射范围内每一页都急切分配物理帧, 包括完全落在
 *   文件 EOF 之外的稀疏页. 应用用一个远大于文件的 mmap(典型: bbolt 用多 GB 的
 *   InitialMmapSize 映射十几 KB 的 etcd db)时, 覆盖整个区域的 populate 会为
 *   全部页逐页分配帧 → 耗尽物理内存 OOM.
 *
 * man 2 mmap (MAP_SHARED 文件映射):
 *   "References to whole pages following the end of a mapped file ... raise a
 *    SIGBUS signal." EOF 之外的页不被预先填充; 最后一个部分页的尾部按 POSIX
 *    读为 0.
 *
 * 修复 (kernel mm/aspace/backend/file.rs `FileBackend::populate`):
 *   循环前算 `eof_page = file_len.div_ceil(4096)`, 对 `pn >= eof_page` 的页跳过
 *   (保持 unmapped → 真实访问缺页 → populate 返回 0 → SIGBUS). `div_ceil`
 *   保留最后部分页(尾部由 page_or_insert 零填充).
 *
 * 覆盖 reviewer 的两点要求:
 *   (1) 小文件 + 远大于文件的 MAP_SHARED|MAP_POPULATE 不预分配/不 OOM;
 *   (2) 最后部分页尾部确为 0 (证明而非空口注释); EOF 外访问为 SIGBUS.
 *
 * 本测例位于 qemu (-m 512M): 256MB 的稀疏映射若被急切预分配会 OOM,
 * 故"mmap 成功且后续断言通过"本身即证明 populate 已按 EOF 收敛.
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <signal.h>
#include <setjmp.h>

#ifndef MAP_POPULATE
#define MAP_POPULATE 0x8000
#endif

#define PAGE 4096UL
#define FILE_SZ 100UL              /* 部分页: 小于一页 */
#define HUGE_LEN (256UL * 1024 * 1024) /* 256MB >> 文件; 急切预分配会 OOM 512M RAM */

/* 捕获越界访问产生的故障. Linux 对共享文件映射 EOF 外访问发 SIGBUS;
 * 同时挂 SIGSEGV 兜底, 保证测例本身不会因信号默认动作而崩溃. */
static sigjmp_buf g_jb;
static volatile sig_atomic_t g_sig;
static volatile sig_atomic_t g_code;
static void on_fault(int sig, siginfo_t *info, void *context)
{
    (void)context;
    g_sig = sig;
    g_code = info ? info->si_code : 0;
    siglongjmp(g_jb, 1);
}

/* 读 *p 一个字节. 返回触发的信号号(0 表示未故障); 值经 *out 带出. */
static int read_fault_signal(volatile unsigned char *p, unsigned char *out)
{
    struct sigaction sa = {0}, old_bus, old_segv;
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGBUS, &sa, &old_bus);
    sigaction(SIGSEGV, &sa, &old_segv);
    g_sig = 0;
    g_code = 0;
    if (sigsetjmp(g_jb, 1) == 0) {
        unsigned char v = *p;
        if (out)
            *out = v;
    }
    sigaction(SIGBUS, &old_bus, NULL);
    sigaction(SIGSEGV, &old_segv, NULL);
    return (int)g_sig;
}

int main(void)
{
    TEST_START("mmap populate bounded at EOF (#1164)");

    const char *path = "/tmp/mpe.dat";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open backing file");
    if (fd < 0) {
        TEST_DONE();
    }

    unsigned char wbuf[FILE_SZ];
    memset(wbuf, 0xAB, FILE_SZ);
    CHECK_RET(write(fd, wbuf, FILE_SZ), (long)FILE_SZ, "write 100-byte partial-page file");

    /* 远大于文件的 MAP_SHARED|MAP_POPULATE: 有 EOF 收敛修复时只 back 1 个文件页,
     * 急切全量预分配则会在 512M RAM 上 OOM. mmap 成功即证明未预分配. */
    void *m = mmap(NULL, HUGE_LEN, PROT_READ, MAP_SHARED | MAP_POPULATE, fd, 0);
    CHECK(m != MAP_FAILED,
          "huge MAP_SHARED|MAP_POPULATE over tiny file succeeds (no eager prealloc/OOM)");
    if (m == MAP_FAILED) {
        close(fd);
        unlink(path);
        TEST_DONE();
    }
    volatile unsigned char *p = (volatile unsigned char *)m;

    /* (a) [0,100): 文件内容 0xAB */
    int content_ok = 1;
    for (unsigned long i = 0; i < FILE_SZ; i++) {
        if (p[i] != 0xAB) {
            content_ok = 0;
            break;
        }
    }
    CHECK(content_ok, "bytes [0,100) map the file content (0xAB)");

    /* (b) [100,4096): 最后部分页尾部必须零填充 (POSIX; reviewer 关注点) */
    unsigned long first_nonzero = 0;
    int tail_zero = 1;
    for (unsigned long i = FILE_SZ; i < PAGE; i++) {
        if (p[i] != 0) {
            tail_zero = 0;
            first_nonzero = i;
            break;
        }
    }
    if (tail_zero) {
        CHECK(1, "partial-page tail [100,4096) is zero-filled");
    } else {
        char mb[128];
        snprintf(mb, sizeof mb, "partial-page tail not zero at off=%lu (val=0x%02x)",
                 first_nonzero, p[first_nonzero]);
        CHECK(0, mb);
    }

    /* (c) offset 4096 (>= eof_page=1) 落在 EOF 之外 → SIGBUS */
    unsigned char v = 0xFF;
    int sig = read_fault_signal(p + PAGE, &v);
    CHECK(sig == SIGBUS, "access at offset 4096 (beyond EOF) raises SIGBUS");

    /* (d) 稀疏区深处(接近 256MB 末尾)同样未预分配 → SIGBUS */
    int sig2 = read_fault_signal(p + (HUGE_LEN - PAGE), NULL);
    CHECK(sig2 == SIGBUS, "access near end of huge sparse mapping raises SIGBUS");

    munmap(m, HUGE_LEN);

    /* Truncate must revoke already-resident PTEs before publishing the new
     * file epoch. The new partial last page keeps its prefix and exposes a
     * zero tail; the following whole page faults with BUS_ADRERR. */
    unsigned char resident[PAGE * 3];
    memset(resident, 0x5c, sizeof(resident));
    CHECK_RET(ftruncate(fd, sizeof(resident)), 0,
              "grow truncate fixture to three pages");
    CHECK_RET(pwrite(fd, resident, sizeof(resident), 0),
              (long)sizeof(resident), "fill truncate fixture");
    volatile unsigned char *trunc_map = mmap(NULL, sizeof(resident),
                                             PROT_READ | PROT_WRITE,
                                             MAP_SHARED, fd, 0);
    CHECK(trunc_map != MAP_FAILED, "map three-page truncate fixture");
    if (trunc_map != MAP_FAILED) {
        CHECK(trunc_map[0] == 0x5c && trunc_map[PAGE] == 0x5c &&
                  trunc_map[PAGE * 2] == 0x5c,
              "fault all truncate fixture pages before shrink");
        CHECK_RET(ftruncate(fd, PAGE + 17), 0,
                  "shrink resident mapping to a partial second page");

        int prefix_ok = 1;
        for (unsigned long i = PAGE; i < PAGE + 17; i++) {
            if (trunc_map[i] != 0x5c) {
                prefix_ok = 0;
                break;
            }
        }
        CHECK(prefix_ok, "truncate preserves bytes before the new partial EOF");

        int truncated_tail_zero = 1;
        for (unsigned long i = PAGE + 17; i < PAGE * 2; i++) {
            if (trunc_map[i] != 0) {
                truncated_tail_zero = 0;
                break;
            }
        }
        CHECK(truncated_tail_zero,
              "truncate invalidates resident partial page and zeroes its tail");

        int truncate_sig = read_fault_signal(trunc_map + PAGE * 2, NULL);
        CHECK(truncate_sig == SIGBUS && g_code == BUS_ADRERR,
              "truncate invalidates resident whole page with SIGBUS/BUS_ADRERR");

        CHECK_RET(ftruncate(fd, sizeof(resident)), 0,
                  "extend truncated mapping back to three pages");
        CHECK(trunc_map[PAGE + 17] == 0 && trunc_map[PAGE * 2] == 0,
              "extended holes fault back as zero after truncate epoch change");
        munmap((void *)trunc_map, sizeof(resident));
    }
    close(fd);
    unlink(path);
    TEST_DONE();
}
