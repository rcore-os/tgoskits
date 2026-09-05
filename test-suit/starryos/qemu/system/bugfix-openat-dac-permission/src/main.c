#define _GNU_SOURCE
/*
 * bug-openat-dac-permission — openat(2) must enforce owner/group/other mode.
 *
 * ground truth: Linux checks read/write/execute permission against the file's
 * owner/group/other mode bits at open time (may_open -> inode_permission);
 * O_TRUNC additionally requires write permission. StarryOS took the caller's
 * credential only to stamp new inodes, never to authorize opening an existing
 * one, so an unrelated user could read/write/truncate a mode-denied file
 * (CWE-732/862). Here a root-owned 0644 file: "other" may read but not write,
 * and O_TRUNC on it must be refused before it can truncate.
 */
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

static int failed;
static void check(int cond, const char *msg)
{
    if (cond) {
        printf("  PASS | %s\n", msg);
    } else {
        printf("  FAIL | %s | errno=%d (%s)\n", msg, errno, strerror(errno));
        failed = 1;
    }
}

int main(void)
{
    check(getuid() == 0, "测试前置: 以 root 启动");

    const char *p = "/tmp/dac_openat_test";
    int fd = open(p, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    check(fd >= 0, "root 创建文件");
    if (fd < 0) {
        printf("=== bug-openat-dac-permission: FAIL ===\n");
        return 1;
    }
    check(write(fd, "hello-dac", 9) == 9, "写入初始内容 (9 字节)");
    close(fd);
    check(chmod(p, 0644) == 0, "chmod 0644 (owner rw, other 仅 r)");

    /* root bypasses DAC. */
    int rf = open(p, O_RDWR);
    check(rf >= 0, "root O_RDWR 允许 (root 旁路)");
    if (rf >= 0) {
        close(rf);
    }

    check(setuid(1000) == 0, "setuid(1000) 掉特权 (成为 other)");

    /* other has read (0644): O_RDONLY allowed, and content readable. */
    int r = open(p, O_RDONLY);
    check(r >= 0, "非 owner O_RDONLY 允许 (other 有 r)");
    if (r >= 0) {
        char buf[16] = {0};
        ssize_t n = read(r, buf, sizeof(buf) - 1);
        check(n == 9 && memcmp(buf, "hello-dac", 9) == 0, "内容完整可读");
        close(r);
    }

    /* other has no write: O_WRONLY and O_TRUNC must be refused with EACCES. */
    errno = 0;
    r = open(p, O_WRONLY);
    check(r == -1 && errno == EACCES, "非 owner O_WRONLY 被拒 EACCES");
    if (r >= 0) {
        close(r);
    }

    errno = 0;
    r = open(p, O_WRONLY | O_TRUNC);
    check(r == -1 && errno == EACCES, "非 owner O_WRONLY|O_TRUNC 被拒 EACCES");
    if (r >= 0) {
        close(r);
    }

    /* the refused O_TRUNC must not have truncated the file. */
    r = open(p, O_RDONLY);
    if (r >= 0) {
        char buf[16] = {0};
        ssize_t n = read(r, buf, sizeof(buf) - 1);
        check(n == 9, "O_TRUNC 被拒后内容未被截断 (仍 9 字节)");
        close(r);
    } else {
        check(0, "复读打开失败");
    }

    printf("=== bug-openat-dac-permission: %s ===\n", failed ? "FAIL" : "PASS");
    return failed;
}
