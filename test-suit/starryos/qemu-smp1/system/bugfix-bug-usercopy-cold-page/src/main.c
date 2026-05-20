#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void *cold_page(void)
{
    void *ptr = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (ptr == MAP_FAILED) {
        return NULL;
    }
    return ptr;
}

static void expect_getcwd_into_cold_page(void)
{
    char *buf = cold_page();
    if (buf == NULL) {
        note_fail("mmap cold getcwd page", strerror(errno));
        return;
    }

    if (chdir("/tmp") != 0) {
        note_fail("chdir /tmp", strerror(errno));
        munmap(buf, 4096);
        return;
    }

    errno = 0;
    char *ret = getcwd(buf, 4096);
    if (ret == buf && strcmp(buf, "/tmp") == 0) {
        note_pass("getcwd writes to untouched anonymous page");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "ret=%p errno=%d (%s) buf='%s'",
                 (void *)ret, errno, strerror(errno), ret != NULL ? buf : "");
        note_fail("getcwd cold page", detail);
    }

    munmap(buf, 4096);
}

static void expect_read_into_cold_page(void)
{
    char *buf = cold_page();
    if (buf == NULL) {
        note_fail("mmap cold read page", strerror(errno));
        return;
    }

    int fd = open("/dev/zero", O_RDONLY);
    if (fd < 0) {
        note_fail("open /dev/zero", strerror(errno));
        munmap(buf, 4096);
        return;
    }

    errno = 0;
    ssize_t nread = read(fd, buf, 128);
    if (nread == 128 && buf[0] == 0 && buf[127] == 0) {
        note_pass("read writes to untouched anonymous page");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "nread=%zd errno=%d (%s) first=%d last=%d",
                 nread, errno, strerror(errno), buf[0], buf[127]);
        note_fail("read cold page", detail);
    }

    close(fd);
    munmap(buf, 4096);
}

static void expect_empty_path_from_cold_page(void)
{
    char *path = cold_page();
    if (path == NULL) {
        note_fail("mmap cold empty path page", strerror(errno));
        return;
    }

    errno = 0;
    int fd = open(path, O_RDONLY);
    if (fd < 0 && errno == ENOENT) {
        note_pass("open reads empty path from untouched anonymous page");
    } else {
        char detail[192];
        snprintf(detail, sizeof(detail),
                 "fd=%d errno=%d (%s), expected ENOENT",
                 fd, errno, strerror(errno));
        note_fail("open cold empty path", detail);
        if (fd >= 0) {
            close(fd);
        }
    }

    munmap(path, 4096);
}

int main(void)
{
    printf("=== bug-usercopy-cold-page ===\n");

    expect_getcwd_into_cold_page();
    expect_read_into_cold_page();
    expect_empty_path_from_cold_page();

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }

    printf("SOME TESTS FAILED\n");
    return 1;
}
