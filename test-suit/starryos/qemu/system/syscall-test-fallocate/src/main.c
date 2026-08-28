#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdbool.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/vfs.h>

#ifndef EXT4_SUPER_MAGIC
#define EXT4_SUPER_MAGIC 0xEF53
#endif

#ifndef FALLOC_FL_KEEP_SIZE
#define FALLOC_FL_KEEP_SIZE 0x01
#endif
#ifndef FALLOC_FL_PUNCH_HOLE
#define FALLOC_FL_PUNCH_HOLE 0x02
#endif
#ifndef FALLOC_FL_ZERO_RANGE
#define FALLOC_FL_ZERO_RANGE 0x10
#endif
#ifndef FALLOC_FL_COLLAPSE_RANGE
#define FALLOC_FL_COLLAPSE_RANGE 0x08
#endif
#ifndef FALLOC_FL_INSERT_RANGE
#define FALLOC_FL_INSERT_RANGE 0x20
#endif

/*
 * fallocate 对比测试:
 *   Linux 作为正确基线，验证 StarryOS 的 errno 优先级和 fd 类型语义。
 *
 * Linux fallocate(2):
 *   int fallocate(int fd, int mode, off_t offset, off_t len);
 *
 * 关键语义:
 *   1. 无效 fd 在 VFS 前返回 EBADF；有效文件先校验 range，再校验 mode
 *   2. ext4 必须支持普通预分配和 FALLOC_FL_KEEP_SIZE
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

    struct statfs ext4_root;
    CHECK_RET(statfs("/root", &ext4_root), 0, "statfs /root 应成功");
    CHECK((unsigned long)ext4_root.f_type == EXT4_SUPER_MAGIC,
          "fallocate 持久化测例必须运行在 ext4 上");

    /* ================================================================
     * 1. 正常分配 — 创建文件并 fallocate 扩展大小
     * ================================================================ */
    {
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "fstat 初始状态");
        CHECK(st.st_size == 0, "初始文件大小为 0");

        CHECK_RET(call_fallocate(fd, 0, 0, 4096), 0,
                  "fallocate(fd, 0, 0, 4096) 应返回 0");

        CHECK_RET(fstat(fd, &st), 0, "fstat 分配后");
        CHECK(st.st_size == 4096, "分配后文件大小应为 4096");
        CHECK(st.st_blocks >= 8, "普通 fallocate 应预留至少一个 4 KiB 块");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 2. fallocate 追加扩展 — offset 超出当前文件末尾
     * ================================================================ */
    {
        char tmpl[] = "/root/test-fallocate-XXXXXX";
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
     *     ext4: 返回 0，预留磁盘块但不改变文件大小
     * ================================================================ */
    {
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(call_fallocate(fd, FALLOC_FL_KEEP_SIZE, 0, 4096), 0,
                  "ext4 FALLOC_FL_KEEP_SIZE 应返回 0");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "KEEP_SIZE 后 fstat 成功");
        CHECK(st.st_size == 0, "KEEP_SIZE 不扩展文件大小");
        CHECK(st.st_blocks >= 8, "KEEP_SIZE 应预留至少一个 4 KiB 块");

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
     * 18. ext4 FALLOC_FL_ZERO_RANGE
     *     读取为零但保留物理预留，不能退化成普通写零或 punch。
     * ================================================================ */
    {
        enum { BLOCK = 4096, BLOCKS = 5 };
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        unsigned char original[BLOCKS * BLOCK];
        unsigned char after[BLOCKS * BLOCK];
        memset(original, 0x5a, sizeof(original));
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(write(fd, original, sizeof(original)), sizeof(original),
                  "写入 ZERO_RANGE extent fixture");
        CHECK_RET(fsync(fd), 0, "ZERO_RANGE fixture fsync");
        struct stat before, st;
        CHECK_RET(fstat(fd, &before), 0, "ZERO_RANGE 前 fstat");
        CHECK_RET(call_fallocate(fd, FALLOC_FL_ZERO_RANGE,
                                 BLOCK + 123, 2 * BLOCK),
                  0, "ext4 ZERO_RANGE 必须成功");
        CHECK_RET(fstat(fd, &st), 0, "ZERO_RANGE 后 fstat");
        CHECK(st.st_size == (off_t)sizeof(original),
              "ZERO_RANGE KEEP_SIZE 缺省时范围未越 EOF，size 不变");
        CHECK(st.st_blocks == before.st_blocks,
              "ZERO_RANGE 应保留物理块计数");
        CHECK_RET(pread(fd, after, sizeof(after), 0), sizeof(after),
                  "ZERO_RANGE 后读回完整内容");
        CHECK(memcmp(after, original, BLOCK + 123) == 0,
              "ZERO_RANGE 左边界外数据保持不变");
        bool zeroed = true;
        for (size_t i = BLOCK + 123; i < 3 * BLOCK + 123; i++)
            zeroed &= after[i] == 0;
        CHECK(zeroed, "ZERO_RANGE 把非对齐范围读为零");
        CHECK(memcmp(after + 3 * BLOCK + 123,
                     original + 3 * BLOCK + 123,
                     sizeof(after) - (3 * BLOCK + 123)) == 0,
              "ZERO_RANGE 右边界外数据保持不变");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 19. ext4 FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE
     *     与 ZERO_RANGE 都读零，但 PUNCH 必须释放完整中间块。
     * ================================================================ */
    {
        enum { BLOCK = 4096, BLOCKS = 5 };
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        unsigned char original[BLOCKS * BLOCK];
        unsigned char after[BLOCKS * BLOCK];
        memset(original, 0xa5, sizeof(original));
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_RET(write(fd, original, sizeof(original)), sizeof(original),
                  "写入 PUNCH_HOLE extent fixture");
        CHECK_RET(fsync(fd), 0, "PUNCH_HOLE fixture fsync");
        struct stat before, st;
        CHECK_RET(fstat(fd, &before), 0, "PUNCH_HOLE 前 fstat");
        CHECK_RET(call_fallocate(fd,
                                 FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
                                 BLOCK + 123, 2 * BLOCK),
                  0, "ext4 PUNCH_HOLE|KEEP_SIZE 必须成功");
        CHECK_RET(fstat(fd, &st), 0, "PUNCH_HOLE 后 fstat 成功");
        CHECK(st.st_size == (off_t)sizeof(original),
              "PUNCH_HOLE|KEEP_SIZE 不改变文件大小");
        CHECK(st.st_blocks + 8 <= before.st_blocks,
              "PUNCH_HOLE 必须至少释放一个 4 KiB 完整块");
        CHECK_RET(pread(fd, after, sizeof(after), 0), sizeof(after),
                  "PUNCH_HOLE 后读回完整内容");
        CHECK(memcmp(after, original, BLOCK + 123) == 0,
              "PUNCH_HOLE 左边界外数据保持不变");
        bool zeroed = true;
        for (size_t i = BLOCK + 123; i < 3 * BLOCK + 123; i++)
            zeroed &= after[i] == 0;
        CHECK(zeroed, "PUNCH_HOLE 把非对齐范围读为零");
        CHECK(memcmp(after + 3 * BLOCK + 123,
                     original + 3 * BLOCK + 123,
                     sizeof(after) - (3 * BLOCK + 123)) == 0,
              "PUNCH_HOLE 右边界外数据保持不变");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 20. 超大 offset (2^60) — Linux 返回 EFBIG
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

    /* ================================================================
     * 21. 有效 fd + 无效 mode + len=-1 — Linux 先返回 EINVAL
     * ================================================================ */
    {
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");

        CHECK_ERR(call_fallocate(fd, 0xdead, 0, -1), EINVAL,
                  "有效 fd 的 len 校验优先于无效 mode");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 22. ext4 FALLOC_FL_COLLAPSE_RANGE
     * ================================================================ */
    {
        enum { BLOCK = 4096, BLOCKS = 4 };
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        unsigned char original[BLOCKS * BLOCK];
        unsigned char after[(BLOCKS - 1) * BLOCK];
        for (size_t block = 0; block < BLOCKS; block++)
            memset(original + block * BLOCK, (int)block + 1, BLOCK);
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, original, sizeof(original)), sizeof(original),
                  "写入 COLLAPSE_RANGE fixture");
        CHECK_RET(fsync(fd), 0, "COLLAPSE_RANGE fixture fsync");

        CHECK_RET(call_fallocate(fd, FALLOC_FL_COLLAPSE_RANGE, BLOCK, BLOCK),
                  0, "ext4 COLLAPSE_RANGE 必须成功");
        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "COLLAPSE_RANGE 后 fstat");
        CHECK(st.st_size == (off_t)sizeof(after),
              "COLLAPSE_RANGE 缩短一个块");
        CHECK_RET(pread(fd, after, sizeof(after), 0), sizeof(after),
                  "COLLAPSE_RANGE 后读回内容");
        CHECK(memcmp(after, original, BLOCK) == 0,
              "COLLAPSE_RANGE 保留左侧数据");
        CHECK(memcmp(after + BLOCK, original + 2 * BLOCK, 2 * BLOCK) == 0,
              "COLLAPSE_RANGE 左移后续数据");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 23. ext4 FALLOC_FL_INSERT_RANGE
     * ================================================================ */
    {
        enum { BLOCK = 4096, BLOCKS = 3 };
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        unsigned char original[BLOCKS * BLOCK];
        unsigned char after[(BLOCKS + 1) * BLOCK];
        for (size_t block = 0; block < BLOCKS; block++)
            memset(original + block * BLOCK, (int)block + 1, BLOCK);
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, original, sizeof(original)), sizeof(original),
                  "写入 INSERT_RANGE fixture");
        CHECK_RET(fsync(fd), 0, "INSERT_RANGE fixture fsync");

        CHECK_RET(call_fallocate(fd, FALLOC_FL_INSERT_RANGE, BLOCK, BLOCK),
                  0, "ext4 INSERT_RANGE 必须成功");
        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "INSERT_RANGE 后 fstat");
        CHECK(st.st_size == (off_t)sizeof(after), "INSERT_RANGE 扩大一个块");
        CHECK_RET(pread(fd, after, sizeof(after), 0), sizeof(after),
                  "INSERT_RANGE 后读回内容");
        CHECK(memcmp(after, original, BLOCK) == 0,
              "INSERT_RANGE 保留左侧数据");
        bool hole = true;
        for (size_t i = BLOCK; i < 2 * BLOCK; i++)
            hole &= after[i] == 0;
        CHECK(hole, "INSERT_RANGE 插入一个读零 hole");
        CHECK(memcmp(after + 2 * BLOCK, original + BLOCK, 2 * BLOCK) == 0,
              "INSERT_RANGE 右移后续数据");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 24. COLLAPSE_RANGE / INSERT_RANGE 边界与 mode 互斥
     * ================================================================ */
    {
        enum { BLOCK = 4096, BLOCKS = 4 };
        char tmpl[] = "/root/test-fallocate-XXXXXX";
        unsigned char original[BLOCKS * BLOCK];
        unsigned char after[BLOCKS * BLOCK];
        memset(original, 0x6d, sizeof(original));
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 应成功");
        CHECK_RET(write(fd, original, sizeof(original)), sizeof(original),
                  "写入 range 边界 fixture");

        CHECK_ERR(call_fallocate(fd, FALLOC_FL_COLLAPSE_RANGE, 1, BLOCK),
                  EINVAL, "COLLAPSE_RANGE offset 必须按文件系统块对齐");
        CHECK_ERR(call_fallocate(fd, FALLOC_FL_COLLAPSE_RANGE, BLOCK, BLOCK - 1),
                  EINVAL, "COLLAPSE_RANGE len 必须按文件系统块对齐");
        CHECK_ERR(call_fallocate(fd, FALLOC_FL_COLLAPSE_RANGE,
                                 (BLOCKS - 1) * BLOCK, BLOCK),
                  EINVAL, "COLLAPSE_RANGE 不得包含 EOF");
        CHECK_ERR(call_fallocate(fd,
                                 FALLOC_FL_COLLAPSE_RANGE | FALLOC_FL_KEEP_SIZE,
                                 BLOCK, BLOCK),
                  EOPNOTSUPP, "COLLAPSE_RANGE 与 KEEP_SIZE 互斥");

        CHECK_ERR(call_fallocate(fd, FALLOC_FL_INSERT_RANGE, 1, BLOCK),
                  EINVAL, "INSERT_RANGE offset 必须按文件系统块对齐");
        CHECK_ERR(call_fallocate(fd, FALLOC_FL_INSERT_RANGE, BLOCK, BLOCK - 1),
                  EINVAL, "INSERT_RANGE len 必须按文件系统块对齐");
        CHECK_ERR(call_fallocate(fd, FALLOC_FL_INSERT_RANGE,
                                 BLOCKS * BLOCK, BLOCK),
                  EINVAL, "INSERT_RANGE offset 必须位于 EOF 之前");
        CHECK_ERR(call_fallocate(fd,
                                 FALLOC_FL_INSERT_RANGE | FALLOC_FL_KEEP_SIZE,
                                 BLOCK, BLOCK),
                  EOPNOTSUPP, "INSERT_RANGE 与 KEEP_SIZE 互斥");

        struct stat st;
        CHECK_RET(fstat(fd, &st), 0, "失败 range 操作后 fstat");
        CHECK(st.st_size == (off_t)sizeof(original),
              "失败 range 操作不得改变文件大小");
        CHECK_RET(pread(fd, after, sizeof(after), 0), sizeof(after),
                  "失败 range 操作后读回内容");
        CHECK(memcmp(after, original, sizeof(original)) == 0,
              "失败 range 操作不得改变文件内容");

        close(fd);
        unlink(tmpl);
    }

    /* ================================================================
     * 25. 已解析 fd 的 range/mode 校验先于写权限与节点类型
     * ================================================================ */
    {
        char tmpl[] = "/tmp/test-fallocate-XXXXXX";
        int fd = mkstemp(tmpl);
        CHECK(fd >= 0, "mkstemp 创建优先级 fixture");
        close(fd);
        int rd_fd = open(tmpl, O_RDONLY);
        CHECK(rd_fd >= 0, "open O_RDONLY 优先级 fixture");
        CHECK_ERR(call_fallocate(rd_fd, 0, 0, -1), EINVAL,
                  "只读 fd 仍先校验 len");
        CHECK_ERR(call_fallocate(rd_fd, 0xdead, 0, 4096), EOPNOTSUPP,
                  "只读 fd 仍先校验 mode");
        close(rd_fd);
        unlink(tmpl);

        int pipe_fds[2];
        CHECK_RET(pipe(pipe_fds), 0, "创建优先级 pipe");
        CHECK_ERR(call_fallocate(pipe_fds[1], 0, 0, -1), EINVAL,
                  "pipe fd 仍先校验 len");
        CHECK_ERR(call_fallocate(pipe_fds[1], 0xdead, 0, 4096), EOPNOTSUPP,
                  "pipe fd 仍先校验 mode");
        close(pipe_fds[0]);
        close(pipe_fds[1]);

        int dir_fd = open("/tmp", O_RDONLY);
        CHECK(dir_fd >= 0, "open /tmp 优先级 fixture");
        CHECK_ERR(call_fallocate(dir_fd, 0, 0, -1), EINVAL,
                  "目录 fd 仍先校验 len");
        CHECK_ERR(call_fallocate(dir_fd, 0xdead, 0, 4096), EOPNOTSUPP,
                  "目录 fd 仍先校验 mode");
        close(dir_fd);
    }

    TEST_DONE();
}
