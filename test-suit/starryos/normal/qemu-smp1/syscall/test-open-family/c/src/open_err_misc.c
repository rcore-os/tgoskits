#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* 收尾的杂项 ERRNO 触发 — 把剩余可触发的全部覆盖。
 *
 * man 2 open §"EINVAL" 系列 原文：
 *   "EINVAL — Invalid value in flags."
 *   "EINVAL — O_CREAT was specified in flags and the final component
 *    (basename) of the new file's pathname is invalid (e.g., it contains
 *    characters not permitted by the underlying filesystem)."
 *   "EINVAL — The final component (basename) of pathname is invalid
 *    (e.g., it contains characters not permitted by the underlying
 *    filesystem)."
 *
 * 本模块覆盖：
 *   (1) raw_open(O_CREAT|O_WRONLY|0x80000000) — 高位非法 flag；接受 fd>=0
 *       或 -1+EINVAL/EBADF（不强求 EINVAL，主要确保 starry 不 crash）
 *   (2) raw_open("/tmp/foo\0bar", ...) — basename 含 NUL；libc strlen 截断；
 *       raw syscall 直达 kernel 验不 crash
 *
 * 不在本模块（环境/特权/未实现/资源）：
 *   - EACCES (starry root bypass)
 *   - EBUSY (块设备特权) / EDQUOT (quota)
 *   - EFBIG/EOVERFLOW (32-bit only)
 *   - EINTR (signal+FIFO 时序)
 *   - ENFILE (system-wide limit) / ENODEV (mknod 特权)
 *   - ENOMEM/ENOSPC (资源耗尽不可控)
 *   - EOPNOTSUPP (TMPFILE starry 未实现)
 *   - EPERM (NOATIME 需 owner mismatch / file seal 需 memfd)
 *   - EROFS (需 ro mount) / ETXTBSY (需 exec) / EWOULDBLOCK (file lease 特权)
 */

#define M_DIR  OF_MOD("err_misc")

/* EINVAL via raw syscall：用一个未定义的 flag 高位（O_BIG 内核 mask 之外）
 * 注：libc open 包装可能会过滤未知位；用 raw syscall 直达内核 */
static int raw_open(const char *path, int flags, mode_t mode)
{
    return (int)syscall(SYS_openat, AT_FDCWD, path, flags, mode);
}

static void einval_unknown_flag_bits(void)
{
    /* 0x80000000 是不在 Linux O_* 集合内的位（实际 Linux kernel 是否拒绝
     * 取决于版本；某些版本 silently ignore）。本测例的目标是验证 starry 不
     * crash；对 EINVAL 不强求。 */
    int fd = raw_open("/tmp/bug_einval_unknown_flags", O_CREAT | O_WRONLY | 0x80000000, 0644);
    /* 接受 fd>=0 或 -1+EINVAL 任意 — 主要确认不 crash */
    CHECK(fd >= 0 || (fd == -1 && (errno == EINVAL || errno == EBADF)),
          "einval misc: O_CREAT|O_WRONLY|0x80000000 不 crash（fd>=0 或合理 errno）");
    if (fd >= 0) {
        close(fd);
        unlink("/tmp/bug_einval_unknown_flags");
    }
}

/* basename 含 NUL：libc strlen 会截断，但 raw syscall 传完整字节
 * NUL 字节使路径在 vm_load_string 后被截断，与原路径不同（变成 "/tmp/foo"）
 *
 * codex P2 (commit 4e4e8aed4): 原断言 `fd >= 0 || fd == -1` 永远为真 —
 * 无信号无失败。改为具体期望：raw_openat(SYS_openat) 应等同 open("/tmp/foo")
 * 因为内核 vm_load_string 在 NUL 处截断。预期 fd >= 0 + 文件实际是 /tmp/foo. */
static void einval_basename_with_nul(void)
{
    /* unlink 先清理可能残留的 /tmp/foo */
    unlink("/tmp/foo");

    char path[] = "/tmp/foo\0bar";
    int fd = raw_open(path, O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0,                                                "einval misc: NUL-截断后等同 \"/tmp/foo\" 应成功 open");

    if (fd >= 0) {
        /* 验证实际打开的是 "/tmp/foo" 而不是 "/tmp/foo\0bar"（NUL 后部分被丢） */
        struct stat st1, st2;
        int rc1 = fstat(fd, &st1);
        int rc2 = stat("/tmp/foo", &st2);
        CHECK(rc1 == 0 && rc2 == 0 && st1.st_ino == st2.st_ino,
              "einval misc: NUL 截断后 fd 对应 /tmp/foo (inode 一致)");
        close(fd);
        unlink("/tmp/foo");
    }
}

int open_err_misc_run(void)
{
    printf("\n----- open_err_misc -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "err_misc setup: mkdir");
    einval_unknown_flag_bits();
    einval_basename_with_nul();
    cleanup_tree(M_DIR);
    printf("  ----- open_err_misc: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
