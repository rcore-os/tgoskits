/*
 * bug-unlinkat-einval.c — unlinkat flags validation
 *
 * Bug：StarryOS 的 sys_unlinkat 不校验 flags，任何非 AT_REMOVEDIR 的比特位都被
 * 默默吞掉，与 Linux 行为不符：man 2 unlinkat 明确 "EINVAL  An invalid flag
 * value was specified in flags."，内核源码 fs/namei.c 中有
 * `if (flag & ~AT_REMOVEDIR) return -EINVAL;`。
 *
 * 影响：
 *   - unlinkat(path, 0x1) 这类带非法 flag 的调用本应失败，却会把文件删掉；
 *   - unlinkat(dir, AT_REMOVEDIR|0x1) 应当 EINVAL，现在会走 remove_file 分支。
 *
 * 测试覆盖：
 *   负向：flags=0x1                 对普通文件 → EINVAL，文件仍存在
 *   负向：flags=AT_SYMLINK_NOFOLLOW 对普通文件 → EINVAL，文件仍存在
 *   负向：flags=AT_REMOVEDIR|0x1    对目录     → EINVAL，目录仍存在
 *   正向：flags=0                   对普通文件 → 成功删除
 *   正向：flags=AT_REMOVEDIR        对目录     → 成功删除
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

/* ========== 内联测试框架 ========== */

static int test_passed = 0;
static int test_failed = 0;

#define TEST_START(name) \
    do { \
        printf("[TEST] %s\n", (name)); \
        test_passed = 0; \
        test_failed = 0; \
    } while(0)

#define CHECK(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno, strerror(errno)); \
            test_failed++; \
        } else { \
            printf("  [OK] %s\n", (msg)); \
            test_passed++; \
        } \
    } while(0)

#define TEST_DONE() \
    do { \
        printf("\n=== 结果: %d 通过, %d 失败 ===\n", test_passed, test_failed); \
        if (test_failed == 0) { printf("TEST PASSED\n"); } \
        return (test_failed == 0) ? EXIT_SUCCESS : EXIT_FAILURE; \
    } while(0)

/* ========== 测试代码 ========== */

static int create_regular_file(const char *path)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return -1;
    }
    close(fd);
    return 0;
}

int main(void)
{
    TEST_START("unlinkat: reject invalid flag bits with EINVAL");

    const char *file_path = "/tmp/bug_unlinkat_einval_file";
    const char *dir_path  = "/tmp/bug_unlinkat_einval_dir";

    /* 预清理，防止上次运行残留 */
    unlink(file_path);
    rmdir(dir_path);

    /* -------- 普通文件：非法 flags -------- */

    CHECK(create_regular_file(file_path) == 0, "创建测试文件");

    /* 1. flags=0x1 (bit 0 未定义) 必须 EINVAL */
    errno = 0;
    int r = unlinkat(AT_FDCWD, file_path, 0x1);
    CHECK(r == -1 && errno == EINVAL,
          "unlinkat(file, 0x1) 返回 -1 且 errno=EINVAL");

    struct stat st;
    CHECK(stat(file_path, &st) == 0,
          "非法 flags 后文件仍然存在");

    /* 2. flags=AT_SYMLINK_NOFOLLOW (0x100) 对 unlinkat 无效 */
    errno = 0;
    r = unlinkat(AT_FDCWD, file_path, AT_SYMLINK_NOFOLLOW);
    CHECK(r == -1 && errno == EINVAL,
          "unlinkat(file, AT_SYMLINK_NOFOLLOW) 返回 -1 且 errno=EINVAL");
    CHECK(stat(file_path, &st) == 0,
          "AT_SYMLINK_NOFOLLOW 后文件仍然存在");

    /* 3. 正向：flags=0 成功删除 */
    errno = 0;
    r = unlinkat(AT_FDCWD, file_path, 0);
    CHECK(r == 0, "unlinkat(file, 0) 正常删除返回 0");
    CHECK(stat(file_path, &st) == -1 && errno == ENOENT,
          "文件已被删除 (ENOENT)");

    /* -------- 目录：非法 flags 组合 -------- */

    CHECK(mkdir(dir_path, 0700) == 0, "创建测试目录");

    /* 4. flags=AT_REMOVEDIR|0x1 含非法位 → EINVAL */
    errno = 0;
    r = unlinkat(AT_FDCWD, dir_path, AT_REMOVEDIR | 0x1);
    CHECK(r == -1 && errno == EINVAL,
          "unlinkat(dir, AT_REMOVEDIR|0x1) 返回 -1 且 errno=EINVAL");
    CHECK(stat(dir_path, &st) == 0,
          "非法 flags 后目录仍然存在");

    /* 5. 正向：flags=AT_REMOVEDIR 正确删除目录 */
    errno = 0;
    r = unlinkat(AT_FDCWD, dir_path, AT_REMOVEDIR);
    CHECK(r == 0, "unlinkat(dir, AT_REMOVEDIR) 正常删除返回 0");
    CHECK(stat(dir_path, &st) == -1 && errno == ENOENT,
          "目录已被删除 (ENOENT)");

    TEST_DONE();
}
