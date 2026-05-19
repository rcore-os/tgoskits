#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

/*
 * fadvise64 对比测试:
 *   Linux 作为正确基线，验证 StarryOS 的 errno 优先级和 fd 类型语义。
 *
 * Linux posix_fadvise(2):
 *   int posix_fadvise(int fd, off_t offset, off_t len, int advice);
 *
 * 关键语义:
 *   1. EBADF 优先级 > ESPIPE > EINVAL
 *   2. 目录 fd 返回成功 (fadvise 仅是 advisory hint)
 *   3. offset 可为负数 (不做实际 I/O)
 */

static inline int call_fadvise(int fd, off_t offset, off_t len, int advice)
{
    errno = 0;
    int ret = posix_fadvise(fd, offset, len, advice);
    /* posix_fadvise returns positive errno on error, not -1 */
    if (ret > 0) {
        errno = ret;
        return -1;
    }
    return ret;
}

int main(void)
{
    TEST_START("fadvise64");

    /* ================================================================
     * 1. 合法 advice 值 (0-5) — 应返回 0
     * ================================================================ */
    {
        const int valid_advice[] = {
            POSIX_FADV_NORMAL,
            POSIX_FADV_SEQUENTIAL,
            POSIX_FADV_RANDOM,
            POSIX_FADV_NOREUSE,
            POSIX_FADV_WILLNEED,
            POSIX_FADV_DONTNEED,
        };
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        for (int i = 0; i < 6; i++) {
            char buf[64];
            snprintf(buf, sizeof(buf), "advice=%d 应返回 0", valid_advice[i]);
            CHECK_RET(call_fadvise(fd, 0, 4096, valid_advice[i]), 0, buf);
        }

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 2. 无效 advice (>5) — 返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fadvise(fd, 0, 4096, 99), EINVAL,
                  "advice=99 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 3. len=0 — 表示到文件末尾，应返回 0
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(call_fadvise(fd, 0, 0, POSIX_FADV_NORMAL), 0,
                  "len=0 应返回 0 (到文件末尾)");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 4. offset 为负数 — Linux 接受 (advice 是 hint，不做 I/O)
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(call_fadvise(fd, -1, 4096, POSIX_FADV_NORMAL), 0,
                  "offset=-1 应返回 0 (advice 是 hint)");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 5. len 为负数 — Linux 返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fadvise(fd, 0, -1, POSIX_FADV_NORMAL), EINVAL,
                  "len=-1 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 6. 无效 fd (-1) — 应返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_fadvise(-1, 0, 4096, POSIX_FADV_NORMAL), EBADF,
                  "fd=-1 应返回 EBADF");
    }

    /* ================================================================
     * 7. 已关闭的 fd — 应返回 EBADF
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_fadvise(fd, 0, 4096, POSIX_FADV_NORMAL), EBADF,
                  "已关闭的 fd 应返回 EBADF");

        unlink(tmpl);
    }

    /* ================================================================
     * 8. pipe fd — Linux 返回 ESPIPE
     * ================================================================ */
    {
        int pipe_fds[2];
        CHECK_RET(pipe(pipe_fds), 0, "创建 pipe");

        CHECK_ERR(call_fadvise(pipe_fds[1], 0, 4096, POSIX_FADV_NORMAL),
                  ESPIPE, "pipe 写端 fadvise 应返回 ESPIPE");

        close(pipe_fds[0]);
        close(pipe_fds[1]);
    }

    /* ================================================================
     * 9. 只读 fd — fadvise 仅给 hint，不应要求写权限
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建临时文件");

        CHECK_RET(call_fadvise(fd, 0, 4096, POSIX_FADV_NORMAL), 0,
                  "读写 fd fadvise 应返回 0");
        close(fd);

        int rd_fd = open(tmpl, O_RDONLY);
        CHECK(rd_fd >= 0, "open O_RDONLY 应成功");

        CHECK_RET(call_fadvise(rd_fd, 0, 4096, POSIX_FADV_NORMAL), 0,
                  "只读 fd fadvise 应返回 0");

        close(rd_fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 10. fd=-1 且 len=-1 — EBADF 优先级高于 EINVAL
     *     Linux fadvise64(-1, 0, -1, NORMAL) 返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_fadvise(-1, 0, -1, POSIX_FADV_NORMAL), EBADF,
                  "fd=-1 且 len=-1, EBADF 优先级高于 EINVAL");
    }

    /* ================================================================
     * 11. 已关闭 fd 且 len=-1 — EBADF 优先级高于 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fadvise-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_fadvise(fd, 0, -1, POSIX_FADV_NORMAL), EBADF,
                  "已关闭 fd 且 len=-1, EBADF 优先级高于 EINVAL");

        unlink(tmpl);
    }

    /* ================================================================
     * 12. 目录 fd — Linux fadvise64 对目录返回成功 (仅 advisory hint)
     * ================================================================ */
    {
        int dir_fd = open("/tmp", O_RDONLY);
        CHECK(dir_fd >= 0, "open /tmp O_RDONLY 应成功");

        CHECK_RET(call_fadvise(dir_fd, 0, 4096, POSIX_FADV_NORMAL), 0,
                  "目录 fd fadvise 应返回 0");

        close(dir_fd);
    }

    TEST_DONE();
}
