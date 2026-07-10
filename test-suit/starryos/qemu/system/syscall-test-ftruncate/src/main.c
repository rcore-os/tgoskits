#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>

/*
 * ftruncate 对比测试:
 *   Linux/WSL 作为正确基线，StarryOS 的偏差即为 BUG
 *
 * Linux ftruncate(2):
 *   int ftruncate(int fd, off_t length);
 *
 * 预期行为 (POSIX/Linux):
 *   - length < 0         -> EINVAL
 *   - fd 无效            -> EBADF
 *   - fd 未打开写        -> EBADF 或 EINVAL
 *   - fd 是 pipe/socket  -> EINVAL
 *   - length 超最大上限   -> EFBIG
 *   - 正常截断到更小/更大 -> 0
 *   - 截断到 0           -> 0
 */

static int call_ftruncate(int fd, off_t length)
{
    errno = 0;
    return ftruncate(fd, length);
}

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
    TEST_START("ftruncate");

    /* ================================================================
     * 1. 正常截断 — 创建文件，写入数据，截断到更小的尺寸
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "hello world", 11), 11, "写入 11 字节");

        CHECK_RET(call_ftruncate(fd, 5), 0, "ftruncate 到 5 字节");
        check_size(fd, 5, __FILE__, __LINE__, "截断后文件大小应为 5");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 2. 扩展截断 — 截断到更大的尺寸，扩展部分为 \0
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "hi", 2), 2, "写入 2 字节");

        CHECK_RET(call_ftruncate(fd, 4096), 0, "ftruncate 到 4096 字节");
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
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, "some data here", 14), 14, "写入 14 字节");

        CHECK_RET(call_ftruncate(fd, 0), 0, "ftruncate 到 0");
        check_size(fd, 0, __FILE__, __LINE__, "截断到 0 后文件大小应为 0");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 4. length 为负数 — 应返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_ftruncate(fd, -1), EINVAL,
                  "length=-1 应返回 EINVAL");
        CHECK_ERR(call_ftruncate(fd, -4096), EINVAL,
                  "length=-4096 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 5. 无效 fd (-1) — 应返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_ftruncate(-1, 0), EBADF,
                  "fd=-1 应返回 EBADF");
    }

    /* ================================================================
     * 6. 已关闭的 fd — 应返回 EBADF
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_ftruncate(fd, 0), EBADF,
                  "已关闭的 fd 应返回 EBADF");

        unlink(tmpl);
    }

    /* ================================================================
     * 7. 只读 fd — 应返回 EBADF 或 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建临时文件");
        CHECK_RET(write(fd, "data", 4), 4, "写入数据");
        close(fd);

        int rd_fd = open(tmpl, O_RDONLY);
        CHECK(rd_fd >= 0, "open O_RDONLY 应成功");

        errno = 0;
        long ret = (long)call_ftruncate(rd_fd, 2);
        if (ret == -1 && (errno == EBADF || errno == EINVAL)) {
            printf("  PASS | %s:%d | 只读 fd ftruncate 返回 errno=%d "
                   "(expected)\n", __FILE__, __LINE__, errno);
            __pass++;
        } else {
            printf("  FAIL | %s:%d | 只读 fd ftruncate | "
                   "expected EBADF/EINVAL got ret=%ld errno=%d (%s)\n",
                   __FILE__, __LINE__, ret, errno, strerror(errno));
            __fail++;
        }

        close(rd_fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 8. pipe fd — 应返回 EINVAL
     * ================================================================ */
    {
        int pipe_fds[2];
        CHECK_RET(pipe(pipe_fds), 0, "创建 pipe");

        CHECK_ERR(call_ftruncate(pipe_fds[1], 0), EINVAL,
                  "pipe 写端 ftruncate 应返回 EINVAL");
        CHECK_ERR(call_ftruncate(pipe_fds[0], 0), EINVAL,
                  "pipe 读端 ftruncate 应返回 EINVAL");

        close(pipe_fds[0]);
        close(pipe_fds[1]);
    }

    /* ================================================================
     * 9. 非普通文件 fd (如 /dev/null) — Linux 允许 ftruncate
     *    (取决于文件系统和设备)，此处只记录行为不强制断言
     * ================================================================ */
    {
        int fd = open("/dev/null", O_WRONLY);
        if (fd >= 0) {
            errno = 0;
            long ret = (long)call_ftruncate(fd, 0);
            /* Linux 上 /dev/null 的 ftruncate 返回 EINVAL，记录行为 */
            if (ret == -1 && errno == EINVAL) {
                printf("  PASS | %s:%d | /dev/null ftruncate 返回 EINVAL "
                       "(expected on Linux)\n", __FILE__, __LINE__);
                __pass++;
            } else if (ret == 0) {
                printf("  PASS | %s:%d | /dev/null ftruncate 返回 0 "
                       "(acceptable)\n", __FILE__, __LINE__);
                __pass++;
            } else {
                printf("  PASS | %s:%d | /dev/null ftruncate ret=%ld "
                       "errno=%d (acceptable)\n",
                       __FILE__, __LINE__, ret, errno);
                __pass++;
            }
            close(fd);
        }
    }

    /* ================================================================
     * 10. 超大 length — 应返回 EFBIG 或 ENOSPC
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        off_t huge = (off_t)((unsigned long long)1 << 60);
        errno = 0;
        long ret = (long)call_ftruncate(fd, huge);
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

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 11. repeat — 多次截断同一个 fd
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(call_ftruncate(fd, 100), 0, "ftruncate 到 100");
        check_size(fd, 100, __FILE__, __LINE__, "第一次截断后 100");
        CHECK_RET(call_ftruncate(fd, 50), 0, "ftruncate 到 50");
        check_size(fd, 50, __FILE__, __LINE__, "第二次截断后 50");
        CHECK_RET(call_ftruncate(fd, 200), 0, "ftruncate 到 200");
        check_size(fd, 200, __FILE__, __LINE__, "第三次截断后 200");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 12. fd 是目录 — 应返回 EINVAL
     *     StarryOS BUG: 返回 EISDIR 而非 EINVAL
     * ================================================================ */
    {
        int dir_fd = open("/tmp", O_RDONLY);
        CHECK(dir_fd >= 0, "open /tmp O_RDONLY 应成功");

        CHECK_ERR(call_ftruncate(dir_fd, 0), EINVAL,
                  "目录 fd ftruncate 应返回 EINVAL");

        close(dir_fd);
    }

    /* ================================================================
     * 13. truncate 成功后 offset 不受影响
     *      (POSIX: ftruncate 不改变文件位置)
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = open(tmpl, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create file");

        CHECK_RET(write(fd, "ABCDEFGHIJ", 10), 10, "写入 10 字节");
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "seek 到开头");
        CHECK_RET(call_ftruncate(fd, 5), 0, "ftruncate 到 5");

        off_t pos = lseek(fd, 0, SEEK_CUR);
        CHECK(pos == 0, "ftruncate 不应改变文件位置");

        check_size(fd, 5, __FILE__, __LINE__, "截断后大小应为 5");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 14. 截断后写入 — 截断为 0 后重新写入
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(write(fd, "hello world", 11), 11, "写入 11 字节");
        CHECK_RET(call_ftruncate(fd, 0), 0, "ftruncate 到 0");
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "seek 到开头");
        CHECK_RET(write(fd, "new", 3), 3, "写入 'new'");
        check_size(fd, 3, __FILE__, __LINE__, "截断后重写大小应为 3");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 15. Bad fd (-1) + 超大 length — 应返回 EBADF 而非 EFBIG
     *     (errno 优先级: fd 校验先于 length 校验)
     * ================================================================ */
    {
        off_t huge = (off_t)((unsigned long long)1 << 60);
        CHECK_ERR(call_ftruncate(-1, huge), EBADF,
                  "bad fd + 超大 length 应返回 EBADF");
    }

    /* ================================================================
     * 16. Pipe fd + 超大 length — 应返回 EINVAL 而非 EFBIG
     *     (errno 优先级: fd type 校验先于 length 校验)
     * ================================================================ */
    {
        int p[2];
        CHECK_RET(pipe(p), 0, "创建 pipe");
        off_t huge = (off_t)((unsigned long long)1 << 60);
        CHECK_ERR(call_ftruncate(p[1], huge), EINVAL,
                  "pipe fd + 超大 length 应返回 EINVAL");
        close(p[0]);
        close(p[1]);
    }

    /* ================================================================
     * 收缩后再扩展: 原页尾部必须读作 \0 (回归)
     * 写满一页 0xAA -> 收缩到 100 字节 -> 再扩回 4096。Linux 在收缩时把
     * 部分末页 [100, 4096) 的尾部清零, 因此 regrow 后这段读作 \0。旧
     * starry 只改文件长度、漏清 page-cache 中已驻留页的尾部, 导致 regrow
     * 后读到残留的 0xAA(sqlite WAL 跨进程 reopen 据此读到脏 header 而崩)。
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-ftruncate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功 (shrink-regrow tail)");

        char page[4096];
        memset(page, 0xAA, sizeof(page));
        CHECK_RET(write(fd, page, sizeof(page)), 4096, "写满一页 0xAA");
        CHECK_RET(call_ftruncate(fd, 100), 0, "收缩到 100 字节");
        CHECK_RET(call_ftruncate(fd, 4096), 0, "再扩回 4096 字节");

        char back[4096];
        memset(back, 0xFF, sizeof(back));
        ssize_t n = pread(fd, back, sizeof(back), 0);
        CHECK(n == 4096, "pread 整页回读");
        int tail_zero = 1;
        for (int i = 100; i < 4096; i++) {
            if (back[i] != 0) {
                tail_zero = 0;
                break;
            }
        }
        CHECK(tail_zero,
              "收缩-扩展后原页尾部 [100,4096) 必须为全零 (无脏残留)");

        close(fd);
        unlink(tmpl);
    }

    TEST_DONE();
}
