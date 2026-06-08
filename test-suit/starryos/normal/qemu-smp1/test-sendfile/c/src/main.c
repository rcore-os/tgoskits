#define _GNU_SOURCE
#define _FILE_OFFSET_BITS 64

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/sendfile.h>
#include <unistd.h>

static int g_pass = 0;
static int g_fail = 0;

#define CHECK(expr)                                      \
    do {                                                 \
        int ok = !!(expr);                               \
        if (!ok) {                                       \
            g_fail++;                                    \
            printf("FAIL: %s:%d: %s\n",                  \
                   __FILE__, __LINE__, #expr);           \
            fflush(stdout);                              \
        } else {                                         \
            g_pass++;                                    \
        }                                                \
    } while (0)

static void write_all(int fd, const char *s) {
    size_t len = strlen(s);
    ssize_t n = write(fd, s, len);
    CHECK(n == (ssize_t)len);
}

static void reset_file(const char *path, const char *content) {
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0);

    if (fd >= 0) {
        write_all(fd, content);
        CHECK(close(fd) == 0);
    }
}

static void reset_file_repeated(const char *path, char byte, size_t len) {
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0);

    if (fd < 0) {
        return;
    }

    char buf[256];
    memset(buf, byte, sizeof(buf));

    while (len > 0) {
        size_t chunk = len < sizeof(buf) ? len : sizeof(buf);
        ssize_t n = write(fd, buf, chunk);
        CHECK(n == (ssize_t)chunk);
        if (n != (ssize_t)chunk) {
            break;
        }
        len -= chunk;
    }

    CHECK(close(fd) == 0);
}

static void read_file_content(const char *path, char *buf, size_t size) {
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0);

    if (fd < 0) {
        if (size > 0) {
            buf[0] = '\0';
        }
        return;
    }

    ssize_t n = read(fd, buf, size - 1);
    CHECK(n >= 0);

    if (n >= 0) {
        buf[n] = '\0';
    } else if (size > 0) {
        buf[0] = '\0';
    }

    CHECK(close(fd) == 0);
}

/*
 * 测试 1：
 * offset != NULL 时：
 * 1. sendfile 应从 *offset 指定的位置读取；
 * 2. 成功后更新 *offset；
 * 3. 不改变 in_fd 自身的当前文件偏移。
 */
static void test_offset_ptr(void) {
    reset_file("sf_in_offset.txt", "abcdef");
    reset_file("sf_out_offset.txt", "");

    int in_fd = open("sf_in_offset.txt", O_RDONLY);
    int out_fd = open("sf_out_offset.txt", O_WRONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 2;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 3);

    printf("offset ptr test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == 3);
    CHECK(off == 5);

    /*
     * offset != NULL 时，in_fd 当前文件偏移不应该被修改。
     * in_fd 刚打开时偏移是 0，所以这里仍然应该是 0。
     */
    off_t in_cur = lseek(in_fd, 0, SEEK_CUR);
    CHECK(in_cur == 0);

    /*
     * out_fd 是输出端，写入 3 字节后，输出端偏移应该变为 3。
     */
    off_t out_cur = lseek(out_fd, 0, SEEK_CUR);
    CHECK(out_cur == 3);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);

    char buf[64];
    read_file_content("sf_out_offset.txt", buf, sizeof(buf));

    /*
     * "abcdef" 从 offset=2 开始读 3 字节，应该得到 "cde"。
     */
    CHECK(strcmp(buf, "cde") == 0);
}

/*
 * 测试 2：
 * offset == NULL 时：
 * 1. sendfile 应从 in_fd 当前文件偏移开始读取；
 * 2. 成功后更新 in_fd 当前文件偏移。
 */
static void test_null_offset(void) {
    reset_file("sf_in_null.txt", "abcdef");
    reset_file("sf_out_null.txt", "");

    int in_fd = open("sf_in_null.txt", O_RDONLY);
    int out_fd = open("sf_out_null.txt", O_WRONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    /*
     * 把输入文件当前偏移设为 1。
     */
    CHECK(lseek(in_fd, 1, SEEK_SET) == 1);

    errno = 0;
    ssize_t ret = sendfile(out_fd, in_fd, NULL, 3);

    printf("null offset test: ret=%ld errno=%d\n",
           (long)ret, errno);

    CHECK(ret == 3);

    /*
     * offset == NULL，所以 in_fd 当前偏移应该从 1 变成 4。
     */
    off_t in_cur = lseek(in_fd, 0, SEEK_CUR);
    CHECK(in_cur == 4);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);

    char buf[64];
    read_file_content("sf_out_null.txt", buf, sizeof(buf));

    /*
     * "abcdef" 从 offset=1 开始读 3 字节，应该得到 "bcd"。
     */
    CHECK(strcmp(buf, "bcd") == 0);
}

/*
 * 测试 3：
 * 从 EOF 位置开始复制，应该返回 0。
 */
static void test_eof_returns_zero(void) {
    reset_file("sf_in_eof.txt", "abc");
    reset_file("sf_out_eof.txt", "");

    int in_fd = open("sf_in_eof.txt", O_RDONLY);
    int out_fd = open("sf_out_eof.txt", O_WRONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 3;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 10);

    printf("eof test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == 0);
    CHECK(off == 3);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);
}

/*
 * 测试 4：
 * count == 0 时，不复制数据，应该返回 0。
 */
static void test_count_zero(void) {
    reset_file("sf_in_zero.txt", "abcdef");
    reset_file("sf_out_zero.txt", "");

    int in_fd = open("sf_in_zero.txt", O_RDONLY);
    int out_fd = open("sf_out_zero.txt", O_WRONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 1;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 0);

    printf("count zero test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == 0);
    CHECK(off == 1);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);

    char buf[64];
    read_file_content("sf_out_zero.txt", buf, sizeof(buf));

    CHECK(strcmp(buf, "") == 0);
}

/*
 * 测试 5：
 * out_fd 如果带 O_APPEND，Linux sendfile 应该失败，
 * 返回 -1，并设置 errno = EINVAL。
 */
static void test_out_fd_append_einval(void) {
    reset_file("sf_in_append.txt", "abcdef");
    reset_file("sf_out_append.txt", "");

    int in_fd = open("sf_in_append.txt", O_RDONLY);
    int out_fd = open("sf_out_append.txt", O_WRONLY | O_APPEND);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 0;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 3);

    printf("O_APPEND test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == -1);
    CHECK(errno == EINVAL);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);
}

/*
 * 测试 6：
 * in_fd 不可读时，应该返回 -1，errno = EBADF。
 */
static void test_in_fd_not_readable_ebadf(void) {
    reset_file("sf_in_not_readable.txt", "abcdef");
    reset_file("sf_out_not_readable.txt", "");

    int in_fd = open("sf_in_not_readable.txt", O_WRONLY);
    int out_fd = open("sf_out_not_readable.txt", O_WRONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 0;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 3);

    printf("input not readable test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == -1);
    CHECK(errno == EBADF);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);
}

/*
 * 测试 7：
 * out_fd 不可写时，应该返回 -1，errno = EBADF。
 */
static void test_out_fd_not_writable_ebadf(void) {
    reset_file("sf_in_out_not_writable.txt", "abcdef");
    reset_file("sf_out_not_writable.txt", "");

    int in_fd = open("sf_in_out_not_writable.txt", O_RDONLY);
    int out_fd = open("sf_out_not_writable.txt", O_RDONLY);

    CHECK(in_fd >= 0);
    CHECK(out_fd >= 0);

    off_t off = 0;
    errno = 0;

    ssize_t ret = sendfile(out_fd, in_fd, &off, 3);

    printf("output not writable test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret == -1);
    CHECK(errno == EBADF);

    CHECK(close(in_fd) == 0);
    CHECK(close(out_fd) == 0);
}

/*
 * 测试 8：
 * in_fd 是 pipe read end 时，Linux sendfile 应该失败，
 * 返回 -1，并设置 errno = EINVAL。
 *
 * 因为 sendfile 的输入端应当是支持 mmap-like 操作的文件，
 * pipe 不满足这个条件。
 */
static void test_pipe_input_einval(void) {
    reset_file("sf_out_pipe.txt", "");

    int pipefd[2];
    CHECK(pipe(pipefd) == 0);

    int out_fd = open("sf_out_pipe.txt", O_WRONLY);
    CHECK(out_fd >= 0);

    write_all(pipefd[1], "abcdef");

    errno = 0;
    ssize_t ret = sendfile(out_fd, pipefd[0], NULL, 3);

    printf("pipe input test: ret=%ld errno=%d\n",
           (long)ret, errno);

    CHECK(ret == -1);
    CHECK(errno == EINVAL);

    CHECK(close(pipefd[0]) == 0);
    CHECK(close(pipefd[1]) == 0);
    CHECK(close(out_fd) == 0);
}

static void test_output_pipe_partial_write_count(void) {
    reset_file_repeated("sf_in_partial_pipe.txt", 'S', 8192);

    int in_fd = open("sf_in_partial_pipe.txt", O_RDONLY);
    CHECK(in_fd >= 0);

    int pipefd[2] = {-1, -1};
    CHECK(pipe(pipefd) == 0);

    if (in_fd < 0 || pipefd[0] < 0 || pipefd[1] < 0) {
        goto out;
    }

    int flags = fcntl(pipefd[1], F_GETFL);
    CHECK(flags >= 0);
    if (flags < 0) {
        goto out;
    }
    if (fcntl(pipefd[1], F_SETFL, flags | O_NONBLOCK) != 0) {
        CHECK(0);
        goto out;
    }

    char fill[512];
    memset(fill, 'P', sizeof(fill));
    for (;;) {
        ssize_t n = write(pipefd[1], fill, sizeof(fill));
        if (n < 0) {
            CHECK(errno == EAGAIN || errno == EWOULDBLOCK);
            break;
        }
        CHECK(n > 0);
        if (n == 0) {
            goto out;
        }
    }

    char drain[256];
    size_t drained = 0;
    while (drained < 4097) {
        size_t want = 4097 - drained;
        if (want > sizeof(drain)) {
            want = sizeof(drain);
        }
        ssize_t n = read(pipefd[0], drain, want);
        CHECK(n > 0);
        if (n <= 0) {
            goto out;
        }
        drained += (size_t)n;
    }

    off_t off = 0;
    errno = 0;
    ssize_t ret = sendfile(pipefd[1], in_fd, &off, 8192);

    printf("output pipe partial write test: ret=%ld errno=%d off=%lld\n",
           (long)ret, errno, (long long)off);

    CHECK(ret > 0);
    CHECK(ret < 8192);
    CHECK(off == ret);

out:
    if (in_fd >= 0) {
        CHECK(close(in_fd) == 0);
    }
    if (pipefd[0] >= 0) {
        CHECK(close(pipefd[0]) == 0);
    }
    if (pipefd[1] >= 0) {
        CHECK(close(pipefd[1]) == 0);
    }
}

int main(void) {
    test_offset_ptr();
    test_null_offset();
    test_eof_returns_zero();
    test_count_zero();
    test_out_fd_append_einval();
    test_in_fd_not_readable_ebadf();
    test_out_fd_not_writable_ebadf();
    test_pipe_input_einval();
    test_output_pipe_partial_write_count();

    printf("DONE: %d pass, %d fail\n", g_pass, g_fail);
    fflush(stdout);

    return g_fail == 0 ? 0 : 1;
}
