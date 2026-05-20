#include "test_framework.h"
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <string.h>
#include <errno.h>

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

#define FILE1 "/tmp/test_dup2_file1"
#define FILE2 "/tmp/test_dup2_file2"
#define CONTENT1 "hello_dup2_file1"
#define CONTENT2 "world_dup2_file2"

/* ====== 统一 helper 函数（static） ====== */

static void safe_close(int *fd)
{
    if (fd && *fd >= 0) {
        close(*fd);
        *fd = -1;
    }
}

static int dupfd_at_least(int fd, int minfd)
{
    return fcntl(fd, F_DUPFD, minfd);
}

static int create_temp_file_with_data(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (data) {
        ssize_t n = write(fd, data, strlen(data));
        if (n < 0) {
            close(fd);
            return -1;
        }
    }
    close(fd);
    return 0;
}

static int read_all_and_compare(int fd, const char *expect, size_t n)
{
    char buf[256];
    if (n > sizeof(buf)) return -1;
    ssize_t r = read(fd, buf, n);
    if (r != (ssize_t)n) return -1;
    return memcmp(buf, expect, n) == 0 ? 0 : -1;
}

/* ====== Part 函数 ====== */

/* Part 1: dup2 基本功能 */
static int part_01_dup2_basic(void)
{
    int fd, newfd, ret;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    fd = open(FILE1, O_RDONLY);
    CHECK_TRUE(fd >= 0, "Part 1: open file1");
    if (fd < 0) return 1;

    newfd = dupfd_at_least(fd, 50);
    CHECK_TRUE(newfd >= 0 && newfd != fd, "Part 1: newfd distinct from fd");
    if (newfd < 0) { close(fd); unlink(FILE1); return 1; }

    ret = dup2(fd, newfd);
    CHECK_RET(ret, newfd, "Part 1: dup2 returns newfd");

    CHECK_TRUE(read_all_and_compare(newfd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 1: newfd reads shared data");

    lseek(newfd, 0, SEEK_SET);
    CHECK_TRUE(read_all_and_compare(fd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 1: fd reads shared data (shared offset)");

    safe_close(&fd);
    safe_close(&newfd);
    unlink(FILE1);
    return 0;
}

/* Part 2: dup2(oldfd, oldfd) 幂等 */
static int part_02_dup2_idempotent(void)
{
    int fd, ret;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    fd = open(FILE1, O_RDWR);
    CHECK_TRUE(fd >= 0, "Part 2: open file1");
    if (fd < 0) return 1;

    ret = dup2(fd, fd);
    CHECK_RET(ret, fd, "Part 2: dup2(fd,fd) returns fd");

    CHECK_TRUE(read_all_and_compare(fd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 2: fd still readable after dup2(fd,fd)");

    ret = write(fd, "test", 4);
    CHECK_TRUE(ret > 0, "Part 2: fd still writable after dup2(fd,fd)");

    safe_close(&fd);
    unlink(FILE1);
    return 0;
}

/* Part 3: dup2(newfd 已打开) 原子 close+dup */
static int part_03_dup2_close_dup(void)
{
    int oldfd, newfd, ret;

    unlink(FILE1);
    unlink(FILE2);
    create_temp_file_with_data(FILE1, CONTENT1);
    create_temp_file_with_data(FILE2, CONTENT2);

    oldfd = open(FILE1, O_RDWR);
    newfd = open(FILE2, O_RDWR);
    CHECK_TRUE(oldfd >= 0 && newfd >= 0 && oldfd != newfd, "Part 3: precondition");
    if (oldfd < 0 || newfd < 0) {
        safe_close(&oldfd);
        safe_close(&newfd);
        unlink(FILE1);
        unlink(FILE2);
        return 1;
    }

    ret = dup2(oldfd, newfd);
    CHECK_RET(ret, newfd, "Part 3: dup2 returns newfd");

    lseek(newfd, 0, SEEK_SET);
    CHECK_TRUE(read_all_and_compare(newfd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 3: newfd now points to oldfd's file");

    lseek(newfd, 0, SEEK_SET);
    write(newfd, "xxx", 3);
    lseek(oldfd, 0, SEEK_SET);
    CHECK_TRUE(read_all_and_compare(oldfd, "xxx", 3) == 0,
               "Part 3: shared open file description");

    safe_close(&oldfd);
    safe_close(&newfd);
    unlink(FILE1);
    unlink(FILE2);
    return 0;
}

/* Part 4: dup2 后关闭原 fd */
static int part_04_dup2_close_original(void)
{
    int fd, newfd, ret;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    fd = open(FILE1, O_RDWR);
    CHECK_TRUE(fd >= 0, "Part 4: open file1");
    if (fd < 0) return 1;

    newfd = dupfd_at_least(fd, 50);
    CHECK_TRUE(newfd >= 0 && newfd != fd, "Part 4: newfd distinct from fd");
    if (newfd < 0) { close(fd); unlink(FILE1); return 1; }

    ret = dup2(fd, newfd);
    CHECK_RET(ret, newfd, "Part 4: dup2 returns newfd");

    safe_close(&fd);

    CHECK_TRUE(read_all_and_compare(newfd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 4: newfd readable after fd closed");

    ret = write(newfd, "test", 4);
    CHECK_TRUE(ret > 0, "Part 4: newfd writable after fd closed");

    safe_close(&newfd);
    unlink(FILE1);
    return 0;
}

/* Part 5: dup2 共享 offset 验证 */
static int part_05_dup2_shared_offset(void)
{
    int fd, newfd, ret;
    off_t pos;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    fd = open(FILE1, O_RDWR);
    CHECK_TRUE(fd >= 0, "Part 5: open file1");
    if (fd < 0) return 1;

    newfd = dupfd_at_least(fd, 50);
    CHECK_TRUE(newfd >= 0 && newfd != fd, "Part 5: newfd distinct from fd");
    if (newfd < 0) { close(fd); unlink(FILE1); return 1; }

    ret = dup2(fd, newfd);
    CHECK_RET(ret, newfd, "Part 5: dup2 returns newfd");
    CHECK_TRUE(newfd >= 0, "Part 5: dup2 success");

    write(fd, "abc", 3);
    pos = lseek(newfd, 0, SEEK_CUR);
    CHECK_TRUE(pos == 3, "Part 5: shared offset after write on fd");

    lseek(newfd, 0, SEEK_SET);
    pos = lseek(fd, 0, SEEK_CUR);
    CHECK_TRUE(pos == 0, "Part 5: shared offset after lseek on newfd");

    safe_close(&fd);
    safe_close(&newfd);
    unlink(FILE1);
    return 0;
}

/* Part 6: 负向测试 */
static int part_06_dup2_negative(void)
{
    int fd, ret;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    fd = open(FILE1, O_RDWR);
    CHECK_TRUE(fd >= 0, "Part 6: open file1");
    if (fd < 0) return 1;

    int valid_fd = fd;
    int err;

    errno = 0;
    ret = dup2(-1, valid_fd);
    err = errno;
    CHECK_RET(ret, -1, "Part 6: dup2(-1, valid_fd) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 6: errno=EBADF");

    errno = 0;
    ret = dup2(valid_fd, -1);
    err = errno;
    CHECK_RET(ret, -1, "Part 6: dup2(valid_fd, -1) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 6: errno=EBADF");

    errno = 0;
    ret = dup2(9999, valid_fd);
    err = errno;
    CHECK_RET(ret, -1, "Part 6: dup2(9999, valid_fd) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 6: errno=EBADF");

    safe_close(&fd);
    unlink(FILE1);
    return 0;
}

/* Part 7: oldfd 无效时 newfd 不被破坏 (I9) */
static int part_07_dup2_oldfd_invalid_preserves_newfd(void)
{
    int newfd, ret;

    unlink(FILE1);
    create_temp_file_with_data(FILE1, CONTENT1);

    newfd = open(FILE1, O_RDONLY);
    CHECK_TRUE(newfd >= 0, "Part 7: open file1 as newfd");
    if (newfd < 0) return 1;

    CHECK_TRUE(read_all_and_compare(newfd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 7: newfd initially readable");

    lseek(newfd, 0, SEEK_SET);

    errno = 0;
    ret = dup2(-1, newfd);
    int err = errno;
    CHECK_RET(ret, -1, "Part 7: dup2(-1, newfd) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 7: errno=EBADF");

    CHECK_TRUE(read_all_and_compare(newfd, CONTENT1, strlen(CONTENT1)) == 0,
               "Part 7: newfd still readable after dup2 failure (I9)");

    safe_close(&newfd);
    unlink(FILE1);
    return 0;
}

/* ====== main ====== */

int main(void)
{
    TEST_START("dup2: semantic validation");

    part_01_dup2_basic();
    part_02_dup2_idempotent();
    part_03_dup2_close_dup();
    part_04_dup2_close_original();
    part_05_dup2_shared_offset();
    part_06_dup2_negative();
    part_07_dup2_oldfd_invalid_preserves_newfd();

    TEST_DONE();
}
