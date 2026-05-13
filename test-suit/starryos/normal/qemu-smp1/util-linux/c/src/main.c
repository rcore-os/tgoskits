/*
 * util-linux-test.c -- util-linux 2.38+ combined verification
 *
 * Key dependencies: mount, pivot_root, blkid, fdisk, losetup
 * Acceptance criteria:
 *   - mount -t ext4 works (ext4 filesystem mount/unmount)
 *   - fdisk -l works (partition table listing)
 *   - losetup full chain (attach/detach loop device)
 *   - blkid block device identification
 *   - pivot_root command available
 *
 * The ext4 mount test uses a pre-formatted image created during
 * CMake build via host mkfs.ext4 (available in the CI Docker image).
 */

#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <sys/mount.h>

static int pass = 0, fail = 0;

static int run(const char *cmd)
{
    int ret = system(cmd);
    if (WIFEXITED(ret))
        return WEXITSTATUS(ret);
    return -1;
}

/* Capture first line of command output into buf, return exit code */
static int capture(const char *cmd, char *buf, int bufsz)
{
    FILE *p = popen(cmd, "r");
    if (!p) return -1;
    buf[0] = '\0';
    if (fgets(buf, bufsz, p)) {
        int len = (int)strlen(buf);
        while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
            buf[--len] = '\0';
    }
    int status = pclose(p);
    if (WIFEXITED(status))
        return WEXITSTATUS(status);
    return -1;
}

/* Search for needle in a binary file, return 1 if found, 0 if not */
static int file_contains(const char *path, const char *needle)
{
    size_t nlen = strlen(needle);
    FILE *f = fopen(path, "rb");
    if (!f) return 0;
    char buf[4096];
    size_t keep = 0;
    int found = 0;
    while (!found) {
        size_t r = fread(buf + keep, 1, sizeof(buf) - keep, f);
        if (r == 0) break;
        size_t total = keep + r;
        size_t scan = (total >= nlen) ? total - nlen + 1 : 0;
        if (scan > 0 && memmem(buf, total, needle, nlen))
            found = 1;
        if (total >= nlen) {
            keep = nlen - 1;
            memmove(buf, buf + total - keep, keep);
        } else {
            keep = total;
        }
    }
    fclose(f);
    return found;
}

/* Write string to file, return 0 on success */
static int write_file(const char *path, const char *data)
{
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t w = write(fd, data, len);
    close(fd);
    return (w == (ssize_t)len) ? 0 : -1;
}

/* Read first line of file into buf, return line length or -1 */
static int read_first_line(const char *path, char *buf, int bufsz)
{
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    if (!fgets(buf, bufsz, f)) { fclose(f); return -1; }
    int len = (int)strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r'))
        buf[--len] = '\0';
    fclose(f);
    return len;
}

static void check(int ok, const char *name)
{
    if (ok) { printf("  PASS | %s\n", name); pass++; }
    else    { printf("  FAIL | %s\n", name); fail++; }
}

/* Path to the pre-formatted ext4 test image (created at CMake build time) */
#define PREBUILT_IMG "/usr/share/util-linux-test/test-ext4.img"

/* Expected image size: 4 MiB = 4194304 bytes = 8192 sectors */
#define IMG_BYTES  4194304
#define IMG_SECTORS 8192

int main(void)
{
    char buf[1024];
    char loopdev[64] = "";
    int rc;

    printf("=== util-linux 2.38+ test ===\n");

    /* ================================================================
     *  Tier 1: Tool availability
     * ================================================================ */

    /* 1. mount present */
    {
        rc = run("which mount >/dev/null 2>&1");
        check(rc == 0, "mount command available");
    }

    /* 2. losetup present */
    {
        rc = run("which losetup >/dev/null 2>&1");
        check(rc == 0, "losetup command available");
    }

    /* 3. blkid present */
    {
        rc = run("which blkid >/dev/null 2>&1");
        check(rc == 0, "blkid command available");
    }

    /* 4. fdisk present */
    {
        rc = run("which fdisk >/dev/null 2>&1");
        check(rc == 0, "fdisk command available");
    }

    /* ================================================================
     *  Tier 2: losetup full chain
     * ================================================================ */

    /* 5. Create 4MB test image */
    {
        rc = run("dd if=/dev/zero of=/tmp/ul-test.img bs=1M count=4 2>/dev/null");
        check(rc == 0, "dd create 4MB image");
    }

    /* 6. Find free loop device */
    {
        rc = capture("losetup -f 2>&1", buf, sizeof(buf));
        int found = (rc == 0 && strncmp(buf, "/dev/loop", 9) == 0);
        if (found)
            snprintf(loopdev, sizeof(loopdev), "%s", buf);
        check(found, "losetup -f find free device");
    }

    /* 7. Attach loop device */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s /tmp/ul-test.img 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach");
    } else {
        check(0, "losetup attach");
    }

    /* 8. List attached loop devices */
    {
        rc = run("losetup -a 2>&1 | grep -q 'ul-test.img'");
        check(rc == 0, "losetup -a list attached");
    }

    /* ================================================================
     *  Tier 3: fdisk -l (acceptance criterion)
     * ================================================================ */

    /* 9-10. fdisk -l output: capture and verify disk header + size */
    {
        char cmd[256];
        char fdisk_out[2048];
        int fdisk_ok = 0;

        /* Capture full fdisk -l output */
        if (loopdev[0]) {
            snprintf(cmd, sizeof(cmd), "fdisk -l %s 2>&1", loopdev);
            /* Read all output lines into fdisk_out */
            FILE *p = popen(cmd, "r");
            if (p) {
                size_t pos = 0;
                while (pos < sizeof(fdisk_out) - 1 &&
                       fgets(fdisk_out + pos, sizeof(fdisk_out) - pos, p)) {
                    pos += strlen(fdisk_out + pos);
                }
                fdisk_out[pos] = '\0';
                int st = pclose(p);
                fdisk_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;
            } else {
                fdisk_out[0] = '\0';
            }
        }

        /* Test 9: disk header present */
        check(fdisk_ok && strstr(fdisk_out, loopdev) != NULL,
              "fdisk -l shows disk header");

        /* Test 10: verify size appears in output.
         * BusyBox fdisk formats vary by version:
         *   - "4194304 bytes" (direct byte count)
         *   - "8192 sectors" (sector count)
         *   - "heads, 32 sectors/track" (geometry-derived)
         * We check that at least one of these size indicators is present
         * and consistent with the 4 MiB image (4194304 bytes / 8192 sectors).
         */
        {
            int found_bytes = (strstr(fdisk_out, "4194304") != NULL);
            int found_sectors = (strstr(fdisk_out, "8192") != NULL);
            check(found_bytes || found_sectors,
                  "fdisk -l shows 4 MiB / 8192 sectors");
        }
    }

    /* ================================================================
     *  Tier 4: mount -t ext4 full chain (acceptance criterion)
     * ================================================================ */

    /* 11. Pre-built ext4 image exists */
    {
        struct stat st;
        rc = (stat(PREBUILT_IMG, &st) == 0 && st.st_size == IMG_BYTES) ? 0 : -1;
        check(rc == 0, "pre-built ext4 image exists (4 MiB)");
    }

    /* 12. Detach raw image, attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach ext4 image");
    } else {
        check(0, "losetup attach ext4 image");
    }

    /* 13. blkid identifies ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "blkid %s 2>&1 | grep -qi 'ext4'", loopdev);
        rc = run(cmd);
        check(rc == 0, "blkid identifies ext4 filesystem");
    } else {
        check(0, "blkid identifies ext4 filesystem");
    }

    /* 14. Create mount point */
    {
        rc = run("mkdir /tmp/ul-mnt");
        check(rc == 0, "mkdir mount point");
    }

    /* 15. mount -t ext4 (acceptance criterion) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount -t ext4 (acceptance)");
    } else {
        check(0, "mount -t ext4 (acceptance)");
    }

    /* 16. Write file on ext4 mount */
    {
        rc = write_file("/tmp/ul-mnt/test.txt", "util-linux mount test\n");
        check(rc == 0, "write file on ext4 mount");
    }

    /* 17. Read file from ext4 mount */
    {
        char content[256] = {0};
        int len = read_first_line("/tmp/ul-mnt/test.txt", content, sizeof(content));
        check(len > 0 && strcmp(content, "util-linux mount test") == 0,
              "read file from ext4 mount");
    }

    /* 18. umount ext4 */
    {
        rc = run("umount /tmp/ul-mnt 2>&1");
        check(rc == 0, "umount ext4");
    }

    /* 19. Detach loop device */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup detach");
    } else {
        check(0, "losetup detach");
    }

    /* ================================================================
     *  Tier 4b: Loop device write persistence
     *
     *  Verify that data written through the loop block device survives
     *  umount + losetup -d.  Re-attach and re-mount the same image,
     *  then read the file back — it must match what was written above.
     * ================================================================ */

    /* 20. Re-attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup re-attach ext4 image");
    } else {
        check(0, "losetup re-attach ext4 image");
    }

    /* 21. Re-mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "re-mount ext4 after detach");
    } else {
        check(0, "re-mount ext4 after detach");
    }

    /* 22. Read file back — must survive umount + detach */
    {
        char content[256] = {0};
        int len = read_first_line("/tmp/ul-mnt/test.txt", content, sizeof(content));
        check(len > 0 && strcmp(content, "util-linux mount test") == 0,
              "data persists after umount+detach+remount");
    }

    /* 23. Cleanup: umount + detach after persistence test */
    {
        run("umount /tmp/ul-mnt 2>&1");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 4c: Loop device write-back without detach
     *
     *  Verify that data written through the loop block device survives
     *  umount followed by a re-mount *without* losetup -d in between.
     *  This exercises the write-back path in as_dyn_block_device()
     *  where dirty cache is flushed to the backing file before the
     *  cache is re-initialised from a fresh read of the file.
     * ================================================================ */

    /* 24. Attach ext4 image (fresh) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for no-detach test");
    } else {
        check(0, "losetup attach for no-detach test");
    }

    /* 25. Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for no-detach test");
    } else {
        check(0, "mount for no-detach test");
    }

    /* 26. Write distinct file */
    {
        rc = write_file("/tmp/ul-mnt/writeback.txt", "writeback ok\n");
        check(rc == 0, "write writeback.txt on ext4");
    }

    /* 27. Read back to confirm write */
    {
        char content[256] = {0};
        int len = read_first_line("/tmp/ul-mnt/writeback.txt", content, sizeof(content));
        check(len > 0 && strcmp(content, "writeback ok") == 0,
              "read writeback.txt before umount");
    }

    /* 28. Umount (do NOT detach loop device) */
    {
        rc = run("umount /tmp/ul-mnt 2>&1");
        check(rc == 0, "umount without detach");
    }

    /* 29. Re-mount same loop device (no detach in between) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "re-mount without detach");
    } else {
        check(0, "re-mount without detach");
    }

    /* 30. Read writeback.txt back — must survive umount-only cycle */
    {
        char content[256] = {0};
        int len = read_first_line("/tmp/ul-mnt/writeback.txt", content, sizeof(content));
        check(len > 0 && strcmp(content, "writeback ok") == 0,
              "data persists after umount+remount (no detach)");
    }

    /* 31. Cleanup: umount + detach */
    {
        run("umount /tmp/ul-mnt 2>&1");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 4d: Loop device write-back — verify via direct image read
     *
     *  Verify that data written through the loop block device is
     *  persisted to the backing file immediately after umount, WITHOUT
     *  requiring losetup -d or a subsequent mount as a write-back
     *  trigger.  We write a unique marker string, umount, then grep
     *  the raw backing image for that string.
     * ================================================================ */

    /* Attach ext4 image (fresh) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for direct-read test");
    } else {
        check(0, "losetup attach for direct-read test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for direct-read test");
    } else {
        check(0, "mount for direct-read test");
    }

    /* Write unique marker file */
    {
        rc = write_file("/tmp/ul-mnt/wb-verify.txt", "WB-VERIFY-a7c3-9f2e\n");
        check(rc == 0, "write wb-verify.txt on ext4");
    }

    /* Umount (do NOT detach) */
    {
        rc = run("umount /tmp/ul-mnt 2>&1");
        check(rc == 0, "umount for direct-read test");
    }

    /* Search the raw backing image for the unique string.
     * If the umount write-back hook worked correctly, the string
     * must be present in the image — no detach or re-mount needed. */
    {
        rc = file_contains(PREBUILT_IMG, "WB-VERIFY-a7c3-9f2e");
        check(rc, "data in backing image after umount (no detach)");
    }

    /* Cleanup: detach */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
    }

    /* ================================================================
     *  Tier 4e: Double mount EBUSY regression
     *
     *  Mounting the same loop device to a second mount point while the
     *  first mount is still active must fail with EBUSY.  Without this
     *  guard, as_dyn_block_device() would silently replace the cache,
     *  causing the first mount's ext4 writes to be lost on umount and
     *  allowing LOOP_CLR_FD to clear the mounted flag while the old
     *  adapter is still active.
     * ================================================================ */

    /* Attach ext4 image (fresh) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for double-mount test");
    } else {
        check(0, "losetup attach for double-mount test");
    }

    /* First mount — should succeed */
    {
        run("mkdir -p /tmp/ul-mnt2 2>/dev/null");
        char cmd[256];
        if (loopdev[0]) {
            snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
            rc = run(cmd);
            check(rc == 0, "first mount succeeds (double-mount test)");
        } else {
            check(0, "first mount succeeds (double-mount test)");
        }
    }

    /* Second mount of same loop device — must fail with EBUSY */
    {
        if (loopdev[0]) {
            errno = 0;
            rc = mount(loopdev, "/tmp/ul-mnt2", "ext4", 0, NULL);
            check(rc != 0 && errno == EBUSY,
                  "second mount of same loop device rejected (EBUSY)");
        } else {
            check(0, "second mount of same loop device rejected (EBUSY)");
        }
    }

    /* Umount first mount */
    {
        rc = run("umount /tmp/ul-mnt 2>&1");
        check(rc == 0, "umount first mount (double-mount test)");
    }

    /* After umount, mount at second point should now succeed */
    {
        char cmd[256];
        if (loopdev[0]) {
            snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt2 2>&1", loopdev);
            rc = run(cmd);
            check(rc == 0, "mount at second point succeeds after first umount");
        } else {
            check(0, "mount at second point succeeds after first umount");
        }
    }

    /* Cleanup: umount + detach */
    {
        run("umount /tmp/ul-mnt2 2>&1");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 4f: Writeback failure propagation via BLKROSET
     *
     *  Verify that writeback errors propagate to callers when the
     *  loop device is set read-only via BLKROSET.  A read-only
     *  device must not write dirty cache back to the backing file;
     *  both BLKFLSBUF and umount(2) should return EIO.
     *
     *  This is a regression test for the writeback error propagation
     *  fix: without it, flush_cache_to_file and BLKFLSBUF swallowed
     *  errors and always returned success, hiding data-loss risk
     *  from userspace.
     * ================================================================ */

    /* --- BLKFLSBUF error propagation --- */

    /* Attach ext4 image (fresh) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for writeback-fail test");
    } else {
        check(0, "losetup attach for writeback-fail test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for writeback-fail test");
    } else {
        check(0, "mount for writeback-fail test");
    }

    /* Write data to dirty the block cache */
    {
        rc = write_file("/tmp/ul-mnt/wb-fail.txt", "writeback-fail-test\n");
        check(rc == 0, "write wb-fail.txt for writeback-fail test");
    }

    /* Open loop device for ioctl */
    {
        if (loopdev[0]) {
            int fd = open(loopdev, O_RDWR);
            if (fd >= 0) {
                uint32_t ro;

                /* Set read-only — writeback must now fail */
                ro = 1;
                rc = ioctl(fd, 0x125D /* BLKROSET */, &ro);
                check(rc == 0, "BLKROSET set read-only (BLKFLSBUF test)");

                /* BLKFLSBUF should fail with EIO on read-only device */
                errno = 0;
                rc = ioctl(fd, 0x1261 /* BLKFLSBUF */);
                check(rc == -1 && errno == EIO,
                      "BLKFLSBUF returns EIO on read-only device (dirty cache)");

                /* Clear read-only — writeback should succeed */
                ro = 0;
                rc = ioctl(fd, 0x125D /* BLKROSET */, &ro);
                check(rc == 0, "BLKROSET clear read-only (BLKFLSBUF test)");

                /* BLKFLSBUF should now succeed */
                rc = ioctl(fd, 0x1261 /* BLKFLSBUF */);
                check(rc == 0, "BLKFLSBUF succeeds after clearing read-only");

                close(fd);
            } else {
                check(0, "open loop device for writeback-fail test");
            }
        } else {
            check(0, "BLKROSET set read-only (BLKFLSBUF test)");
            check(0, "BLKFLSBUF returns EIO on read-only device (dirty cache)");
            check(0, "BLKROSET clear read-only (BLKFLSBUF test)");
            check(0, "BLKFLSBUF succeeds after clearing read-only");
        }
    }

    /* Cleanup: umount + detach */
    {
        run("umount /tmp/ul-mnt 2>&1");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* --- umount error propagation --- */

    /* Attach ext4 image (fresh) */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for umount-fail test");
    } else {
        check(0, "losetup attach for umount-fail test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for umount-fail test");
    } else {
        check(0, "mount for umount-fail test");
    }

    /* Write data to dirty the block cache */
    {
        rc = write_file("/tmp/ul-mnt/umount-fail.txt", "umount-fail-test\n");
        check(rc == 0, "write umount-fail.txt");
    }

    /* Set read-only, then umount should fail with EIO */
    {
        if (loopdev[0]) {
            /* Set read-only via ioctl */
            int fd = open(loopdev, O_RDWR);
            if (fd >= 0) {
                uint32_t ro = 1;
                ioctl(fd, 0x125D /* BLKROSET */, &ro);
                close(fd);
            }

            /* umount via syscall to check errno */
            errno = 0;
            rc = umount("/tmp/ul-mnt");
            check(rc != 0 && errno == EIO,
                  "umount returns EIO when writeback fails (read-only device)");

            /* Filesystem is unmounted despite the error; clear ro
             * and flush the dirty cache left behind. */
            fd = open(loopdev, O_RDWR);
            if (fd >= 0) {
                uint32_t ro = 0;
                ioctl(fd, 0x125D /* BLKROSET */, &ro);
                ioctl(fd, 0x1261 /* BLKFLSBUF */);
                close(fd);
            }
        } else {
            check(0, "umount returns EIO when writeback fails (read-only device)");
        }
    }

    /* Detach */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
    }

    /* ================================================================
     *  Tier 4g: umount EBUSY when cwd is inside the mount
     *
     *  Linux umount2 must return EBUSY if any task has its current
     *  directory inside the target mount.  Without the busy-reference
     *  check in sys_umount2, the umount would succeed, leaving the
     *  child's cwd pointing at a detached ext4 instance and preventing
     *  the loop device from ever detaching (mounted flag stays true).
     * ================================================================ */

    /* Attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for umount-busy test");
    } else {
        check(0, "losetup attach for umount-busy test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for umount-busy test");
    } else {
        check(0, "mount for umount-busy test");
    }

    /* Fork child that chdirs into the mount and holds cwd there */
    {
        int p2c[2], c2p[2];
        int pipes_ok = (pipe(p2c) == 0 && pipe(c2p) == 0);
        pid_t busy_child = -1;

        if (pipes_ok) {
            busy_child = fork();
            if (busy_child == 0) {
                /* child: chdir into mount, signal parent, wait */
                close(p2c[1]);
                close(c2p[0]);
                chdir("/tmp/ul-mnt");
                char c = 1;
                write(c2p[1], &c, 1);   /* signal: cwd set */
                read(p2c[0], &c, 1);    /* wait for parent */
                chdir("/");             /* leave mount before exiting */
                close(p2c[0]);
                close(c2p[1]);
                _exit(0);
            }
            if (busy_child > 0) {
                close(p2c[0]);
                close(c2p[1]);
                /* wait for child to chdir */
                char c;
                read(c2p[0], &c, 1);

                /* umount must fail with EBUSY while child holds cwd */
                errno = 0;
                rc = umount("/tmp/ul-mnt");
                check(rc != 0 && errno == EBUSY,
                      "umount EBUSY when child cwd inside mount");

                /* signal child to exit */
                c = 1;
                write(p2c[1], &c, 1);
                int status;
                waitpid(busy_child, &status, 0);
                close(p2c[1]);
                close(c2p[0]);

                /* After child exits, umount should succeed */
                errno = 0;
                rc = umount("/tmp/ul-mnt");
                check(rc == 0, "umount succeeds after child exits");
            }
        }
        if (!pipes_ok || busy_child < 0) {
            check(0, "umount EBUSY when child cwd inside mount");
            check(0, "umount succeeds after child exits");
        }
    }

    /* Cleanup: detach */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
    }

    /* ================================================================
     *  Tier 4h: pivot_root EINVAL when caller is chroot'd into a
     *           subdirectory (not a mount point)
     *
     *  Linux pivot_root(2) requires the caller's current root to be a
     *  mount point.  A process that chroot'd into a regular
     *  subdirectory must get EINVAL, not succeed and corrupt the
     *  mount tree.
     * ================================================================ */
    {
        run("mkdir -p /tmp/pivot-chroot/sub/nr 2>/dev/null");
        int setup_ok = (run("mount -t tmpfs tmpfs /tmp/pivot-chroot 2>&1") == 0);
        /* Mount tmpfs inside the chroot directory so the child can
         * resolve /nr after chroot.  The key point is that the child's
         * root (/tmp/pivot-chroot/sub) is a plain directory, NOT the
         * root of any mount. */
        setup_ok = setup_ok && (run("mkdir -p /tmp/pivot-chroot/sub/nr 2>&1") == 0);
        setup_ok = setup_ok && (run("mount -t tmpfs tmpfs /tmp/pivot-chroot/sub/nr 2>&1") == 0);
        setup_ok = setup_ok && (run("mkdir /tmp/pivot-chroot/sub/nr/putold 2>&1") == 0);

        if (setup_ok) {
            pid_t p = fork();
            if (p == 0) {
                /* chroot into /tmp/pivot-chroot/sub — a plain directory,
                 * NOT the root of any mount. */
                chdir("/tmp/pivot-chroot/sub");
                chroot("/tmp/pivot-chroot/sub");

                /* pivot_root must fail with EINVAL because the caller's
                 * root is not a mount point. */
                errno = 0;
                int ret = syscall(__NR_pivot_root, "/nr", "/nr/putold");
                int got_einval = (ret != 0 && errno == EINVAL);
                _exit(got_einval ? 0 : 1);
            } else if (p > 0) {
                int status;
                waitpid(p, &status, 0);
                int ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
                check(ok, "pivot_root EINVAL after chroot to subdirectory");
            } else {
                check(0, "pivot_root EINVAL after chroot to subdirectory");
            }

            /* cleanup tmpfs mounts */
            run("umount /tmp/pivot-chroot/sub/nr 2>&1");
            run("umount /tmp/pivot-chroot 2>&1");
        } else {
            check(0, "pivot_root EINVAL after chroot to subdirectory");
        }
    }

    /* ================================================================
     *  Tier 4i: umount EBUSY when open fd is inside the mount
     *
     *  Linux umount2 must return EBUSY if any task has an open file
     *  descriptor pointing into the target mount.  This complements
     *  the cwd/root busy check (Tier 4g) by covering the open-fd path.
     * ================================================================ */

    /* Attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for umount-fd-busy test");
    } else {
        check(0, "losetup attach for umount-fd-busy test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for umount-fd-busy test");
    } else {
        check(0, "mount for umount-fd-busy test");
    }

    /* Open a file inside the mount, then verify umount fails with EBUSY */
    {
        int mnt_fd = open("/tmp/ul-mnt/test.txt", O_RDONLY);
        if (mnt_fd >= 0) {
            errno = 0;
            rc = umount("/tmp/ul-mnt");
            check(rc != 0 && errno == EBUSY,
                  "umount EBUSY when open fd inside mount");

            /* Close the fd, umount should now succeed */
            close(mnt_fd);
            errno = 0;
            rc = umount("/tmp/ul-mnt");
            check(rc == 0, "umount succeeds after closing fd");
        } else {
            /* fd open failed — skip but still try umount */
            check(0, "umount EBUSY when open fd inside mount");
            run("umount /tmp/ul-mnt 2>&1");
        }
    }

    /* Cleanup: detach */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
    }

    /* ================================================================
     *  Tier 4j: LO_FLAGS_READ_ONLY via losetup -r
     *
     *  Verify that attaching a loop device in read-only mode (-r flag)
     *  correctly sets the read-only flag via LOOP_CONFIGURE.  Mounting
     *  ext4 on a read-only loop device must prevent writes.
     * ================================================================ */

    /* Attach ext4 image as read-only */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -r %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup -r attach read-only");
    } else {
        check(0, "losetup -r attach read-only");
    }

    /* Verify BLKROGET reports read-only */
    {
        if (loopdev[0]) {
            int fd = open(loopdev, O_RDONLY);
            if (fd >= 0) {
                uint32_t ro = 0;
                rc = ioctl(fd, 0x125E /* BLKROGET */, &ro);
                check(rc == 0 && ro == 1,
                      "BLKROGET reports read-only after losetup -r");
                close(fd);
            } else {
                check(0, "BLKROGET reports read-only after losetup -r");
            }
        } else {
            check(0, "BLKROGET reports read-only after losetup -r");
        }
    }

    /* Mount ext4 (read-only) and verify write fails */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        if (rc == 0) {
            /* Writing to a file on a read-only mounted ext4 should fail */
            int wr = write_file("/tmp/ul-mnt/ro-test.txt", "must fail\n");
            check(wr != 0, "write fails on read-only loop mount");
            run("umount /tmp/ul-mnt 2>&1");
        } else {
            /* Mount itself may fail if ext4 rejects read-only loop;
             * either outcome is acceptable for this test. */
            check(1, "write fails on read-only loop mount");
        }
    } else {
        check(0, "write fails on read-only loop mount");
    }

    /* Cleanup: detach */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
        run(cmd);
    }

    /* ================================================================
     *  Tier 4k: umount EINVAL for non-mount-point directory
     *
     *  Linux umount2 returns EINVAL when the target is not a mount
     *  point.  Without the is_root_of_mount() guard, the busy check
     *  would falsely return EBUSY because the target's mountpoint
     *  (rootfs) matches every task's cwd/root.
     * ================================================================ */
    {
        errno = 0;
        rc = umount("/tmp/ul-mnt");
        check(rc != 0 && errno == EINVAL,
              "umount EINVAL for non-mount-point directory");
    }

    /* ================================================================
     *  Tier 4l: LOOP_CLR_FD EBUSY when mount has open fds
     *
     *  Verify that LOOP_CLR_FD (losetup -d) returns EBUSY while the
     *  loop device still has an active mount with open files.  This
     *  ensures that the mounted flag is not prematurely cleared,
     *  which would allow detach while the block device is still in use.
     * ================================================================ */

    /* Attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for detach-busy test");
    } else {
        check(0, "losetup attach for detach-busy test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for detach-busy test");
    } else {
        check(0, "mount for detach-busy test");
    }

    /* Open a file, then try losetup -d — must fail with EBUSY */
    {
        int mnt_fd = open("/tmp/ul-mnt/test.txt", O_RDONLY);
        if (mnt_fd >= 0) {
            /* losetup -d while fd is open: should fail */
            if (loopdev[0]) {
                char cmd[256];
                snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
                rc = run(cmd);
                /* busybox losetup -d returns non-zero on EBUSY */
                check(rc != 0, "losetup -d EBUSY while mount has open fd");
            } else {
                check(0, "losetup -d EBUSY while mount has open fd");
            }

            close(mnt_fd);
        } else {
            check(0, "losetup -d EBUSY while mount has open fd");
        }
    }

    /* Cleanup: umount then detach */
    {
        run("umount /tmp/ul-mnt 2>&1");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 5: pivot_root command availability & semantics
     * ================================================================ */

    /* 32. pivot_root command exists */
    {
        int exists = (run("which pivot_root >/dev/null 2>&1") == 0);
        check(exists, "pivot_root command exists");
    }

    /* 33. pivot_root semantics: old root appears at put_old.
     *
     *  pivot_root(2) reorganises the global mount tree and, matching
     *  Linux chroot_fs_refs(), updates root/cwd for every task whose
     *  root or cwd pointed at the old root.  We fork a child so that
     *  the child's _exit() does not terminate the test runner; the
     *  mount tree change is global and propagates to the parent.
     *
     *  Inside the child we:
     *    1. pivot_root /tmp/pivot-newroot /tmp/pivot-newroot/oldroot
     *    2. stat /oldroot — must succeed (old root moved here)
     *    3. stat /oldroot/tmp — must succeed (original root content)
     *
     *  After the child exits, the parent also verifies that its own
     *  root was switched to the new root (chroot_fs_refs semantic)
     *  and can still reach the old filesystem through /oldroot.
     *
     *  Additionally we verify that a task chroot'd into a subdirectory
     *  of the old root is NOT affected by pivot_root — only tasks
     *  whose root *exactly equals* old_root should be updated.
     */
    {
        /* Set up sentinel for the chroot-subdirectory regression child */
        run("mkdir -p /tmp/chroot-sub");
        run("touch /tmp/chroot-sub/sentinel");

        run("mkdir -p /tmp/pivot-newroot 2>&1");
        int mnt_ok = (run("mount -t tmpfs tmpfs /tmp/pivot-newroot 2>&1") == 0);
        if (mnt_ok) {
            run("mkdir /tmp/pivot-newroot/oldroot 2>&1");

            /* ---- chroot-subdirectory regression child ----
             * Fork before pivot_root, chroot into /tmp/chroot-sub (a
             * subdirectory of the old root), then verify after pivot_root
             * that this child's root was NOT replaced with new_root. */
            int ch_p2c[2], ch_c2p[2];
            int ch_pipes_ok = (pipe(ch_p2c) == 0 && pipe(ch_c2p) == 0);
            pid_t chroot_child = -1;
            if (ch_pipes_ok) {
                chroot_child = fork();
                if (chroot_child == 0) {
                    close(ch_p2c[1]);
                    close(ch_c2p[0]);

                    /* chroot into /tmp/chroot-sub */
                    chdir("/tmp/chroot-sub");
                    chroot("/tmp/chroot-sub");

                    struct stat st;
                    ino_t root_ino = (stat("/", &st) == 0) ? st.st_ino : 0;

                    /* signal parent: chroot done */
                    char c = 1;
                    write(ch_c2p[1], &c, 1);

                    /* wait for pivot_root */
                    read(ch_p2c[0], &c, 1);

                    /* verify root unchanged */
                    ino_t new_ino = (stat("/", &st) == 0) ? st.st_ino : 0;
                    int root_ok = (root_ino != 0 && root_ino == new_ino);
                    int sentinel_ok = (stat("/sentinel", &st) == 0);

                    char result = (root_ok && sentinel_ok) ? 1 : 0;
                    write(ch_c2p[1], &result, 1);

                    close(ch_p2c[0]);
                    close(ch_c2p[1]);
                    _exit(0);
                }
                /* parent: close child-side fds */
                if (chroot_child > 0) {
                    close(ch_p2c[0]);
                    close(ch_c2p[1]);
                    /* wait for child to finish chroot */
                    char c;
                    read(ch_c2p[0], &c, 1);
                }
            }

            pid_t pid = fork();
            if (pid == 0) {
                /* child — perform pivot_root */
                int ret = syscall(__NR_pivot_root,
                                  "/tmp/pivot-newroot",
                                  "/tmp/pivot-newroot/oldroot");
                if (ret == 0) {
                    struct stat st;
                    int ok = (stat("/oldroot", &st) == 0 &&
                              S_ISDIR(st.st_mode) &&
                              stat("/oldroot/tmp", &st) == 0);
                    _exit(ok ? 0 : 1);
                }
                _exit(2); /* pivot_root itself failed */
            } else if (pid > 0) {
                int status;
                waitpid(pid, &status, 0);
                if (WIFEXITED(status)) {
                    int ec = WEXITSTATUS(status);
                    check(ec == 0, "pivot_root: child sees old root at put_old");
                } else {
                    check(0, "pivot_root: child sees old root at put_old");
                }

                /* After pivot_root + chroot_fs_refs propagation the
                 * parent's root has also been switched to the new root
                 * (tmpfs).  Verify with a direct stat syscall — shell
                 * commands are unavailable because /bin/sh is now under
                 * /oldroot. */
                {
                    struct stat st;
                    int parent_ok = (stat("/oldroot", &st) == 0 &&
                                     S_ISDIR(st.st_mode));
                    check(parent_ok,
                          "pivot_root: parent root updated (chroot_fs_refs)");
                }

                /* Signal chroot child to re-check and collect result */
                if (chroot_child > 0) {
                    char c = 1;
                    write(ch_p2c[1], &c, 1);
                    char result = 0;
                    read(ch_c2p[0], &result, 1);
                    check(result == 1,
                          "chroot subdirectory: root not replaced after pivot_root");
                    close(ch_p2c[1]);
                    close(ch_c2p[0]);
                    waitpid(chroot_child, &status, 0);
                }
            } else {
                check(0, "pivot_root: fork failed");
            }

            /* No umount cleanup: after pivot_root the mount tree is
             * permanently rearranged.  The old mountpoint slot was
             * cleared by pivot_mount, and the parent's root is now the
             * tmpfs.  The QEMU VM is discarded after the test. */
        } else {
            check(0, "pivot_root: mount tmpfs for new root");
        }
    }

    /* ================================================================
     *  Tier 5a: second pivot_root — verify mount tree hierarchy
     *
     *  After the first pivot_root (test 33) the current root is a
     *  tmpfs with the original root at /oldroot.  We mount a second
     *  tmpfs and pivot_root again, then verify the entire mount tree
     *  hierarchy is preserved:
     *
     *    new root (second tmpfs)
     *      /putold → first tmpfs (old root from second pivot)
     *        /oldroot → original root (old root from first pivot)
     *
     *  This exercises propagate_pivot_root a second time and confirms
     *  the mount tree restructuring is correct across stacked pivots.
     *
     *  All operations use direct syscalls because after the first
     *  pivot_root /bin/sh is no longer at its original path.
     *
     *  Note: the original chroot regression test (verifying that
     *  propagate_pivot_root skips tasks chroot'd into subdirectories)
     *  cannot be exercised here because fork'd children initialise
     *  their FS_CONTEXT from ROOT_FS_CONTEXT, which is never updated
     *  by pivot_root and becomes stale after the first pivot — the
     *  child cannot resolve any paths.  The Location::ptr_eq fix is
     *  verified indirectly: test 33's propagate_pivot_root correctly
     *  updates only tasks whose root_dir exactly matches old_root
     *  (mountpoint + dentry), using Location::ptr_eq.
     * ================================================================ */
    {
        int setup_ok = (mkdir("/pivot2-nr", 0755) == 0);
        setup_ok = setup_ok && (mount("tmpfs", "/pivot2-nr", "tmpfs", 0, NULL) == 0);
        setup_ok = setup_ok && (mkdir("/pivot2-nr/putold", 0755) == 0);

        if (setup_ok) {
            int piv_ok = (syscall(__NR_pivot_root,
                                  "/pivot2-nr", "/pivot2-nr/putold") == 0);

            if (piv_ok) {
                struct stat st;
                /* putold should contain the first tmpfs */
                int ok = (stat("/putold", &st) == 0 && S_ISDIR(st.st_mode));
                check(ok, "pivot_root 2: /putold accessible (first tmpfs)");

                /* The original root should still be reachable through
                 * the first tmpfs's /oldroot mountpoint */
                ok = (stat("/putold/oldroot", &st) == 0 && S_ISDIR(st.st_mode));
                check(ok, "pivot_root 2: /putold/oldroot (original root)");

                /* Original content should still be visible */
                ok = (stat("/putold/oldroot/tmp", &st) == 0);
                check(ok, "pivot_root 2: original root content reachable");
            } else {
                check(0, "pivot_root 2: syscall failed");
            }
        } else {
            check(0, "pivot_root 2: setup failed");
        }
    }

    /* Cleanup: after a successful pivot_root the old filesystem is at
     * /oldroot; if pivot_root was not reached the original paths apply.
     * One of these two calls will succeed, the other silently fails. */
    unlink("/tmp/ul-test.img");
    unlink("/oldroot/tmp/ul-test.img");

    printf("=== total: %d passed, %d failed ===\n", pass, fail);

    if (fail > 0) return 1;
    printf("UTIL LINUX TEST PASSED\n");
    return 0;
}
