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
     */
    {
        run("mkdir -p /tmp/pivot-newroot 2>&1");
        int mnt_ok = (run("mount -t tmpfs tmpfs /tmp/pivot-newroot 2>&1") == 0);
        if (mnt_ok) {
            run("mkdir /tmp/pivot-newroot/oldroot 2>&1");

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
