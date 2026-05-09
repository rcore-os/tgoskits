#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>

/*
 * truncate 对比测试:
 *   Linux/WSL 作为正确基线，StarryOS 的偏差即为 BUG
 *
 * Linux truncate(2):
 *   int truncate(const char *path, off_t length);
 *
 * 预期行为 (POSIX/Linux):
 *   - length < 0          -> EINVAL
 *   - path == NULL        -> EFAULT
 *   - 文件不存在           -> ENOENT
 *   - path 是目录          -> EISDIR
 *   - length 超最大文件大小 -> EFBIG
 *   - 正常截断到更小/更大    -> 0 (扩展部分读作 0)
 *   - 截断到 0             -> 0
 */

static int call_truncate(const char *path, off_t length)
{
    errno = 0;
    return truncate(path, length);
}

/* 检查文件大小是否等于预期 */
static int check_size(int fd, off_t expected, const char *file, int line,
                      const char *msg)
{
    struct stat st;
    if (fstat(fd, &st) != 0) {
        printf("  FAIL | %s:%d | %s | fstat failed errno=%d (%s)\n",
               file, line, msg, errno, strerror(errno));
        __fail++;
        return 0;
    }
    if (st.st_size == expected) {
        printf("  PASS | %s:%d | %s (size=%ld)\n", file, line, msg,
               (long)st.st_size);
        __pass++;
        return 1;
    } else {
        printf("  FAIL | %s:%d | %s | expected size=%ld got=%ld\n",
               file, line, msg, (long)expected, (long)st.st_size);
        __fail++;
        return 0;
    }
}

int main(void)
{
    TEST_START("truncate");

    /* ================================================================
     * 1. 正常截断 — 创建文件，写入数据，截断到更小的尺寸
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "hello world", 11), 11, "写入 11 字节");
        close(fd);

        CHECK_RET(call_truncate(tmpl, 5), 0, "truncate 到 5 字节");

        fd = open(tmpl, O_RDONLY);
        CHECK(fd >= 0, "重新打开文件");
        check_size(fd, 5, __FILE__, __LINE__, "截断后文件大小应为 5");
        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 2. 扩展截断 — 截断到更大的尺寸，扩展部分为 \0
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "hi", 2), 2, "写入 2 字节");
        close(fd);

        CHECK_RET(call_truncate(tmpl, 4096), 0, "truncate 到 4096 字节");

        fd = open(tmpl, O_RDONLY);
        CHECK(fd >= 0, "重新打开文件");
        check_size(fd, 4096, __FILE__, __LINE__, "扩展后文件大小应为 4096");

        /* 验证扩展部分读作 \0 */
        char buf[16] = {0};
        ssize_t n = pread(fd, buf, sizeof(buf), 2);
        CHECK(n == sizeof(buf), "pread 扩展区域");
        int all_zero = 1;
        for (int i = 0; i < (int)sizeof(buf); i++) {
            if (buf[i] != 0) { all_zero = 0; break; }
        }
        CHECK(all_zero, "扩展区域应为全零");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 3. 截断到 0 — 文件变空
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "some data here", 14), 14, "写入 14 字节");
        close(fd);

        CHECK_RET(call_truncate(tmpl, 0), 0, "truncate 到 0");

        fd = open(tmpl, O_RDONLY);
        CHECK(fd >= 0, "重新打开文件");
        check_size(fd, 0, __FILE__, __LINE__, "截断到 0 后文件大小应为 0");
        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 4. length 为负数 — 应返回 EINVAL
     * ================================================================ */
    {
        CHECK_ERR(call_truncate("/dev/null", -1), EINVAL,
                  "length=-1 应返回 EINVAL");
        CHECK_ERR(call_truncate("/dev/null", -4096), EINVAL,
                  "length=-4096 应返回 EINVAL");
    }

    /* ================================================================
     * 5. 文件不存在 — 应返回 ENOENT
     * ================================================================ */
    {
        CHECK_ERR(call_truncate("/tmp/no-such-file-truncate-test", 0),
                  ENOENT, "不存在文件应返回 ENOENT");
    }

    /* ================================================================
     * 6. path 是目录 — 应返回 EISDIR
     * ================================================================ */
    {
        CHECK_ERR(call_truncate("/tmp", 0), EISDIR,
                  "目录应返回 EISDIR");
        CHECK_ERR(call_truncate("/", 0), EISDIR,
                  "根目录应返回 EISDIR");
    }

    /* ================================================================
     * 7. 路径中间不是目录 — 应返回 ENOTDIR
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        char badpath[256];
        snprintf(badpath, sizeof(badpath), "%s/sub/file", tmpl);
        CHECK_ERR(call_truncate(badpath, 0), ENOTDIR,
                  "路径中间是文件不是目录应返回 ENOTDIR");

        unlink(tmpl);
    }

    /* ================================================================
     * 8. path 为空字符串 — 应返回 ENOENT
     * ================================================================ */
    {
        CHECK_ERR(call_truncate("", 0), ENOENT,
                  "空路径应返回 ENOENT");
    }

    /* ================================================================
     * 9. 超大 length — 应返回 EFBIG 或 ENOSPC
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        off_t huge = (off_t)((unsigned long long)1 << 60);
        errno = 0;
        long ret = (long)call_truncate(tmpl, huge);
        if (ret == -1 && (errno == EFBIG || errno == ENOSPC ||
                          errno == EOVERFLOW)) {
            printf("  PASS | %s:%d | 超大 length 返回 errno=%d (expected)\n",
                   __FILE__, __LINE__, errno);
            __pass++;
        } else if (ret == 0) {
            printf("  FAIL | %s:%d | 超大 length 不应返回 0 "
                   "(BUG: 未检查 length 上限)\n",
                   __FILE__, __LINE__);
            __fail++;
        } else {
            printf("  FAIL | %s:%d | 超大 length 意外结果 | "
                   "ret=%ld errno=%d (%s)\n",
                   __FILE__, __LINE__, ret, errno, strerror(errno));
            __fail++;
        }

        unlink(tmpl);
    }

    /* ================================================================
     * 10. repeat — 多次截断同一个文件
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_RET(call_truncate(tmpl, 100), 0, "truncate 到 100");
        CHECK_RET(call_truncate(tmpl, 50), 0, "truncate 到 50");
        CHECK_RET(call_truncate(tmpl, 200), 0, "truncate 到 200");

        fd = open(tmpl, O_RDONLY);
        CHECK(fd >= 0, "重新打开文件");
        check_size(fd, 200, __FILE__, __LINE__, "多次截断后应为 200");
        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 11. 只读文件 truncate 权限检查
     *     root (fsuid=0) 绕过写权限检查 (CAP_FOWNER)，
     *     非 root 用户对 0444 文件 truncate 返回 EACCES。
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "data", 4), 4, "写入 4 字节");
        close(fd);

        CHECK_RET(chmod(tmpl, 0444), 0, "chmod 0444 只读");

        /* root 可绕过写权限检查 */
        CHECK_RET(call_truncate(tmpl, 2), 0,
                  "root truncate 0444 文件成功 (CAP_FOWNER 绕过)");

        /* 切换到非 root 用户 (fsuid=1000) */
        CHECK_RET(setreuid(-1, 1000), 0, "setreuid 到 1000");
        CHECK_ERR(call_truncate(tmpl, 2), EACCES,
                  "非 root truncate 0444 文件应返回 EACCES");

        /* 恢复 root */
        CHECK_RET(setreuid(-1, 0), 0, "恢复 root");
        CHECK_RET(chmod(tmpl, 0644), 0, "恢复权限");
        unlink(tmpl);
    }

    /* ================================================================
     * 12. 通过 symlink truncate — 应截断目标文件
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-truncate-target-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "hello world", 11), 11, "写入 11 字节");
        close(fd);

        char linkpath[] = "/tmp/test-truncate-link-XXXXXX";
        /* use a static name pattern since we control the target */
        CHECK_RET(symlink(tmpl, linkpath), 0, "创建 symlink");

        CHECK_RET(call_truncate(linkpath, 5), 0, "通过 symlink truncate");

        fd = open(tmpl, O_RDONLY);
        CHECK(fd >= 0, "打开目标文件");
        check_size(fd, 5, __FILE__, __LINE__, "symlink 目标文件应为 5");
        close(fd);

        unlink(linkpath);
        unlink(tmpl);
    }

    /* ================================================================
     * 13. 不存在路径 + 超大 length — 应返回 ENOENT 而非 EFBIG
     *     (errno 优先级: 路径解析先于 length 校验)
     * ================================================================ */
    {
        off_t huge = (off_t)((unsigned long long)1 << 60);
        CHECK_ERR(call_truncate("/tmp/no-such-file-XXXXXX", huge),
                  ENOENT,
                  "不存在文件 + 超大 length 应返回 ENOENT");
    }

    /* ================================================================
     * 14. 目录 + 超大 length — 应返回 EISDIR 而非 EFBIG
     *     (errno 优先级: type 校验先于 length 校验)
     * ================================================================ */
    {
        off_t huge = (off_t)((unsigned long long)1 << 60);
        CHECK_ERR(call_truncate("/tmp", huge), EISDIR,
                  "目录 + 超大 length 应返回 EISDIR");
    }

    TEST_DONE();
}
