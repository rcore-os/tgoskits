#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <sys/mman.h>
#include <unistd.h>

/*
 * 只读共享 mmap 页 fdatasync 不返 EBUSY 的回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   etcd 的 bbolt 以只读方式 (PROT_READ, MAP_SHARED) mmap 自己的 db 文件, 而通过
 *   pwrite 写盘。fdatasync 时页缓存回写发现这一脏页被只读映射着, 内核的
 *   `protect_dirty_page` (mm/aspace/backend/file.rs) 旧实现只处理 WRITE 页,
 *   把一个非可写的 4K 映射页当成 "非预期页大小" 返回 false ->
 *   `protect_dirty_pages_before_writeback` 抛 ResourceBusy -> fdatasync 得到
 *   致命的 EBUSY, etcd 启动即崩。
 *
 *   根因修复: 只读共享页无法经由映射写脏, 无需 write-protect, 直接报告成功;
 *   仍对可写页照常剥夺 WRITE 位以便下次写触发缺页。
 *
 * 断言 (旧内核在核心断言 #6/#8 上 FAIL=EBUSY, 修复后全 PASS):
 *   在 ext4 根文件系统上建两页文件, 以 PROT_READ|MAP_SHARED 映射, 触发缺页把两页
 *   映射进来, 再经 fd 用 pwrite 把页写脏, 然后 fdatasync/fsync 必须成功返回而非
 *   EBUSY; 最后经独立 fd pread 验证写入持久落盘。
 */

#define PAGE 4096u
#define TWO_PAGES (2u * PAGE)

/* 选一个 ext4 根文件系统上可写的探测路径 (避开可能是 tmpfs 的 /tmp)。*/
static const char *pick_probe_path(void)
{
    static const char *cands[] = {
        "/root/.mmap_ro_fdatasync.tmp",
        "/.mmap_ro_fdatasync.tmp",
        "/usr/.mmap_ro_fdatasync.tmp",
    };
    for (size_t i = 0; i < sizeof cands / sizeof cands[0]; i++) {
        int fd = open(cands[i], O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd >= 0) {
            close(fd);
            unlink(cands[i]);
            return cands[i];
        }
    }
    return NULL;
}

int main(void)
{
    TEST_START("read-only shared mmap page fdatasync must not return EBUSY");

    const char *path = pick_probe_path();
    CHECK(path != NULL, "找到可写 ext4 探测路径");

    int fd = path ? open(path, O_RDWR | O_CREAT | O_TRUNC, 0644) : -1;
    CHECK(fd >= 0, "open(O_RDWR|O_CREAT) 成功");

    char init[TWO_PAGES];
    memset(init, 0x11, sizeof init);
    ssize_t w = fd >= 0 ? write(fd, init, sizeof init) : -1;
    CHECK(w == (ssize_t)sizeof init, "初始写入两页");

    /* 只读共享映射 - 与 bbolt 一致: 只读映射, 写走 pwrite。*/
    unsigned char *p = MAP_FAILED;
    if (fd >= 0)
        p = mmap(NULL, TWO_PAGES, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap(PROT_READ, MAP_SHARED) 成功");

    int have = (fd >= 0 && p != MAP_FAILED);
    if (have) {
        /* 读两页触发缺页, 把只读页真正映射进页表 (protect_dirty_page 才会命中)。*/
        volatile unsigned char sink = 0;
        sink ^= p[0];
        sink ^= p[PAGE];
        (void)sink;
    }

    /* 经 fd 把第 0 页写脏 (不经映射), 制造 "只读映射着的脏页"。*/
    char patch0[PAGE];
    memset(patch0, 0x22, sizeof patch0);
    ssize_t pw0 = have ? pwrite(fd, patch0, sizeof patch0, 0) : -1;
    CHECK(pw0 == (ssize_t)sizeof patch0, "pwrite 页0 写脏成功");

    /* 核心断言: 只读映射脏页 fdatasync 必须成功, 旧内核在此返回 EBUSY。*/
    int rc = have ? fdatasync(fd) : -1;
    CHECK(rc == 0, "只读映射脏页 fdatasync 返回 0 (非 EBUSY)");

    /* 第二条路径: 再写脏第 1 页, fsync (含元数据) 也必须成功。*/
    char patch1[PAGE];
    memset(patch1, 0x33, sizeof patch1);
    ssize_t pw1 = have ? pwrite(fd, patch1, sizeof patch1, PAGE) : -1;
    CHECK(pw1 == (ssize_t)sizeof patch1, "pwrite 页1 写脏成功");

    int rc2 = have ? fsync(fd) : -1;
    CHECK(rc2 == 0, "只读映射脏页 fsync 返回 0 (非 EBUSY)");

    int munmap_ok = have ? (munmap(p, TWO_PAGES) == 0) : 0;
    CHECK(munmap_ok, "munmap 成功");

    /* 持久性 sanity: 独立 fd pread 读回, 验证 pwrite 的新数据已落盘。*/
    int rfd = path ? open(path, O_RDONLY) : -1;
    CHECK(rfd >= 0, "独立 fd 重新打开成功");
    unsigned char back = 0xff;
    ssize_t r = rfd >= 0 ? pread(rfd, &back, 1, 0) : -1;
    CHECK(r == 1 && back == 0x22, "pread 读回 fdatasync 落盘的数据");
    if (rfd >= 0)
        close(rfd);

    if (fd >= 0)
        close(fd);
    if (path)
        unlink(path);

    // 11 = 本文件 CHECK 总数; 少跑(早退) -> FAIL, 堵死假阳性。
    TEST_DONE(11);
}
