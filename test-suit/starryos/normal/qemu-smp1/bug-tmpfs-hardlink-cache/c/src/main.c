/*
 * bug-tmpfs-hardlink-cache: hard-linked tmpfs paths must share cached content.
 *
 * StarryOS tmpfs stores regular file data in the axfs-ng page cache. A hard
 * link creates a second DirEntry for the same inode, so the new DirEntry must
 * inherit the source DirEntry's user_data/cache. Otherwise the new path sees
 * the correct inode, nlink, and size but reads zero-filled data.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int fail_errno(const char *msg)
{
    printf("FAIL: %s errno=%d (%s)\n", msg, errno, strerror(errno));
    printf("TEST FAILED\n");
    return 1;
}

static int fail_msg(const char *msg)
{
    printf("FAIL: %s\n", msg);
    printf("TEST FAILED\n");
    return 1;
}

int main(void)
{
    static const char payload[] = "hi\n";
    char buf[sizeof(payload)] = {0};
    const char *a = "/tmp/bug_tmpfs_hardlink_a";
    const char *b = "/tmp/bug_tmpfs_hardlink_b";

    printf("=== bug-tmpfs-hardlink-cache ===\n");

    unlink(a);
    unlink(b);

    int fd = open(a, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        return fail_errno("open source");
    }
    ssize_t written = write(fd, payload, sizeof(payload) - 1);
    if (written != (ssize_t)sizeof(payload) - 1) {
        close(fd);
        return fail_msg("short write to source");
    }
    close(fd);

    if (link(a, b) != 0) {
        return fail_errno("link source to destination");
    }

    struct stat sa;
    struct stat sb;
    if (stat(a, &sa) != 0 || stat(b, &sb) != 0) {
        return fail_errno("stat hardlink pair");
    }
    if (sa.st_ino != sb.st_ino || sa.st_nlink != 2 || sb.st_nlink != 2) {
        printf("FAIL: hardlink metadata mismatch ino=(%lu,%lu) nlink=(%lu,%lu)\n",
               (unsigned long)sa.st_ino, (unsigned long)sb.st_ino,
               (unsigned long)sa.st_nlink, (unsigned long)sb.st_nlink);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: hardlink metadata is shared\n");

    fd = open(b, O_RDONLY);
    if (fd < 0) {
        return fail_errno("open hardlink destination");
    }
    ssize_t readn = read(fd, buf, sizeof(payload) - 1);
    close(fd);
    if (readn != (ssize_t)sizeof(payload) - 1 || memcmp(buf, payload, sizeof(payload) - 1) != 0) {
        printf("FAIL: destination content mismatch: read=%zd bytes=%02x %02x %02x\n", readn,
               (unsigned char)buf[0], (unsigned char)buf[1], (unsigned char)buf[2]);
        printf("TEST FAILED\n");
        return 1;
    }
    printf("PASS: hardlink destination reads source content\n");

    unlink(b);
    unlink(a);
    printf("TEST PASSED\n");
    return 0;
}
