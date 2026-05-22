#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>

/*
 * fallocate 对比测试:
 *   Linux 作为正确基线，验证 StarryOS 的 errno 优先级和 fd 类型语义。
 *
 * Linux fallocate(2):
 *   int fallocate(int fd, int mode, off_t offset, off_t len);
 *
 * 关键语义:
 *   1. EBADF 优先级 > ESPIPE > EOPNOTSUPP > EINVAL
 *   2. mode != 0 返回 EOPNOTSUPP (Linux 文件系统默认不支持)
 *   3. offset < 0 或 len <= 0 返回 EINVAL
 */

static int call_fallocate(int fd, int mode, off_t offset, off_t len)
{
    errno = 0;
    return fallocate(fd, mode, offset, len);
}

/*
 * 检查 ret == 0（成功），或 ret == -1 且 errno 在合法集合内。
 * 用于 mode flag / 超大 offset 测试。
 */
static void check_ret_or_err(long ret, int n_ok, const int *ok_errnos,
                             const char *file, int line, const char *msg)
{
    if (ret == 0) {
        printf("  PASS | %s:%d | %s (ret=0)\n", file, line, msg);
        __pass++;
    } else if (ret == -1) {
        for (int i = 0; i < n_ok; i++) {
            if (errno == ok_errnos[i]) {
                printf("  PASS | %s:%d | %s (errno=%d, acceptable)\n",
                       file, line, msg, errno);
                __pass++;
                return;
            }
        }
        printf("  FAIL | %s:%d | %s | unexpected errno=%d (%s)\n",
               file, line, msg, errno, strerror(errno));
        __fail++;
    } else {
        printf("  FAIL | %s:%d | %s | unexpected ret=%ld errno=%d (%s)\n",
               file, line, msg, ret, errno, strerror(errno));
        __fail++;
    }
}

int main(void)
{
    TEST_START("fallocate");

    /* ================================================================
     * 1. 正常分配 — 创建文件并 fallocate 扩展大小
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "fstat 初始状态");
        CHECK(st.st_size == 0, "初始文件大小为 0");

        CHECK_RET(call_fallocate(fd, 0, 0, 4096), 0,
                  "fallocate(fd, 0, 0, 4096) 应返回 0");

        CHECK_RET(fstat(fd, &st), 0, "fstat 分配后");
        CHECK(st.st_size == 4096, "分配后文件大小应为 4096");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 2. fallocate 追加扩展 — offset 超出当前文件末尾
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(call_fallocate(fd, 0, 0, 4096), 0,
                  "第一段: fallocate(fd, 0, 0, 4096)");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "fstat 第一段后");
        CHECK(st.st_size == 4096, "第一段后文件大小 4096");

        CHECK_RET(call_fallocate(fd, 0, 8192, 4096), 0,
                  "第二段: fallocate(fd, 0, 8192, 4096)");

        CHECK_RET(fstat(fd, &st), 0, "fstat 第二段后");
        CHECK(st.st_size == 12288, "两段分配后文件大小 12288");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 3. len=0 — Linux 返回 EINVAL (POSIX: len <= 0 为无效参数)
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(write(fd, "hello", 5), 5, "写入 5 字节");

        CHECK_ERR(call_fallocate(fd, 0, 0, 0), EINVAL,
                  "len=0 应返回 EINVAL");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "fstat len=0 后");
        CHECK(st.st_size == 5, "len=0 不应改变文件大小");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 4. offset 为负数 — Linux 返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fallocate(fd, 0, -1, 4096), EINVAL,
                  "offset=-1 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 5. len 为负数 — Linux 返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fallocate(fd, 0, 0, -1), EINVAL,
                  "len=-1 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 6. offset 为负数且 len 为负数 — Linux 返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fallocate(fd, 0, -1, -1), EINVAL,
                  "offset=-1, len=-1 应返回 EINVAL");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 7. 无效 fd (-1) — 应返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_fallocate(-1, 0, 0, 4096), EBADF,
                  "fd=-1 应返回 EBADF");
    }

    /* ================================================================
     * 8. 已关闭的 fd — 应返回 EBADF
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_fallocate(fd, 0, 0, 4096), EBADF,
                  "已关闭的 fd 应返回 EBADF");

        unlink(tmpl);
    }

    /* ================================================================
     * 9. fd=-1 + mode=0xdead — EBADF 优先级高于 EOPNOTSUPP
     *    Linux fallocate(-1, 0xdead, 0, 4096) 返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_fallocate(-1, 0xdead, 0, 4096), EBADF,
                  "fd=-1 且 mode=0xdead, EBADF 优先级高于 EOPNOTSUPP");
    }

    /* ================================================================
     * 10. fd=-1 + len=-1 — EBADF 优先级高于 EINVAL
     *     Linux fallocate(-1, 0, 0, -1) 返回 EBADF
     * ================================================================ */
    {
        CHECK_ERR(call_fallocate(-1, 0, 0, -1), EBADF,
                  "fd=-1 且 len=-1, EBADF 优先级高于 EINVAL");
    }

    /* ================================================================
     * 11. 已关闭 fd + mode=0xdead — EBADF 优先级高于 EOPNOTSUPP
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_fallocate(fd, 0xdead, 0, 4096), EBADF,
                  "已关闭 fd 且 mode=0xdead, EBADF 优先级高于 EOPNOTSUPP");

        unlink(tmpl);
    }

    /* ================================================================
     * 12. 已关闭 fd + len=-1 — EBADF 优先级高于 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        close(fd);

        CHECK_ERR(call_fallocate(fd, 0, 0, -1), EBADF,
                  "已关闭 fd 且 len=-1, EBADF 优先级高于 EINVAL");

        unlink(tmpl);
    }

    /* ================================================================
     * 13. 只读 fd — Linux 返回 EBADF
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建临时文件");
        close(fd);

        int rd_fd = open(tmpl, O_RDONLY);
        CHECK(rd_fd >= 0, "open O_RDONLY 应成功");

        CHECK_ERR(call_fallocate(rd_fd, 0, 0, 4096), EBADF,
                  "只读 fd 上 fallocate 应返回 EBADF");

        close(rd_fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 14. pipe fd — Linux 返回 ESPIPE
     * ================================================================ */
    {
        int pipe_fds[2];
        CHECK_RET(pipe(pipe_fds), 0, "创建 pipe");

        CHECK_ERR(call_fallocate(pipe_fds[1], 0, 0, 4096), ESPIPE,
                  "pipe 写端 fallocate 应返回 ESPIPE");

        close(pipe_fds[0]);
        close(pipe_fds[1]);
    }

    /* ================================================================
     * 15. 目录 fd — Linux 返回 EBADF (目录不可写入)
     * ================================================================ */
    {
        int dir_fd = open("/tmp", O_RDONLY);
        CHECK(dir_fd >= 0, "open /tmp O_RDONLY 应成功");

        CHECK_ERR(call_fallocate(dir_fd, 0, 0, 4096), EBADF,
                  "目录 fd 上 fallocate 应返回 EBADF");

        close(dir_fd);
    }

    /* ================================================================
     * 16. mode = FALLOC_FL_KEEP_SIZE (0x01)
     *     Linux: 返回 0 (文件系统支持) 或 EOPNOTSUPP (不支持)
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        errno = 0;
        long ret = (long)call_fallocate(fd, FALLOC_FL_KEEP_SIZE, 0, 4096);
        {
            const int ok[] = { EOPNOTSUPP };
            check_ret_or_err(ret, 1, ok, __FILE__, __LINE__,
                             "FALLOC_FL_KEEP_SIZE: 期望 0 或 EOPNOTSUPP");
        }

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 17. mode = 0xdead (随机无效 flag)
     *     Linux: 返回 EOPNOTSUPP
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        errno = 0;
        long ret = (long)call_fallocate(fd, 0xdead, 0, 4096);
        {
            const int ok[] = { EOPNOTSUPP };
            check_ret_or_err(ret, 1, ok, __FILE__, __LINE__,
                             "mode=0xdead: 期望 EOPNOTSUPP");
        }

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 18. mode = FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE
     *     Linux: 返回 0 (tmpfs/ext4 支持) 或 EOPNOTSUPP (不支持)
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(write(fd, "1234567890", 10), 10, "写入 10 字节");

        errno = 0;
        long ret = (long)call_fallocate(fd,
                                        FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
                                        0, 5);
        {
            const int ok[] = { EOPNOTSUPP };
            check_ret_or_err(ret, 1, ok, __FILE__, __LINE__,
                             "FALLOC_FL_PUNCH_HOLE|KEEP_SIZE: 期望 0 或 EOPNOTSUPP");
        }

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 19. 超大 offset (2^60) — Linux 返回 EFBIG
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        off_t big = (off_t)((unsigned long long)1 << 60);
        errno = 0;
        long ret = (long)call_fallocate(fd, 0, big, 4096);
        /* 超大 offset 必须失败，不应返回 0 */
        if (ret == -1 && (errno == EFBIG || errno == ENOSPC ||
                          errno == EOVERFLOW)) {
            printf("  PASS | %s:%d | 超大 offset 返回 errno=%d (expected)\n",
                   __FILE__, __LINE__, errno);
            __pass++;
        } else if (ret == 0) {
            printf("  FAIL | %s:%d | 超大 offset 不应返回 0 "
                   "(StarryOS BUG: 未检查 offset 上限)\n",
                   __FILE__, __LINE__);
            __fail++;
        } else {
            printf("  FAIL | %s:%d | 超大 offset 意外结果 | "
                   "ret=%ld errno=%d (%s)\n",
                   __FILE__, __LINE__, ret, errno, strerror(errno));
            __fail++;
        }

        close(fd);
        unlink(tmpl);
    }

    TEST_DONE();
}
