/*
 * test_file_io_edge.c -- file I/O edge cases
 *
 * Verify:
 *   1. ftruncate: negative length returns EINVAL
 *   2. pwrite64: negative offset returns EINVAL
 *   3. lseek SEEK_SET: negative offset returns EINVAL
 *   4. readahead: descriptor access, object type, and count validation
 */

#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <string.h>
#include <errno.h>

#define TEST_FILE "/tmp/file_io_edge_test"

static int create_test_file(void)
{
    int fd = open(TEST_FILE, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
        if (write(fd, "hello world\n", 12) != 12) {
            close(fd);
            return -1;
        }
    }
    return fd;
}

static void test_readahead(void)
{
    int fd;
    int pipefd[2] = {-1, -1};

    /*
     * Linux v6.12 (adc218676eef) ksys_readahead() accepts a readable
     * regular file and leaves the shared file offset unchanged.
     */
    fd = create_test_file();
    CHECK(fd >= 0, "readahead regular file: create file");
    if (fd >= 0) {
        close(fd);
        fd = open(TEST_FILE, O_RDONLY);
        CHECK(fd >= 0, "readahead regular file: open O_RDONLY");
    }
    if (fd >= 0) {
        CHECK_RET(lseek(fd, 3, SEEK_SET), 3,
                  "readahead regular file: set offset 3");
        CHECK_RET(syscall(SYS_readahead, fd, (long)0, (size_t)4096), 0,
                  "readahead(readable regular file) returns 0");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 3,
                  "readahead leaves the file offset unchanged");

        /*
         * v6.12 does not reject a negative offset in ksys_readahead() or
         * generic_fadvise(). The forced readahead starts past EOF and is a
         * no-op, so the syscall still returns 0.
         */
        CHECK_RET(syscall(SYS_readahead, fd, (long)-1, (size_t)4096), 0,
                  "readahead negative offset is a no-op on Linux v6.12");
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0,
                          (size_t)INT64_MAX + 1),
                  EINVAL,
                  "readahead count above signed loff_t returns EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* The read-mode check precedes type and count validation. */
    fd = create_test_file();
    CHECK(fd >= 0, "readahead write-only fd: create file");
    if (fd >= 0) {
        close(fd);
        fd = open(TEST_FILE, O_WRONLY);
        CHECK(fd >= 0, "readahead write-only fd: open O_WRONLY");
    }
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EBADF,
                  "readahead(O_WRONLY fd) returns EBADF");
        CHECK_ERR(syscall(SYS_readahead, fd, (long)-1, (size_t)4096), EBADF,
                  "readahead(O_WRONLY fd, negative offset) returns EBADF");
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0,
                          (size_t)INT64_MAX + 1),
                  EBADF,
                  "readahead(O_WRONLY fd, oversized count) returns EBADF");
        close(fd);
    }
    unlink(TEST_FILE);

    /* O_PATH descriptors have no read mode and are rejected with EBADF. */
    fd = create_test_file();
    CHECK(fd >= 0, "readahead O_PATH fd: create file");
    if (fd >= 0) {
        close(fd);
        fd = open(TEST_FILE, O_PATH | O_CLOEXEC);
        CHECK(fd >= 0, "readahead O_PATH fd: open O_PATH");
    }
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EBADF,
                  "readahead(O_PATH fd) returns EBADF");
        close(fd);
    }
    unlink(TEST_FILE);

    /* A memfd is backed by a regular file and supports readahead. */
    fd = (int)syscall(SYS_memfd_create, "readahead", 0);
    CHECK(fd >= 0, "readahead memfd: create");
    if (fd >= 0) {
        CHECK_RET(syscall(SYS_readahead, fd, (long)0, (size_t)4096), 0,
                  "readahead(memfd) returns 0");
        close(fd);
    }

    /* Pipes and directories are not readahead-capable file types. */
    CHECK_RET(pipe(pipefd), 0, "readahead pipe: create pipe");
    if (pipefd[0] >= 0 && pipefd[1] >= 0) {
        CHECK_ERR(syscall(SYS_readahead, pipefd[0], (long)0, (size_t)4096),
                  EINVAL, "readahead(read end of pipe) returns EINVAL");
        close(pipefd[0]);
        close(pipefd[1]);
    }

    fd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(fd >= 0, "readahead directory: open directory");
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EINVAL,
                  "readahead(directory fd) returns EINVAL");
        close(fd);
    }

    /* A File wrapper is not sufficient: character devices are unsupported. */
    fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0, "readahead character device: open /dev/null");
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EINVAL,
                  "readahead(character device) returns EINVAL");
        close(fd);
    }

    fd = open("/dev/null", O_WRONLY | O_CLOEXEC);
    CHECK(fd >= 0, "readahead write-only character device: open /dev/null");
    if (fd >= 0) {
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EBADF,
                  "readahead(write-only character device) returns EBADF");
        close(fd);
    }

    /* Closed and invalid descriptors take precedence over type checks. */
    fd = create_test_file();
    CHECK(fd >= 0, "readahead closed fd: create file");
    if (fd >= 0) {
        close(fd);
        CHECK_ERR(syscall(SYS_readahead, fd, (long)0, (size_t)4096), EBADF,
                  "readahead(closed fd) returns EBADF");
    }
    CHECK_ERR(syscall(SYS_readahead, -1, (long)0, (size_t)4096), EBADF,
              "readahead(invalid fd) returns EBADF");
    unlink(TEST_FILE);
}

int main(void)
{
    TEST_START("file_io_edge: ftruncate/pwrite64/lseek/readahead edge cases");

    /* ftruncate 正常截断 */
    int fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 正常截断: 打开文件");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 5), 0, "ftruncate(5) 成功");
        CHECK_RET(lseek(fd, 0, SEEK_END), 5, "ftruncate(5) 后文件大小为 5");
        close(fd);
    }
    unlink(TEST_FILE);

    /* ftruncate 负 length 应返回 EINVAL
     * Linux fs/open.c do_truncate(): if (length < 0) return -EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 负值测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(ftruncate(fd, -1), EINVAL, "ftruncate(-1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* ftruncate 截断为 0 */
    fd = create_test_file();
    CHECK(fd >= 0, "ftruncate 截断为 0: 打开文件");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 0), 0, "ftruncate(0) 成功");
        CHECK_RET(lseek(fd, 0, SEEK_END), 0, "ftruncate(0) 后文件大小为 0");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pwrite 正常写入 */
    fd = create_test_file();
    CHECK(fd >= 0, "pwrite 正常写入: 打开文件");
    if (fd >= 0) {
        CHECK_RET(pwrite(fd, "XY", 2, 5), 2, "pwrite(fd, \"XY\", 2, 5) 写入 2 字节");
        char buf[13] = {0};
        CHECK_RET(pread(fd, buf, 12, 0), 12, "pread 读回 12 字节");
        CHECK(memcmp(buf, "helloXYorld\n", 12) == 0, "pwrite 后偏移 5 处为 XY");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pwrite 负 offset 应返回 EINVAL
     * Linux fs/read_write.c ksys_pwrite64(): if (pos < 0) return -EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "pwrite 负 offset 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(pwrite(fd, "X", 1, -1), EINVAL, "pwrite(fd, \"X\", 1, -1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* pread 负 offset 应返回 EINVAL（验证已有修复） */
    fd = create_test_file();
    CHECK(fd >= 0, "pread 负 offset 测试: 打开文件");
    if (fd >= 0) {
        char buf[16];
        CHECK_ERR(pread(fd, buf, 1, -1), EINVAL, "pread(fd, buf, 1, -1) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek SEEK_SET 负 offset 应返回 EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek 负 offset 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(lseek(fd, -1, SEEK_SET), EINVAL, "lseek(fd, -1, SEEK_SET) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek SEEK_SET 正常使用 */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek SEEK_SET 正常: 打开文件");
    if (fd >= 0) {
        CHECK_RET(lseek(fd, 5, SEEK_SET), 5, "lseek(fd, 5, SEEK_SET) == 5");
        CHECK_RET(lseek(fd, 0, SEEK_END), 12, "lseek(fd, 0, SEEK_END) == 12");
        close(fd);
    }
    unlink(TEST_FILE);

    /* lseek 无效 whence 返回 EINVAL */
    fd = create_test_file();
    CHECK(fd >= 0, "lseek 无效 whence 测试: 打开文件");
    if (fd >= 0) {
        CHECK_ERR(lseek(fd, 0, 99), EINVAL, "lseek(fd, 0, 99) 返回 EINVAL");
        close(fd);
    }
    unlink(TEST_FILE);

    test_readahead();

    TEST_DONE();
}
