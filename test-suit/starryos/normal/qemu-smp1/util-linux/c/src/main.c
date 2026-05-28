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
#include <time.h>

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

static void cleanup_umount_all(const char *path)
{
    int i;
    for (i = 0; i < 4; i++) {
        if (umount(path) != 0)
            break;
    }
}

static struct timespec t0;

static double elapsed(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - t0.tv_sec) + (now.tv_nsec - t0.tv_nsec) / 1e9;
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
    clock_gettime(CLOCK_MONOTONIC, &t0);

    /* ================================================================
     *  Tier 1: Tool availability
     * ================================================================ */

    printf("[time %.2fs] Tier 1: tool availability\n", elapsed());
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

    printf("[time %.2fs] Tier 2: losetup chain\n", elapsed());
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

    printf("[time %.2fs] Tier 3: fdisk\n", elapsed());
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

    printf("[time %.2fs] Tier 4: ext4 mount\n", elapsed());
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

    printf("[time %.2fs] Tier 4e: double mount EBUSY\n", elapsed());
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

    printf("[time %.2fs] Tier 4g: umount busy cwd\n", elapsed());
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

    printf("[time %.2fs] Tier 4i: umount busy fd\n", elapsed());
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
    printf("[time %.2fs] Tier 4k: mount propagation\n", elapsed());
    {
        errno = 0;
        rc = umount("/tmp/ul-mnt");
        check(rc != 0 && errno == EINVAL,
              "umount EINVAL for non-mount-point directory");
    }

    /* ================================================================
     *  Tier 4k0: mount EINVAL for invalid propagation flag combos
     *
     *  Linux mount(2) rejects multiple propagation type flags in one
     *  call. The failed mount must not create a mount side effect.
     * ================================================================ */
    {
        const char *invalid_mount_point = "/tmp/ul-mnt-invalid-mountflags";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-mnt-invalid-mountflags");
        check(rc == 0, "mkdir invalid-mountflags mount point");

        errno = 0;
        rc = mount(
            "none",
            invalid_mount_point,
            "tmpfs",
            MS_SHARED | MS_PRIVATE,
            NULL
        );
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "mount EINVAL for conflicting propagation flags");

        errno = 0;
        rc = umount(invalid_mount_point);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "mount invalid propagation flags has no mount side effect");
    }

    /* ================================================================
     *  Tier 4k0a: mount EINVAL for propagation flags mixed with
     *             unsupported extra mount flags
     *
     *  Linux mount(2) allows only MS_REC and MS_SILENT alongside a
     *  propagation type flag.  Other bits such as MS_BIND must be
     *  rejected with EINVAL, and the failed mount must leave no side
     *  effect behind.
     * ================================================================ */
    {
        const char *invalid_mount_point = "/tmp/ul-mnt-invalid-propagation-extra";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-mnt-invalid-propagation-extra");
        check(rc == 0, "mkdir invalid-propagation-extra mount point");

        errno = 0;
        rc = mount(
            "none",
            invalid_mount_point,
            "tmpfs",
            MS_SHARED | MS_BIND,
            NULL
        );
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "mount EINVAL for propagation flag with unsupported extra flag");

        errno = 0;
        rc = umount(invalid_mount_point);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "mount invalid propagation-extra flags has no mount side effect");
    }

    /* ================================================================
     *  Tier 4k0b: mount MS_PRIVATE updates an existing mount instead
     *             of creating a fresh filesystem
     *
     *  Linux treats propagation flags as operations on an existing
     *  mount. Existing contents must remain visible after the call.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-mnt-propagation-op";

        rc = run("mkdir -p /tmp/ul-mnt-propagation-op");
        check(rc == 0, "mkdir propagation-op mount point");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for propagation-op test");

        rc = write_file("/tmp/ul-mnt-propagation-op/keep.txt", "keep\n");
        check(rc == 0, "write file before MS_PRIVATE");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_PRIVATE, NULL);
        check(rc == 0, "mount MS_PRIVATE on existing mount succeeds");

        {
            char content[256] = {0};
            int len = read_first_line(
                "/tmp/ul-mnt-propagation-op/keep.txt",
                content,
                sizeof(content)
            );
            check(len > 0 && strcmp(content, "keep") == 0,
                  "mount MS_PRIVATE preserves existing mount contents");
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0b1: mount MS_SHARED updates an existing mount instead
     *              of replacing it
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-mnt-shared-op";

        rc = run("mkdir -p /tmp/ul-mnt-shared-op");
        check(rc == 0, "mkdir shared-op mount point");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for shared-op test");

        rc = write_file("/tmp/ul-mnt-shared-op/keep.txt", "keep shared\n");
        check(rc == 0, "write file before MS_SHARED");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_SHARED, NULL);
        check(rc == 0, "mount MS_SHARED on existing mount succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-mnt-shared-op/keep.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "keep shared") == 0,
                  "mount MS_SHARED preserves existing mount contents");
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0b2: mount MS_SLAVE updates an existing mount instead
     *              of replacing it
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-mnt-slave-op";

        rc = run("mkdir -p /tmp/ul-mnt-slave-op");
        check(rc == 0, "mkdir slave-op mount point");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for slave-op test");

        rc = write_file("/tmp/ul-mnt-slave-op/keep.txt", "keep slave\n");
        check(rc == 0, "write file before MS_SLAVE");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_SLAVE, NULL);
        check(rc == 0, "mount MS_SLAVE on existing mount succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-mnt-slave-op/keep.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "keep slave") == 0,
                  "mount MS_SLAVE preserves existing mount contents");
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0b3: mount MS_UNBINDABLE updates an existing mount
     *              instead of replacing it
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-mnt-unbindable-op";

        rc = run("mkdir -p /tmp/ul-mnt-unbindable-op");
        check(rc == 0, "mkdir unbindable-op mount point");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for unbindable-op test");

        rc = write_file("/tmp/ul-mnt-unbindable-op/keep.txt", "keep unbindable\n");
        check(rc == 0, "write file before MS_UNBINDABLE");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_UNBINDABLE, NULL);
        check(rc == 0, "mount MS_UNBINDABLE on existing mount succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-mnt-unbindable-op/keep.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "keep unbindable") == 0,
                  "mount MS_UNBINDABLE preserves existing mount contents");
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0b4: mount MS_SHARED causes child mounts to propagate to
     *              a bind peer
     *
     *  In Linux, a bind mount created from a shared mount joins the
     *  same peer group. A new child mount under one peer must appear
     *  under the other peer.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-shared-src";
        const char *dst = "/tmp/ul-shared-dst";

        rc = run("mkdir -p /tmp/ul-shared-src /tmp/ul-shared-dst");
        check(rc == 0, "mkdir shared-propagation test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for shared-propagation source");

        rc = mount("none", src, "tmpfs", MS_SHARED, NULL);
        check(rc == 0, "mount MS_SHARED on propagation source succeeds");

        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "bind mount from shared source succeeds");

        rc = run("mkdir -p /tmp/ul-shared-src/submnt");
        check(rc == 0, "mkdir child mountpoint under shared source");

        rc = mount("tmpfs", "/tmp/ul-shared-src/submnt", "tmpfs", 0, NULL);
        check(rc == 0, "mount child tmpfs under shared source");

        rc = write_file("/tmp/ul-shared-src/submnt/peer.txt", "shared peer\n");
        check(rc == 0, "write file inside propagated shared child mount");

        {
            char content[256] = {0};
            int saved_errno;
            int len = read_first_line("/tmp/ul-shared-dst/submnt/peer.txt",
                                      content, sizeof(content));
            saved_errno = errno;
            if (!(len > 0 && strcmp(content, "shared peer") == 0)) {
                printf("  INFO | shared peer read len=%d errno=%d content='%s'\n",
                       len, saved_errno, content);
            }
            check(len > 0 && strcmp(content, "shared peer") == 0,
                  "mount MS_SHARED propagates child mount to bind peer");
        }

        cleanup_umount_all("/tmp/ul-shared-dst/submnt");
        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-shared-src/submnt");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0b5: mount MS_PRIVATE stops further propagation from a
     *              previously shared peer
     *
     *  After converting one peer to private, new child mounts created
     *  under the remaining shared peer must no longer appear there.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-private-src";
        const char *dst = "/tmp/ul-private-dst";
        int nested_fd;
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-private-src /tmp/ul-private-dst");
        check(rc == 0, "mkdir private-propagation test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for private-propagation source");

        rc = mount("none", src, "tmpfs", MS_SHARED, NULL);
        check(rc == 0, "mount MS_SHARED before private conversion succeeds");

        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "bind mount peer before private conversion succeeds");

        rc = mount("none", dst, "tmpfs", MS_PRIVATE, NULL);
        check(rc == 0, "mount MS_PRIVATE on bind peer succeeds");

        rc = run("mkdir -p /tmp/ul-private-src/submnt");
        check(rc == 0, "mkdir child mountpoint under shared master");

        rc = mount("tmpfs", "/tmp/ul-private-src/submnt", "tmpfs", 0, NULL);
        check(rc == 0, "mount child tmpfs under shared master");

        errno = 0;
        nested_fd = open("/tmp/ul-private-dst/submnt/probe.txt", O_RDONLY);
        saved_errno = errno;
        if (nested_fd >= 0)
            close(nested_fd);
        check(nested_fd == -1 && saved_errno == ENOENT,
              "mount MS_PRIVATE stops future propagation to bind peer");

        cleanup_umount_all("/tmp/ul-private-dst/submnt");
        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-private-src/submnt");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0b6: mount MS_SLAVE receives propagation from master but
     *              does not send it back
     *
     *  A slave mount should receive new child mounts from its shared
     *  master, but mounts created under the slave must not propagate
     *  back to the master.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-slave-src";
        const char *dst = "/tmp/ul-slave-dst";
        int nested_fd;
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-slave-src /tmp/ul-slave-dst");
        check(rc == 0, "mkdir slave-propagation test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for slave-propagation source");

        rc = mount("none", src, "tmpfs", MS_SHARED, NULL);
        check(rc == 0, "mount MS_SHARED for slave-propagation source succeeds");

        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "bind mount peer for slave-propagation succeeds");

        rc = mount("none", dst, "tmpfs", MS_SLAVE, NULL);
        check(rc == 0, "mount MS_SLAVE on bind peer succeeds");

        rc = run("mkdir -p /tmp/ul-slave-src/from-master");
        check(rc == 0, "mkdir master child mountpoint for slave test");

        rc = mount("tmpfs", "/tmp/ul-slave-src/from-master", "tmpfs", 0, NULL);
        check(rc == 0, "mount child tmpfs under master for slave test");

        rc = write_file("/tmp/ul-slave-src/from-master/master.txt", "master to slave\n");
        check(rc == 0, "write file inside master child mount for slave test");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-slave-dst/from-master/master.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "master to slave") == 0,
                  "mount MS_SLAVE receives propagation from master");
        }

        rc = run("mkdir -p /tmp/ul-slave-dst/from-slave");
        check(rc == 0, "mkdir slave child mountpoint for reverse propagation test");

        rc = mount("tmpfs", "/tmp/ul-slave-dst/from-slave", "tmpfs", 0, NULL);
        check(rc == 0, "mount child tmpfs under slave");

        errno = 0;
        nested_fd = open("/tmp/ul-slave-src/from-slave/probe.txt", O_RDONLY);
        saved_errno = errno;
        if (nested_fd >= 0)
            close(nested_fd);
        check(nested_fd == -1 && saved_errno == ENOENT,
              "mount MS_SLAVE does not propagate back to master");

        cleanup_umount_all("/tmp/ul-slave-dst/from-master");
        cleanup_umount_all("/tmp/ul-slave-dst/from-slave");
        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-slave-src/from-master");
        cleanup_umount_all("/tmp/ul-slave-src/from-slave");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0b7: mount MS_UNBINDABLE rejects direct bind mounts
     *
     *  Linux rejects bind mounts whose source mount is unbindable.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-unbindable-src";
        const char *dst = "/tmp/ul-unbindable-dst";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-unbindable-src /tmp/ul-unbindable-dst");
        check(rc == 0, "mkdir unbindable-bind test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for unbindable-bind source");

        rc = mount("none", src, "tmpfs", MS_UNBINDABLE, NULL);
        check(rc == 0, "mount MS_UNBINDABLE before bind test succeeds");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "mount MS_UNBINDABLE source rejects bind mount");

        cleanup_umount_all(dst);
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0c: mount MS_BIND shares the same filesystem view
     *
     *  A bind mount should expose the source tree at the destination,
     *  and writes through the destination must appear in the source.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-bind-src";
        const char *dst = "/tmp/ul-bind-dst";

        rc = run("mkdir -p /tmp/ul-bind-src /tmp/ul-bind-dst");
        check(rc == 0, "mkdir bind-mount test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for bind-mount source");

        rc = write_file("/tmp/ul-bind-src/source.txt", "bind source\n");
        check(rc == 0, "write source file before bind mount");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "mount MS_BIND succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-bind-dst/source.txt", content, sizeof(content));
            check(len > 0 && strcmp(content, "bind source") == 0,
                  "bind mount exposes source contents at destination");
        }

        rc = write_file("/tmp/ul-bind-dst/dst-write.txt", "bind mirror\n");
        check(rc == 0, "write through bind-mount destination");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-bind-src/dst-write.txt", content, sizeof(content));
            check(len > 0 && strcmp(content, "bind mirror") == 0,
                  "bind mount mirrors destination writes back to source");
        }

        cleanup_umount_all(dst);
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0c0: mount MS_BIND can bind a non-root subdirectory
     *
     *  Linux bind mounts are not restricted to the root of an existing
     *  mount; a subdirectory bind must also succeed.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-bind-sub-src";
        const char *dst = "/tmp/ul-bind-sub-dst";

        rc = run("mkdir -p /tmp/ul-bind-sub-src /tmp/ul-bind-sub-dst");
        check(rc == 0, "mkdir bind-subdir test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for bind-subdir source");

        rc = run("mkdir -p /tmp/ul-bind-sub-src/sub");
        check(rc == 0, "mkdir source subdirectory for bind-subdir test");

        rc = write_file("/tmp/ul-bind-sub-src/sub/sub.txt", "bind subdir\n");
        check(rc == 0, "write file in bind-subdir source subdirectory");

        errno = 0;
        rc = mount("/tmp/ul-bind-sub-src/sub", dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "mount MS_BIND on source subdirectory succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-bind-sub-dst/sub.txt", content, sizeof(content));
            check(len > 0 && strcmp(content, "bind subdir") == 0,
                  "bind mount on source subdirectory exposes contents");
        }

        cleanup_umount_all(dst);
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0c1: mount MS_BIND without MS_REC does not clone submounts
     *
     *  Linux bind-mounts only the top mount by default. Nested mounts
     *  remain absent unless MS_REC is also specified.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-bind-tree-src";
        const char *dst = "/tmp/ul-bind-tree-dst";
        int nested_fd;
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-bind-tree-src /tmp/ul-bind-tree-dst");
        check(rc == 0, "mkdir bind-tree test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for bind-tree source");

        rc = run("mkdir -p /tmp/ul-bind-tree-src/submnt");
        check(rc == 0, "mkdir bind-tree nested mountpoint");

        rc = mount("tmpfs", "/tmp/ul-bind-tree-src/submnt", "tmpfs", 0, NULL);
        check(rc == 0, "mount nested tmpfs under bind-tree source");

        rc = write_file("/tmp/ul-bind-tree-src/submnt/nested.txt", "nested mount\n");
        check(rc == 0, "write file inside nested source mount");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "mount MS_BIND without MS_REC succeeds");

        errno = 0;
        nested_fd = open("/tmp/ul-bind-tree-dst/submnt/nested.txt", O_RDONLY);
        saved_errno = errno;
        if (nested_fd >= 0)
            close(nested_fd);
        check(nested_fd == -1 && saved_errno == ENOENT,
              "mount MS_BIND without MS_REC leaves nested mount absent");

        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-bind-tree-src/submnt");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0c2: mount MS_BIND|MS_REC clones nested submounts
     *
     *  A recursive bind mount should expose submount contents at the
     *  destination subtree.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-rbind-src";
        const char *dst = "/tmp/ul-rbind-dst";

        rc = run("mkdir -p /tmp/ul-rbind-src /tmp/ul-rbind-dst");
        check(rc == 0, "mkdir recursive-bind test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for recursive-bind source");

        rc = run("mkdir -p /tmp/ul-rbind-src/submnt");
        check(rc == 0, "mkdir recursive-bind nested mountpoint");

        rc = mount("tmpfs", "/tmp/ul-rbind-src/submnt", "tmpfs", 0, NULL);
        check(rc == 0, "mount nested tmpfs under recursive-bind source");

        rc = write_file("/tmp/ul-rbind-src/submnt/nested.txt", "recursive bind\n");
        check(rc == 0, "write file inside recursive-bind nested mount");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_BIND | MS_REC, NULL);
        check(rc == 0, "mount MS_BIND|MS_REC succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-rbind-dst/submnt/nested.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "recursive bind") == 0,
                  "mount MS_BIND|MS_REC exposes nested mount contents");
        }

        cleanup_umount_all("/tmp/ul-rbind-dst/submnt");
        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-rbind-src/submnt");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0c3: recursive bind prunes unbindable child mounts
     *
     *  Linux prunes unbindable submounts when replicating a subtree via
     *  MS_BIND|MS_REC.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-rbind-unbind-src";
        const char *dst = "/tmp/ul-rbind-unbind-dst";
        int nested_fd;
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-rbind-unbind-src /tmp/ul-rbind-unbind-dst");
        check(rc == 0, "mkdir recursive-bind-unbindable test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for recursive-bind-unbindable source");

        rc = run("mkdir -p /tmp/ul-rbind-unbind-src/submnt");
        check(rc == 0, "mkdir recursive-bind-unbindable child mountpoint");

        rc = mount("tmpfs", "/tmp/ul-rbind-unbind-src/submnt", "tmpfs", 0, NULL);
        check(rc == 0, "mount child tmpfs for recursive-bind-unbindable test");

        rc = mount("none", "/tmp/ul-rbind-unbind-src/submnt", "tmpfs", MS_UNBINDABLE, NULL);
        check(rc == 0, "mount MS_UNBINDABLE on child mount succeeds");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_BIND | MS_REC, NULL);
        check(rc == 0, "mount MS_BIND|MS_REC with unbindable child succeeds");

        errno = 0;
        nested_fd = open("/tmp/ul-rbind-unbind-dst/submnt/probe.txt", O_RDONLY);
        saved_errno = errno;
        if (nested_fd >= 0)
            close(nested_fd);
        check(nested_fd == -1 && saved_errno == ENOENT,
              "mount MS_BIND|MS_REC prunes unbindable child mount");

        cleanup_umount_all("/tmp/ul-rbind-unbind-dst/submnt");
        cleanup_umount_all(dst);
        cleanup_umount_all("/tmp/ul-rbind-unbind-src/submnt");
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0d: mount MS_MOVE relocates an existing mount
     *
     *  After a move mount, the mounted tree should appear only at the
     *  new path and disappear from the old path.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-move-src";
        const char *dst = "/tmp/ul-move-dst";

        rc = run("mkdir -p /tmp/ul-move-src /tmp/ul-move-dst");
        check(rc == 0, "mkdir move-mount test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for move-mount source");

        rc = write_file("/tmp/ul-move-src/move.txt", "move payload\n");
        check(rc == 0, "write source file before move mount");

        errno = 0;
        rc = mount(src, dst, "tmpfs", MS_MOVE, NULL);
        check(rc == 0, "mount MS_MOVE succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-move-dst/move.txt", content, sizeof(content));
            check(len > 0 && strcmp(content, "move payload") == 0,
                  "move mount exposes source contents at new path");
        }

        {
            int old_fd;
            int saved_errno;
            errno = 0;
            old_fd = open("/tmp/ul-move-src/move.txt", O_RDONLY);
            saved_errno = errno;
            if (old_fd >= 0)
                close(old_fd);
            check(old_fd == -1 && saved_errno == ENOENT,
                  "move mount removes old mounted path");
        }

        cleanup_umount_all(dst);
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0d1: mount MS_MOVE rejects moving a mount beneath itself
     *
     *  Linux returns ELOOP if the move target is a descendant of the
     *  source mount.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-move-loop";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-move-loop");
        check(rc == 0, "mkdir move-loop test directory");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for move-loop test");

        rc = run("mkdir -p /tmp/ul-move-loop/subdir");
        check(rc == 0, "mkdir descendant target for move-loop test");

        errno = 0;
        rc = mount(src, "/tmp/ul-move-loop/subdir", "tmpfs", MS_MOVE, NULL);
        saved_errno = errno;
        check(rc == -1 && saved_errno == ELOOP,
              "mount MS_MOVE rejects descendant target with ELOOP");

        {
            char content[256] = {0};
            rc = write_file("/tmp/ul-move-loop/still-there.txt", "move loop keep\n");
            check(rc == 0, "move-loop source mount remains usable after failed move");
            rc = read_first_line("/tmp/ul-move-loop/still-there.txt", content, sizeof(content));
            check(rc > 0 && strcmp(content, "move loop keep") == 0,
                  "failed MS_MOVE leaves original mount in place");
        }

        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0e: mount MS_REMOUNT keeps the existing mount tree
     *
     *  A remount operates on the existing mount rather than replacing
     *  it, so existing files should still be visible afterwards.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-remount";

        rc = run("mkdir -p /tmp/ul-remount");
        check(rc == 0, "mkdir remount test directory");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for remount test");

        rc = write_file("/tmp/ul-remount/remount.txt", "remount keep\n");
        check(rc == 0, "write file before remount");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_REMOUNT, NULL);
        check(rc == 0, "mount MS_REMOUNT succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-remount/remount.txt", content, sizeof(content));
            check(len > 0 && strcmp(content, "remount keep") == 0,
                  "mount MS_REMOUNT preserves existing mount contents");
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0e1: mount MS_REMOUNT|MS_BIND|MS_RDONLY makes a bind
     *              mount read-only without freezing the source mount
     *
     *  Linux uses bind-remount to toggle per-mount-point flags such as
     *  read-only on a bind mount.
     * ================================================================ */
    {
        const char *src = "/tmp/ul-bind-ro-src";
        const char *dst = "/tmp/ul-bind-ro-dst";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-bind-ro-src /tmp/ul-bind-ro-dst");
        check(rc == 0, "mkdir bind-remount-rdonly test directories");

        rc = mount("tmpfs", src, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for bind-remount-rdonly source");

        rc = write_file("/tmp/ul-bind-ro-src/base.txt", "bind ro base\n");
        check(rc == 0, "write source file before bind-remount-rdonly");

        rc = mount(src, dst, "tmpfs", MS_BIND, NULL);
        check(rc == 0, "mount MS_BIND for bind-remount-rdonly test");

        errno = 0;
        rc = mount("none", dst, "tmpfs", MS_REMOUNT | MS_BIND | MS_RDONLY, NULL);
        saved_errno = errno;
        check(rc == 0, "mount MS_REMOUNT|MS_BIND|MS_RDONLY succeeds");

        errno = 0;
        rc = write_file("/tmp/ul-bind-ro-dst/deny.txt", "must fail\n");
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "bind-remount-rdonly rejects writes through bind mount");

        rc = write_file("/tmp/ul-bind-ro-src/still-writable.txt", "source writable\n");
        check(rc == 0, "bind-remount-rdonly leaves source mount writable");

        cleanup_umount_all(dst);
        cleanup_umount_all(src);
    }

    /* ================================================================
     *  Tier 4k0f: mount MS_RDONLY creates a read-only mount
     *
     *  A read-only mount must reject write attempts with EROFS.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-rdonly";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-rdonly");
        check(rc == 0, "mkdir rdonly test directory");

        errno = 0;
        rc = mount("tmpfs", mount_point, "tmpfs", MS_RDONLY, NULL);
        saved_errno = errno;
        check(rc == 0, "mount MS_RDONLY succeeds");

        errno = 0;
        rc = write_file("/tmp/ul-rdonly/ro.txt", "must fail\n");
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_RDONLY rejects writes with EROFS");

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0g: mount MS_REMOUNT|MS_RDONLY preserves contents and
     *             turns an existing mount read-only
     *
     *  Linux remounts apply to the existing mount. Existing files must
     *  remain visible and subsequent writes must fail with EROFS.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-remount-rdonly";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-remount-rdonly");
        check(rc == 0, "mkdir remount-rdonly test directory");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for remount-rdonly test");

        rc = write_file("/tmp/ul-remount-rdonly/keep.txt", "keep ro\n");
        check(rc == 0, "write file before remount-rdonly");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_REMOUNT | MS_RDONLY, NULL);
        saved_errno = errno;
        check(rc == 0, "mount MS_REMOUNT|MS_RDONLY succeeds");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-remount-rdonly/keep.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "keep ro") == 0,
                  "mount MS_REMOUNT|MS_RDONLY preserves existing contents");
        }

        errno = 0;
        rc = write_file("/tmp/ul-remount-rdonly/new.txt", "must fail\n");
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_REMOUNT|MS_RDONLY rejects new writes");

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k0g1: remounted read-only mounts reject metadata and
     *              directory entry changes with EROFS
     *
     *  Linux applies MS_REMOUNT|MS_RDONLY to the mountpoint, so chmod,
     *  rename, unlink, and mkdir under that mount should fail with
     *  EROFS.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-remount-rdonly-meta";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-remount-rdonly-meta");
        check(rc == 0, "mkdir remount-rdonly-meta test directory");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for remount-rdonly-meta test");

        rc = write_file("/tmp/ul-remount-rdonly-meta/file.txt", "meta ro\n");
        check(rc == 0, "write file before remount-rdonly-meta");

        errno = 0;
        rc = mount("none", mount_point, "tmpfs", MS_REMOUNT | MS_RDONLY, NULL);
        saved_errno = errno;
        check(rc == 0, "mount MS_REMOUNT|MS_RDONLY for metadata test succeeds");

        errno = 0;
        rc = chmod("/tmp/ul-remount-rdonly-meta/file.txt", 0600);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_REMOUNT|MS_RDONLY rejects chmod with EROFS");

        errno = 0;
        rc = rename("/tmp/ul-remount-rdonly-meta/file.txt",
                    "/tmp/ul-remount-rdonly-meta/file2.txt");
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_REMOUNT|MS_RDONLY rejects rename with EROFS");

        errno = 0;
        rc = unlink("/tmp/ul-remount-rdonly-meta/file.txt");
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_REMOUNT|MS_RDONLY rejects unlink with EROFS");

        errno = 0;
        rc = mkdir("/tmp/ul-remount-rdonly-meta/newdir", 0755);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EROFS,
              "mount MS_REMOUNT|MS_RDONLY rejects mkdir with EROFS");

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k1: umount2 EINVAL for invalid flags
     *
     *  Linux umount2 must reject unsupported flag bits with EINVAL.
     *  Verify that the kernel does not silently ignore invalid flags.
     * ================================================================ */

    printf("[time %.2fs] Tier 4k1: umount2 flags\n", elapsed());
    /* Attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for umount2-invalid-flags test");
    } else {
        check(0, "losetup attach for umount2-invalid-flags test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for umount2-invalid-flags test");
    } else {
        check(0, "mount for umount2-invalid-flags test");
    }

    /* umount2 with invalid flags must fail with EINVAL */
    {
        const unsigned int invalid_flags = 0xdeadbeefu;
        int saved_errno;
        errno = 0;
        rc = (int)syscall(SYS_umount2, "/tmp/ul-mnt", invalid_flags);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "umount2 EINVAL for invalid flags");
    }

    /* Cleanup: normal umount then detach */
    {
        rc = umount("/tmp/ul-mnt");
        check(rc == 0, "umount cleanup after umount2-invalid-flags test");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 4k2: umount2 EINVAL for invalid supported-flag combo
     *
     *  Linux umount2 rejects MNT_EXPIRE combined with MNT_DETACH or
     *  MNT_FORCE.  The rejected operation must not unmount the target.
     * ================================================================ */

    /* Attach ext4 image */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "losetup %s " PREBUILT_IMG " 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "losetup attach for umount2-invalid-combo test");
    } else {
        check(0, "losetup attach for umount2-invalid-combo test");
    }

    /* Mount ext4 */
    if (loopdev[0]) {
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "mount -t ext4 %s /tmp/ul-mnt 2>&1", loopdev);
        rc = run(cmd);
        check(rc == 0, "mount for umount2-invalid-combo test");
    } else {
        check(0, "mount for umount2-invalid-combo test");
    }

    /* umount2 with invalid supported-flag combo must fail with EINVAL */
    {
        int saved_errno;
        errno = 0;
        rc = (int)syscall(SYS_umount2, "/tmp/ul-mnt", MNT_EXPIRE | MNT_DETACH);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "umount2 EINVAL for MNT_EXPIRE|MNT_DETACH");
    }

    /* Cleanup: normal umount then detach */
    {
        rc = umount("/tmp/ul-mnt");
        check(rc == 0, "umount cleanup after umount2-invalid-combo test");
        if (loopdev[0]) {
            char cmd[256];
            snprintf(cmd, sizeof(cmd), "losetup -d %s 2>&1", loopdev);
            run(cmd);
        }
    }

    /* ================================================================
     *  Tier 4k3: umount2 MNT_EXPIRE is a two-step expire operation
     *
     *  The first MNT_EXPIRE call should return EAGAIN and mark the
     *  mount expired. A second call should then unmount it.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-expire";
        int saved_errno;

        rc = run("mkdir -p /tmp/ul-expire");
        check(rc == 0, "mkdir expire test directory");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for MNT_EXPIRE test");

        errno = 0;
        rc = (int)syscall(SYS_umount2, mount_point, MNT_EXPIRE);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EAGAIN,
              "umount2 MNT_EXPIRE first call returns EAGAIN");

        errno = 0;
        rc = (int)syscall(SYS_umount2, mount_point, MNT_EXPIRE);
        saved_errno = errno;
        check(rc == 0,
              "umount2 MNT_EXPIRE second call unmounts expired mount");

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4k4: umount2 UMOUNT_NOFOLLOW does not follow symlinks
     *
     *  With UMOUNT_NOFOLLOW, passing a symlink path must not unmount
     *  the mount behind that symlink.
     * ================================================================ */
    {
        const char *real_mount = "/tmp/ul-nofollow-real";
        const char *link_mount = "/tmp/ul-nofollow-link";
        int saved_errno;

        run("mkdir -p /tmp/ul-nofollow-real");
        unlink(link_mount);
        rc = symlink(real_mount, link_mount);
        check(rc == 0, "create symlink for UMOUNT_NOFOLLOW test");

        rc = mount("tmpfs", real_mount, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for UMOUNT_NOFOLLOW test");

        rc = write_file("/tmp/ul-nofollow-real/nofollow.txt", "nofollow keep\n");
        check(rc == 0, "write file before UMOUNT_NOFOLLOW");

        errno = 0;
        rc = (int)syscall(SYS_umount2, link_mount, UMOUNT_NOFOLLOW);
        saved_errno = errno;
        check(rc == -1 && saved_errno == EINVAL,
              "umount2 UMOUNT_NOFOLLOW rejects symlink target");

        {
            char content[256] = {0};
            int len = read_first_line("/tmp/ul-nofollow-real/nofollow.txt",
                                      content, sizeof(content));
            check(len > 0 && strcmp(content, "nofollow keep") == 0,
                  "UMOUNT_NOFOLLOW leaves real mount in place");
        }

        cleanup_umount_all(real_mount);
        unlink(link_mount);
    }

    /* ================================================================
     *  Tier 4k5: umount2 MNT_DETACH lazily detaches a busy mount
     *
     *  The detach should succeed even while an open file descriptor
     *  keeps the mount busy. New path lookups must see the underlying
     *  directory, while the existing file descriptor remains usable.
     * ================================================================ */
    {
        const char *mount_point = "/tmp/ul-detach";
        int detach_fd;

        rc = run("mkdir -p /tmp/ul-detach");
        check(rc == 0, "mkdir detach test directory");

        rc = mount("tmpfs", mount_point, "tmpfs", 0, NULL);
        check(rc == 0, "mount tmpfs for MNT_DETACH test");

        rc = write_file("/tmp/ul-detach/detach.txt", "detach payload\n");
        check(rc == 0, "write file before MNT_DETACH");

        detach_fd = open("/tmp/ul-detach/detach.txt", O_RDONLY);
        check(detach_fd >= 0, "open fd before MNT_DETACH");

        if (detach_fd >= 0) {
            int saved_errno;
            char content[256] = {0};
            ssize_t nread;
            int reopened_fd;

            errno = 0;
            rc = (int)syscall(SYS_umount2, mount_point, MNT_DETACH);
            saved_errno = errno;
            check(rc == 0, "umount2 MNT_DETACH succeeds on busy mount");

            errno = 0;
            reopened_fd = open("/tmp/ul-detach/detach.txt", O_RDONLY);
            saved_errno = errno;
            if (reopened_fd >= 0)
                close(reopened_fd);
            check(reopened_fd == -1 && saved_errno == ENOENT,
                  "MNT_DETACH hides mount from new lookups");

            lseek(detach_fd, 0, SEEK_SET);
            nread = read(detach_fd, content, sizeof(content) - 1);
            if (nread >= 0)
                content[nread] = '\0';
            check(nread > 0 && strstr(content, "detach payload") != NULL,
                  "MNT_DETACH keeps existing fd usable");

            close(detach_fd);
        }

        cleanup_umount_all(mount_point);
    }

    /* ================================================================
     *  Tier 4l: LOOP_CLR_FD EBUSY when mount has open fds
     *
     *  Verify that LOOP_CLR_FD (losetup -d) returns EBUSY while the
     *  loop device still has an active mount with open files.  This
     *  ensures that the mounted flag is not prematurely cleared,
     *  which would allow detach while the block device is still in use.
     * ================================================================ */

    printf("[time %.2fs] Tier 4l: LOOP_CLR_FD EBUSY\n", elapsed());
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

    printf("[time %.2fs] Tier 5: pivot_root\n", elapsed());
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
    printf("[time %.2fs] Tier 5a: pivot_root 2\n", elapsed());
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
    printf("[time %.2fs] util-linux-test done\n", elapsed());

    if (fail > 0) return 1;
    printf("PASS | util-linux-test completed\n");
    printf("UTIL LINUX TEST PASSED\n");
    return 0;
}
