/*
 * test_file_io_edge.c -- file I/O edge cases
 *
 * Verify:
 *   1. ftruncate: negative length returns EINVAL
 *   2. pwrite64: negative offset returns EINVAL
 *   3. lseek SEEK_SET: negative offset returns EINVAL
 *   4. readahead: validation, cache residency, and bounded prefetch
 */

#include "test_framework.h"
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <string.h>
#include <errno.h>

#define TEST_FILE "/tmp/file_io_edge_test"
#define READAHEAD_FIXTURE \
    "/usr/share/starry-test-suit/file-io-edge/readahead-cold"
#define FIXTURE_PAGE_SIZE 4096
#define FIXTURE_FULL_PAGES 40
#define FIXTURE_TAIL_BYTES 123
#define FIXTURE_PAGE_COUNT (FIXTURE_FULL_PAGES + 1)
#define FIXTURE_MAPPING_PAGES (FIXTURE_PAGE_COUNT + 1)
#define FIXTURE_SIZE \
    ((size_t)(FIXTURE_PAGE_SIZE * FIXTURE_FULL_PAGES + FIXTURE_TAIL_BYTES))
#define FIXTURE_MAPPING_SIZE \
    ((size_t)(FIXTURE_MAPPING_PAGES * FIXTURE_PAGE_SIZE))
#define READAHEAD_WINDOW_PAGES 32

static long page_resident(unsigned char *mapping, size_t page_size,
                          size_t page_index)
{
    unsigned char resident = 0;

    errno = 0;
    if (syscall(SYS_mincore, mapping + page_index * page_size, page_size,
                &resident) != 0) {
        return -1;
    }
    return resident & 1;
}

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

static void test_readahead_cache_effects(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    unsigned char cold[FIXTURE_MAPPING_PAGES] = {0};
    unsigned char residency[FIXTURE_MAPPING_PAGES] = {0};
    unsigned char before_boundaries[FIXTURE_MAPPING_PAGES] = {0};
    unsigned char after_boundaries[FIXTURE_MAPPING_PAGES] = {0};
    struct stat status = {0};
    unsigned char *mapping;
    int any_resident = 0;
    int all_window_resident = 1;
    int any_outside_window = 0;
    int fd = -1;
    int direct_fd = -1;
    long mincore_result;
    size_t eof_offset;

    CHECK_RET(page_size, FIXTURE_PAGE_SIZE,
              "readahead fixture: runtime page size is 4096");
    if (page_size != FIXTURE_PAGE_SIZE) {
        return;
    }

    fd = open(READAHEAD_FIXTURE, O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0, "readahead fixture: open cold file O_RDONLY");
    if (fd < 0) {
        return;
    }

    CHECK_RET(fstat(fd, &status), 0, "readahead fixture: stat");
    CHECK(status.st_size == (off_t)FIXTURE_SIZE,
          "readahead fixture: 40 full pages plus a partial tail page");

    mapping = mmap(NULL, FIXTURE_MAPPING_SIZE, PROT_READ, MAP_PRIVATE, fd, 0);
    CHECK(mapping != MAP_FAILED, "readahead fixture: map without reading");
    if (mapping == MAP_FAILED) {
        close(fd);
        return;
    }

    mincore_result =
        syscall(SYS_mincore, mapping, FIXTURE_MAPPING_SIZE, cold);
    CHECK_RET(mincore_result, 0, "readahead fixture: inspect initial cache");
    if (mincore_result == 0) {
        for (size_t i = 0; i < FIXTURE_MAPPING_PAGES; i++) {
            any_resident |= cold[i] & 1;
        }
        CHECK(!any_resident,
              "readahead fixture: no page is resident before the hint");
    }

    CHECK_RET(lseek(fd, 17, SEEK_SET), 17,
              "readahead fixture: set an unrelated file offset");
    CHECK_RET(syscall(SYS_readahead, fd,
                      (long)(2 * (size_t)page_size + 17),
                      (size_t)((size_t)page_size + 31)),
              0, "readahead fixture: accept a non-aligned range");
    CHECK_RET(lseek(fd, 0, SEEK_CUR), 17,
              "readahead fixture: preserve the file offset");
    CHECK_RET(page_resident(mapping, (size_t)page_size, 2), 1,
              "readahead fixture: prefetch the first partial page");
    CHECK_RET(page_resident(mapping, (size_t)page_size, 3), 1,
              "readahead fixture: prefetch the last partial page");
    CHECK_RET(page_resident(mapping, (size_t)page_size, 1), 0,
              "readahead fixture: do not prefetch before the range");
    CHECK_RET(page_resident(mapping, (size_t)page_size, 4), 0,
              "readahead fixture: do not prefetch after the range");

    /*
     * Linux v6.12 treats a zero length as "to EOF". Start inside the
     * penultimate page: the hint must publish both it and the partial tail
     * page, which distinguishes EOF semantics from a zero-page no-op.
     */
    eof_offset = FIXTURE_SIZE - (size_t)page_size;
    CHECK_RET(page_resident(mapping, (size_t)page_size,
                            FIXTURE_FULL_PAGES),
              0, "readahead fixture: tail page starts cold");
    CHECK_RET(syscall(SYS_readahead, fd, (long)eof_offset, (size_t)0), 0,
              "readahead fixture: zero count means through EOF");
    CHECK_RET(page_resident(mapping, (size_t)page_size,
                            FIXTURE_FULL_PAGES - 1),
              1, "readahead fixture: zero count caches the penultimate page");
    CHECK_RET(page_resident(mapping, (size_t)page_size,
                            FIXTURE_FULL_PAGES),
              1, "readahead fixture: zero count caches the remaining tail");
    CHECK_RET(page_resident(mapping, (size_t)page_size,
                            FIXTURE_PAGE_COUNT),
              0, "readahead fixture: zero count does not cache beyond EOF");

    mincore_result =
        syscall(SYS_mincore, mapping, FIXTURE_MAPPING_SIZE,
                before_boundaries);
    CHECK_RET(mincore_result, 0,
              "readahead fixture: snapshot cache before no-op boundaries");
    CHECK_RET(syscall(SYS_readahead, fd, (long)FIXTURE_SIZE,
                      (size_t)page_size),
              0, "readahead fixture: offset at EOF succeeds");
    CHECK_RET(syscall(SYS_readahead, fd, (long)INT64_MAX,
                      (size_t)page_size),
              0, "readahead fixture: maximum offset succeeds");
    CHECK_RET(syscall(SYS_readahead, fd,
                      (long)(INT64_MAX - (int64_t)page_size + 1),
                      (size_t)page_size),
              0, "readahead fixture: offset plus count saturates");
    CHECK_RET(syscall(SYS_mincore, mapping, FIXTURE_MAPPING_SIZE,
                      after_boundaries),
              0, "readahead fixture: inspect cache after no-op boundaries");
    CHECK(memcmp(before_boundaries, after_boundaries,
                 sizeof(before_boundaries)) == 0,
          "readahead fixture: EOF and extreme offsets add no cache pages");
    CHECK_RET(lseek(fd, 0, SEEK_CUR), 17,
              "readahead fixture: boundary hints preserve the offset");

    /*
     * A synchronous implementation is bounded to the existing 32-page
     * window. Pages 0 through 31, and only those remaining outside the
     * earlier ranges, must be cached when the hint returns.
     */
    CHECK_RET(syscall(SYS_readahead, fd, (long)0, FIXTURE_SIZE), 0,
              "readahead fixture: accept a request larger than one window");
    mincore_result =
        syscall(SYS_mincore, mapping, FIXTURE_MAPPING_SIZE, residency);
    CHECK_RET(mincore_result, 0,
              "readahead fixture: inspect the bounded prefetch window");
    if (mincore_result == 0) {
        for (size_t i = 0; i < READAHEAD_WINDOW_PAGES; i++) {
            all_window_resident &= (residency[i] & 1) != 0;
        }
        for (size_t i = READAHEAD_WINDOW_PAGES;
             i < (size_t)FIXTURE_FULL_PAGES - 1; i++) {
            any_outside_window |= residency[i] & 1;
        }
        CHECK(all_window_resident,
              "readahead fixture: cache every page in the first window");
        CHECK(!any_outside_window,
              "readahead fixture: leave pages beyond the window cold");
    }

    /*
     * O_DIRECT uses a direct backend for this open, but the same inode still
     * has a live cached mapping. Readahead must publish into that inode cache
     * instead of treating every O_DIRECT descriptor as non-cacheable.
     */
    CHECK_RET(page_resident(mapping, (size_t)page_size, 35), 0,
              "readahead O_DIRECT: target page starts cold");
    direct_fd = open(READAHEAD_FIXTURE, O_RDONLY | O_DIRECT | O_CLOEXEC);
    CHECK(direct_fd >= 0, "readahead O_DIRECT: open the mapped inode");
    if (direct_fd >= 0) {
        CHECK_RET(syscall(SYS_readahead, direct_fd,
                          (long)(35 * (size_t)page_size),
                          (size_t)page_size),
                  0, "readahead O_DIRECT: return success");
        CHECK_RET(page_resident(mapping, (size_t)page_size, 35), 1,
                  "readahead O_DIRECT: populate the shared inode cache");
        close(direct_fd);
    }

    CHECK_RET(munmap(mapping, FIXTURE_MAPPING_SIZE), 0,
              "readahead fixture: unmap");
    close(fd);
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

    test_readahead_cache_effects();
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
