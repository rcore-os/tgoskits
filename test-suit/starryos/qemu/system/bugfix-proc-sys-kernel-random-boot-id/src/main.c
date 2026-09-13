#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define BOOT_ID_PATH "/proc/sys/kernel/random/boot_id"
#define BOOT_ID_LENGTH 37

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

static int is_lowercase_hex(char value)
{
    return (value >= '0' && value <= '9') || (value >= 'a' && value <= 'f');
}

static int has_canonical_boot_id_format(const char boot_id[BOOT_ID_LENGTH])
{
    static const size_t hyphen_offsets[] = {8, 13, 18, 23};
    int has_nonzero_digit = 0;

    if (boot_id[BOOT_ID_LENGTH - 1] != '\n') {
        return 0;
    }
    if (boot_id[14] != '4' ||
        (boot_id[19] != '8' && boot_id[19] != '9' && boot_id[19] != 'a' &&
         boot_id[19] != 'b')) {
        return 0;
    }

    for (size_t offset = 0; offset < BOOT_ID_LENGTH - 1; offset++) {
        int is_hyphen = 0;

        for (size_t index = 0; index < sizeof(hyphen_offsets) / sizeof(hyphen_offsets[0]);
             index++) {
            if (offset == hyphen_offsets[index]) {
                is_hyphen = 1;
                break;
            }
        }

        if (is_hyphen) {
            if (boot_id[offset] != '-') {
                return 0;
            }
            continue;
        }

        if (!is_lowercase_hex(boot_id[offset])) {
            return 0;
        }
        has_nonzero_digit |= boot_id[offset] != '0';
    }

    return has_nonzero_digit;
}

static ssize_t read_boot_id(int fd, char boot_id[BOOT_ID_LENGTH])
{
    size_t total = 0;

    while (total < BOOT_ID_LENGTH) {
        ssize_t count = read(fd, boot_id + total, BOOT_ID_LENGTH - total);

        if (count <= 0) {
            return count < 0 ? -1 : (ssize_t)total;
        }
        total += (size_t)count;
    }

    return (ssize_t)total;
}

int main(void)
{
    char boot_id[BOOT_ID_LENGTH];
    char repeated_boot_id[BOOT_ID_LENGTH];
    char second_fd_boot_id[BOOT_ID_LENGTH];
    char extra;
    struct stat metadata;
    int fd;
    int second_fd;
    int valid_boot_id;

    printf("=== bugfix-proc-sys-kernel-random-boot-id ===\n");

    errno = 0;
    fd = open(BOOT_ID_PATH, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
#if defined(__x86_64__)
        /* qemu-x86_64 boots through UEFI, whose RNG protocol is required here. */
        expect_true(0, "open boot ID proc file read-only");
#else
        /*
         * Platforms without a trusted boot entropy source (UEFI RNG or a
         * 32-byte FDT /chosen/rng-seed) omit boot_id while retaining the
         * parent directory, so open(2) must fail with ENOENT.
         */
        expect_true(errno == ENOENT, "boot ID omitted without trusted entropy (ENOENT)");
#endif
        goto out;
    }
    expect_true(1, "open boot ID proc file read-only");

    errno = 0;
    expect_true(
        fstat(fd, &metadata) == 0 && S_ISREG(metadata.st_mode) &&
            (metadata.st_mode & 0777) == 0444,
        "boot ID proc file is a regular file with mode 0444"
    );

    errno = 0;
    expect_true(
        read_boot_id(fd, boot_id) == BOOT_ID_LENGTH,
        "read the exact boot ID length"
    );
    valid_boot_id = has_canonical_boot_id_format(boot_id);
    expect_true(valid_boot_id, "boot ID is a nonzero canonical UUID followed by newline");
    if (valid_boot_id) {
        printf("OBSERVE: boot_id=%.*s", BOOT_ID_LENGTH, boot_id);
    }

    errno = 0;
    expect_true(read(fd, &extra, sizeof(extra)) == 0, "boot ID reaches EOF");
    expect_true(lseek(fd, 0, SEEK_SET) == 0, "boot ID seeks to start");

    errno = 0;
    expect_true(
        read_boot_id(fd, repeated_boot_id) == BOOT_ID_LENGTH &&
            memcmp(boot_id, repeated_boot_id, sizeof(boot_id)) == 0,
        "boot ID remains stable after seek and reread"
    );

    errno = 0;
    second_fd = open(BOOT_ID_PATH, O_RDONLY | O_CLOEXEC);
    expect_true(second_fd >= 0, "open a second boot ID reader");
    if (second_fd >= 0) {
        errno = 0;
        expect_true(
            read_boot_id(second_fd, second_fd_boot_id) == BOOT_ID_LENGTH &&
                memcmp(boot_id, second_fd_boot_id, sizeof(boot_id)) == 0,
            "all readers observe the same boot ID"
        );
        close(second_fd);
    }

    close(fd);

out:
    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("STARRY_PROC_SYS_KERNEL_RANDOM_BOOT_ID_PASSED\n");
        printf("STARRY_GROUPED_TEST_PASSED: bugfix-proc-sys-kernel-random-boot-id\n");
        return EXIT_SUCCESS;
    }

    printf("STARRY_GROUPED_TEST_FAILED: bugfix-proc-sys-kernel-random-boot-id\n");
    return EXIT_FAILURE;
}
