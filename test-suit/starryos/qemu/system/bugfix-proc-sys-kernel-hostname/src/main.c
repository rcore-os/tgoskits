#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>

static int passed;
static int failed;

static void expect_true(int condition, const char *name)
{
    if (condition) {
        printf("PASS: %s\n", name);
        passed++;
        return;
    }
    printf("FAIL: %s: errno=%d (%s)\n", name, errno, strerror(errno));
    failed++;
}

int main(void)
{
    const char updated_hostname[] = "starry-proc-hostname";
    char hostname[65] = {0};
    char original_hostname[65] = {0};
    size_t original_hostname_len = 0;
    struct utsname uts;
    char content[128] = {0};

    printf("=== bugfix-proc-sys-kernel-hostname ===\n");

    errno = 0;
    expect_true(gethostname(hostname, sizeof(hostname)) == 0,
                "gethostname succeeds");
    expect_true(uname(&uts) == 0, "uname succeeds");
    expect_true(strcmp(hostname, uts.nodename) == 0,
                "gethostname matches uname nodename");

    errno = 0;
    int fd = open("/proc/sys/kernel/hostname", O_RDONLY | O_CLOEXEC);
    expect_true(fd >= 0, "open /proc/sys/kernel/hostname");
    if (fd >= 0) {
        struct stat st;
        expect_true(fstat(fd, &st) == 0 && S_ISREG(st.st_mode),
                    "hostname sysctl is a regular file");

        errno = 0;
        ssize_t count = read(fd, content, sizeof(content));
        expect_true(count > 0, "read hostname sysctl");
        if (count > 0) {
            size_t hostname_len = strlen(hostname);
            expect_true((size_t)count == hostname_len + 1,
                        "hostname sysctl has exact length");
            expect_true(memcmp(content, hostname, hostname_len) == 0 &&
                            content[hostname_len] == '\n',
                        "hostname sysctl equals current hostname plus newline");
            expect_true(memchr(content, '\0', (size_t)count) == NULL,
                        "hostname sysctl has no NUL padding");
            if ((size_t)count <= sizeof(original_hostname) &&
                content[count - 1] == '\n') {
                original_hostname_len = (size_t)count - 1;
                memcpy(original_hostname, content, original_hostname_len);
            }
        }

        char extra;
        expect_true(read(fd, &extra, 1) == 0,
                    "hostname sysctl reaches EOF");
        expect_true(lseek(fd, 0, SEEK_SET) == 0,
                    "hostname sysctl seeks to start");
        memset(content, 0, sizeof(content));
        ssize_t repeated = read(fd, content, sizeof(content));
        expect_true(repeated > 0 &&
                        (size_t)repeated == strlen(hostname) + 1 &&
                        memcmp(content, hostname, strlen(hostname)) == 0 &&
                        content[strlen(hostname)] == '\n',
                    "hostname sysctl repeats the same UTS value");
        close(fd);
    }

    char update_request[sizeof(updated_hostname) + 1];
    snprintf(update_request, sizeof(update_request), "%s\n", updated_hostname);
    errno = 0;
    fd = open("/proc/sys/kernel/hostname", O_WRONLY | O_CLOEXEC);
    expect_true(fd >= 0, "open hostname sysctl for writing");
    if (fd >= 0) {
        ssize_t written = write(fd, update_request, strlen(update_request));
        expect_true(written == (ssize_t)strlen(update_request),
                    "write hostname sysctl with trailing newline");
        close(fd);

        if (written == (ssize_t)strlen(update_request)) {
            memset(hostname, 0, sizeof(hostname));
            expect_true(gethostname(hostname, sizeof(hostname)) == 0,
                        "gethostname after proc sysctl write");
            expect_true(strcmp(hostname, updated_hostname) == 0,
                        "proc sysctl write updates gethostname");

            memset(&uts, 0, sizeof(uts));
            expect_true(uname(&uts) == 0, "uname after proc sysctl write");
            expect_true(strcmp(uts.nodename, updated_hostname) == 0,
                        "proc sysctl write updates uname nodename");

            fd = open("/proc/sys/kernel/hostname", O_RDONLY | O_CLOEXEC);
            expect_true(fd >= 0, "reopen hostname sysctl after write");
            if (fd >= 0) {
                memset(content, 0, sizeof(content));
                ssize_t count = read(fd, content, sizeof(content));
                expect_true(count == (ssize_t)strlen(updated_hostname) + 1,
                            "read updated hostname sysctl");
                expect_true(memcmp(content, updated_hostname,
                                   strlen(updated_hostname)) == 0 &&
                                content[strlen(updated_hostname)] == '\n',
                            "hostname sysctl readback matches updated UTS value");
                close(fd);
            }
        }
    }

    char oversized_hostname[67];
    memset(oversized_hostname, 'a', 65);
    oversized_hostname[65] = '\n';
    oversized_hostname[66] = '\0';
    errno = 0;
    fd = open("/proc/sys/kernel/hostname", O_WRONLY | O_CLOEXEC);
    expect_true(fd >= 0, "open hostname sysctl for oversized write");
    if (fd >= 0) {
        expect_true(write(fd, oversized_hostname, 66) == -1 && errno == EINVAL,
                    "hostname sysctl rejects names longer than 64 bytes");
        close(fd);
    }

    fd = open("/proc/sys/kernel/hostname", O_WRONLY | O_CLOEXEC);
    if (fd >= 0) {
        ssize_t restored =
            write(fd, original_hostname, original_hostname_len);
        expect_true(restored == (ssize_t)original_hostname_len,
                    "restore original hostname");
        close(fd);
    } else {
        expect_true(0, "open hostname sysctl to restore original hostname");
    }

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_PROC_SYS_HOSTNAME_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-proc-sys-kernel-hostname\n");
        return EXIT_SUCCESS;
    }
    printf("STARRY_GROUPED_TEST_FAILED: bugfix-proc-sys-kernel-hostname\n");
    return EXIT_FAILURE;
}
