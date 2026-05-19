/*
 * bug-rename-replace.c — rename(2) 替换已存在文件的行为验证
 *
 * Bug 背景：
 *   GNU sed -i (in-place 编辑) 在 StarryOS 上产生空文件。
 *   例如: echo "hello" > /tmp/f; sed -i 's/hello/world/' /tmp/f
 *   预期: /tmp/f 内容为 "world"
 *   实际: /tmp/f 内容为空字符串
 *
 * 根因分析：
 *   GNU sed 实现 -i 的步骤:
 *     1. open(原文件, O_RDONLY)                — 读取原始内容
 *     2. mkstemp(同目录临时文件)                — 创建临时文件
 *     3. 将修改后内容 write 到临时文件
 *     4. close 临时文件
 *     5. rename(临时文件路径, 原文件路径)        — 原子替换
 *
 *   POSIX rename(2) 语义: 当 newpath 已存在时，原子性地用 oldpath
 *   替换 newpath（oldpath 消失，newpath 指向原 oldpath 的内容）。
 *
 *   在 StarryOS 上，rename 替换已存在文件时，原文件的目录项被移除，
 *   但临时文件的内容未能正确链接到原路径，导致原文件变为空文件。
 *
 * 触发条件：
 *   rename(新文件, 已存在文件) — newpath 必须已存在才会触发此 bug
 *   rename(新文件, 不存在路径) — 正常工作
 *
 * 测试覆盖：
 *   1. rename(temp, existing)          — 基本替换，验证内容
 *   2. rename(temp, existing) + fd 打开 — 模拟 sed 同时持有原文件 fd
 *   3. 完整 sed -i 模拟                — write temp → close → rename → verify
 *   4. 交叉大小验证                    — 确认替换后文件大小来自 temp
 *   5. rename(temp, nonexist)          — 对照组：目标不存在时是否正常
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ========== 测试框架 ========== */

static int test_passed = 0;
static int test_failed = 0;

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

/* ========== 辅助函数 ========== */

static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

static int read_file(const char *path, char *buf, int bufsz)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    int n = (int)read(fd, buf, bufsz - 1);
    close(fd);
    if (n < 0) return -1;
    buf[n] = '\0';
    return n;
}

int main(void)
{
    printf("[TEST] rename: replacing an existing file (sed -i pattern)\n\n");

    const char *original = "/tmp/bug_rename_original.txt";
    const char *tempfile  = "/tmp/bug_rename_temp.txt";
    const char *target3   = "/tmp/bug_rename_nonexist.txt";

    /* 预清理 */
    unlink(original);
    unlink(tempfile);
    unlink(target3);

    /* ---- Test 1: 基本 rename 替换 ---- */
    printf("== Test 1: rename(temp, existing) — basic replace ==\n");
    {
        write_file(original, "OLD CONTENT\n");
        write_file(tempfile,  "NEW CONTENT\n");

        errno = 0;
        int rc = rename(tempfile, original);
        CHECK(rc == 0, "rename(temp, original) returns 0");

        char buf[256] = {0};
        int n = read_file(original, buf, sizeof(buf));
        CHECK(n > 0, "original file is not empty after rename");
        CHECK(strcmp(buf, "NEW CONTENT\n") == 0,
              "original file has new content (NEW CONTENT)");

        CHECK(access(tempfile, F_OK) != 0 && errno == ENOENT,
              "temp file no longer exists (ENOENT)");
    }

    /* ---- Test 2: rename 时原文件 fd 仍打开 (模拟 sed 读原文件) ---- */
    printf("\n== Test 2: rename with original fd open (sed read pattern) ==\n");
    {
        write_file(original, "BEFORE\n");
        write_file(tempfile,  "AFTER\n");

        /* sed 打开原文件读取 */
        int fd_orig = open(original, O_RDONLY);
        CHECK(fd_orig >= 0, "open original for reading");

        char buf_before[64] = {0};
        read(fd_orig, buf_before, sizeof(buf_before));
        CHECK(strcmp(buf_before, "BEFORE\n") == 0,
              "original content readable via open fd");

        /* sed 写临时文件 */
        write_file(tempfile, "AFTER\n");

        /* sed 执行 rename */
        errno = 0;
        int rc = rename(tempfile, original);
        CHECK(rc == 0, "rename(temp, original) with fd open returns 0");

        /* 通过路径读取 — 应看到新内容 */
        char buf_after[64] = {0};
        int n = read_file(original, buf_after, sizeof(buf_after));
        CHECK(n > 0, "original path is not empty after rename");
        CHECK(strcmp(buf_after, "AFTER\n") == 0,
              "original path has new content (AFTER)");

        close(fd_orig);

        /* 关闭 fd 后再验证一次 */
        char buf_final[64] = {0};
        read_file(original, buf_final, sizeof(buf_final));
        CHECK(strcmp(buf_final, "AFTER\n") == 0,
              "content persists after closing original fd");
    }

    /* ---- Test 3: 完整 sed -i 模拟 ---- */
    printf("\n== Test 3: full sed -i simulation ==\n");
    {
        write_file(original, "foo bar baz\n");

        /* 模拟 sed: 在同目录创建临时文件, 写入替换后内容 */
        write_file(tempfile, "foo replaced baz\n");

        errno = 0;
        int rc = rename(tempfile, original);
        CHECK(rc == 0, "rename returns 0");

        char buf[256] = {0};
        read_file(original, buf, sizeof(buf));
        CHECK(strcmp(buf, "foo replaced baz\n") == 0,
              "file has replaced content (foo replaced baz)");
        CHECK(strstr(buf, "bar") == NULL,
              "old substring 'bar' no longer present");
    }

    /* ---- Test 4: 交叉大小验证 ---- */
    printf("\n== Test 4: file size after rename (cross-size) ==\n");
    {
        write_file(original, "AAAAAAAAAA\n");  /* 11 bytes */
        write_file(tempfile,  "BBB\n");         /*  4 bytes */

        errno = 0;
        int rc = rename(tempfile, original);
        CHECK(rc == 0, "rename returns 0");

        struct stat st;
        CHECK(stat(original, &st) == 0, "stat succeeds");
        CHECK(st.st_size == 4,
              "file size is 4 (from temp), not 11 (from original)");

        char buf[64] = {0};
        read_file(original, buf, sizeof(buf));
        CHECK(strcmp(buf, "BBB\n") == 0,
              "content is BBB (from temp), not AAAA (from original)");
    }

    /* ---- Test 5: 对照组 — 目标不存在时 rename ---- */
    printf("\n== Test 5: rename(temp, nonexist) — control group ==\n");
    {
        write_file(tempfile, "CONTROL\n");
        unlink(target3);  /* 确保目标不存在 */

        errno = 0;
        int rc = rename(tempfile, target3);
        CHECK(rc == 0, "rename(temp, nonexist) returns 0");

        char buf[64] = {0};
        read_file(target3, buf, sizeof(buf));
        CHECK(strcmp(buf, "CONTROL\n") == 0,
              "target file has temp content (CONTROL)");

        CHECK(access(tempfile, F_OK) != 0,
              "source file no longer exists");
    }

    /* 清理 */
    unlink(original);
    unlink(tempfile);
    unlink(target3);

    printf("\n=== 结果: %d 通过, %d 失败 ===\n", test_passed, test_failed);
    if (test_failed == 0) {
        printf("TEST PASSED\n");
        return EXIT_SUCCESS;
    }
    return EXIT_FAILURE;
}
