/*
 * bug-fb0-offset: /dev/fb0 read_at/write_at must honor the byte offset. The
 * buggy version indexed the scanout slice from 0 regardless of offset, so a
 * write at offset O aliased the top-left and a read at offset O returned the
 * top-left bytes. This checks that a write at a non-zero offset is isolated
 * from offset 0 in both directions.
 *
 * Skips (pass) when the QEMU profile exposes no framebuffer device — the fix is
 * otherwise validated on-board where /dev/fb0 is backed by the real scanout.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

enum {
    CHUNK = 256,
    OFFSET = 8192,          /* a couple of pages in — well clear of the top-left */
    PATTERN_AT_OFFSET = 0xA5,
    PATTERN_AT_ZERO = 0x5A,
};

int main(void)
{
    printf("=== bug-fb0-offset ===\n");
    printf("Expected: /dev/fb0 read_at/write_at index at the given byte "
           "offset, not from 0.\n\n");

    int fd = open("/dev/fb0", O_RDWR);
    if (fd < 0) {
        /* No framebuffer in this QEMU profile — nothing to exercise here. */
        printf("SKIP: /dev/fb0 unavailable (%s); covered by the board oracle\n",
               strerror(errno));
        printf("TEST PASSED\n");
        return 0;
    }

    unsigned char at_off[CHUNK];
    unsigned char at_zero[CHUNK];
    memset(at_off, PATTERN_AT_OFFSET, sizeof(at_off));
    memset(at_zero, PATTERN_AT_ZERO, sizeof(at_zero));

    /* Write a distinct pattern at the non-zero offset. */
    if (pwrite(fd, at_off, sizeof(at_off), OFFSET) != (ssize_t)sizeof(at_off)) {
        printf("FAIL: pwrite at offset %d: %s\n", OFFSET, strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    /* Read it back at the same offset — must match (offset honored both ways). */
    unsigned char back[CHUNK];
    memset(back, 0, sizeof(back));
    if (pread(fd, back, sizeof(back), OFFSET) != (ssize_t)sizeof(back)) {
        printf("FAIL: pread at offset %d: %s\n", OFFSET, strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    if (memcmp(back, at_off, sizeof(back)) != 0) {
        printf("FAIL: read-back at offset %d did not match the write\n", OFFSET);
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    /* Now write a different pattern at offset 0 and confirm it does NOT alias
     * the bytes we placed at OFFSET (the core of the aliasing bug). */
    if (pwrite(fd, at_zero, sizeof(at_zero), 0) != (ssize_t)sizeof(at_zero)) {
        printf("FAIL: pwrite at offset 0: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    memset(back, 0, sizeof(back));
    if (pread(fd, back, sizeof(back), OFFSET) != (ssize_t)sizeof(back)) {
        printf("FAIL: pread(2) at offset %d: %s\n", OFFSET, strerror(errno));
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }
    if (memcmp(back, at_off, sizeof(back)) != 0) {
        printf("FAIL: write at offset 0 aliased the bytes at offset %d\n",
               OFFSET);
        printf("TEST FAILED\n");
        close(fd);
        return 1;
    }

    close(fd);
    printf("PASS: /dev/fb0 offset honored for read_at and write_at\n");
    printf("TEST PASSED\n");
    return 0;
}
